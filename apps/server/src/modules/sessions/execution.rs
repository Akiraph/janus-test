use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};

use crate::modules::workspace_sync::interface::WorkspaceHandle;
use crate::platform::{
    clock::{Clock, SystemClock, format_utc},
    id::{CheckpointId, MessageId, SessionId, TimelineItemId, TurnId},
};

use super::interface::SessionsInterface;
use super::types::{
    ContextMessage, CreatedTurnInput, ExecutionTurn, SessionCommandState, SessionsError,
    TurnStatus, TurnTransition,
};

impl SessionsInterface {
    pub async fn execution_turn(&self, turn_id: TurnId) -> Result<ExecutionTurn, SessionsError> {
        let row = sqlx::query(
            "SELECT turn.id, turn.session_id, session.project_id, project.owner_id, \
                    turn.status, turn.sequence, session.active_turn_id, session.next_model_ref \
             FROM turns AS turn \
             JOIN sessions AS session ON session.id = turn.session_id \
             JOIN projects AS project ON project.id = session.project_id \
             WHERE turn.id = ?",
        )
        .bind(turn_id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(SessionsError::NotFound)?;
        let id: String = row.try_get("id")?;
        let active_turn_id: Option<String> = row.try_get("active_turn_id")?;
        let status: String = row.try_get("status")?;
        Ok(ExecutionTurn {
            active: active_turn_id.as_deref() == Some(id.as_str()),
            id,
            session_id: row.try_get("session_id")?,
            project_id: row.try_get("project_id")?,
            owner_id: row.try_get("owner_id")?,
            status: status
                .parse()
                .map_err(|error: String| SessionsError::Internal(anyhow::anyhow!(error)))?,
            sequence: row.try_get("sequence")?,
            next_model_ref: row.try_get("next_model_ref")?,
        })
    }

    pub async fn turn_is_runnable(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<bool, SessionsError> {
        let runnable: i64 = sqlx::query_scalar(
            "SELECT EXISTS( \
                SELECT 1 FROM turns AS turn \
                JOIN sessions AS session ON session.id = turn.session_id \
                WHERE turn.id = ? AND turn.session_id = ? AND turn.status = 'running' \
                  AND session.active_turn_id = turn.id \
             )",
        )
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .fetch_one(&self.pool)
        .await?;
        Ok(runnable == 1)
    }

    pub async fn context_messages(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<ContextMessage>, SessionsError> {
        let rows = sqlx::query(
            "SELECT message.turn_id, message.kind, message.body_json, \
                    COALESCE(message.timeline_sequence, 0) AS timeline_sequence \
             FROM messages AS message \
             LEFT JOIN turns AS source_turn ON source_turn.id = message.turn_id \
             JOIN turns AS current_turn ON current_turn.id = ? \
             WHERE message.session_id = ? AND current_turn.session_id = message.session_id \
               AND message.status = 'active' \
               AND (message.turn_id IS NULL OR message.turn_id = current_turn.id OR \
                    (source_turn.sequence < current_turn.sequence AND \
                     source_turn.status IN ('completed', 'failed', 'interrupted', 'handed_off'))) \
             ORDER BY COALESCE(message.timeline_sequence, 0), message.created_at, message.id",
        )
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let body_json: String = row.try_get("body_json")?;
                Ok(ContextMessage {
                    turn_id: row.try_get("turn_id")?,
                    kind: row.try_get("kind")?,
                    body: serde_json::from_str(&body_json)?,
                    timeline_sequence: row.try_get("timeline_sequence")?,
                })
            })
            .collect()
    }

    pub async fn turn_inputs_after(
        &self,
        turn_id: TurnId,
        after_sequence: i64,
    ) -> Result<Vec<ContextMessage>, SessionsError> {
        let rows = sqlx::query(
            "SELECT turn_id, kind, body_json, COALESCE(timeline_sequence, 0) AS timeline_sequence \
             FROM messages \
             WHERE turn_id = ? AND status = 'active' AND kind = 'user' \
               AND COALESCE(timeline_sequence, 0) > ? \
             ORDER BY COALESCE(timeline_sequence, 0), created_at, id",
        )
        .bind(turn_id.to_string())
        .bind(after_sequence)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let body_json: String = row.try_get("body_json")?;
                Ok(ContextMessage {
                    turn_id: row.try_get("turn_id")?,
                    kind: row.try_get("kind")?,
                    body: serde_json::from_str(&body_json)?,
                    timeline_sequence: row.try_get("timeline_sequence")?,
                })
            })
            .collect()
    }

    pub async fn current_workspace_revision(
        &self,
        session_id: SessionId,
    ) -> Result<String, SessionsError> {
        let session = self.get_session(session_id).await?;
        Ok(self
            .workspace_sync
            .current_revision(&WorkspaceHandle(session.workspace_handle))
            .await?
            .0)
    }

    pub async fn lock_session_command_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        expected_version: &str,
        now: &str,
    ) -> Result<SessionCommandState, SessionsError> {
        let row = sqlx::query(
            "SELECT project_id, state, workspace_handle, active_turn_id, version \
             FROM sessions WHERE id = ?",
        )
        .bind(session_id.to_string())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(SessionsError::NotFound)?;
        let state: String = row.try_get("state")?;
        if state == "deleting" {
            return Err(SessionsError::SessionDeleting);
        }
        let current_version: String = row.try_get("version")?;
        if current_version != expected_version {
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.to_owned(),
                current: current_version,
            });
        }
        let session_version = format!("v_{}", SessionId::new());
        let updated = sqlx::query(
            "UPDATE sessions SET version = ?, updated_at = ?, last_activity_at = ? \
             WHERE id = ? AND version = ? AND state != 'deleting'",
        )
        .bind(&session_version)
        .bind(now)
        .bind(now)
        .bind(session_id.to_string())
        .bind(expected_version)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            let current =
                sqlx::query_scalar::<_, String>("SELECT version FROM sessions WHERE id = ?")
                    .bind(session_id.to_string())
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or(SessionsError::NotFound)?;
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.to_owned(),
                current,
            });
        }
        Ok(SessionCommandState {
            project_id: row.try_get("project_id")?,
            state,
            workspace_handle: row.try_get("workspace_handle")?,
            active_turn_id: row.try_get("active_turn_id")?,
            session_version,
        })
    }

    pub async fn turn_status_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: &str,
    ) -> Result<Option<TurnStatus>, SessionsError> {
        let status: Option<String> = sqlx::query_scalar("SELECT status FROM turns WHERE id = ?")
            .bind(turn_id)
            .fetch_optional(&mut *tx)
            .await?;
        status
            .map(|status| {
                status
                    .parse()
                    .map_err(|error: String| SessionsError::Internal(anyhow::anyhow!(error)))
            })
            .transpose()
    }

    pub async fn has_queued_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
    ) -> Result<bool, SessionsError> {
        let exists: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM turns WHERE session_id = ? AND status = 'queued')",
        )
        .bind(session_id.to_string())
        .fetch_one(&mut *tx)
        .await?;
        Ok(exists == 1)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_turn_input_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        content: &str,
        actor: &Value,
        predecessor_turn_id: Option<&str>,
        source_ask_id: Option<&str>,
        checkpoint_revision: Option<&str>,
        now: &str,
    ) -> Result<CreatedTurnInput, SessionsError> {
        let next_sequence: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM turns WHERE session_id = ?",
        )
        .bind(session_id.to_string())
        .fetch_one(&mut *tx)
        .await?;
        let display_order = self.next_timeline_position_in_tx(tx, session_id).await?;
        let turn_id = TurnId::new();
        let message_id = MessageId::new();
        let timeline_item_id = TimelineItemId::new();
        sqlx::query(
            "INSERT INTO turns \
             (id, session_id, sequence, status, input_message_id, model_snapshot_json, \
              predecessor_turn_id, handoff_from_turn_id, handoff_to_turn_id, \
              completion_summary_json, completion_reason, cancellation_reason, \
              input_tokens, output_tokens, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'queued', ?, ?, ?, ?, NULL, NULL, NULL, NULL, 0, 0, ?, ?, ?)",
        )
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .bind(next_sequence)
        .bind(message_id.to_string())
        .bind(json!({"provider_id": null, "upstream_model_id": null}).to_string())
        .bind(predecessor_turn_id)
        .bind(predecessor_turn_id)
        .bind(format!("v_{}", TurnId::new()))
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        let mut body = json!({"parts": [{"type": "text", "text": content}]});
        if let Some(source_ask_id) = source_ask_id {
            body["turn_input"] = json!({"kind": "ask_answer", "source_ask_id": source_ask_id});
        }
        sqlx::query(
            "INSERT INTO messages \
             (id, session_id, turn_id, actor_json, kind, body_json, status, \
              timeline_sequence, version, created_at) \
             VALUES (?, ?, ?, ?, 'user', ?, 'active', ?, ?, ?)",
        )
        .bind(message_id.to_string())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(actor.to_string())
        .bind(body.to_string())
        .bind(display_order)
        .bind(format!("v_{}", MessageId::new()))
        .bind(now)
        .execute(&mut *tx)
        .await?;

        let mut projection = json!({
            "kind": "user_message",
            "message_id": message_id.to_string(),
            "turn_id": turn_id.to_string(),
            "text": content,
        });
        if let Some(source_ask_id) = source_ask_id {
            projection["source_ask_id"] = json!(source_ask_id);
        }
        sqlx::query(
            "INSERT INTO timeline_items \
             (id, session_id, turn_id, kind, source_resource_id, display_order, \
              projection_json, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'user_message', ?, ?, ?, 'active', ?, ?, ?)",
        )
        .bind(timeline_item_id.to_string())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(message_id.to_string())
        .bind(display_order)
        .bind(projection.to_string())
        .bind(format!("v_{}", TimelineItemId::new()))
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        let checkpoint_id = if let Some(workspace_revision) = checkpoint_revision {
            Some(
                self.insert_checkpoint_in_tx(
                    tx,
                    session_id,
                    &turn_id.to_string(),
                    &message_id.to_string(),
                    display_order,
                    workspace_revision,
                    now,
                )
                .await?,
            )
        } else {
            None
        };

        Ok(CreatedTurnInput {
            turn_id: turn_id.to_string(),
            message_id: message_id.to_string(),
            timeline_item_id: timeline_item_id.to_string(),
            sequence: next_sequence,
            display_order,
            checkpoint_id,
        })
    }

    pub async fn activate_created_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: &str,
        now: &str,
    ) -> Result<bool, SessionsError> {
        let promoted = sqlx::query(
            "UPDATE turns SET status = 'running', updated_at = ? \
             WHERE id = ? AND session_id = ? AND status = 'queued'",
        )
        .bind(now)
        .bind(turn_id)
        .bind(session_id.to_string())
        .execute(&mut *tx)
        .await?;
        if promoted.rows_affected() != 1 {
            return Ok(false);
        }
        let claimed = sqlx::query(
            "UPDATE sessions SET state = 'active', active_turn_id = ?, updated_at = ? \
             WHERE id = ? AND active_turn_id IS NULL",
        )
        .bind(turn_id)
        .bind(now)
        .bind(session_id.to_string())
        .execute(&mut *tx)
        .await?;
        Ok(claimed.rows_affected() == 1)
    }

    pub async fn begin_handoff_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        predecessor_turn_id: &str,
        successor_turn_id: &str,
        now: &str,
    ) -> Result<bool, SessionsError> {
        let settled = sqlx::query(
            "UPDATE turns SET status = 'handed_off', handoff_to_turn_id = ?, \
                    completion_reason = 'handoff', updated_at = ? \
             WHERE id = ? AND session_id = ? AND status = 'waiting_for_job' \
               AND EXISTS(SELECT 1 FROM sessions WHERE id = ? AND active_turn_id = ?)",
        )
        .bind(successor_turn_id)
        .bind(now)
        .bind(predecessor_turn_id)
        .bind(session_id.to_string())
        .bind(session_id.to_string())
        .bind(predecessor_turn_id)
        .execute(&mut *tx)
        .await?;
        Ok(settled.rows_affected() == 1)
    }

    pub async fn activate_handoff_successor_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        predecessor_turn_id: &str,
        successor_turn_id: &str,
        now: &str,
    ) -> Result<bool, SessionsError> {
        let promoted = sqlx::query(
            "UPDATE turns SET status = 'running', updated_at = ? \
             WHERE id = ? AND session_id = ? AND status = 'queued' \
               AND predecessor_turn_id = ?",
        )
        .bind(now)
        .bind(successor_turn_id)
        .bind(session_id.to_string())
        .bind(predecessor_turn_id)
        .execute(&mut *tx)
        .await?;
        if promoted.rows_affected() != 1 {
            return Ok(false);
        }
        let claimed = sqlx::query(
            "UPDATE sessions SET state = 'active', active_turn_id = ?, updated_at = ? \
             WHERE id = ? AND active_turn_id = ?",
        )
        .bind(successor_turn_id)
        .bind(now)
        .bind(session_id.to_string())
        .bind(predecessor_turn_id)
        .execute(&mut *tx)
        .await?;
        Ok(claimed.rows_affected() == 1)
    }

    pub async fn append_turn_input_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: TurnId,
        content: &str,
        input_kind: &str,
        source_ask_id: Option<&str>,
        actor: &Value,
        now: &str,
    ) -> Result<(String, String, i64), SessionsError> {
        let display_order = self.next_timeline_position_in_tx(tx, session_id).await?;
        let message_id = MessageId::new();
        let timeline_item_id = TimelineItemId::new();
        let body = json!({
            "parts": [{"type": "text", "text": content}],
            "turn_input": {"kind": input_kind, "source_ask_id": source_ask_id},
        });
        sqlx::query(
            "INSERT INTO messages \
             (id, session_id, turn_id, actor_json, kind, body_json, status, \
              timeline_sequence, version, created_at) \
             VALUES (?, ?, ?, ?, 'user', ?, 'active', ?, ?, ?)",
        )
        .bind(message_id.to_string())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(actor.to_string())
        .bind(body.to_string())
        .bind(display_order)
        .bind(format!("v_{}", MessageId::new()))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        let projection = json!({
            "kind": input_kind,
            "message_id": message_id.to_string(),
            "turn_id": turn_id.to_string(),
            "text": content,
            "source_ask_id": source_ask_id,
        });
        sqlx::query(
            "INSERT INTO timeline_items \
             (id, session_id, turn_id, kind, source_resource_id, display_order, \
              projection_json, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 'active', ?, ?, ?)",
        )
        .bind(timeline_item_id.to_string())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(input_kind)
        .bind(message_id.to_string())
        .bind(display_order)
        .bind(projection.to_string())
        .bind(format!("v_{}", TimelineItemId::new()))
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        Ok((
            message_id.to_string(),
            timeline_item_id.to_string(),
            display_order,
        ))
    }

    pub async fn append_assistant_message_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: TurnId,
        text: &str,
        tool_calls: &Value,
        actor: &Value,
        now: &str,
    ) -> Result<(String, Option<String>, i64), SessionsError> {
        let display_order = self.next_timeline_position_in_tx(tx, session_id).await?;
        let message_id = MessageId::new();
        let body = json!({
            "parts": [{"type": "text", "text": text}],
            "tool_calls": tool_calls,
        });
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
        .bind(display_order)
        .bind(format!("v_{}", MessageId::new()))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        if text.is_empty() {
            return Ok((message_id.to_string(), None, display_order));
        }
        let timeline_item_id = TimelineItemId::new();
        sqlx::query(
            "INSERT INTO timeline_items \
             (id, session_id, turn_id, kind, source_resource_id, display_order, \
              projection_json, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'assistant_message', ?, ?, ?, 'active', ?, ?, ?)",
        )
        .bind(timeline_item_id.to_string())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(message_id.to_string())
        .bind(display_order)
        .bind(
            json!({"kind": "assistant_message", "message_id": message_id.to_string(), "text": text})
                .to_string(),
        )
        .bind(format!("v_{}", TimelineItemId::new()))
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        Ok((
            message_id.to_string(),
            Some(timeline_item_id.to_string()),
            display_order,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn append_tool_result_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: TurnId,
        tool_call_id: &str,
        provider_call_id: &str,
        tool_name: &str,
        status: &str,
        summary: &Value,
        model_parts: &Value,
        actor: &Value,
        now: &str,
    ) -> Result<(String, String, i64), SessionsError> {
        let display_order = self.next_timeline_position_in_tx(tx, session_id).await?;
        let timeline_item_id = TimelineItemId::new();
        sqlx::query(
            "INSERT INTO timeline_items \
             (id, session_id, turn_id, kind, source_resource_id, display_order, \
              projection_json, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'tool_call', ?, ?, ?, 'active', ?, ?, ?)",
        )
        .bind(timeline_item_id.to_string())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(tool_call_id)
        .bind(display_order)
        .bind(
            json!({
                "kind": "tool_call",
                "tool_call_id": tool_call_id,
                "tool_name": tool_name,
                "status": status,
                "summary": summary,
            })
            .to_string(),
        )
        .bind(format!("v_{}", TimelineItemId::new()))
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        let message_id = MessageId::new();
        sqlx::query(
            "INSERT INTO messages \
             (id, session_id, turn_id, actor_json, kind, body_json, status, \
              timeline_sequence, version, created_at) \
             VALUES (?, ?, ?, ?, 'tool_result_ref', ?, 'active', ?, ?, ?)",
        )
        .bind(message_id.to_string())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(actor.to_string())
        .bind(
            json!({
                "parts": model_parts,
                "tool_call_id": provider_call_id,
                "resource_tool_call_id": tool_call_id,
            })
            .to_string(),
        )
        .bind(display_order)
        .bind(format!("v_{}", MessageId::new()))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        Ok((
            message_id.to_string(),
            timeline_item_id.to_string(),
            display_order,
        ))
    }

    pub async fn transition_active_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: TurnId,
        from_status: TurnStatus,
        to_status: TurnStatus,
        reason: Option<&str>,
        now: &str,
    ) -> Result<Option<TurnTransition>, SessionsError> {
        let changed = sqlx::query(
            "UPDATE turns SET status = ?, completion_reason = COALESCE(?, completion_reason), \
                    updated_at = ? \
             WHERE id = ? AND session_id = ? AND status = ? \
               AND EXISTS(SELECT 1 FROM sessions WHERE id = ? AND active_turn_id = ?)",
        )
        .bind(to_status.as_str())
        .bind(reason)
        .bind(now)
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .bind(from_status.as_str())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() != 1 {
            return Ok(None);
        }
        let session_version = format!("v_{}", SessionId::new());
        let session_changed = sqlx::query(
            "UPDATE sessions SET version = ?, updated_at = ? \
             WHERE id = ? AND active_turn_id = ?",
        )
        .bind(&session_version)
        .bind(now)
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        if session_changed.rows_affected() != 1 {
            return Err(SessionsError::Internal(anyhow::anyhow!(
                "active Turn lost Session ownership during transition"
            )));
        }
        Ok(Some(TurnTransition {
            from_status,
            to_status,
            session_version,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn settle_active_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: TurnId,
        from_status: TurnStatus,
        terminal_status: TurnStatus,
        reason: Option<&str>,
        summary: Option<&Value>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        now: &str,
    ) -> Result<Option<TurnTransition>, SessionsError> {
        let summary_json = summary.map(Value::to_string);
        let changed = if terminal_status == TurnStatus::Canceled {
            sqlx::query(
                "UPDATE turns SET status = 'canceled', \
                        cancellation_reason = COALESCE(?, cancellation_reason), updated_at = ? \
                 WHERE id = ? AND session_id = ? AND status = ? \
                   AND EXISTS(SELECT 1 FROM sessions WHERE id = ? AND active_turn_id = ?)",
            )
            .bind(reason)
            .bind(now)
            .bind(turn_id.to_string())
            .bind(session_id.to_string())
            .bind(from_status.as_str())
            .bind(session_id.to_string())
            .bind(turn_id.to_string())
            .execute(&mut *tx)
            .await?
        } else {
            sqlx::query(
                "UPDATE turns SET status = ?, completion_reason = COALESCE(?, completion_reason), \
                        completion_summary_json = COALESCE(?, completion_summary_json), \
                        input_tokens = COALESCE(?, input_tokens), \
                        output_tokens = COALESCE(?, output_tokens), updated_at = ? \
                 WHERE id = ? AND session_id = ? AND status = ? \
                   AND EXISTS(SELECT 1 FROM sessions WHERE id = ? AND active_turn_id = ?)",
            )
            .bind(terminal_status.as_str())
            .bind(reason)
            .bind(summary_json)
            .bind(input_tokens)
            .bind(output_tokens)
            .bind(now)
            .bind(turn_id.to_string())
            .bind(session_id.to_string())
            .bind(from_status.as_str())
            .bind(session_id.to_string())
            .bind(turn_id.to_string())
            .execute(&mut *tx)
            .await?
        };
        if changed.rows_affected() != 1 {
            return Ok(None);
        }
        let session_version = format!("v_{}", SessionId::new());
        let released = sqlx::query(
            "UPDATE sessions SET state = 'ready', active_turn_id = NULL, version = ?, \
                    updated_at = ?, last_activity_at = ? \
             WHERE id = ? AND active_turn_id = ?",
        )
        .bind(&session_version)
        .bind(now)
        .bind(now)
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        if released.rows_affected() != 1 {
            return Err(SessionsError::Internal(anyhow::anyhow!(
                "terminal Turn lost Session ownership during settlement"
            )));
        }
        Ok(Some(TurnTransition {
            from_status,
            to_status: terminal_status,
            session_version,
        }))
    }

    pub async fn insert_checkpoint_for_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: TurnId,
        workspace_revision: &str,
        now: &str,
    ) -> Result<(), SessionsError> {
        let row = sqlx::query(
            "SELECT input_message_id, \
                    COALESCE((SELECT timeline_sequence FROM messages WHERE id = turns.input_message_id), 0) \
                        AS timeline_position \
             FROM turns WHERE id = ? AND session_id = ?",
        )
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(SessionsError::NotFound)?;
        let message_id: String = row.try_get("input_message_id")?;
        let timeline_position: i64 = row.try_get("timeline_position")?;
        self.insert_checkpoint_in_tx(
            tx,
            session_id,
            &turn_id.to_string(),
            &message_id,
            timeline_position,
            workspace_revision,
            now,
        )
        .await
        .map(|_| ())
    }

    async fn next_timeline_position_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
    ) -> Result<i64, SessionsError> {
        Ok(sqlx::query_scalar(
            "SELECT COALESCE(MAX(position), 0) + 1 FROM (\
                 SELECT display_order AS position FROM timeline_items WHERE session_id = ? \
                 UNION ALL \
                 SELECT timeline_sequence AS position FROM messages \
                 WHERE session_id = ? AND timeline_sequence IS NOT NULL\
             )",
        )
        .bind(session_id.to_string())
        .bind(session_id.to_string())
        .fetch_one(&mut *tx)
        .await?)
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_checkpoint_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: &str,
        message_id: &str,
        timeline_position: i64,
        workspace_revision: &str,
        now: &str,
    ) -> Result<String, SessionsError> {
        let checkpoint_id = CheckpointId::new().to_string();
        sqlx::query(
            "INSERT INTO checkpoints \
             (id, session_id, kind, timeline_position, workspace_revision_id, \
              source_message_id, source_turn_id, created_at) \
             VALUES (?, ?, 'pre_turn', ?, ?, ?, ?, ?)",
        )
        .bind(&checkpoint_id)
        .bind(session_id.to_string())
        .bind(timeline_position)
        .bind(workspace_revision)
        .bind(message_id)
        .bind(turn_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        Ok(checkpoint_id)
    }

    pub fn now(&self) -> String {
        format_utc(SystemClock.now())
    }
}
