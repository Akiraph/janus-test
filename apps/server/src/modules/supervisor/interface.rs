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
};
use sqlx::SqlitePool;

use super::registry::{SCHEMA_VERSION, registry};
use super::tools::{ToolContext, execute_tool};
use super::types::{SupervisorError, ToolResultPart};

const MAX_ROUNDS: usize = 12;

#[derive(Clone)]
pub struct SupervisorInterface {
    pool: SqlitePool,
    events: EventStore,
    models: ModelsInterface,
    workspace: WorkspaceSyncInterface,
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
            owner_id,
        }
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

            let events = self.models.stream_completion(req).await?;
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
                // Failed stream — no tool execution.
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
                self.fail_turn(session_id, turn_id, &detail).await?;
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
            for (ord, tc) in tool_calls.iter().enumerate() {
                let outcome = self
                    .run_one_tool(session_id, turn_id, &round_id, ord as i64, tc, &actor)
                    .await?;
                if let Some(fs) = outcome.finish_summary {
                    finish_summary = Some(fs);
                    finished = true;
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
                if finished {
                    break;
                }
            }
            chat.extend(round_tool_messages);
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

const SYSTEM_PROMPT: &str = "You are the Janus Supervisor coding agent. \
You may only use registered tools on the Session workspace. \
Call finish(summary) when the user request is complete. \
Do not attempt Apply, Sync, Git write, or Main workspace access.";

struct TurnRow {
    session_id: String,
    project_id: String,
    status: String,
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
            workspace: &self.workspace,
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
}
