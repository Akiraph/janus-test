//! Turn execution loop (M3 Stage 4).

use serde_json::{Value, json};
use sqlx::Row;

use crate::modules::models::interface::ModelsInterface;
use crate::modules::models::stream_types::{
    ChatMessage, ChatRole, CompletedToolCall, ContentPart, ModelRequest, ModelStreamEvent, ToolSpec,
};
use crate::modules::workspace_sync::interface::WorkspaceSyncInterface;
use crate::platform::{
    clock::{Clock, SystemClock, format_utc},
    events::{EventStore, NewEvent},
    id::{CorrelationId, MessageId, RoundId, SessionId, TimelineItemId, ToolCallId, TurnId},
    sleeper::{Sleeper, SystemSleeper},
};
use sqlx::SqlitePool;

use super::context::SYSTEM_PROMPT;
use super::registry::{SCHEMA_VERSION, registry};
use super::retry::{FaultClass, MAX_ATTEMPTS_PER_CANDIDATE, classify, RetryDecision};
use super::tools::{ToolContext, execute_tool};
use super::types::{SupervisorError, ToolResultPart};

const MAX_ROUNDS: usize = 12;

#[derive(Clone)]
pub struct SupervisorInterface {
    pool: SqlitePool,
    events: EventStore,
    models: ModelsInterface,
    workspace: WorkspaceSyncInterface,
    runtime: Option<crate::modules::runtime::interface::RuntimeInterface>,
    retry_sleeper: std::sync::Arc<dyn Sleeper>,
    /// Owner id used to resolve provider credentials (Phase 1 single owner).
    owner_id: String,
}

impl SupervisorInterface {
    pub fn new(
        pool: SqlitePool,
        events: EventStore,
        models: ModelsInterface,
        workspace: WorkspaceSyncInterface,
        owner_id: String,
    ) -> Self {
        Self {
            pool,
            events,
            models,
            workspace,
            runtime: None,
            retry_sleeper: std::sync::Arc::new(SystemSleeper),
            owner_id,
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
        if turn.status != "running" {
            return Ok(()); // idempotent
        }
        let session_id: SessionId = turn
            .session_id
            .parse()
            .map_err(|_| SupervisorError::SessionNotFound)?;

        let (provider_id, upstream_model_id) = self.resolve_model(&turn.session_id).await?;

        let mut chat: Vec<ChatMessage> = self.load_chat_history(session_id).await?;
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
        let mut finish_summary: Option<Value> = None;
        let mut total_in: i64 = 0;
        let mut total_out: i64 = 0;

        for round_seq in 1..=MAX_ROUNDS {
            // Honor Cancel mid-loop: if the Turn has been moved to `canceling`
            // by sessions, stop without starting a new Round. Final settlement
            // (`canceled` vs `interrupted`) is the application/runtime's job.
            let current_status: Option<String> =
                sqlx::query_scalar("SELECT status FROM turns WHERE id = ?")
                    .bind(turn_id.to_string())
                    .fetch_optional(&self.pool)
                    .await?;
            if current_status.as_deref() == Some("canceling") {
                return Ok(());
            }

            // Inject Steers that arrived since the last Round boundary. Steer
            // messages are ordinary user messages with body_json.steer = true;
            // they become visible here and only here (design: next safe Round).
            let steers = self.drain_pending_steers(session_id, turn_id, &chat).await?;
            chat.extend(steers);

            let round_id = RoundId::new();
            let now = format_utc(SystemClock.now());
            let version = format!("v_{}", RoundId::new());
            sqlx::query(
                "INSERT INTO rounds \
                 (id, turn_id, sequence, context_version, status, candidate_snapshot_json, \
                  final_attempt_id, output_summary_json, input_tokens, output_tokens, \
                  stop_reason, version, created_at, updated_at) \
                 VALUES (?, ?, ?, '1', 'running', NULL, NULL, NULL, 0, 0, NULL, ?, ?, ?)",
            )
            .bind(round_id.to_string())
            .bind(turn_id.to_string())
            .bind(round_seq as i64)
            .bind(&version)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await?;

            let req = ModelRequest {
                owner_id: self.owner_id.clone(),
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
                self.fail_round(&round_id, &detail).await?;
                let decision = classify_failed(&events).expect("Failed event present");
                if decision.class == FaultClass::Transient {
                    self.enter_waiting_for_model(session_id, turn_id, &detail)
                        .await?;
                } else {
                    self.fail_turn(session_id, turn_id, &detail).await?;
                }
                return Ok(());
            };

            total_in += usage.input_tokens as i64;
            total_out += usage.output_tokens as i64;
            self.succeed_round(
                &round_id,
                &attempt_id,
                usage.input_tokens as i64,
                usage.output_tokens as i64,
                stop_reason.as_deref(),
                &text,
            )
            .await?;

            // Commit assistant message + timeline (formal, replaces provisional).
            if !text.is_empty() {
                self.append_assistant_message(session_id, turn_id, &text, &actor)
                    .await?;
                chat.push(ChatMessage {
                    role: ChatRole::Assistant,
                    parts: vec![ContentPart::Text { text: text.clone() }],
                    tool_call_id: None,
                });
            }

            if tool_calls.is_empty() {
                // Model stopped without tools — complete turn with text as summary.
                finish_summary = Some(json!({
                    "summary": if text.is_empty() { "completed without tools" } else { &text },
                    "main_changes": "",
                    "risks": "",
                }));
                finished = true;
                break;
            }

            // Execute tools in order; abort tools if any prior stream had failed (already handled).
            let mut round_tool_messages: Vec<ChatMessage> = Vec::new();
            let mut wait_state: Option<String> = None;
            for (ord, tc) in tool_calls.iter().enumerate() {
                let outcome = self
                    .run_one_tool(session_id, turn_id, &round_id, ord as i64, tc, &actor)
                    .await?;
                if let Some(fs) = outcome.finish_summary {
                    finish_summary = Some(fs);
                    finished = true;
                }
                if let Some(ws) = outcome.wait_state {
                    wait_state = Some(ws);
                }
                // Feed tool result text/json back (images as content parts, no Base64 in history text).
                let mut parts = Vec::new();
                for p in outcome.parts {
                    match p {
                        ToolResultPart::Text { text } => {
                            parts.push(ContentPart::Text { text });
                        }
                        ToolResultPart::Json { value } => {
                            parts.push(ContentPart::Text {
                                text: value.to_string(),
                            });
                        }
                        ToolResultPart::Image {
                            mime,
                            bytes,
                            width,
                            height,
                            ..
                        } => {
                            parts.push(ContentPart::Image {
                                mime,
                                bytes,
                                width: Some(width),
                                height: Some(height),
                            });
                        }
                    }
                }
                if parts.is_empty() {
                    parts.push(ContentPart::Text {
                        text: outcome.summary.to_string(),
                    });
                }
                round_tool_messages.push(ChatMessage {
                    role: ChatRole::Tool,
                    parts,
                    tool_call_id: Some(tc.id.clone()),
                });
                if finished || wait_state.is_some() {
                    break;
                }
            }
            chat.extend(round_tool_messages);
            if let Some(ws) = wait_state {
                // Park the Turn (waiting_for_job / waiting_for_ask). Resume is
                // driven by application::runtime_events / answer_ask.
                self.park_turn(session_id, turn_id, &ws).await?;
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
                finish_summary.unwrap_or(json!({"summary": "done"})),
                total_in,
                total_out,
            )
            .await?;
        } else {
            self.fail_turn(session_id, turn_id, "max rounds exceeded")
                .await?;
        }
        Ok(())
    }
}

struct TurnRow {
    session_id: String,
    project_id: String,
    status: String,
}

/// Inspect the streamed events and return a `RetryDecision` only if the stream
/// ended in `Failed` (no `Completed`). Used both by `try_round_stream`'s inner
/// loop and by the Round-level posture: a `Transient` final failure parks the
/// Turn on `waiting_for_model`; `Config`/`Fatal` fail it.
fn classify_failed(events: &[ModelStreamEvent]) -> Option<RetryDecision> {
    let failed = events.iter().rev().find_map(|e| match e {
        ModelStreamEvent::Failed {
            code, detail, attempt_id: _,
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

impl SupervisorInterface {
    async fn load_turn(&self, turn_id: TurnId) -> Result<TurnRow, SupervisorError> {
        let row = sqlx::query(
            "SELECT t.session_id, t.status, s.project_id \
             FROM turns t JOIN sessions s ON s.id = t.session_id WHERE t.id = ?",
        )
        .bind(turn_id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(SupervisorError::TurnNotFound)?;
        Ok(TurnRow {
            session_id: row.try_get("session_id")?,
            project_id: row.try_get("project_id")?,
            status: row.try_get("status")?,
        })
    }

    async fn resolve_model(&self, session_id: &str) -> Result<(String, String), SupervisorError> {
        // Prefer session.next_model_ref JSON {provider_id, upstream_model_id};
        // else first enabled model on any enabled provider for owner.
        let next: Option<String> =
            sqlx::query_scalar("SELECT next_model_ref FROM sessions WHERE id = ?")
                .bind(session_id)
                .fetch_optional(&self.pool)
                .await?
                .flatten();
        if let Some(raw) = next
            && let Ok(v) = serde_json::from_str::<Value>(&raw)
            && let (Some(p), Some(m)) = (
                v.get("provider_id").and_then(|x| x.as_str()),
                v.get("upstream_model_id").and_then(|x| x.as_str()),
            )
        {
            return Ok((p.to_owned(), m.to_owned()));
        }
        let providers = self.models.providers(&self.owner_id).await?;
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
    ) -> Result<Vec<ChatMessage>, SupervisorError> {
        let rows = sqlx::query(
            "SELECT kind, body_json FROM messages \
             WHERE session_id = ? AND status = 'active' ORDER BY created_at ASC",
        )
        .bind(session_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::new();
        for row in rows {
            let kind: String = row.try_get("kind")?;
            let body_json: String = row.try_get("body_json")?;
            let body: Value = serde_json::from_str(&body_json).unwrap_or(json!({}));
            let text = body
                .get("parts")
                .and_then(|p| p.as_array())
                .and_then(|arr| {
                    arr.iter().find_map(|part| {
                        if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                            part.get("text")
                                .and_then(|t| t.as_str())
                                .map(|s| s.to_owned())
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or_default();
            let role = match kind.as_str() {
                "user" => ChatRole::User,
                "assistant" => ChatRole::Assistant,
                "system" => ChatRole::System,
                _ => continue,
            };
            out.push(ChatMessage {
                role,
                parts: vec![ContentPart::Text { text }],
                tool_call_id: None,
            });
        }
        Ok(out)
    }

    /// Pull Steer messages for this Turn that are not yet reflected in `chat`.
    /// Steer is recorded by sessions as a user message with `body_json.steer =
    /// true`; it becomes visible at the next safe Round boundary (here).
    async fn drain_pending_steers(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        chat: &[ChatMessage],
    ) -> Result<Vec<ChatMessage>, SupervisorError> {
        let rows = sqlx::query(
            "SELECT body_json FROM messages \
             WHERE session_id = ? AND turn_id = ? AND status = 'active' AND kind = 'user' \
             ORDER BY created_at ASC",
        )
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        // Count how many user messages are already in chat so we only inject
        // the ones that arrived after the last Round (including Steers).
        let already = chat
            .iter()
            .filter(|m| matches!(m.role, ChatRole::User))
            .count();
        let mut out = Vec::new();
        for (idx, row) in rows.into_iter().enumerate() {
            if idx < already {
                continue;
            }
            let body_json: String = row.try_get("body_json")?;
            let body: Value = serde_json::from_str(&body_json).unwrap_or(json!({}));
            let text = body
                .get("parts")
                .and_then(|p| p.as_array())
                .and_then(|arr| {
                    arr.iter().find_map(|part| {
                        if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                            part.get("text")
                                .and_then(|t| t.as_str())
                                .map(|s| s.to_owned())
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            // Prefix Steers so the model can tell them from the original request.
            let is_steer = body.get("steer").and_then(|v| v.as_bool()).unwrap_or(false);
            let text = if is_steer {
                format!("[steer] {text}")
            } else {
                text
            };
            out.push(ChatMessage {
                role: ChatRole::User,
                parts: vec![ContentPart::Text { text }],
                tool_call_id: None,
            });
        }
        Ok(out)
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
        sqlx::query(
            "UPDATE turns SET status = 'waiting_for_model', completion_reason = ?, \
             completion_summary_json = ?, updated_at = ? \
             WHERE id = ? AND status IN ('running', 'waiting_for_model')",
        )
        .bind(reason)
        .bind(json!({"waiting_for_model": reason}).to_string())
        .bind(&now)
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        // Session version advances so clients re-read the Turn projection, but
        // the active slot stays held — the Turn is paused, not terminal.
        sqlx::query(
            "UPDATE sessions SET version = ?, updated_at = ? WHERE id = ?",
        )
        .bind(format!("v_{}", SessionId::new()))
        .bind(&now)
        .bind(session_id.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
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
                    "to": "waiting_for_model",
                    "reason": reason,
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
        if turn.status == "running" {
            return self.execute_turn(turn_id).await;
        }
        if turn.status != "waiting_for_model" {
            return Ok(());
        }
        let session_id: SessionId = turn
            .session_id
            .parse()
            .map_err(|_| SupervisorError::SessionNotFound)?;
        let now = format_utc(SystemClock.now());
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE turns SET status = 'running', updated_at = ? \
             WHERE id = ? AND status = 'waiting_for_model'",
        )
        .bind(&now)
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.commit().await?;
            return Ok(());
        }
        sqlx::query("UPDATE sessions SET version = ?, updated_at = ? WHERE id = ?")
            .bind(format!("v_{}", SessionId::new()))
            .bind(&now)
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
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
                    "from": "waiting_for_model",
                    "to": "running",
                    "route": "retry_model",
                }),
            })
            .await;
        self.execute_turn(turn_id).await
    }

    async fn succeed_round(
        &self,
        round_id: &RoundId,
        attempt_id: &str,
        input_tokens: i64,
        output_tokens: i64,
        stop_reason: Option<&str>,
        text: &str,
    ) -> Result<(), SupervisorError> {
        let now = format_utc(SystemClock.now());
        sqlx::query(
            "UPDATE rounds SET status = 'succeeded', final_attempt_id = ?, input_tokens = ?, \
             output_tokens = ?, stop_reason = ?, output_summary_json = ?, updated_at = ? WHERE id = ?",
        )
        .bind(attempt_id)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(stop_reason)
        .bind(json!({"text": text}).to_string())
        .bind(&now)
        .bind(round_id.to_string())
        .execute(&self.pool)
        .await?;
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
                    "status": "succeeded",
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens,
                }),
            })
            .await;
        Ok(())
    }

    async fn fail_round(&self, round_id: &RoundId, detail: &str) -> Result<(), SupervisorError> {
        let now = format_utc(SystemClock.now());
        sqlx::query(
            "UPDATE rounds SET status = 'failed', stop_reason = ?, output_summary_json = ?, \
             updated_at = ? WHERE id = ?",
        )
        .bind("error")
        .bind(json!({"error": detail}).to_string())
        .bind(&now)
        .bind(round_id.to_string())
        .execute(&self.pool)
        .await?;
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

    async fn append_assistant_message(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        text: &str,
        actor: &Value,
    ) -> Result<(), SupervisorError> {
        let message_id = MessageId::new();
        let timeline_id = TimelineItemId::new();
        let now = format_utc(SystemClock.now());
        let next_order: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(display_order), 0) + 1 FROM timeline_items WHERE session_id = ?",
        )
        .bind(session_id.to_string())
        .fetch_one(&self.pool)
        .await?;
        let body = json!({"parts": [{"type": "text", "text": text}]});
        sqlx::query(
            "INSERT INTO messages \
             (id, session_id, turn_id, actor_json, kind, body_json, status, \
              timeline_sequence, version, created_at) \
             VALUES (?, ?, ?, ?, 'assistant', ?, 'active', ?, ?, ?)",
        )
        .bind(message_id.to_string())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(actor.to_string())
        .bind(body.to_string())
        .bind(next_order)
        .bind(format!("v_{}", MessageId::new()))
        .bind(&now)
        .execute(&self.pool)
        .await?;
        let projection = json!({
            "kind": "assistant_message",
            "message_id": message_id.to_string(),
            "text": text,
        });
        sqlx::query(
            "INSERT INTO timeline_items \
             (id, session_id, turn_id, kind, source_resource_id, display_order, \
              projection_json, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'assistant_message', ?, ?, ?, 'active', ?, ?, ?)",
        )
        .bind(timeline_id.to_string())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(message_id.to_string())
        .bind(next_order)
        .bind(projection.to_string())
        .bind(format!("v_{}", TimelineItemId::new()))
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        let _ = self
            .events
            .append(NewEvent {
                event_type: "timeline.item_created".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "timeline_item_id": timeline_id.to_string(),
                    "kind": "assistant_message",
                }),
            })
            .await;
        Ok(())
    }

    async fn run_one_tool(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        round_id: &RoundId,
        ord: i64,
        tc: &CompletedToolCall,
        actor: &Value,
    ) -> Result<super::types::ToolOutcome, SupervisorError> {
        let tool_call_id = ToolCallId::new();
        let now = format_utc(SystemClock.now());
        let input: Value = serde_json::from_str(&tc.arguments_json).unwrap_or(json!({}));
        sqlx::query(
            "INSERT INTO tool_calls \
             (id, round_id, ord, tool_name, schema_version, input_json, result_summary_json, \
              status, actor_json, error_code, started_at, ended_at, version) \
             VALUES (?, ?, ?, ?, ?, ?, NULL, 'running', ?, NULL, ?, NULL, ?)",
        )
        .bind(tool_call_id.to_string())
        .bind(round_id.to_string())
        .bind(ord)
        .bind(&tc.name)
        .bind(SCHEMA_VERSION)
        .bind(input.to_string())
        .bind(actor.to_string())
        .bind(&now)
        .bind(format!("v_{}", ToolCallId::new()))
        .execute(&self.pool)
        .await?;
        let _ = self
            .events
            .append(NewEvent {
                event_type: "tool_call.created".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "tool_call", "id": tool_call_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "tool_call_id": tool_call_id.to_string(),
                    "tool_name": tc.name,
                    "status": "running",
                }),
            })
            .await;

        let ctx = ToolContext {
            session_id,
            turn_id,
            tool_call_id,
            workspace: &self.workspace,
            runtime: self.runtime.as_ref(),
            pool: &self.pool,
            actor: actor.clone(),
        };
        let outcome = execute_tool(&ctx, &tc.name, &input).await?;
        let ended = format_utc(SystemClock.now());
        let status = if outcome.ok { "succeeded" } else { "failed" };
        sqlx::query(
            "UPDATE tool_calls SET status = ?, result_summary_json = ?, error_code = ?, \
             ended_at = ? WHERE id = ?",
        )
        .bind(status)
        .bind(outcome.summary.to_string())
        .bind(&outcome.error_code)
        .bind(&ended)
        .bind(tool_call_id.to_string())
        .execute(&self.pool)
        .await?;

        let timeline_id = TimelineItemId::new();
        let next_order: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(display_order), 0) + 1 FROM timeline_items WHERE session_id = ?",
        )
        .bind(session_id.to_string())
        .fetch_one(&self.pool)
        .await?;
        let projection = json!({
            "kind": "tool_call",
            "tool_call_id": tool_call_id.to_string(),
            "tool_name": tc.name,
            "status": status,
            "summary": outcome.summary,
        });
        sqlx::query(
            "INSERT INTO timeline_items \
             (id, session_id, turn_id, kind, source_resource_id, display_order, \
              projection_json, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'tool_call', ?, ?, ?, 'active', ?, ?, ?)",
        )
        .bind(timeline_id.to_string())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(tool_call_id.to_string())
        .bind(next_order)
        .bind(projection.to_string())
        .bind(format!("v_{}", TimelineItemId::new()))
        .bind(&ended)
        .bind(&ended)
        .execute(&self.pool)
        .await?;

        let _ = self
            .events
            .append(NewEvent {
                event_type: "tool_call.changed".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "tool_call", "id": tool_call_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "tool_call_id": tool_call_id.to_string(),
                    "tool_name": tc.name,
                    "status": status,
                    "summary": outcome.summary,
                }),
            })
            .await;
        Ok(outcome)
    }

    /// Park a running Turn into a `waiting_for_*` status without releasing the
    /// active slot. Used after Job / Ask tools that block the Round loop.
    async fn park_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        wait_state: &str,
    ) -> Result<(), SupervisorError> {
        let now = format_utc(SystemClock.now());
        sqlx::query(
            "UPDATE turns SET status = ?, updated_at = ? \
             WHERE id = ? AND status = 'running'",
        )
        .bind(wait_state)
        .bind(&now)
        .bind(turn_id.to_string())
        .execute(&self.pool)
        .await?;
        sqlx::query("UPDATE sessions SET version = ?, updated_at = ? WHERE id = ?")
            .bind(format!("v_{}", SessionId::new()))
            .bind(&now)
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await?;
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
                    "from": "running",
                    "to": wait_state,
                }),
            })
            .await;
        Ok(())
    }

    async fn complete_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        summary: Value,
        input_tokens: i64,
        output_tokens: i64,
    ) -> Result<(), SupervisorError> {
        let now = format_utc(SystemClock.now());
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE turns SET status = 'completed', completion_summary_json = ?, \
             completion_reason = 'finish', input_tokens = ?, output_tokens = ?, \
             updated_at = ? WHERE id = ?",
        )
        .bind(summary.to_string())
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(&now)
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE sessions SET state = 'ready', active_turn_id = NULL, updated_at = ?, \
             version = ?, last_activity_at = ? WHERE id = ?",
        )
        .bind(&now)
        .bind(format!("v_{}", SessionId::new()))
        .bind(&now)
        .bind(session_id.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
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
                    "from": "running",
                    "to": "completed",
                    "summary": summary,
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
        sqlx::query(
            "UPDATE turns SET status = 'failed', completion_reason = ?, \
             completion_summary_json = ?, updated_at = ? WHERE id = ?",
        )
        .bind(reason)
        .bind(json!({"error": reason}).to_string())
        .bind(&now)
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE sessions SET state = 'ready', active_turn_id = NULL, updated_at = ?, \
             version = ? WHERE id = ?",
        )
        .bind(&now)
        .bind(format!("v_{}", SessionId::new()))
        .bind(session_id.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
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
                    "from": "running",
                    "to": "failed",
                    "reason": reason,
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
            let default = if default.is_empty() { None } else { Some(default) };
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
