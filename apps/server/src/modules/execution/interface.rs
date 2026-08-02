//! Turn execution loop (M3 Stage 4).

use std::{collections::HashSet, future::Future};

use crate::modules::models::interface::{
    ChatMessage, ChatRole, CompletedToolCall, ContentPart, ModelRequest, ModelStreamEvent,
    ModelsInterface, StreamChannel, ToolSpec,
};
use crate::modules::projects::interface::ProjectsInterface;
use crate::modules::runtime::interface::{JobProjection, JobStatus};
use crate::modules::sessions::interface::{
    ActiveTurnOutcome, AppendAssistantMessage, ExecutionTurn, ModelAttemptStatus,
    SessionsInterface, TurnModelAttempt, TurnStatus,
};
use crate::platform::id::{AskId, AttemptId, JobId, RoundId, SessionId, ToolCallId, TurnId};
use janus_infrastructure::clock::now_utc_str;
use janus_infrastructure::{
    events::{EventStore, NewEvent},
    id::CorrelationId,
    managed_storage::BlobStore,
    unit_of_work::UnitOfWork,
};
use janus_workspace::interface::WorkspaceInterface;
use serde_json::{Value, json};
use sqlx::{SqliteConnection, SqlitePool};

use super::context::SYSTEM_PROMPT;
use super::registry::{SCHEMA_VERSION, available_tools};
use super::retry::{FaultClass, MAX_ATTEMPTS_PER_CANDIDATE, classify, classify_fault};
use super::tools::{
    ToolContext, attach_tool_display, execute_tool, read_attachment_bytes, supported_image_mime,
};
pub use super::types::{
    AskAnswer, AskAnswerDisposition, AskClosure, AskMode, AskRequest, AskStatus, CompletionSummary,
    ExecutionError, ExpiredAsk, ToolCallSettlement, ToolCallStatus, ToolExecutionDisposition,
    ToolOutcome, ToolResultPart, TurnExecutionOutcome, TurnWait,
};

const MAX_ROUNDS: usize = 12;

#[derive(Debug, Clone)]
pub struct ContextUsageView {
    pub estimated_input_tokens: i64,
    pub context_limit: i64,
    pub compact_status: String,
    pub created_at: String,
}

#[derive(Clone)]
struct AcceptedToolCall {
    id: ToolCallId,
    ordinal: i64,
    request: CompletedToolCall,
}

struct ExecutedToolCall {
    outcome: ToolOutcome,
    message: ChatMessage,
}

enum CompleteTurnOutcome {
    Completed,
    WaitingForJob,
}

#[derive(Clone)]
pub struct ExecutionInterface {
    pool: SqlitePool,
    events: EventStore,
    unit_of_work: UnitOfWork,
    models: ModelsInterface,
    projects: ProjectsInterface,
    workspace: WorkspaceInterface,
    sessions: SessionsInterface,
    blobs: BlobStore,
    runtime: crate::modules::runtime::interface::RuntimeInterface,
}

impl ExecutionInterface {
    // The composition root injects each capability explicitly; bundling these
    // dependencies would hide the execution boundary behind a service locator.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: SqlitePool,
        events: EventStore,
        models: ModelsInterface,
        projects: ProjectsInterface,
        workspace: WorkspaceInterface,
        sessions: SessionsInterface,
        blobs: BlobStore,
        runtime: crate::modules::runtime::interface::RuntimeInterface,
    ) -> Self {
        let unit_of_work = UnitOfWork::new(pool.clone(), events.clone());
        Self {
            pool,
            events,
            unit_of_work,
            models,
            projects,
            workspace,
            sessions,
            blobs,
            runtime,
        }
    }

    pub async fn latest_context_usage(
        &self,
        session_id: SessionId,
    ) -> Result<Option<ContextUsageView>, ExecutionError> {
        let row: Option<(i64, i64, String, String)> = sqlx::query_as(
            "SELECT estimated_input_tokens, context_limit, compact_status, created_at \
             FROM context_versions WHERE session_id = ? ORDER BY sequence DESC LIMIT 1",
        )
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(estimated_input_tokens, context_limit, compact_status, created_at)| {
                ContextUsageView {
                    estimated_input_tokens,
                    context_limit,
                    compact_status,
                    created_at,
                }
            },
        ))
    }

    pub async fn latest_model_attempt_for_turn(
        &self,
        turn_id: TurnId,
    ) -> Result<Option<TurnModelAttempt>, ExecutionError> {
        let round_ids = sqlx::query_scalar::<_, String>(
            "SELECT id FROM rounds WHERE turn_id = ? ORDER BY sequence",
        )
        .bind(turn_id.to_string())
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|id| id.parse::<RoundId>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ExecutionError::Internal(anyhow::anyhow!(error)))?;
        let Some(attempt) = self.models.latest_attempt_for_rounds(&round_ids).await? else {
            return Ok(None);
        };
        let status = match attempt.status.as_str() {
            "running" => ModelAttemptStatus::Running,
            "succeeded" => ModelAttemptStatus::Succeeded,
            "failed" => ModelAttemptStatus::Failed,
            "canceled" => ModelAttemptStatus::Canceled,
            "interrupted" => ModelAttemptStatus::Interrupted,
            other => {
                return Err(ExecutionError::Internal(anyhow::anyhow!(
                    "unknown model attempt status {other}"
                )));
            }
        };
        Ok(Some(TurnModelAttempt {
            attempt: attempt.attempt,
            status,
            detail: attempt.detail,
        }))
    }

    /// Execute a running Turn until finish tool, model stop without tools, or failure.
    pub async fn execute_turn(
        &self,
        turn_id: TurnId,
    ) -> Result<TurnExecutionOutcome, ExecutionError> {
        let turn = self.load_turn(turn_id).await?;
        if turn.status != TurnStatus::Running || !turn.active {
            return Ok(TurnExecutionOutcome::default()); // idempotent
        }
        let session_id = turn.session_id;
        let owner_id = self.projects.owner_id(turn.project_id).await?;

        let Some(model_snapshot) = turn.model_snapshot.as_ref() else {
            self.enter_waiting_for_model(session_id, turn_id, "model is not configured")
                .await?;
            return Ok(TurnExecutionOutcome::default());
        };
        let provider_id = model_snapshot.provider_id.clone();
        let upstream_model_id = model_snapshot.upstream_model_id.clone();

        let (mut chat, mut input_cursor) = self
            .load_chat_history(session_id, turn_id, model_snapshot.supports_images)
            .await?;
        // Ensure system prefix once.
        if !chat
            .first()
            .is_some_and(|m| matches!(m.role, ChatRole::System))
        {
            chat.insert(
                0,
                ChatMessage {
                    role: ChatRole::System,
                    parts: vec![ContentPart::Text {
                        text: SYSTEM_PROMPT.into(),
                    }],
                    tool_call_id: None,
                    tool_calls: Vec::new(),
                },
            );
        }

        let has_attachments = !self
            .sessions
            .list_attachments(session_id)
            .await
            .map_err(|error| {
                ExecutionError::Internal(anyhow::anyhow!("list attachments: {error}"))
            })?
            .is_empty();
        let tools: Vec<ToolSpec> = available_tools(has_attachments)
            .into_iter()
            .map(|t| ToolSpec {
                name: t.name.into(),
                description: t.description.into(),
                parameters: t.parameters,
            })
            .collect();

        let actor = json!({"kind": "execution"});
        let mut finished = false;
        let mut finish_summary: Option<CompletionSummary> = None;

        let last_round_sequence: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(sequence), 0) FROM rounds WHERE turn_id = ?")
                .bind(turn_id.to_string())
                .fetch_one(&self.pool)
                .await?;
        if last_round_sequence >= MAX_ROUNDS as i64 {
            self.fail_turn(session_id, turn_id, "max rounds exceeded")
                .await?;
            return Ok(TurnExecutionOutcome::default());
        }

        for round_seq in (last_round_sequence + 1)..=MAX_ROUNDS as i64 {
            if !self.sessions.turn_is_runnable(session_id, turn_id).await? {
                return Ok(TurnExecutionOutcome::default());
            }

            let (turn_inputs, next_cursor) = self
                .load_turn_inputs_after(
                    session_id,
                    turn_id,
                    input_cursor,
                    model_snapshot.supports_images,
                )
                .await?;
            chat.extend(turn_inputs);
            input_cursor = next_cursor;

            let round_id = RoundId::new();
            let now = now_utc_str();
            let version = format!("v_{}", RoundId::new());
            let mut work = self.unit_of_work.begin().await?;
            if !self
                .sessions
                .turn_is_runnable_in_tx(work.connection(), session_id, turn_id)
                .await?
            {
                work.rollback().await?;
                return Ok(TurnExecutionOutcome::default());
            }
            let inserted = sqlx::query(
                "INSERT INTO rounds \
                 (id, turn_id, sequence, context_version, status, candidate_snapshot_json, \
                  final_attempt_id, output_summary_json, input_tokens, output_tokens, \
                  stop_reason, version, created_at, updated_at) \
                 VALUES (?, ?, ?, '1', 'running', NULL, NULL, NULL, 0, 0, NULL, ?, ?, ?)",
            )
            .bind(round_id.to_string())
            .bind(turn_id.to_string())
            .bind(round_seq)
            .bind(&version)
            .bind(&now)
            .bind(&now)
            .execute(work.connection())
            .await?;
            if inserted.rows_affected() != 1 {
                work.rollback().await?;
                return Ok(TurnExecutionOutcome::default());
            }
            work.append_event(NewEvent {
                event_type: "round.changed".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "round", "id": round_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "round_id": round_id.to_string(),
                    "turn_id": turn_id.to_string(),
                    "sequence": round_seq,
                    "status": "running",
                }),
            })
            .await?;
            work.commit().await?;

            let req = ModelRequest {
                owner_id: owner_id.clone(),
                provider_id: provider_id.clone(),
                upstream_model_id: upstream_model_id.clone(),
                parameters: model_snapshot.parameters.clone(),
                messages: chat.clone(),
                tools: tools.clone(),
                round_id: Some(round_id.to_string()),
                project_id: Some(turn.project_id.to_string()),
                session_id: Some(session_id.to_string()),
                turn_id: Some(turn_id.to_string()),
            };

            let stream_events = self.events.clone();
            let stream_actor = actor.clone();
            let stream_project_id = turn.project_id.to_string();
            let stream_session_id = session_id.to_string();
            let stream_turn_id = turn_id.to_string();
            let stream_round_id = round_id.to_string();
            let mut publish_stream_event = move |event: ModelStreamEvent| {
                let events = stream_events.clone();
                let actor = stream_actor.clone();
                let project_id = stream_project_id.clone();
                let session_id = stream_session_id.clone();
                let turn_id = stream_turn_id.clone();
                let round_id = stream_round_id.clone();
                async move {
                    match event {
                        ModelStreamEvent::Delta {
                            attempt_id,
                            sequence,
                            channel,
                            text,
                            provisional,
                            usage,
                        } => {
                            let channel = match channel {
                                StreamChannel::Text => "text",
                                StreamChannel::ReasoningSummary => "reasoning_summary",
                                StreamChannel::ToolCallPreview => "tool_call_preview",
                            };
                            let usage_value = usage.as_ref().map(|u| {
                                json!({
                                    "input_tokens": u.input_tokens,
                                    "output_tokens": u.output_tokens,
                                })
                            });
                            let _ = events
                                .append(NewEvent {
                                    event_type: "model.stream_delta".into(),
                                    actor,
                                    resource: Some(json!({"kind": "round", "id": round_id})),
                                    correlation_id: CorrelationId::new().to_string(),
                                    causation_id: None,
                                    payload: json!({
                                        "project_id": project_id,
                                        "session_id": session_id,
                                        "turn_id": turn_id,
                                        "round_id": round_id,
                                        "attempt_id": attempt_id,
                                        "sequence": sequence,
                                        "channel": channel,
                                        "delta": text,
                                        "provisional": provisional,
                                        "usage": usage_value,
                                    }),
                                })
                                .await;
                        }
                        ModelStreamEvent::Retrying {
                            attempt_id,
                            attempt,
                            detail,
                            retry_after_ms,
                        } => {
                            let _ = events
                                .append(NewEvent {
                                    event_type: "model.attempt_retrying".into(),
                                    actor,
                                    resource: Some(json!({"kind": "turn", "id": turn_id})),
                                    correlation_id: CorrelationId::new().to_string(),
                                    causation_id: None,
                                    payload: json!({
                                        "project_id": project_id,
                                        "session_id": session_id,
                                        "turn_id": turn_id,
                                        "round_id": round_id,
                                        "attempt_id": attempt_id,
                                        "attempt": attempt,
                                        "max_attempts": MAX_ATTEMPTS_PER_CANDIDATE - 1,
                                        "detail": detail,
                                        "retry_after_ms": retry_after_ms,
                                    }),
                                })
                                .await;
                        }
                        // Completed/Failed tool-call deltas and terminal Completed
                        // do not need a live SSE frame — the next turn.status_changed
                        // / timeline.item_created event carries the durable result.
                        ModelStreamEvent::ToolCallDelta { .. }
                        | ModelStreamEvent::Completed { .. }
                        | ModelStreamEvent::Failed { .. } => {}
                    }
                }
            };
            let events = self
                .try_round_stream(req, &mut publish_stream_event)
                .await?;
            if !self.sessions.turn_is_runnable(session_id, turn_id).await? {
                return Ok(TurnExecutionOutcome::default());
            }

            let completed = events.iter().find_map(|e| match e {
                ModelStreamEvent::Completed {
                    attempt_id,
                    usage,
                    stop_reason,
                    tool_calls,
                    text,
                    reasoning,
                    reasoning_duration_ms,
                } => Some((
                    attempt_id.clone(),
                    usage.clone(),
                    stop_reason.clone(),
                    tool_calls.clone(),
                    text.clone(),
                    reasoning.clone(),
                    *reasoning_duration_ms,
                )),
                _ => None,
            });

            let Some((
                attempt_id,
                usage,
                stop_reason,
                tool_calls,
                text,
                reasoning,
                reasoning_duration_ms,
            )) = completed
            else {
                // Failed stream — no tool execution. Retryable provider faults
                // park the Turn in `waiting_for_model` so the user/UI can call
                // retry-model; deterministic faults fail the Turn immediately.
                let detail = events
                    .iter()
                    .find_map(|e| match e {
                        ModelStreamEvent::Failed { code, detail, .. } => {
                            Some(format!("{code}: {detail}"))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| "stream failed".into());
                self.fail_round(session_id, turn_id, &round_id, &detail)
                    .await?;
                let class = classify_failed(&events).expect("Failed event present");
                if class == FaultClass::Transient {
                    self.enter_waiting_for_model(session_id, turn_id, &detail)
                        .await?;
                } else {
                    self.fail_turn(session_id, turn_id, &detail).await?;
                }
                return Ok(TurnExecutionOutcome::default());
            };

            let Some(accepted_calls) = self
                .accept_round_response(
                    session_id,
                    turn_id,
                    &round_id,
                    &attempt_id,
                    usage.input_tokens as i64,
                    usage.output_tokens as i64,
                    stop_reason.as_deref(),
                    &text,
                    &reasoning,
                    reasoning_duration_ms,
                    &tool_calls,
                    &actor,
                )
                .await?
            else {
                return Ok(TurnExecutionOutcome::default());
            };

            if !text.is_empty() || !tool_calls.is_empty() {
                chat.push(ChatMessage {
                    role: ChatRole::Assistant,
                    parts: vec![ContentPart::Text { text: text.clone() }],
                    tool_call_id: None,
                    tool_calls: tool_calls.clone(),
                });
            }

            if tool_calls.is_empty() {
                // Model stopped without tools — complete turn with text as summary.
                finish_summary = Some(CompletionSummary::from_text(&text));
                finished = true;
                break;
            }

            // Execute tools in declaration order. After a blocking wait or finish,
            // still settle every remaining declared call with an explicit
            // model-visible result so protocol history stays complete.
            let mut round_tool_messages: Vec<ChatMessage> = Vec::new();
            let mut wait: Option<TurnWait> = None;
            let mut stop_executing = false;
            let mut skip_reason = "a prior tool blocked or finished this Round";
            for accepted_call in &accepted_calls {
                if stop_executing {
                    let message = self
                        .settle_unrun_tool_call(
                            session_id,
                            turn_id,
                            accepted_call,
                            skip_reason,
                            &actor,
                        )
                        .await?;
                    round_tool_messages.push(message);
                    continue;
                }
                if !self.sessions.turn_is_runnable(session_id, turn_id).await? {
                    return Ok(TurnExecutionOutcome::default());
                }
                let Some(executed) = self
                    .run_one_tool(session_id, turn_id, accepted_call, &actor)
                    .await?
                else {
                    return Ok(TurnExecutionOutcome::default());
                };
                let ExecutedToolCall { outcome, message } = executed;
                if let Some(fs) = outcome.finish_summary {
                    finish_summary = Some(fs);
                    finished = true;
                    stop_executing = true;
                    skip_reason = "a prior tool finished the Turn";
                }
                if let Some(next_wait) = outcome.wait {
                    wait = Some(match wait {
                        Some(current) => current.combine(next_wait),
                        None => next_wait,
                    });
                    stop_executing = true;
                    skip_reason = "a prior tool is waiting for Job or Ask";
                }
                round_tool_messages.push(message);
            }
            chat.extend(round_tool_messages);
            if let Some(wait) = wait {
                return Ok(TurnExecutionOutcome::coordinate(wait));
            }
            if finished {
                break;
            }
        }

        if finished {
            let completion = self
                .complete_turn(
                    session_id,
                    turn_id,
                    finish_summary.unwrap_or_else(|| CompletionSummary::from_text("")),
                )
                .await?;
            if matches!(completion, CompleteTurnOutcome::WaitingForJob) {
                return Ok(TurnExecutionOutcome::coordinate(TurnWait::job()));
            }
        } else {
            self.fail_turn(session_id, turn_id, "max rounds exceeded")
                .await?;
        }
        Ok(TurnExecutionOutcome::default())
    }
}

/// Inspect the streamed events and return the fault class only when the stream
/// ended in `Failed` (no `Completed`). Used by the Round-level posture: a
/// `Transient` final failure (retries exhausted) parks the Turn on
/// `waiting_for_model` so the UI can surface the reason; `Config` fails it.
fn classify_failed(events: &[ModelStreamEvent]) -> Option<FaultClass> {
    let failed = events.iter().rev().find_map(|e| match e {
        ModelStreamEvent::Failed {
            code,
            detail,
            attempt_id: _,
        } => Some((code.clone(), detail.clone())),
        _ => None,
    })?;
    Some(classify_fault(&failed.0, &failed.1))
}

fn tool_result_message(outcome: &ToolOutcome, provider_call_id: &str) -> (ChatMessage, Value) {
    let mut model_parts = Vec::new();
    let mut durable_parts = Vec::new();
    for part in &outcome.parts {
        match part {
            ToolResultPart::Text { text } => {
                model_parts.push(ContentPart::Text { text: text.clone() });
                durable_parts.push(json!({"type": "text", "text": text}));
            }
            ToolResultPart::Json { value } => {
                let text = value.to_string();
                model_parts.push(ContentPart::Text { text: text.clone() });
                durable_parts.push(json!({"type": "text", "text": text}));
            }
            ToolResultPart::Image {
                mime,
                bytes,
                width,
                height,
                path,
                content_revision,
                derived,
                content_hash,
            } => {
                model_parts.push(ContentPart::Image {
                    mime: mime.clone(),
                    bytes: bytes.clone(),
                    width: Some(*width),
                    height: Some(*height),
                });
                durable_parts.push(json!({
                    "type": "image_reference",
                    "mime": mime,
                    "path": path,
                    "content_revision": content_revision,
                    "derived": derived,
                    "content_hash": content_hash,
                    "width": width,
                    "height": height,
                }));
                durable_parts.push(json!({
                    "type": "text",
                    "text": format!(
                        "[image result: {path}, {mime}, {width}x{height}, hash={content_hash}]"
                    ),
                }));
            }
        }
    }
    if model_parts.is_empty() {
        let text = outcome.summary.to_string();
        model_parts.push(ContentPart::Text { text: text.clone() });
        durable_parts.push(json!({"type": "text", "text": text}));
    }
    (
        ChatMessage {
            role: ChatRole::Tool,
            parts: model_parts,
            tool_call_id: Some(provider_call_id.to_owned()),
            tool_calls: Vec::new(),
        },
        Value::Array(durable_parts),
    )
}

impl ExecutionInterface {
    async fn load_turn(&self, turn_id: TurnId) -> Result<ExecutionTurn, ExecutionError> {
        match self.sessions.execution_turn(turn_id).await {
            Ok(turn) => Ok(turn),
            Err(crate::modules::sessions::interface::SessionsError::NotFound) => {
                Err(ExecutionError::TurnNotFound)
            }
            Err(error) => Err(ExecutionError::Sessions(error)),
        }
    }

    async fn load_chat_history(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        supports_images: bool,
    ) -> Result<(Vec<ChatMessage>, i64), ExecutionError> {
        let rows = self.sessions.context_messages(session_id, turn_id).await?;
        let mut out = Vec::new();
        let mut input_cursor = 0;
        let current_turn_id = turn_id.to_string();
        for row in rows {
            let role = match row.kind.as_str() {
                "user" => ChatRole::User,
                "assistant" => ChatRole::Assistant,
                "system" => ChatRole::System,
                "tool_result_ref" => ChatRole::Tool,
                _ => continue,
            };
            if row.turn_id.as_deref() == Some(current_turn_id.as_str()) && row.kind == "user" {
                input_cursor = input_cursor.max(row.timeline_sequence);
            }
            let tool_call_id = row
                .body
                .get("tool_call_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let tool_calls = row
                .body
                .get("tool_calls")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default();
            let include_images = supports_images
                && row.turn_id.as_deref() == Some(current_turn_id.as_str())
                && row.kind == "user";
            out.push(ChatMessage {
                role,
                parts: self
                    .message_parts(session_id, &row.body, include_images)
                    .await?,
                tool_call_id,
                tool_calls,
            });
        }
        Ok((out, input_cursor))
    }

    async fn load_turn_inputs_after(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        input_cursor: i64,
        supports_images: bool,
    ) -> Result<(Vec<ChatMessage>, i64), ExecutionError> {
        let rows = self
            .sessions
            .turn_inputs_after(turn_id, input_cursor)
            .await?;
        let mut out = Vec::new();
        let mut next_cursor = input_cursor;
        for row in rows {
            next_cursor = next_cursor.max(row.timeline_sequence);
            let mut parts = self
                .message_parts(session_id, &row.body, supports_images)
                .await?;
            if parts.is_empty() {
                continue;
            }
            let input_kind = row
                .body
                .get("turn_input")
                .and_then(|value| value.get("kind"))
                .and_then(Value::as_str);
            let prefix = match input_kind {
                Some("steer") => Some("[steer] "),
                Some("ask_answer") => Some("[ask answer] "),
                _ => None,
            };
            if let Some(prefix) = prefix {
                if let Some(ContentPart::Text { text }) = parts
                    .iter_mut()
                    .find(|part| matches!(part, ContentPart::Text { .. }))
                {
                    text.insert_str(0, prefix);
                } else {
                    parts.insert(
                        0,
                        ContentPart::Text {
                            text: prefix.trim_end().to_owned(),
                        },
                    );
                }
            }
            out.push(ChatMessage {
                role: ChatRole::User,
                parts,
                tool_call_id: None,
                tool_calls: Vec::new(),
            });
        }
        Ok((out, next_cursor))
    }

    async fn message_parts(
        &self,
        session_id: SessionId,
        body: &Value,
        include_images: bool,
    ) -> Result<Vec<ContentPart>, ExecutionError> {
        let mut parts = Vec::new();
        for part in body
            .get("parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match part.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str)
                        && !text.is_empty()
                    {
                        parts.push(ContentPart::Text {
                            text: text.to_owned(),
                        });
                    }
                }
                Some("attachment_reference") => {
                    let attachment_id = part
                        .get("attachment_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow::anyhow!("attachment reference is missing its id"))?
                        .parse()
                        .map_err(|error| anyhow::anyhow!("invalid attachment id: {error}"))?;
                    let attachment = self
                        .sessions
                        .get_attachment(session_id, attachment_id)
                        .await?;
                    let metadata = json!({
                        "id": attachment.id.to_string(),
                        "name": attachment.name,
                        "mime": attachment.mime,
                        "byte_size": attachment.byte_size,
                    });
                    parts.push(ContentPart::Text {
                        text: format!(
                            "[Session attachment {metadata}. Use attachment.read or attachment.save with this id.]"
                        ),
                    });
                    if include_images && attachment.blob_sha.is_some() {
                        let bytes = read_attachment_bytes(&self.blobs, &attachment)
                            .await?
                            .ok_or_else(|| {
                                ExecutionError::Internal(anyhow::anyhow!(
                                    "attachment {} content is unavailable",
                                    attachment.id
                                ))
                            })?;
                        if let Some(mime) = supported_image_mime(&bytes) {
                            parts.push(ContentPart::Image {
                                mime: mime.to_owned(),
                                bytes,
                                width: None,
                                height: None,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(parts)
    }

    /// Run one Round's model stream with the M4 retry policy applied to a
    /// single candidate (the resolved primary model). Failover across
    /// `model_failover` candidates is a Stage-6 extension left to a follow-up;
    /// the loop below already honors `MAX_ATTEMPTS_PER_CANDIDATE` and the
    /// `RetryDecision` classifier so a transient fault (429/503/timeout)
    /// retries in place with bounded backoff before bubbling `Failed` up to the
    /// caller (which then parks the Turn on `waiting_for_model`).
    ///
    /// Provisional attempts that fail never contribute tool calls; their only
    /// durable footprint is the `model_attempts` rows the stream layer already
    /// writes. Usage from any succeeded attempt is reported by the stream layer
    /// in its `Completed` event and aggregated normally.
    async fn try_round_stream<F, Fut>(
        &self,
        req: ModelRequest,
        on_event: &mut F,
    ) -> Result<Vec<ModelStreamEvent>, ExecutionError>
    where
        F: FnMut(ModelStreamEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        let mut last_events: Vec<ModelStreamEvent> = Vec::new();
        // `attempt` is the *attempt* index, 0-based: 0 is the initial attempt,
        // 1..=5 are the retries surfaced to the UI as "Reconnecting (X/5)".
        for attempt in 0..MAX_ATTEMPTS_PER_CANDIDATE {
            let events = self
                .models
                .stream_completion_with(req.clone(), on_event)
                .await?;
            // Success / non-retryable-config: stop.
            let failed = events.iter().rev().find_map(|e| match e {
                ModelStreamEvent::Failed {
                    attempt_id,
                    code,
                    detail,
                } => Some((attempt_id.clone(), code.clone(), detail.clone())),
                _ => None,
            });
            let Some((attempt_id, code, detail)) = failed else {
                return Ok(events); // Completed — normal path
            };
            let retry_attempt = attempt + 1; // retry index the UI surfaces
            let decision = classify(&code, &detail, retry_attempt);
            last_events = events.clone();
            match decision.class {
                FaultClass::Config => {
                    // Deterministic fault — re-sending cannot help. Bubble the
                    // Failed up so the caller parks/fails the Turn.
                    return Ok(events);
                }
                FaultClass::Transient => {
                    if retry_attempt >= MAX_ATTEMPTS_PER_CANDIDATE {
                        // Out of retries — bubble the Failed up so the caller
                        // parks on waiting_for_model with the reason.
                        return Ok(events);
                    }
                    // Surface the retry to the UI *before* sleeping, so the
                    // status row reads "Reconnecting ({retry_attempt}/5): {detail}"
                    // while the backoff elapses — not only after it.
                    let retrying = ModelStreamEvent::Retrying {
                        attempt_id: attempt_id.clone(),
                        attempt: retry_attempt,
                        detail: detail.clone(),
                        retry_after_ms: decision.retry_after.as_millis() as u64,
                    };
                    on_event(retrying).await;
                    tokio::time::sleep(decision.retry_after).await;
                }
            }
        }
        Ok(last_events)
    }

    /// Park a running Turn in `waiting_for_model` with a typed diagnostic so the
    /// UI can surface retry-model. The active slot stays held (the Turn is not
    /// terminal). Stage 6 owns the full retry classifier; Stage 4 only records
    /// the durable pause and the reason.
    pub async fn enter_waiting_for_model(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        reason: &str,
    ) -> Result<(), ExecutionError> {
        let now = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        let transition = self
            .sessions
            .wait_for_model_in_tx(work.connection(), session_id, turn_id, reason, &now)
            .await?;
        let Some(transition) = transition else {
            work.rollback().await?;
            return Ok(());
        };
        work.append_event(NewEvent {
            event_type: "turn.status_changed".into(),
            actor: json!({"kind": "execution"}),
            resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "turn_id": turn_id.to_string(),
                "from": transition.from_status.as_str(),
                "to": transition.to_status.as_str(),
                "reason": reason,
                "session_version": transition.session_version,
            }),
        })
        .await?;
        work.commit().await?;
        Ok(())
    }

    /// Resume a `waiting_for_model` Turn to `running` without executing it.
    ///
    /// Application schedules the Turn through `ExecutionCoordinator` after this command
    /// commits. Idempotent: already-`running` returns `true` so a coalesced wake
    /// can still claim execution; non-waiting states return `false`.
    pub async fn retry_model(&self, turn_id: TurnId) -> Result<bool, ExecutionError> {
        let turn = self.load_turn(turn_id).await?;
        if turn.status == TurnStatus::Running {
            return Ok(turn.active);
        }
        if turn.status != TurnStatus::WaitingForModel {
            return Ok(false);
        }
        let session_id = turn.session_id;
        let now = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        let transition = self
            .sessions
            .retry_waiting_model_in_tx(work.connection(), session_id, turn_id, &now)
            .await?;
        let Some(transition) = transition else {
            work.rollback().await?;
            return Ok(false);
        };
        work.append_event(NewEvent {
            event_type: "turn.status_changed".into(),
            actor: json!({"kind": "execution"}),
            resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "turn_id": turn_id.to_string(),
                "from": transition.from_status.as_str(),
                "to": transition.to_status.as_str(),
                "route": "retry_model",
                "session_version": transition.session_version,
            }),
        })
        .await?;
        work.commit().await?;
        Ok(transition.to_status == TurnStatus::Running)
    }

    #[allow(clippy::too_many_arguments)]
    async fn accept_round_response(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        round_id: &RoundId,
        attempt_id: &str,
        input_tokens: i64,
        output_tokens: i64,
        stop_reason: Option<&str>,
        text: &str,
        reasoning: &str,
        reasoning_duration_ms: Option<u64>,
        tool_calls: &[CompletedToolCall],
        actor: &Value,
    ) -> Result<Option<Vec<AcceptedToolCall>>, ExecutionError> {
        let now = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        if !self
            .sessions
            .turn_is_runnable_in_tx(work.connection(), session_id, turn_id)
            .await?
        {
            return Ok(None);
        }
        let accepted = sqlx::query(
            "UPDATE rounds SET status = 'succeeded', final_attempt_id = ?, input_tokens = ?, \
             output_tokens = ?, stop_reason = ?, output_summary_json = ?, updated_at = ? \
             WHERE id = ? AND status = 'running' AND turn_id = ?",
        )
        .bind(attempt_id)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(stop_reason)
        .bind(json!({"text": text, "reasoning": reasoning}).to_string())
        .bind(&now)
        .bind(round_id.to_string())
        .bind(turn_id.to_string())
        .execute(work.connection())
        .await?;
        if accepted.rows_affected() != 1 {
            return Ok(None);
        }

        let declared_calls = serde_json::to_value(tool_calls)?;
        let (_, timeline_item_id, _) = self
            .sessions
            .append_assistant_message_in_tx(
                work.connection(),
                AppendAssistantMessage {
                    session_id,
                    turn_id,
                    round_id: *round_id,
                    text,
                    reasoning,
                    duration_ms: reasoning_duration_ms
                        .map(|duration| duration.min(i64::MAX as u64) as i64),
                    tool_calls: &declared_calls,
                    actor,
                    now: &now,
                },
            )
            .await?;
        let mut persisted_calls = Vec::with_capacity(tool_calls.len());
        for (ordinal, request) in tool_calls.iter().enumerate() {
            let id = ToolCallId::new();
            let input = serde_json::from_str::<Value>(&request.arguments_json)
                .unwrap_or_else(|_| json!({}));
            sqlx::query(
                "INSERT INTO tool_calls \
                 (id, round_id, ord, tool_name, schema_version, input_json, result_summary_json, \
                  status, actor_json, error_code, provider_call_id, started_at, ended_at, version) \
                 VALUES (?, ?, ?, ?, ?, ?, NULL, 'requested', ?, NULL, ?, NULL, NULL, ?)",
            )
            .bind(id.to_string())
            .bind(round_id.to_string())
            .bind(ordinal as i64)
            .bind(&request.name)
            .bind(SCHEMA_VERSION)
            .bind(input.to_string())
            .bind(actor.to_string())
            .bind(&request.id)
            .bind(format!("v_{}", ToolCallId::new()))
            .execute(work.connection())
            .await?;
            persisted_calls.push(AcceptedToolCall {
                id,
                ordinal: ordinal as i64,
                request: request.clone(),
            });
        }
        let correlation_id = CorrelationId::new().to_string();
        work.append_event(NewEvent {
            event_type: "round.changed".into(),
            actor: json!({"kind": "execution"}),
            resource: Some(json!({"kind": "round", "id": round_id.to_string()})),
            correlation_id: correlation_id.clone(),
            causation_id: None,
            payload: json!({
                "round_id": round_id.to_string(),
                "status": "succeeded",
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
            }),
        })
        .await?;
        if let Some(timeline_item_id) = timeline_item_id {
            work.append_event(NewEvent {
                event_type: "timeline.item_created".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
                correlation_id: correlation_id.clone(),
                causation_id: None,
                payload: json!({
                    "timeline_item_id": timeline_item_id,
                    "kind": "assistant_message",
                }),
            })
            .await?;
        }
        for call in &persisted_calls {
            work.append_event(NewEvent {
                event_type: "tool_call.created".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "tool_call", "id": call.id.to_string()})),
                correlation_id: correlation_id.clone(),
                causation_id: None,
                payload: json!({
                    "tool_call_id": call.id.to_string(),
                    "provider_call_id": call.request.id,
                    "tool_name": call.request.name,
                    "status": "requested",
                    "ordinal": call.ordinal,
                }),
            })
            .await?;
        }
        work.commit().await?;
        Ok(Some(persisted_calls))
    }

    async fn fail_round(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        round_id: &RoundId,
        detail: &str,
    ) -> Result<(), ExecutionError> {
        let now = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        if !self
            .sessions
            .turn_is_runnable_in_tx(work.connection(), session_id, turn_id)
            .await?
        {
            return Ok(());
        }
        let changed = sqlx::query(
            "UPDATE rounds SET status = 'failed', stop_reason = ?, output_summary_json = ?, \
              updated_at = ? WHERE id = ? AND status = 'running' AND turn_id = ?",
        )
        .bind("error")
        .bind(json!({"error": detail}).to_string())
        .bind(&now)
        .bind(round_id.to_string())
        .bind(turn_id.to_string())
        .execute(work.connection())
        .await?;
        if changed.rows_affected() != 1 {
            return Ok(());
        }
        work.append_event(NewEvent {
            event_type: "round.changed".into(),
            actor: json!({"kind": "execution"}),
            resource: Some(json!({"kind": "round", "id": round_id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "round_id": round_id.to_string(),
                "status": "failed",
                "detail": detail,
            }),
        })
        .await?;
        work.commit().await?;
        Ok(())
    }

    async fn settle_unrun_tool_call(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        accepted: &AcceptedToolCall,
        reason: &str,
        actor: &Value,
    ) -> Result<ChatMessage, ExecutionError> {
        let now = now_utc_str();
        let input = serde_json::from_str::<Value>(&accepted.request.arguments_json)
            .unwrap_or_else(|_| json!({}));
        let text = format!(
            "tool `{}` was not executed because {reason}",
            accepted.request.name
        );
        let durable_parts = json!([{"type": "text", "text": text}]);
        let message = ChatMessage {
            role: ChatRole::Tool,
            parts: vec![ContentPart::Text { text: text.clone() }],
            tool_call_id: Some(accepted.request.id.clone()),
            tool_calls: Vec::new(),
        };
        let mut outcome = ToolOutcome {
            disposition: ToolExecutionDisposition::Failed,
            parts: vec![ToolResultPart::Text { text: text.clone() }],
            summary: json!({
                "ok": false,
                "skipped": true,
                "reason": reason,
                "detail": text,
            }),
            error_code: Some("TOOL_SKIPPED_AFTER_BLOCK".into()),
            finish_summary: None,
            wait: None,
        };
        attach_tool_display(&accepted.request.name, &input, &mut outcome);
        let summary = outcome.summary;
        let mut work = self.unit_of_work.begin().await?;
        let changed = sqlx::query(
            "UPDATE tool_calls SET status = 'canceled', result_summary_json = ?, error_code = ?, \
              ended_at = ?, version = ? \
             WHERE id = ? AND status = 'requested' \
               AND round_id IN (SELECT id FROM rounds WHERE turn_id = ?)",
        )
        .bind(summary.to_string())
        .bind("TOOL_SKIPPED_AFTER_BLOCK")
        .bind(&now)
        .bind(format!("v_{}", ToolCallId::new()))
        .bind(accepted.id.to_string())
        .bind(turn_id.to_string())
        .execute(work.connection())
        .await?;
        if changed.rows_affected() == 1 {
            let (_, timeline_item_id, _) = self
                .sessions
                .append_tool_result_in_tx(
                    work.connection(),
                    session_id,
                    turn_id,
                    &accepted.id.to_string(),
                    &accepted.request.id,
                    &accepted.request.name,
                    "canceled",
                    &summary,
                    &durable_parts,
                    actor,
                    &now,
                )
                .await?;
            work.append_event(NewEvent {
                event_type: "tool_call.changed".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "tool_call", "id": accepted.id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "tool_call_id": accepted.id.to_string(),
                    "provider_call_id": accepted.request.id,
                    "tool_name": accepted.request.name,
                    "status": "canceled",
                    "summary": summary,
                    "timeline_item_id": timeline_item_id,
                    "skipped": true,
                }),
            })
            .await?;
            work.commit().await?;
        } else {
            work.rollback().await?;
        }
        Ok(message)
    }

    async fn read_paths_for_turn(
        &self,
        turn_id: TurnId,
    ) -> Result<HashSet<String>, ExecutionError> {
        let paths: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT json_extract(tc.input_json, '$.path') FROM tool_calls tc \
             JOIN rounds r ON r.id = tc.round_id \
             WHERE r.turn_id = ? AND tc.tool_name = 'read' AND tc.status = 'succeeded'",
        )
        .bind(turn_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        Ok(paths.into_iter().flatten().collect())
    }

    async fn persist_tool_effects_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: TurnId,
        tool_call_id: ToolCallId,
        summary: &mut Value,
        now: &str,
    ) -> Result<(), ExecutionError> {
        if let Some(job_id) = summary.get("job_id").and_then(Value::as_str) {
            sqlx::query("UPDATE tool_calls SET job_id = ? WHERE id = ? AND status = 'running'")
                .bind(job_id)
                .bind(tool_call_id.to_string())
                .execute(&mut *tx)
                .await?;
        }

        let Some(plan) = summary.get("plan").cloned() else {
            return Ok(());
        };
        let plan_id = plan
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("plan id is missing"))?;
        let todos = plan.get("todos").cloned().unwrap_or_else(|| json!([]));
        let evidence = plan.get("evidence").cloned().unwrap_or_else(|| json!([]));
        let sequence: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM plan_versions WHERE turn_id = ?",
        )
        .bind(turn_id.to_string())
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT OR IGNORE INTO plan_versions \
             (id, turn_id, sequence, plan_json, evidence_json, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(plan_id)
        .bind(turn_id.to_string())
        .bind(sequence)
        .bind(todos.to_string())
        .bind(evidence.to_string())
        .bind(now)
        .execute(&mut *tx)
        .await?;
        if let Some(object) = summary.as_object_mut() {
            object.insert("plan_version_id".into(), Value::String(plan_id.into()));
            object.insert("sequence".into(), Value::from(sequence));
        }
        Ok(())
    }

    async fn run_one_tool(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        accepted: &AcceptedToolCall,
        actor: &Value,
    ) -> Result<Option<ExecutedToolCall>, ExecutionError> {
        let now = now_utc_str();
        let input: Value =
            serde_json::from_str(&accepted.request.arguments_json).unwrap_or_else(|_| json!({}));
        let mut work = self.unit_of_work.begin().await?;
        if !self
            .sessions
            .turn_is_runnable_in_tx(work.connection(), session_id, turn_id)
            .await?
        {
            work.rollback().await?;
            return Ok(None);
        }
        let started = sqlx::query(
            "UPDATE tool_calls SET status = 'running', started_at = ?, version = ? \
             WHERE id = ? AND status = 'requested' \
               AND round_id IN (SELECT id FROM rounds WHERE turn_id = ?)",
        )
        .bind(&now)
        .bind(format!("v_{}", ToolCallId::new()))
        .bind(accepted.id.to_string())
        .bind(turn_id.to_string())
        .execute(work.connection())
        .await?;
        if started.rows_affected() != 1 {
            work.rollback().await?;
            return Ok(None);
        }
        work.append_event(NewEvent {
            event_type: "tool_call.changed".into(),
            actor: actor.clone(),
            resource: Some(json!({"kind": "tool_call", "id": accepted.id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "tool_call_id": accepted.id.to_string(),
                "provider_call_id": accepted.request.id,
                "tool_name": accepted.request.name,
                "status": "running",
            }),
        })
        .await?;
        work.commit().await?;

        let read_paths = self.read_paths_for_turn(turn_id).await?;
        let ctx = ToolContext {
            session_id,
            turn_id,
            tool_call_id: accepted.id,
            workspace: &self.workspace,
            sessions: &self.sessions,
            blobs: &self.blobs,
            runtime: &self.runtime,
            read_paths: &read_paths,
            actor: actor.clone(),
        };
        let mut outcome = match execute_tool(&ctx, &accepted.request.name, &input).await {
            Ok(outcome) => outcome,
            Err(error @ ExecutionError::Storage(_))
            | Err(error @ ExecutionError::Serde(_))
            | Err(error @ ExecutionError::Internal(_)) => return Err(error),
            Err(error) => {
                let mut outcome = super::types::ToolOutcome {
                    disposition: ToolExecutionDisposition::Failed,
                    parts: vec![ToolResultPart::Text {
                        text: error.to_string(),
                    }],
                    summary: json!({
                        "ok": false,
                        "error": error.to_string(),
                        "detail": error.to_string(),
                    }),
                    error_code: Some("TOOL_EXECUTION_FAILED".into()),
                    finish_summary: None,
                    wait: None,
                };
                attach_tool_display(&accepted.request.name, &input, &mut outcome);
                outcome
            }
        };
        let ended = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        self.persist_tool_effects_in_tx(
            work.connection(),
            turn_id,
            accepted.id,
            &mut outcome.summary,
            &ended,
        )
        .await?;
        if outcome.summary.get("plan_version_id").is_some()
            && let Some(ToolResultPart::Json { value }) = outcome.parts.first_mut()
        {
            *value = outcome.summary.clone();
        }
        let (message, durable_parts) = tool_result_message(&outcome, &accepted.request.id);
        let status = outcome.disposition.as_str();
        let ended_at = (!outcome.disposition.is_waiting()).then_some(ended.as_str());
        let finalized = sqlx::query(
            "UPDATE tool_calls SET status = ?, result_summary_json = ?, error_code = ?, \
              ended_at = ?, version = ? WHERE id = ? AND status = 'running'",
        )
        .bind(status)
        .bind(outcome.summary.to_string())
        .bind(&outcome.error_code)
        .bind(ended_at)
        .bind(format!("v_{}", ToolCallId::new()))
        .bind(accepted.id.to_string())
        .execute(work.connection())
        .await?;
        if finalized.rows_affected() != 1 {
            work.rollback().await?;
            return Ok(None);
        }
        let (_, timeline_item_id, _) = self
            .sessions
            .append_tool_result_in_tx(
                work.connection(),
                session_id,
                turn_id,
                &accepted.id.to_string(),
                &accepted.request.id,
                &accepted.request.name,
                status,
                &outcome.summary,
                &durable_parts,
                actor,
                &ended,
            )
            .await?;
        work.append_event(NewEvent {
            event_type: "tool_call.changed".into(),
            actor: actor.clone(),
            resource: Some(json!({"kind": "tool_call", "id": accepted.id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "tool_call_id": accepted.id.to_string(),
                "provider_call_id": accepted.request.id,
                "tool_name": accepted.request.name,
                "status": status,
                "summary": outcome.summary,
                "timeline_item_id": timeline_item_id,
            }),
        })
        .await?;
        work.commit().await?;
        Ok(Some(ExecutedToolCall { outcome, message }))
    }

    pub async fn waiting_job_ids(&self, limit: i64) -> Result<Vec<JobId>, ExecutionError> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT job_id FROM tool_calls \
             WHERE status = 'waiting' AND job_id IS NOT NULL \
             ORDER BY started_at ASC LIMIT ?",
        )
        .bind(limit.clamp(1, 100))
        .fetch_all(&self.pool)
        .await?;
        ids.into_iter()
            .map(|id| {
                id.parse()
                    .map_err(|_| ExecutionError::Internal(anyhow::anyhow!("invalid Job id")))
            })
            .collect()
    }

    pub async fn settle_job_tool_call_in_tx(
        &self,
        tx: &mut sqlx::SqliteConnection,
        job: &JobProjection,
        now: &str,
    ) -> Result<Option<ToolCallSettlement>, ExecutionError> {
        if !job.status.is_terminal() {
            return Ok(None);
        }
        let row: Option<(String, Option<String>, String, String)> = sqlx::query_as(
            "SELECT call.tool_name, call.provider_call_id, round.turn_id, call.input_json \
             FROM tool_calls AS call \
             JOIN rounds AS round ON round.id = call.round_id \
             WHERE call.id = ? AND call.status = 'waiting' \
               AND (call.job_id IS NULL OR call.job_id = ?)",
        )
        .bind(job.initiated_by_tool_call_id.to_string())
        .bind(job.id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let Some((tool_name, provider_call_id, source_turn_id, input_json)) = row else {
            return Ok(None);
        };
        let provider_call_id = provider_call_id.ok_or_else(|| {
            ExecutionError::Internal(anyhow::anyhow!("waiting Tool Call has no Provider call id"))
        })?;
        let (status, error_code, disposition) = match job.status {
            JobStatus::Succeeded => (
                ToolCallStatus::Succeeded,
                None,
                ToolExecutionDisposition::Succeeded,
            ),
            JobStatus::Failed => (
                ToolCallStatus::Failed,
                Some("JOB_FAILED"),
                ToolExecutionDisposition::Failed,
            ),
            JobStatus::Canceled => (
                ToolCallStatus::Canceled,
                Some("JOB_CANCELED"),
                ToolExecutionDisposition::Failed,
            ),
            JobStatus::Lost => (
                ToolCallStatus::Lost,
                Some("JOB_LOST"),
                ToolExecutionDisposition::Failed,
            ),
            JobStatus::Queued | JobStatus::Running => return Ok(None),
        };
        let summary = json!({
            "job_id": job.id.to_string(),
            "status": job.status.as_str(),
            "exit": job.exit,
            "usage": job.usage,
            "log_stream_id": job.log_stream_id.to_string(),
            "command_summary": job.command_summary,
        });
        let mut outcome = ToolOutcome {
            disposition,
            parts: vec![ToolResultPart::Text {
                text: format!(
                    "job {} {} (exit={:?}, log_stream={})",
                    job.id,
                    job.status.as_str(),
                    job.exit.as_ref().and_then(|exit| exit.exit_code),
                    job.log_stream_id,
                ),
            }],
            summary,
            error_code: error_code.map(str::to_owned),
            finish_summary: None,
            wait: None,
        };
        let input = serde_json::from_str::<Value>(&input_json).unwrap_or_else(|_| json!({}));
        attach_tool_display(&tool_name, &input, &mut outcome);
        let summary = outcome.summary.clone();
        let (_, model_parts) = tool_result_message(&outcome, &provider_call_id);
        let changed = sqlx::query(
            "UPDATE tool_calls SET status = ?, result_summary_json = ?, error_code = ?, \
                    job_id = ?, ended_at = ?, version = ? \
             WHERE id = ? AND status = 'waiting' \
               AND (job_id IS NULL OR job_id = ?)",
        )
        .bind(status.as_str())
        .bind(summary.to_string())
        .bind(&outcome.error_code)
        .bind(job.id.to_string())
        .bind(now)
        .bind(format!("v_{}", ToolCallId::new()))
        .bind(job.initiated_by_tool_call_id.to_string())
        .bind(job.id.to_string())
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() != 1 {
            return Ok(None);
        }
        Ok(Some(ToolCallSettlement {
            tool_call_id: job.initiated_by_tool_call_id.to_string(),
            source_turn_id,
            provider_call_id,
            tool_name,
            status,
            summary,
            model_parts,
        }))
    }

    async fn complete_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        summary: CompletionSummary,
    ) -> Result<CompleteTurnOutcome, ExecutionError> {
        let now = now_utc_str();
        let summary_value = serde_json::to_value(&summary)?;
        let mut work = self.unit_of_work.begin().await?;
        let unfinished_calls: i64 = sqlx::query_scalar(
            "SELECT COUNT(1) FROM tool_calls AS call \
             JOIN rounds AS round ON round.id = call.round_id \
             WHERE round.turn_id = ? AND call.status IN ('requested', 'running', 'waiting')",
        )
        .bind(turn_id.to_string())
        .fetch_one(work.connection())
        .await?;
        let unfinished_jobs = self
            .runtime
            .has_unfinished_jobs_in_tx(work.connection(), turn_id)
            .await
            .map_err(|error| {
                ExecutionError::Internal(anyhow::anyhow!(
                    "inspect unfinished Jobs before Turn completion: {error}"
                ))
            })?;
        if unfinished_calls > 0 {
            return Err(ExecutionError::Internal(anyhow::anyhow!(
                "Turn completion attempted with unfinished Tool Calls"
            )));
        }
        if unfinished_jobs {
            work.rollback().await?;
            return Ok(CompleteTurnOutcome::WaitingForJob);
        }
        let (input_tokens, output_tokens): (i64, i64) = sqlx::query_as(
            "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0) \
             FROM rounds WHERE turn_id = ? AND status = 'succeeded'",
        )
        .bind(turn_id.to_string())
        .fetch_one(work.connection())
        .await?;
        let transition = self
            .sessions
            .settle_active_turn_in_tx(
                work.connection(),
                session_id,
                turn_id,
                ActiveTurnOutcome::Completed {
                    summary: &summary_value,
                    input_tokens,
                    output_tokens,
                },
                &now,
            )
            .await?;
        let Some(transition) = transition else {
            work.rollback().await?;
            return Ok(CompleteTurnOutcome::Completed);
        };
        work.append_event(NewEvent {
            event_type: "turn.status_changed".into(),
            actor: json!({"kind": "execution"}),
            resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "turn_id": turn_id.to_string(),
                "from": transition.from_status.as_str(),
                "to": transition.to_status.as_str(),
                "summary": summary_value,
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "session_version": transition.session_version,
            }),
        })
        .await?;
        work.commit().await?;
        Ok(CompleteTurnOutcome::Completed)
    }

    async fn fail_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        reason: &str,
    ) -> Result<(), ExecutionError> {
        let now = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        let summary = json!({"error": reason});
        let transition = self
            .sessions
            .settle_active_turn_in_tx(
                work.connection(),
                session_id,
                turn_id,
                ActiveTurnOutcome::Failed {
                    reason,
                    summary: &summary,
                },
                &now,
            )
            .await?;
        let Some(transition) = transition else {
            work.rollback().await?;
            return Ok(());
        };
        work.append_event(NewEvent {
            event_type: "turn.status_changed".into(),
            actor: json!({"kind": "execution"}),
            resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "turn_id": turn_id.to_string(),
                "from": transition.from_status.as_str(),
                "to": transition.to_status.as_str(),
                "reason": reason,
                "session_version": transition.session_version,
            }),
        })
        .await?;
        work.commit().await?;
        Ok(())
    }

    pub async fn settle_execution_failure(
        &self,
        turn_id: TurnId,
        reason: &str,
    ) -> Result<(), ExecutionError> {
        let turn = self.load_turn(turn_id).await?;
        if turn.status != TurnStatus::Running || !turn.active {
            return Ok(());
        }
        let session_id = turn.session_id;
        self.fail_turn(session_id, turn_id, reason).await
    }

    pub(crate) async fn interrupt_execution_in_tx(
        &self,
        tx: &mut SqliteConnection,
        now: &str,
    ) -> Result<(), ExecutionError> {
        sqlx::query(
            "UPDATE rounds SET status = 'interrupted', stop_reason = 'control_plane_restart', \
                    version = ?, updated_at = ? WHERE status = 'running'",
        )
        .bind(format!("v_{}", RoundId::new()))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE tool_calls SET status = 'canceled', error_code = 'CONTROL_PLANE_RESTART', \
                    ended_at = ?, version = ? WHERE status = 'requested'",
        )
        .bind(now)
        .bind(format!("v_{}", ToolCallId::new()))
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE tool_calls SET status = 'lost', error_code = 'CONTROL_PLANE_RESTART', \
                    ended_at = ?, version = ? WHERE status IN ('running', 'waiting')",
        )
        .bind(now)
        .bind(format!("v_{}", ToolCallId::new()))
        .execute(&mut *tx)
        .await?;
        self.close_asks_in_tx(tx, None, AskClosure::ControlPlaneRestart, now)
            .await?;
        Ok(())
    }

    pub async fn cancel_execution_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: TurnId,
        now: &str,
    ) -> Result<Vec<RoundId>, ExecutionError> {
        let round_ids = self.round_ids_for_turns_in_tx(tx, &[turn_id]).await?;
        sqlx::query(
            "UPDATE tool_calls SET status = 'canceled', error_code = 'USER_CANCEL', \
                    ended_at = ?, version = ? \
             WHERE status IN ('requested', 'running', 'waiting') \
               AND round_id IN (SELECT id FROM rounds WHERE turn_id = ?)",
        )
        .bind(now)
        .bind(format!("v_{}", ToolCallId::new()))
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE rounds SET status = 'canceled', stop_reason = 'user_cancel', \
                    version = ?, updated_at = ? WHERE turn_id = ? AND status = 'running'",
        )
        .bind(format!("v_{}", RoundId::new()))
        .bind(now)
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        self.close_asks_in_tx(tx, Some(turn_id), AskClosure::UserCancel, now)
            .await?;
        Ok(round_ids)
    }

    pub(crate) async fn round_ids_for_turns_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_ids: &[TurnId],
    ) -> Result<Vec<RoundId>, ExecutionError> {
        let mut rounds = Vec::new();
        for turn_id in turn_ids {
            let rows = sqlx::query_scalar::<_, String>(
                "SELECT id FROM rounds WHERE turn_id = ? ORDER BY sequence",
            )
            .bind(turn_id.to_string())
            .fetch_all(&mut *tx)
            .await?;
            for id in rows {
                rounds.push(
                    id.parse::<RoundId>()
                        .map_err(|error| ExecutionError::Internal(anyhow::anyhow!(error)))?,
                );
            }
        }
        Ok(rounds)
    }

    pub(crate) async fn delete_session_execution_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_ids: &[TurnId],
        attempt_ids: &[AttemptId],
    ) -> Result<(), ExecutionError> {
        for attempt_id in attempt_ids {
            sqlx::query("DELETE FROM stream_diagnostics WHERE attempt_id = ?")
                .bind(attempt_id.to_string())
                .execute(&mut *tx)
                .await?;
        }
        for turn_id in turn_ids {
            sqlx::query("DELETE FROM asks WHERE turn_id = ?")
                .bind(turn_id.to_string())
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM plan_versions WHERE turn_id = ?")
                .bind(turn_id.to_string())
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM rounds WHERE turn_id = ?")
                .bind(turn_id.to_string())
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("DELETE FROM context_versions WHERE session_id = ?")
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM compact_summaries WHERE session_id = ?")
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?;
        Ok(())
    }

    pub async fn close_open_asks_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: TurnId,
        closure: AskClosure,
        now: &str,
    ) -> Result<u64, ExecutionError> {
        self.close_asks_in_tx(tx, Some(turn_id), closure, now).await
    }

    async fn close_asks_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: Option<TurnId>,
        closure: AskClosure,
        now: &str,
    ) -> Result<u64, ExecutionError> {
        let turn_id = turn_id.map(|value| value.to_string());
        let result = sqlx::query(
            "UPDATE asks SET status = ?, closure_reason = ?, version = ?, updated_at = ? \
             WHERE status = ? AND (? IS NULL OR turn_id = ?)",
        )
        .bind(closure.status().as_str())
        .bind(closure.reason())
        .bind(format!("v_{}", AskId::new()))
        .bind(now)
        .bind(AskStatus::Open.as_str())
        .bind(turn_id.as_deref())
        .bind(turn_id.as_deref())
        .execute(&mut *tx)
        .await?;
        Ok(result.rows_affected())
    }

    /// Create an Ask row inside a shared transaction. The Turn's move to
    /// `waiting_for_ask` is performed by `sessions::pause_turn_for` (sessions
    /// owns turns); the coordinator opens one tx, writes the Ask here, then
    /// pauses the Turn. Returns nothing — the caller already knows the ask id.
    pub async fn create_ask_in_tx(
        &self,
        tx: &mut sqlx::sqlite::SqliteConnection,
        request: &AskRequest,
        now: &str,
    ) -> Result<bool, ExecutionError> {
        let inserted = sqlx::query(
            "INSERT INTO asks \
             (id, turn_id, tool_call_id, mode, prompt_json, choices_json, default_json, \
              answer_json, status, expires_at, answered_at, version, created_at, updated_at) \
             SELECT ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, NULL, ?, ?, ? \
             WHERE EXISTS( \
                 SELECT 1 FROM tool_calls AS call \
                 JOIN rounds AS round ON round.id = call.round_id \
                 WHERE call.id = ? AND round.turn_id = ? \
                   AND call.status = 'waiting' \
             )",
        )
        .bind(request.id.to_string())
        .bind(request.turn_id.to_string())
        .bind(request.tool_call_id.to_string())
        .bind(request.mode.as_str())
        .bind(request.prompt.to_string())
        .bind(request.choices.to_string())
        .bind(request.default.as_ref().map(Value::to_string))
        .bind(AskStatus::Open.as_str())
        .bind(request.expires_at.as_deref())
        .bind(format!("v_{}", AskId::new()))
        .bind(now)
        .bind(now)
        .bind(request.tool_call_id.to_string())
        .bind(request.turn_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(ExecutionError::Storage)?;
        Ok(inserted.rows_affected() == 1)
    }

    pub async fn answer_ask_in_tx(
        &self,
        tx: &mut SqliteConnection,
        ask_id: AskId,
        answer: &Value,
        now: &str,
    ) -> Result<AskAnswer, ExecutionError> {
        use sqlx::Row;
        let row = sqlx::query("SELECT turn_id, status FROM asks WHERE id = ?")
            .bind(ask_id.to_string())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ExecutionError::AskNotFound)?;
        let turn_id = row
            .try_get::<String, _>("turn_id")?
            .parse::<TurnId>()
            .map_err(|error| ExecutionError::Internal(anyhow::anyhow!(error)))?;
        let stored_status = row.try_get::<String, _>("status")?;
        let status = AskStatus::try_from(stored_status.as_str()).map_err(|()| {
            ExecutionError::Internal(anyhow::anyhow!(
                "invalid Ask status in storage: {stored_status}"
            ))
        })?;
        if status != AskStatus::Open {
            return Ok(AskAnswer {
                ask_id,
                turn_id,
                disposition: if status == AskStatus::Answered {
                    AskAnswerDisposition::Duplicate
                } else {
                    AskAnswerDisposition::Late
                },
                tool_call: None,
            });
        }
        let result = sqlx::query(
            "UPDATE asks SET status = ?, answer_json = ?, answered_at = ?, updated_at = ?, \
                    version = ? WHERE id = ? AND status = ?",
        )
        .bind(AskStatus::Answered.as_str())
        .bind(answer.to_string())
        .bind(now)
        .bind(now)
        .bind(format!("v_{}", AskId::new()))
        .bind(ask_id.to_string())
        .bind(AskStatus::Open.as_str())
        .execute(&mut *tx)
        .await?;
        let disposition = if result.rows_affected() == 1 {
            AskAnswerDisposition::Accepted
        } else {
            let stored_status =
                sqlx::query_scalar::<_, String>("SELECT status FROM asks WHERE id = ?")
                    .bind(ask_id.to_string())
                    .fetch_one(&mut *tx)
                    .await?;
            let status = AskStatus::try_from(stored_status.as_str()).map_err(|()| {
                ExecutionError::Internal(anyhow::anyhow!(
                    "invalid Ask status in storage: {stored_status}"
                ))
            })?;
            if status == AskStatus::Answered {
                AskAnswerDisposition::Duplicate
            } else {
                AskAnswerDisposition::Late
            }
        };
        let tool_call = if disposition == AskAnswerDisposition::Accepted {
            Some(
                self.settle_ask_tool_call_in_tx(tx, ask_id, AskStatus::Answered, now)
                    .await?,
            )
        } else {
            None
        };
        Ok(AskAnswer {
            ask_id,
            turn_id,
            disposition,
            tool_call,
        })
    }

    async fn settle_ask_tool_call_in_tx(
        &self,
        tx: &mut SqliteConnection,
        ask_id: AskId,
        ask_status: AskStatus,
        now: &str,
    ) -> Result<ToolCallSettlement, ExecutionError> {
        let row: Option<(String, String, String, Option<String>, String)> = sqlx::query_as(
            "SELECT ask.turn_id, ask.tool_call_id, call.tool_name, call.provider_call_id, call.input_json \
             FROM asks AS ask \
             JOIN tool_calls AS call ON call.id = ask.tool_call_id \
             JOIN rounds AS round ON round.id = call.round_id \
             WHERE ask.id = ? AND ask.status = ? AND round.turn_id = ask.turn_id \
               AND call.status = 'waiting'",
        )
        .bind(ask_id.to_string())
        .bind(ask_status.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        let Some((source_turn_id, tool_call_id, tool_name, provider_call_id, input_json)) = row
        else {
            return Err(ExecutionError::Internal(anyhow::anyhow!(
                "settled Ask has no matching waiting Tool Call"
            )));
        };
        let provider_call_id = provider_call_id.ok_or_else(|| {
            ExecutionError::Internal(anyhow::anyhow!(
                "waiting Ask Tool Call has no Provider call id"
            ))
        })?;
        let summary = json!({
            "ask_id": ask_id.to_string(),
            "status": ask_status.as_str(),
        });
        let mut outcome = ToolOutcome {
            disposition: ToolExecutionDisposition::Succeeded,
            parts: vec![ToolResultPart::Text {
                text: format!(
                    "ask_user {} (ask_id={ask_id}); the response is recorded as attributed user input",
                    ask_status.as_str()
                ),
            }],
            summary,
            error_code: None,
            finish_summary: None,
            wait: None,
        };
        let input = serde_json::from_str::<Value>(&input_json).unwrap_or_else(|_| json!({}));
        attach_tool_display(&tool_name, &input, &mut outcome);
        let summary = outcome.summary.clone();
        let (_, model_parts) = tool_result_message(&outcome, &provider_call_id);
        let changed = sqlx::query(
            "UPDATE tool_calls SET status = ?, result_summary_json = ?, error_code = NULL, \
                    ended_at = ?, version = ? WHERE id = ? AND status = 'waiting'",
        )
        .bind(ToolCallStatus::Succeeded.as_str())
        .bind(summary.to_string())
        .bind(now)
        .bind(format!("v_{}", ToolCallId::new()))
        .bind(&tool_call_id)
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() != 1 {
            return Err(ExecutionError::Internal(anyhow::anyhow!(
                "waiting Ask Tool Call changed during settlement"
            )));
        }
        Ok(ToolCallSettlement {
            tool_call_id,
            source_turn_id,
            provider_call_id,
            tool_name,
            status: ToolCallStatus::Succeeded,
            summary,
            model_parts,
        })
    }

    pub async fn has_open_asks_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: TurnId,
    ) -> Result<bool, ExecutionError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(1) FROM asks \
             WHERE turn_id = ? AND status = ?",
        )
        .bind(turn_id.to_string())
        .bind(AskStatus::Open.as_str())
        .fetch_one(&mut *tx)
        .await?;
        Ok(count > 0)
    }

    pub async fn expire_due_asks_in_tx(
        &self,
        tx: &mut SqliteConnection,
        now: &str,
        limit: u32,
    ) -> Result<Vec<ExpiredAsk>, ExecutionError> {
        let rows = sqlx::query(
            "SELECT id, turn_id, default_json FROM asks \
             WHERE status = ? AND mode = 'best_effort' \
               AND expires_at IS NOT NULL AND expires_at <= ? \
             ORDER BY expires_at, id LIMIT ?",
        )
        .bind(AskStatus::Open.as_str())
        .bind(now)
        .bind(i64::from(limit.clamp(1, 500)))
        .fetch_all(&mut *tx)
        .await?;
        use sqlx::Row;
        let mut out = Vec::new();
        for row in rows {
            let ask_id = row
                .try_get::<String, _>("id")?
                .parse::<AskId>()
                .map_err(|error| ExecutionError::Internal(anyhow::anyhow!(error)))?;
            let turn_id = row
                .try_get::<String, _>("turn_id")?
                .parse::<TurnId>()
                .map_err(|error| ExecutionError::Internal(anyhow::anyhow!(error)))?;
            let default = row
                .try_get::<Option<String>, _>("default_json")?
                .map(|value| serde_json::from_str::<Value>(&value))
                .transpose()?;
            let changed = sqlx::query(
                "UPDATE asks SET status = ?, answer_json = COALESCE(answer_json, ?), \
                        version = ?, updated_at = ? WHERE id = ? AND status = ?",
            )
            .bind(AskStatus::Expired.as_str())
            .bind(default.as_ref().map(Value::to_string))
            .bind(format!("v_{}", AskId::new()))
            .bind(now)
            .bind(ask_id.to_string())
            .bind(AskStatus::Open.as_str())
            .execute(&mut *tx)
            .await?;
            if changed.rows_affected() == 1 {
                let tool_call = self
                    .settle_ask_tool_call_in_tx(tx, ask_id, AskStatus::Expired, now)
                    .await?;
                out.push(ExpiredAsk {
                    ask_id,
                    turn_id,
                    default,
                    tool_call,
                });
            }
        }
        Ok(out)
    }
}
