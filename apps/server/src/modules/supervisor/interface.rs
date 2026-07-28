//! Turn execution loop (M3 Stage 4).

use crate::modules::models::interface::ModelsInterface;
use crate::modules::models::stream_types::{
    ChatMessage, ChatRole, CompletedToolCall, ContentPart, ModelRequest, ModelStreamEvent, ToolSpec,
};
use crate::modules::sessions::interface::SessionsInterface;
use crate::modules::sessions::types::{ExecutionTurn, TurnStatus};
use crate::modules::workspace_sync::interface::WorkspaceSyncInterface;
use crate::platform::{
    clock::{Clock, SystemClock, format_utc},
    events::{EventStore, NewEvent},
    id::{CorrelationId, RoundId, SessionId, ToolCallId, TurnId},
    sleeper::{Sleeper, SystemSleeper},
};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::context::SYSTEM_PROMPT;
use super::registry::{SCHEMA_VERSION, registry};
use super::retry::{FaultClass, MAX_ATTEMPTS_PER_CANDIDATE, RetryDecision, classify};
use super::tools::{ToolContext, execute_tool};
use super::types::{CompletionSummary, SupervisorError, ToolOutcome, ToolResultPart, TurnWait};

const MAX_ROUNDS: usize = 12;

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

#[derive(Clone)]
pub struct SupervisorInterface {
    pool: SqlitePool,
    events: EventStore,
    models: ModelsInterface,
    workspace: WorkspaceSyncInterface,
    sessions: SessionsInterface,
    runtime: Option<crate::modules::runtime::interface::RuntimeInterface>,
    retry_sleeper: std::sync::Arc<dyn Sleeper>,
}

impl SupervisorInterface {
    pub fn new(
        pool: SqlitePool,
        events: EventStore,
        models: ModelsInterface,
        workspace: WorkspaceSyncInterface,
        sessions: SessionsInterface,
    ) -> Self {
        Self {
            pool,
            events,
            models,
            workspace,
            sessions,
            runtime: None,
            retry_sleeper: std::sync::Arc::new(SystemSleeper),
        }
    }

    /// Inject a sleeper for the retry/cooldown loop (tests use `FakeSleeper`).
    pub fn with_retry_sleeper(mut self, sleeper: std::sync::Arc<dyn Sleeper>) -> Self {
        self.retry_sleeper = sleeper;
        self
    }

    /// Bind a Runtime interface so Stage-5 tools (bash/job/service/delegate_cli)
    /// can reach the Session executor. Optional so M3 unit tests keep working.
    pub fn with_runtime(
        mut self,
        runtime: crate::modules::runtime::interface::RuntimeInterface,
    ) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// Execute a running Turn until finish tool, model stop without tools, or failure.
    pub async fn execute_turn(&self, turn_id: TurnId) -> Result<(), SupervisorError> {
        let turn = self.load_turn(turn_id).await?;
        if turn.status != TurnStatus::Running || !turn.active {
            return Ok(()); // idempotent
        }
        let session_id: SessionId = turn
            .session_id
            .parse()
            .map_err(|_| SupervisorError::SessionNotFound)?;

        let (provider_id, upstream_model_id) = match self.resolve_model(&turn).await {
            Ok(model) => model,
            Err(SupervisorError::ModelNotConfigured) => {
                self.enter_waiting_for_model(session_id, turn_id, "model is not configured")
                    .await?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };

        let (mut chat, mut input_cursor) = self.load_chat_history(session_id, turn_id).await?;
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

        let tools: Vec<ToolSpec> = registry()
            .into_iter()
            .map(|t| ToolSpec {
                name: t.name.into(),
                description: t.description.into(),
                parameters: t.parameters,
            })
            .collect();

        let actor = json!({"kind": "supervisor"});
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
            return Ok(());
        }

        for round_seq in (last_round_sequence + 1)..=MAX_ROUNDS as i64 {
            if !self.sessions.turn_is_runnable(session_id, turn_id).await? {
                return Ok(());
            }

            let (turn_inputs, next_cursor) =
                self.load_turn_inputs_after(turn_id, input_cursor).await?;
            chat.extend(turn_inputs);
            input_cursor = next_cursor;

            let round_id = RoundId::new();
            let now = format_utc(SystemClock.now());
            let version = format!("v_{}", RoundId::new());
            let inserted = sqlx::query(
                "INSERT INTO rounds \
                 (id, turn_id, sequence, context_version, status, candidate_snapshot_json, \
                  final_attempt_id, output_summary_json, input_tokens, output_tokens, \
                  stop_reason, version, created_at, updated_at) \
                 SELECT ?, ?, ?, '1', 'running', NULL, NULL, NULL, 0, 0, NULL, ?, ?, ? \
                 WHERE EXISTS(SELECT 1 FROM turns AS turn \
                              JOIN sessions AS session ON session.id = turn.session_id \
                              WHERE turn.id = ? AND turn.session_id = ? \
                                AND turn.status = 'running' \
                                AND session.active_turn_id = turn.id)",
            )
            .bind(round_id.to_string())
            .bind(turn_id.to_string())
            .bind(round_seq)
            .bind(&version)
            .bind(&now)
            .bind(&now)
            .bind(turn_id.to_string())
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await?;
            if inserted.rows_affected() != 1 {
                return Ok(());
            }

            let req = ModelRequest {
                owner_id: turn.owner_id.clone(),
                provider_id: provider_id.clone(),
                upstream_model_id: upstream_model_id.clone(),
                messages: chat.clone(),
                tools: tools.clone(),
                round_id: Some(round_id.to_string()),
                project_id: Some(turn.project_id.clone()),
                session_id: Some(session_id.to_string()),
                turn_id: Some(turn_id.to_string()),
            };

            let events = self.try_round_stream(req).await?;
            if !self.sessions.turn_is_runnable(session_id, turn_id).await? {
                return Ok(());
            }
            // Emit provisional deltas as public events (best-effort).
            for ev in &events {
                if let ModelStreamEvent::Delta {
                    attempt_id,
                    sequence,
                    text,
                    provisional,
                    ..
                } = ev
                {
                    let _ = self
                        .events
                        .append(NewEvent {
                            event_type: "model.stream_delta".into(),
                            actor: actor.clone(),
                            resource: Some(json!({"kind": "round", "id": round_id.to_string()})),
                            correlation_id: CorrelationId::new().to_string(),
                            causation_id: None,
                            payload: json!({
                                "round_id": round_id.to_string(),
                                "attempt_id": attempt_id,
                                "sequence": sequence,
                                "channel": "text",
                                "delta": text,
                                "provisional": provisional,
                            }),
                        })
                        .await;
                }
            }

            let completed = events.iter().find_map(|e| match e {
                ModelStreamEvent::Completed {
                    attempt_id,
                    usage,
                    stop_reason,
                    tool_calls,
                    text,
                } => Some((
                    attempt_id.clone(),
                    usage.clone(),
                    stop_reason.clone(),
                    tool_calls.clone(),
                    text.clone(),
                )),
                _ => None,
            });

            let Some((attempt_id, usage, stop_reason, tool_calls, text)) = completed else {
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
                let decision = classify_failed(&events).expect("Failed event present");
                if decision.class == FaultClass::Transient {
                    self.enter_waiting_for_model(session_id, turn_id, &detail)
                        .await?;
                } else {
                    self.fail_turn(session_id, turn_id, &detail).await?;
                }
                return Ok(());
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
                    &tool_calls,
                    &actor,
                )
                .await?
            else {
                return Ok(());
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

            // Execute tools in order; abort tools if any prior stream had failed (already handled).
            let mut round_tool_messages: Vec<ChatMessage> = Vec::new();
            let mut wait: Option<TurnWait> = None;
            for accepted_call in &accepted_calls {
                if !self.sessions.turn_is_runnable(session_id, turn_id).await? {
                    return Ok(());
                }
                let Some(executed) = self
                    .run_one_tool(session_id, turn_id, accepted_call, &actor)
                    .await?
                else {
                    return Ok(());
                };
                let ExecutedToolCall { outcome, message } = executed;
                if let Some(fs) = outcome.finish_summary {
                    finish_summary = Some(fs);
                    finished = true;
                }
                if let Some(next_wait) = outcome.wait {
                    wait = Some(match wait {
                        Some(current) => current.combine(next_wait),
                        None => next_wait,
                    });
                }
                round_tool_messages.push(message);
            }
            chat.extend(round_tool_messages);
            if let Some(wait) = wait {
                // Park the Turn (waiting_for_job / waiting_for_ask). Resume is
                // driven by application::runtime_events / answer_ask.
                self.park_turn(session_id, turn_id, wait).await?;
                return Ok(());
            }
            if finished {
                break;
            }
        }

        if finished {
            self.complete_turn(
                session_id,
                turn_id,
                finish_summary.unwrap_or_else(|| CompletionSummary::from_text("")),
            )
            .await?;
        } else {
            self.fail_turn(session_id, turn_id, "max rounds exceeded")
                .await?;
        }
        Ok(())
    }
}

/// Inspect the streamed events and return a `RetryDecision` only if the stream
/// ended in `Failed` (no `Completed`). Used both by `try_round_stream`'s inner
/// loop and by the Round-level posture: a `Transient` final failure parks the
/// Turn on `waiting_for_model`; `Config`/`Fatal` fail it.
fn classify_failed(events: &[ModelStreamEvent]) -> Option<RetryDecision> {
    let failed = events.iter().rev().find_map(|e| match e {
        ModelStreamEvent::Failed {
            code,
            detail,
            attempt_id: _,
        } => Some((code.clone(), detail.clone())),
        _ => None,
    })?;
    // Count how many earlier Failed events preceded this one so the backoff
    // schedule advances across retry attempts within a candidate.
    let prior = events
        .iter()
        .filter(|e| matches!(e, ModelStreamEvent::Failed { .. }))
        .count()
        .saturating_sub(1);
    Some(classify(&failed.0, &failed.1, prior))
}

fn message_text(body: &Value) -> String {
    body.get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| {
            (part.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| part.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("")
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

impl SupervisorInterface {
    async fn load_turn(&self, turn_id: TurnId) -> Result<ExecutionTurn, SupervisorError> {
        match self.sessions.execution_turn(turn_id).await {
            Ok(turn) => Ok(turn),
            Err(crate::modules::sessions::types::SessionsError::NotFound) => {
                Err(SupervisorError::TurnNotFound)
            }
            Err(error) => Err(SupervisorError::Sessions(error)),
        }
    }

    async fn resolve_model(
        &self,
        turn: &ExecutionTurn,
    ) -> Result<(String, String), SupervisorError> {
        // Prefer session.next_model_ref JSON {provider_id, upstream_model_id};
        // else first enabled model on any enabled provider for owner.
        if let Some(raw) = turn.next_model_ref.as_deref()
            && let Ok(v) = serde_json::from_str::<Value>(&raw)
            && let (Some(p), Some(m)) = (
                v.get("provider_id").and_then(|x| x.as_str()),
                v.get("upstream_model_id").and_then(|x| x.as_str()),
            )
        {
            return Ok((p.to_owned(), m.to_owned()));
        }
        let providers = self.models.providers(&turn.owner_id).await?;
        for p in providers {
            if !p.enabled {
                continue;
            }
            if let Some(m) = p.models.iter().find(|m| m.enabled) {
                return Ok((p.id, m.upstream_model_id.clone()));
            }
        }
        Err(SupervisorError::ModelNotConfigured)
    }

    async fn load_chat_history(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<(Vec<ChatMessage>, i64), SupervisorError> {
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
            out.push(ChatMessage {
                role,
                parts: vec![ContentPart::Text {
                    text: message_text(&row.body),
                }],
                tool_call_id,
                tool_calls,
            });
        }
        Ok((out, input_cursor))
    }

    async fn load_turn_inputs_after(
        &self,
        turn_id: TurnId,
        input_cursor: i64,
    ) -> Result<(Vec<ChatMessage>, i64), SupervisorError> {
        let rows = self
            .sessions
            .turn_inputs_after(turn_id, input_cursor)
            .await?;
        let mut out = Vec::new();
        let mut next_cursor = input_cursor;
        for row in rows {
            next_cursor = next_cursor.max(row.timeline_sequence);
            let text = message_text(&row.body);
            if text.is_empty() {
                continue;
            }
            let input_kind = row
                .body
                .get("turn_input")
                .and_then(|value| value.get("kind"))
                .and_then(Value::as_str);
            let text = match input_kind {
                Some("steer") => format!("[steer] {text}"),
                Some("ask_answer") => format!("[ask answer] {text}"),
                _ => text,
            };
            out.push(ChatMessage {
                role: ChatRole::User,
                parts: vec![ContentPart::Text { text }],
                tool_call_id: None,
                tool_calls: Vec::new(),
            });
        }
        Ok((out, next_cursor))
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
    async fn try_round_stream(
        &self,
        req: ModelRequest,
    ) -> Result<Vec<ModelStreamEvent>, SupervisorError> {
        let mut last_events: Vec<ModelStreamEvent> = Vec::new();
        for attempt in 0..MAX_ATTEMPTS_PER_CANDIDATE {
            let events = self.models.stream_completion(req.clone()).await?;
            // Success / non-retryable-fatal / non-retryable-config: stop.
            let decision = classify_failed(&events);
            match decision {
                None => return Ok(events), // Completed — normal path
                Some(d) => {
                    last_events = events.clone();
                    match d.class {
                        FaultClass::Transient => {
                            if attempt + 1 < MAX_ATTEMPTS_PER_CANDIDATE {
                                self.retry_sleeper.sleep(d.retry_after).await;
                                continue;
                            }
                            // Out of retries — bubble the Failed up so the
                            // caller parks on waiting_for_model.
                            return Ok(events);
                        }
                        FaultClass::Config | FaultClass::Fatal => {
                            // Config/fatal faults are not retried in place.
                            return Ok(events);
                        }
                    }
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
    ) -> Result<(), SupervisorError> {
        let now = format_utc(SystemClock.now());
        let mut tx = self.pool.begin().await?;
        let transition = self
            .sessions
            .transition_active_turn_in_tx(
                &mut *tx,
                session_id,
                turn_id,
                TurnStatus::Running,
                TurnStatus::WaitingForModel,
                Some(reason),
                &now,
            )
            .await?;
        tx.commit().await?;
        let Some(transition) = transition else {
            return Ok(());
        };
        let _ = self
            .events
            .append(NewEvent {
                event_type: "turn.status_changed".into(),
                actor: json!({"kind": "supervisor"}),
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
            .await;
        Ok(())
    }

    /// Retry-model entry: resume a `waiting_for_model` Turn to `running` and
    /// re-enter the execution loop. Credentials/config are re-resolved on the
    /// next Round via `resolve_model`. Idempotent if the Turn is already running.
    pub async fn retry_model(&self, turn_id: TurnId) -> Result<(), SupervisorError> {
        let turn = self.load_turn(turn_id).await?;
        if turn.status == TurnStatus::Running {
            return Ok(());
        }
        if turn.status != TurnStatus::WaitingForModel {
            return Ok(());
        }
        let session_id: SessionId = turn
            .session_id
            .parse()
            .map_err(|_| SupervisorError::SessionNotFound)?;
        let now = format_utc(SystemClock.now());
        let mut tx = self.pool.begin().await?;
        let transition = self
            .sessions
            .transition_active_turn_in_tx(
                &mut *tx,
                session_id,
                turn_id,
                TurnStatus::WaitingForModel,
                TurnStatus::Running,
                None,
                &now,
            )
            .await?;
        tx.commit().await?;
        let Some(transition) = transition else {
            return Ok(());
        };
        let _ = self
            .events
            .append(NewEvent {
                event_type: "turn.status_changed".into(),
                actor: json!({"kind": "supervisor"}),
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
            .await;
        self.execute_turn(turn_id).await
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
        tool_calls: &[CompletedToolCall],
        actor: &Value,
    ) -> Result<Option<Vec<AcceptedToolCall>>, SupervisorError> {
        let now = format_utc(SystemClock.now());
        let mut tx = self.pool.begin().await?;
        let accepted = sqlx::query(
            "UPDATE rounds SET status = 'succeeded', final_attempt_id = ?, input_tokens = ?, \
             output_tokens = ?, stop_reason = ?, output_summary_json = ?, updated_at = ? \
             WHERE id = ? AND status = 'running' \
               AND EXISTS(SELECT 1 FROM turns AS turn \
                          JOIN sessions AS session ON session.id = turn.session_id \
                          WHERE turn.id = ? AND turn.session_id = ? AND turn.status = 'running' \
                            AND session.active_turn_id = turn.id)",
        )
        .bind(attempt_id)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(stop_reason)
        .bind(json!({"text": text}).to_string())
        .bind(&now)
        .bind(round_id.to_string())
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .execute(&mut *tx)
        .await?;
        if accepted.rows_affected() != 1 {
            return Ok(None);
        }

        let declared_calls = serde_json::to_value(tool_calls)?;
        let (_, timeline_item_id, _) = self
            .sessions
            .append_assistant_message_in_tx(
                &mut *tx,
                session_id,
                turn_id,
                text,
                &declared_calls,
                actor,
                &now,
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
            .execute(&mut *tx)
            .await?;
            persisted_calls.push(AcceptedToolCall {
                id,
                ordinal: ordinal as i64,
                request: request.clone(),
            });
        }
        tx.commit().await?;

        let correlation_id = CorrelationId::new().to_string();
        let _ = self
            .events
            .append(NewEvent {
                event_type: "round.changed".into(),
                actor: json!({"kind": "supervisor"}),
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
            .await;
        if let Some(timeline_item_id) = timeline_item_id {
            let _ = self
                .events
                .append(NewEvent {
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
                .await;
        }
        for call in &persisted_calls {
            let _ = self
                .events
                .append(NewEvent {
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
                .await;
        }
        Ok(Some(persisted_calls))
    }

    async fn fail_round(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        round_id: &RoundId,
        detail: &str,
    ) -> Result<(), SupervisorError> {
        let now = format_utc(SystemClock.now());
        let changed = sqlx::query(
            "UPDATE rounds SET status = 'failed', stop_reason = ?, output_summary_json = ?, \
              updated_at = ? WHERE id = ? AND status = 'running' \
              AND EXISTS(SELECT 1 FROM turns AS turn \
                         JOIN sessions AS session ON session.id = turn.session_id \
                         WHERE turn.id = ? AND turn.session_id = ? \
                           AND turn.status = 'running' \
                           AND session.active_turn_id = turn.id)",
        )
        .bind("error")
        .bind(json!({"error": detail}).to_string())
        .bind(&now)
        .bind(round_id.to_string())
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .execute(&self.pool)
        .await?;
        if changed.rows_affected() != 1 {
            return Ok(());
        }
        let _ = self
            .events
            .append(NewEvent {
                event_type: "round.changed".into(),
                actor: json!({"kind": "supervisor"}),
                resource: Some(json!({"kind": "round", "id": round_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "round_id": round_id.to_string(),
                    "status": "failed",
                    "detail": detail,
                }),
            })
            .await;
        Ok(())
    }

    async fn run_one_tool(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        accepted: &AcceptedToolCall,
        actor: &Value,
    ) -> Result<Option<ExecutedToolCall>, SupervisorError> {
        let now = format_utc(SystemClock.now());
        let input: Value =
            serde_json::from_str(&accepted.request.arguments_json).unwrap_or_else(|_| json!({}));
        let started = sqlx::query(
            "UPDATE tool_calls SET status = 'running', started_at = ?, version = ? \
             WHERE id = ? AND status = 'requested' \
               AND EXISTS(SELECT 1 FROM turns AS turn \
                          JOIN sessions AS session ON session.id = turn.session_id \
                          WHERE turn.id = ? AND turn.session_id = ? AND turn.status = 'running' \
                            AND session.active_turn_id = turn.id)",
        )
        .bind(&now)
        .bind(format!("v_{}", ToolCallId::new()))
        .bind(accepted.id.to_string())
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .execute(&self.pool)
        .await?;
        if started.rows_affected() != 1 {
            return Ok(None);
        }

        let ctx = ToolContext {
            session_id,
            turn_id,
            tool_call_id: accepted.id,
            workspace: &self.workspace,
            runtime: self.runtime.as_ref(),
            pool: &self.pool,
            actor: actor.clone(),
        };
        let outcome = match execute_tool(&ctx, &accepted.request.name, &input).await {
            Ok(outcome) => outcome,
            Err(error @ SupervisorError::Storage(_))
            | Err(error @ SupervisorError::Serde(_))
            | Err(error @ SupervisorError::Internal(_)) => return Err(error),
            Err(error) => super::types::ToolOutcome {
                ok: false,
                parts: vec![ToolResultPart::Text {
                    text: error.to_string(),
                }],
                summary: json!({
                    "ok": false,
                    "error": error.to_string(),
                }),
                error_code: Some("TOOL_EXECUTION_FAILED".into()),
                finish_summary: None,
                wait: None,
            },
        };
        let (message, durable_parts) = tool_result_message(&outcome, &accepted.request.id);
        let ended = format_utc(SystemClock.now());
        let status = if !outcome.ok {
            "failed"
        } else if outcome.wait.is_some() {
            "waiting"
        } else {
            "succeeded"
        };
        let ended_at = (status != "waiting").then_some(ended.as_str());
        let mut tx = self.pool.begin().await?;
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
        .execute(&mut *tx)
        .await?;
        if finalized.rows_affected() != 1 {
            return Ok(None);
        }
        let (_, timeline_item_id, _) = self
            .sessions
            .append_tool_result_in_tx(
                &mut *tx,
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
        tx.commit().await?;

        let _ = self
            .events
            .append(NewEvent {
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
            .await;
        Ok(Some(ExecutedToolCall { outcome, message }))
    }

    /// Park a running Turn into a `waiting_for_*` status without releasing the
    /// active slot. Used after Job / Ask tools that block the Round loop.
    async fn park_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        wait: TurnWait,
    ) -> Result<(), SupervisorError> {
        let now = format_utc(SystemClock.now());
        let to_status = match wait {
            TurnWait::Job => TurnStatus::WaitingForJob,
            TurnWait::Ask => TurnStatus::WaitingForAsk,
        };
        let mut tx = self.pool.begin().await?;
        let transition = self
            .sessions
            .transition_active_turn_in_tx(
                &mut *tx,
                session_id,
                turn_id,
                TurnStatus::Running,
                to_status,
                None,
                &now,
            )
            .await?;
        tx.commit().await?;
        let Some(transition) = transition else {
            return Ok(());
        };
        let _ = self
            .events
            .append(NewEvent {
                event_type: "turn.status_changed".into(),
                actor: json!({"kind": "supervisor"}),
                resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "turn_id": turn_id.to_string(),
                    "from": transition.from_status.as_str(),
                    "to": transition.to_status.as_str(),
                    "session_version": transition.session_version,
                }),
            })
            .await;
        Ok(())
    }

    async fn complete_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        summary: CompletionSummary,
    ) -> Result<(), SupervisorError> {
        let now = format_utc(SystemClock.now());
        let summary_value = serde_json::to_value(&summary)?;
        let mut tx = self.pool.begin().await?;
        let unfinished_calls: i64 = sqlx::query_scalar(
            "SELECT COUNT(1) FROM tool_calls AS call \
             JOIN rounds AS round ON round.id = call.round_id \
             WHERE round.turn_id = ? AND call.status IN ('requested', 'running', 'waiting')",
        )
        .bind(turn_id.to_string())
        .fetch_one(&mut *tx)
        .await?;
        let unfinished_jobs = match &self.runtime {
            Some(runtime) => runtime
                .has_unfinished_jobs_in_tx(&mut *tx, turn_id)
                .await
                .map_err(|error| {
                    SupervisorError::Internal(anyhow::anyhow!(
                        "inspect unfinished Jobs before Turn completion: {error}"
                    ))
                })?,
            None => false,
        };
        if unfinished_calls > 0 || unfinished_jobs {
            return Err(SupervisorError::Internal(anyhow::anyhow!(
                "Turn completion attempted with unfinished finite work"
            )));
        }
        let (input_tokens, output_tokens): (i64, i64) = sqlx::query_as(
            "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0) \
             FROM rounds WHERE turn_id = ? AND status = 'succeeded'",
        )
        .bind(turn_id.to_string())
        .fetch_one(&mut *tx)
        .await?;
        let transition = self
            .sessions
            .settle_active_turn_in_tx(
                &mut *tx,
                session_id,
                turn_id,
                TurnStatus::Running,
                TurnStatus::Completed,
                Some("finish"),
                Some(&summary_value),
                Some(input_tokens),
                Some(output_tokens),
                &now,
            )
            .await?;
        tx.commit().await?;
        let Some(transition) = transition else {
            return Ok(());
        };
        let _ = self
            .events
            .append(NewEvent {
                event_type: "turn.status_changed".into(),
                actor: json!({"kind": "supervisor"}),
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
            .await;
        Ok(())
    }

    async fn fail_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        reason: &str,
    ) -> Result<(), SupervisorError> {
        let now = format_utc(SystemClock.now());
        let mut tx = self.pool.begin().await?;
        let summary = json!({"error": reason});
        let transition = self
            .sessions
            .settle_active_turn_in_tx(
                &mut *tx,
                session_id,
                turn_id,
                TurnStatus::Running,
                TurnStatus::Failed,
                Some(reason),
                Some(&summary),
                None,
                None,
                &now,
            )
            .await?;
        tx.commit().await?;
        let Some(transition) = transition else {
            return Ok(());
        };
        let _ = self
            .events
            .append(NewEvent {
                event_type: "turn.status_changed".into(),
                actor: json!({"kind": "supervisor"}),
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
            .await;
        Ok(())
    }

    /// Mark residual running turns/rounds failed on process start (no tool replay).
    pub async fn recover_running_on_startup(&self) -> Result<u64, SupervisorError> {
        let now = format_utc(SystemClock.now());
        let r1 = sqlx::query(
            "UPDATE turns SET status = 'failed', completion_reason = 'control_plane_restart', \
             updated_at = ? WHERE status = 'running'",
        )
        .bind(&now)
        .execute(&self.pool)
        .await?
        .rows_affected();
        let _ = sqlx::query(
            "UPDATE rounds SET status = 'failed', stop_reason = 'control_plane_restart', \
             updated_at = ? WHERE status = 'running'",
        )
        .bind(&now)
        .execute(&self.pool)
        .await?;
        let _ = sqlx::query(
            "UPDATE sessions SET state = 'ready', active_turn_id = NULL, updated_at = ? \
             WHERE active_turn_id IS NOT NULL",
        )
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(r1)
    }

    /// Close every still-open Ask owned by `turn_id` as `canceled`, inside the
    /// shared Handoff transaction. A late answer to one of these Asks would try
    /// to resume a Turn we are handing off; canceling them first makes that
    /// answer a no-op (or, via `answer_ask`, a re-routed Steer/new Turn).
    pub async fn close_open_asks_in_tx(
        &self,
        tx: &mut sqlx::sqlite::SqliteConnection,
        turn_id: &str,
        now: &str,
    ) -> Result<(), SupervisorError> {
        sqlx::query(
            "UPDATE asks SET status = 'canceled', updated_at = ? \
             WHERE turn_id = ? AND status = 'open'",
        )
        .bind(now)
        .bind(turn_id)
        .execute(&mut *tx)
        .await
        .map_err(SupervisorError::Storage)?;
        Ok(())
    }

    /// Create an Ask row inside a shared transaction. The Turn's move to
    /// `waiting_for_ask` is performed by `sessions::pause_turn_for` (sessions
    /// owns turns); the coordinator opens one tx, writes the Ask here, then
    /// pauses the Turn. Returns nothing — the caller already knows the ask id.
    pub async fn create_ask_in_tx(
        &self,
        tx: &mut sqlx::sqlite::SqliteConnection,
        ask_id: &str,
        turn_id: &str,
        tool_call_id: &str,
        mode: &str,
        prompt_json: &str,
        choices_json: &str,
        default_json: Option<&str>,
        expires_at: Option<&str>,
        version: &str,
        now: &str,
    ) -> Result<(), SupervisorError> {
        sqlx::query(
            "INSERT INTO asks \
             (id, turn_id, tool_call_id, mode, prompt_json, choices_json, default_json, \
              answer_json, status, expires_at, answered_at, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, NULL, 'open', ?, NULL, ?, ?, ?)",
        )
        .bind(ask_id)
        .bind(turn_id)
        .bind(tool_call_id)
        .bind(mode)
        .bind(prompt_json)
        .bind(choices_json)
        .bind(default_json)
        .bind(expires_at)
        .bind(version)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(SupervisorError::Storage)?;
        Ok(())
    }

    /// Record the user's answer to an open Ask inside the shared transaction.
    /// Returns whether the Ask was still open (true) — a `false` result means it
    /// had already been answered/expired/canceled, which the coordinator turns
    /// into a late-answer re-route (Steer or a new Turn with attribution).
    pub async fn record_ask_answer_in_tx(
        &self,
        tx: &mut sqlx::sqlite::SqliteConnection,
        ask_id: &str,
        answer_json: &str,
        now: &str,
    ) -> Result<bool, SupervisorError> {
        let result = sqlx::query(
            "UPDATE asks SET status = 'answered', answer_json = ?, answered_at = ?, updated_at = ? \
             WHERE id = ? AND status = 'open'",
        )
        .bind(answer_json)
        .bind(now)
        .bind(now)
        .bind(ask_id)
        .execute(&mut *tx)
        .await
        .map_err(SupervisorError::Storage)?;
        Ok(result.rows_affected() == 1)
    }

    /// Expire best-effort Asks whose `expires_at` is at/before `now`. For each,
    /// apply the predefined `default_json` as the answer and mark `expired`, so
    /// the coordinator resumes the Turn with the default. Returns the list of
    /// (ask_id, turn_id, default_json) so the coordinator can resume each Turn.
    pub async fn expire_open_asks(
        &self,
        now: &str,
    ) -> Result<Vec<(String, String, Option<String>)>, SupervisorError> {
        let rows = sqlx::query(
            "SELECT id, turn_id, COALESCE(default_json, '') FROM asks \
             WHERE status = 'open' AND mode = 'best_effort' \
               AND expires_at IS NOT NULL AND expires_at <= ?",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .map_err(SupervisorError::Storage)?;
        use sqlx::Row;
        let mut out = Vec::new();
        for row in rows {
            let id: String = row.try_get("id")?;
            let turn_id: String = row.try_get("turn_id")?;
            let default: String = row.try_get(2)?;
            let default = if default.is_empty() {
                None
            } else {
                Some(default)
            };
            sqlx::query(
                "UPDATE asks SET status = 'expired', answer_json = COALESCE(answer_json, ?), \
                 updated_at = ? WHERE id = ? AND status = 'open'",
            )
            .bind(default.as_deref())
            .bind(now)
            .bind(&id)
            .execute(&self.pool)
            .await
            .map_err(SupervisorError::Storage)?;
            out.push((id, turn_id, default));
        }
        Ok(out)
    }
}
