use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};

use crate::modules::workspace_sync::interface::WorkspaceHandle;
use crate::platform::{
    clock::{Clock, SystemClock, format_utc},
    id::{AttachmentId, CheckpointId, MessageId, SessionId, TimelineItemId, TurnId},
};

use super::interface::SessionsInterface;
use super::types::{
    ActiveTurnOutcome, AppendAssistantMessage, ContextMessage, CreatedTurnInput, ExecutionTurn,
    MAX_ATTACHMENTS, MAX_MESSAGE_BYTES, QueuedTurnCandidate, RecordAskAnswer, RecordedTurnInput,
    RecoveredTurn, SessionCommandState, SessionModelPreference, SessionsError, TurnBlockerOutcome,
    TurnBlockers, TurnModelSnapshot, TurnStatus, TurnTransition,
};

const TURN_IS_RUNNABLE_SQL: &str = "SELECT EXISTS( \
        SELECT 1 FROM turns AS turn \
        JOIN sessions AS session ON session.id = turn.session_id \
        WHERE turn.id = ? AND turn.session_id = ? AND turn.status = 'running' \
          AND session.active_turn_id = turn.id \
     )";

struct ActiveTurnTransition<'a> {
    session_id: SessionId,
    turn_id: TurnId,
    from_status: TurnStatus,
    to_status: TurnStatus,
    reason: Option<&'a str>,
    now: &'a str,
}

struct MessageAttachment {
    id: AttachmentId,
    name: String,
    mime: String,
    byte_size: u64,
}

impl SessionsInterface {
    pub async fn execution_turn(&self, turn_id: TurnId) -> Result<ExecutionTurn, SessionsError> {
        let row = sqlx::query(
            "SELECT turn.id, turn.session_id, session.project_id, \
                    turn.status, turn.sequence, turn.model_snapshot_json, session.active_turn_id \
             FROM turns AS turn \
             JOIN sessions AS session ON session.id = turn.session_id \
             WHERE turn.id = ?",
        )
        .bind(turn_id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(SessionsError::NotFound)?;
        let id: TurnId = row
            .try_get::<String, _>("id")?
            .parse::<TurnId>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
        let active_turn_id: Option<String> = row.try_get("active_turn_id")?;
        let active = active_turn_id.as_deref() == Some(id.to_string().as_str());
        let status: String = row.try_get("status")?;
        let model_snapshot_json: String = row.try_get("model_snapshot_json")?;
        Ok(ExecutionTurn {
            active,
            id,
            session_id: row
                .try_get::<String, _>("session_id")?
                .parse::<SessionId>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?,
            project_id: row
                .try_get::<String, _>("project_id")?
                .parse::<crate::platform::id::ProjectId>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?,
            status: status
                .parse()
                .map_err(|error: String| SessionsError::Internal(anyhow::anyhow!(error)))?,
            sequence: row.try_get("sequence")?,
            model_snapshot: TurnModelSnapshot::parse(&model_snapshot_json)?,
        })
    }

    pub async fn turn_is_runnable(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<bool, SessionsError> {
        let runnable: i64 = sqlx::query_scalar(TURN_IS_RUNNABLE_SQL)
            .bind(turn_id.to_string())
            .bind(session_id.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(runnable == 1)
    }

    pub async fn turn_is_runnable_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<bool, SessionsError> {
        let runnable: i64 = sqlx::query_scalar(TURN_IS_RUNNABLE_SQL)
            .bind(turn_id.to_string())
            .bind(session_id.to_string())
            .fetch_one(&mut *tx)
            .await?;
        Ok(runnable == 1)
    }

    pub async fn reconcile_turn_blockers_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: TurnId,
        blockers: TurnBlockers,
        now: &str,
    ) -> Result<TurnBlockerOutcome, SessionsError> {
        let row = sqlx::query(
            "SELECT turn.session_id, turn.status, session.active_turn_id, session.version \
             FROM turns AS turn \
             JOIN sessions AS session ON session.id = turn.session_id \
             WHERE turn.id = ?",
        )
        .bind(turn_id.to_string())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(SessionsError::NotFound)?;
        let session_id = row
            .try_get::<String, _>("session_id")?
            .parse::<SessionId>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
        let current_status = row
            .try_get::<String, _>("status")?
            .parse::<TurnStatus>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
        let active_turn_id: Option<String> = row.try_get("active_turn_id")?;
        let active = active_turn_id.as_deref() == Some(turn_id.to_string().as_str());
        let current_session_version: String = row.try_get("version")?;
        let can_reconcile = active
            && matches!(
                current_status,
                TurnStatus::Running | TurnStatus::WaitingForAsk | TurnStatus::WaitingForJob
            );
        let target_status = blockers.status();
        if !can_reconcile || current_status == target_status {
            return Ok(TurnBlockerOutcome {
                session_id,
                status: current_status,
                active,
                session_version: current_session_version,
                transition: None,
            });
        }
        let transition = self
            .transition_active_turn_in_tx(
                tx,
                ActiveTurnTransition {
                    session_id,
                    turn_id,
                    from_status: current_status,
                    to_status: target_status,
                    reason: None,
                    now,
                },
            )
            .await?;
        let session_version = transition
            .as_ref()
            .map(|transition| transition.session_version.clone())
            .unwrap_or(current_session_version);
        Ok(TurnBlockerOutcome {
            session_id,
            status: transition
                .as_ref()
                .map_or(current_status, |transition| transition.to_status),
            active,
            session_version,
            transition,
        })
    }

    pub(crate) async fn interrupt_active_turns_in_tx(
        &self,
        tx: &mut SqliteConnection,
        now: &str,
    ) -> Result<Vec<RecoveredTurn>, SessionsError> {
        let rows = sqlx::query(
            "SELECT id, session_id, status FROM turns \
             WHERE status IN ('running', 'waiting_for_job', 'waiting_for_ask', \
                              'waiting_for_model', 'canceling') \
             ORDER BY session_id, sequence",
        )
        .fetch_all(&mut *tx)
        .await?;
        let mut recovered = Vec::with_capacity(rows.len());
        for row in rows {
            let turn_id = row
                .try_get::<String, _>("id")?
                .parse::<TurnId>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
            let session_id = row
                .try_get::<String, _>("session_id")?
                .parse::<SessionId>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
            let from_status = row
                .try_get::<String, _>("status")?
                .parse::<TurnStatus>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
            let turn_version = format!("v_{}", TurnId::new());
            let changed = sqlx::query(
                "UPDATE turns SET status = 'interrupted', \
                        completion_reason = 'control_plane_restart', version = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(&turn_version)
            .bind(now)
            .bind(turn_id.to_string())
            .bind(from_status.as_str())
            .execute(&mut *tx)
            .await?;
            if changed.rows_affected() != 1 {
                continue;
            }
            let next_session_version = format!("v_{}", SessionId::new());
            let released = sqlx::query(
                "UPDATE sessions SET state = 'ready', active_turn_id = NULL, version = ?, \
                        updated_at = ?, last_activity_at = ? \
                 WHERE id = ? AND active_turn_id = ?",
            )
            .bind(&next_session_version)
            .bind(now)
            .bind(now)
            .bind(session_id.to_string())
            .bind(turn_id.to_string())
            .execute(&mut *tx)
            .await?;
            recovered.push(RecoveredTurn {
                turn_id,
                session_id,
                from_status,
                turn_version,
                session_version: (released.rows_affected() == 1).then_some(next_session_version),
            });
        }
        Ok(recovered)
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
        model_preference: Option<Option<&SessionModelPreference>>,
        now: &str,
    ) -> Result<SessionCommandState, SessionsError> {
        let row = sqlx::query(
            "SELECT project_id, state, workspace_handle, next_model_ref, active_turn_id, version \
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
        let stored_next_model_ref: Option<String> = row.try_get("next_model_ref")?;
        let next_model_ref = match model_preference {
            Some(Some(preference)) => Some(serde_json::to_string(preference)?),
            Some(None) => None,
            None => stored_next_model_ref,
        };
        let updated = if model_preference.is_some() {
            sqlx::query(
                "UPDATE sessions SET next_model_ref = ?, version = ?, updated_at = ?, \
                 last_activity_at = ? WHERE id = ? AND version = ? AND state != 'deleting'",
            )
            .bind(&next_model_ref)
            .bind(&session_version)
            .bind(now)
            .bind(now)
            .bind(session_id.to_string())
            .bind(expected_version)
            .execute(&mut *tx)
            .await?
        } else {
            sqlx::query(
                "UPDATE sessions SET version = ?, updated_at = ?, last_activity_at = ? \
                 WHERE id = ? AND version = ? AND state != 'deleting'",
            )
            .bind(&session_version)
            .bind(now)
            .bind(now)
            .bind(session_id.to_string())
            .bind(expected_version)
            .execute(&mut *tx)
            .await?
        };
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
            next_model_ref,
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
        attachment_ids: &[AttachmentId],
        model_snapshot: Option<&TurnModelSnapshot>,
        checkpoint_revision: Option<&str>,
        now: &str,
    ) -> Result<CreatedTurnInput, SessionsError> {
        if content.trim().is_empty() && attachment_ids.is_empty() {
            return Err(SessionsError::Validation(
                "message content or an attachment is required".into(),
            ));
        }
        if attachment_ids.len() > usize::from(MAX_ATTACHMENTS) {
            return Err(SessionsError::Validation(format!(
                "a message supports at most {MAX_ATTACHMENTS} attachments"
            )));
        }
        let mut unique = std::collections::BTreeSet::new();
        let mut attachments = Vec::with_capacity(attachment_ids.len());
        let mut message_bytes =
            u64::try_from(content.len()).map_err(|error| SessionsError::Internal(error.into()))?;
        for attachment_id in attachment_ids {
            if !unique.insert(attachment_id.to_string()) {
                return Err(SessionsError::Validation(
                    "attachment ids must be unique".into(),
                ));
            }
            let row: Option<(String, String, i64)> = sqlx::query_as(
                "SELECT name, mime, byte_size FROM attachments \
                 WHERE id = ? AND session_id = ? AND lifecycle IN ('draft', 'attached')",
            )
            .bind(attachment_id.to_string())
            .bind(session_id.to_string())
            .fetch_optional(&mut *tx)
            .await?;
            let Some((name, mime, byte_size)) = row else {
                return Err(SessionsError::Validation(
                    "attachment is missing or belongs to another session".into(),
                ));
            };
            let byte_size =
                u64::try_from(byte_size).map_err(|error| SessionsError::Internal(error.into()))?;
            message_bytes = message_bytes
                .checked_add(byte_size)
                .ok_or_else(|| SessionsError::Validation("message is too large".into()))?;
            attachments.push(MessageAttachment {
                id: *attachment_id,
                name,
                mime,
                byte_size,
            });
        }
        if message_bytes > MAX_MESSAGE_BYTES {
            return Err(SessionsError::Validation(format!(
                "message content and attachments exceed {MAX_MESSAGE_BYTES} bytes"
            )));
        }
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
        let model_snapshot_json = serde_json::to_string(&model_snapshot)?;
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
        .bind(model_snapshot_json)
        .bind(predecessor_turn_id)
        .bind(predecessor_turn_id)
        .bind(format!("v_{}", TurnId::new()))
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        let mut parts = Vec::with_capacity(attachments.len() + 1);
        if !content.is_empty() {
            parts.push(json!({"type": "text", "text": content}));
        }
        parts.extend(attachments.iter().map(|attachment| {
            json!({
                "type": "attachment_reference",
                "attachment_id": attachment.id.to_string(),
                "name": attachment.name,
                "mime": attachment.mime,
                "byte_size": attachment.byte_size,
            })
        }));
        let mut body = json!({"parts": parts});
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

        for (ordinal, attachment) in attachments.iter().enumerate() {
            sqlx::query(
                "INSERT INTO message_attachments (message_id, attachment_id, ord) \
                 VALUES (?, ?, ?)",
            )
            .bind(message_id.to_string())
            .bind(attachment.id.to_string())
            .bind(i64::try_from(ordinal).map_err(|error| SessionsError::Internal(error.into()))?)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE attachments SET lifecycle = 'attached', version = ? \
                 WHERE id = ? AND lifecycle = 'draft'",
            )
            .bind(format!("v_{}", AttachmentId::new()))
            .bind(attachment.id.to_string())
            .execute(&mut *tx)
            .await?;
        }

        let mut projection = json!({
            "kind": "user_message",
            "message_id": message_id.to_string(),
            "turn_id": turn_id.to_string(),
            "text": content,
            "attachments": attachments.iter().map(|attachment| json!({
                "id": attachment.id.to_string(),
                "name": attachment.name,
                "mime": attachment.mime,
                "byte_size": attachment.byte_size,
            })).collect::<Vec<_>>(),
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

    pub async fn record_ask_answer_in_tx(
        &self,
        tx: &mut SqliteConnection,
        input: RecordAskAnswer<'_>,
    ) -> Result<RecordedTurnInput, SessionsError> {
        let RecordAskAnswer {
            session_id,
            turn_id,
            ask_id,
            answer,
            actor,
            now,
        } = input;
        let display_order = self.next_timeline_position_in_tx(tx, session_id).await?;
        let message_id = MessageId::new();
        let timeline_item_id = TimelineItemId::new();
        let text = answer
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| answer.to_string());
        let body = json!({
            "parts": [{"type": "text", "text": text}],
            "turn_input": {"kind": "ask_answer", "source_ask_id": ask_id.to_string()},
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
            "kind": "user_message",
            "message_id": message_id.to_string(),
            "turn_id": turn_id.to_string(),
            "text": text,
            "source_ask_id": ask_id.to_string(),
        });
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
        Ok(RecordedTurnInput {
            message_id: message_id.to_string(),
            timeline_item_id: timeline_item_id.to_string(),
            display_order,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn append_steer_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        expected_turn_id: Option<TurnId>,
        content: &str,
        expected_version: &str,
        actor: &Value,
        source_ask_id: Option<&str>,
        now: &str,
    ) -> Result<(super::types::SteerResult, String), SessionsError> {
        let row = sqlx::query(
            "SELECT session.state, session.active_turn_id, session.version, turn.status \
             FROM sessions AS session \
             LEFT JOIN turns AS turn ON turn.id = session.active_turn_id \
             WHERE session.id = ?",
        )
        .bind(session_id.to_string())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(SessionsError::NotFound)?;
        if row.try_get::<String, _>("state")? == "deleting" {
            return Err(SessionsError::SessionDeleting);
        }
        let current_version: String = row.try_get("version")?;
        if current_version != expected_version {
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.to_owned(),
                current: current_version,
            });
        }
        let active_turn_id = row
            .try_get::<Option<String>, _>("active_turn_id")?
            .ok_or(SessionsError::TurnNotInteractive)?
            .parse::<TurnId>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
        if expected_turn_id.is_some_and(|expected| expected != active_turn_id) {
            return Err(SessionsError::TurnNotInteractive);
        }
        let status = row
            .try_get::<Option<String>, _>("status")?
            .ok_or(SessionsError::TurnNotInteractive)?
            .parse::<TurnStatus>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
        if !status.is_interactive() {
            return Err(SessionsError::TurnNotInteractive);
        }

        let display_order = self.next_timeline_position_in_tx(tx, session_id).await?;
        let message_id = MessageId::new();
        let timeline_item_id = TimelineItemId::new();
        let mut body = json!({"parts": [{"type": "text", "text": content}], "steer": true});
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
        .bind(active_turn_id.to_string())
        .bind(actor.to_string())
        .bind(body.to_string())
        .bind(display_order)
        .bind(format!("v_{}", MessageId::new()))
        .bind(now)
        .execute(&mut *tx)
        .await?;

        let mut projection = json!({
            "kind": "steer",
            "message_id": message_id.to_string(),
            "turn_id": active_turn_id.to_string(),
            "text": content,
        });
        if let Some(source_ask_id) = source_ask_id {
            projection["source_ask_id"] = json!(source_ask_id);
        }
        sqlx::query(
            "INSERT INTO timeline_items \
             (id, session_id, turn_id, kind, source_resource_id, display_order, \
              projection_json, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'steer', ?, ?, ?, 'active', ?, ?, ?)",
        )
        .bind(timeline_item_id.to_string())
        .bind(session_id.to_string())
        .bind(active_turn_id.to_string())
        .bind(message_id.to_string())
        .bind(display_order)
        .bind(projection.to_string())
        .bind(format!("v_{}", TimelineItemId::new()))
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        let session_version = format!("v_{}", SessionId::new());
        let changed = sqlx::query(
            "UPDATE sessions SET version = ?, updated_at = ? \
             WHERE id = ? AND version = ? AND active_turn_id = ?",
        )
        .bind(&session_version)
        .bind(now)
        .bind(session_id.to_string())
        .bind(expected_version)
        .bind(active_turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() != 1 {
            return Err(SessionsError::Internal(anyhow::anyhow!(
                "Session changed while recording Steer"
            )));
        }
        Ok((
            super::types::SteerResult {
                turn_id: active_turn_id.to_string(),
                message_id: message_id.to_string(),
                session_version,
            },
            timeline_item_id.to_string(),
        ))
    }

    pub async fn activate_created_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: &str,
        model_snapshot: Option<&TurnModelSnapshot>,
        now: &str,
    ) -> Result<bool, SessionsError> {
        let model_snapshot_json = serde_json::to_string(&model_snapshot)?;
        let promoted = sqlx::query(
            "UPDATE turns SET status = 'running', model_snapshot_json = ?, updated_at = ? \
             WHERE id = ? AND session_id = ? AND status = 'queued'",
        )
        .bind(model_snapshot_json)
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
        model_snapshot: Option<&TurnModelSnapshot>,
        now: &str,
    ) -> Result<bool, SessionsError> {
        let model_snapshot_json = serde_json::to_string(&model_snapshot)?;
        let promoted = sqlx::query(
            "UPDATE turns SET status = 'running', model_snapshot_json = ?, updated_at = ? \
             WHERE id = ? AND session_id = ? AND status = 'queued' \
               AND predecessor_turn_id = ?",
        )
        .bind(model_snapshot_json)
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

    pub async fn append_assistant_message_in_tx(
        &self,
        tx: &mut SqliteConnection,
        input: AppendAssistantMessage<'_>,
    ) -> Result<(String, Option<String>, i64), SessionsError> {
        let AppendAssistantMessage {
            session_id,
            turn_id,
            round_id,
            text,
            reasoning,
            duration_ms,
            tool_calls,
            actor,
            now,
        } = input;
        let display_order = self.next_timeline_position_in_tx(tx, session_id).await?;
        let message_id = MessageId::new();
        let body = json!({
            "parts": [{"type": "text", "text": text}],
            "reasoning": reasoning,
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
        if text.is_empty() && reasoning.is_empty() {
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
        .bind({
            let mut proj = json!({
                "kind": "assistant_message",
                "message_id": message_id.to_string(),
                "round_id": round_id.to_string(),
                "text": text,
                "reasoning": reasoning,
            });
            if let Some(ms) = duration_ms {
                proj.as_object_mut()
                    .expect("proj is object")
                    .insert("duration_ms".into(), json!(ms));
            }
            proj.to_string()
        })
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

    #[allow(clippy::too_many_arguments)]
    pub async fn replace_tool_result_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        source_turn_id: TurnId,
        tool_call_id: &str,
        provider_call_id: &str,
        tool_name: &str,
        status: &str,
        summary: &Value,
        model_parts: &Value,
        now: &str,
    ) -> Result<String, SessionsError> {
        let projection = json!({
            "kind": "tool_call",
            "tool_call_id": tool_call_id,
            "tool_name": tool_name,
            "status": status,
            "summary": summary,
        });
        let timeline_item_id: Option<String> = sqlx::query_scalar(
            "UPDATE timeline_items SET projection_json = ?, version = ?, updated_at = ? \
             WHERE session_id = ? AND turn_id = ? AND kind = 'tool_call' \
               AND source_resource_id = ? AND status = 'active' \
             RETURNING id",
        )
        .bind(projection.to_string())
        .bind(format!("v_{}", TimelineItemId::new()))
        .bind(now)
        .bind(session_id.to_string())
        .bind(source_turn_id.to_string())
        .bind(tool_call_id)
        .fetch_optional(&mut *tx)
        .await?;
        let timeline_item_id = timeline_item_id.ok_or_else(|| {
            SessionsError::Internal(anyhow::anyhow!("Tool Call timeline projection is missing"))
        })?;

        let message = json!({
            "parts": model_parts,
            "tool_call_id": provider_call_id,
            "resource_tool_call_id": tool_call_id,
        });
        let updated = sqlx::query(
            "UPDATE messages SET body_json = ?, version = ? \
             WHERE session_id = ? AND turn_id = ? AND kind = 'tool_result_ref' \
               AND status = 'active' \
               AND json_extract(body_json, '$.resource_tool_call_id') = ?",
        )
        .bind(message.to_string())
        .bind(format!("v_{}", MessageId::new()))
        .bind(session_id.to_string())
        .bind(source_turn_id.to_string())
        .bind(tool_call_id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(SessionsError::Internal(anyhow::anyhow!(
                "Tool Call protocol result is missing or duplicated"
            )));
        }
        Ok(timeline_item_id)
    }

    pub async fn wait_for_model_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: TurnId,
        reason: &str,
        now: &str,
    ) -> Result<Option<TurnTransition>, SessionsError> {
        self.transition_active_turn_in_tx(
            tx,
            ActiveTurnTransition {
                session_id,
                turn_id,
                from_status: TurnStatus::Running,
                to_status: TurnStatus::WaitingForModel,
                reason: Some(reason),
                now,
            },
        )
        .await
    }

    pub async fn retry_waiting_model_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: TurnId,
        now: &str,
    ) -> Result<Option<TurnTransition>, SessionsError> {
        self.transition_active_turn_in_tx(
            tx,
            ActiveTurnTransition {
                session_id,
                turn_id,
                from_status: TurnStatus::WaitingForModel,
                to_status: TurnStatus::Running,
                reason: None,
                now,
            },
        )
        .await
    }

    async fn transition_active_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
        transition: ActiveTurnTransition<'_>,
    ) -> Result<Option<TurnTransition>, SessionsError> {
        let ActiveTurnTransition {
            session_id,
            turn_id,
            from_status,
            to_status,
            reason,
            now,
        } = transition;
        if !from_status.can_transition_to(to_status) {
            return Err(SessionsError::Internal(anyhow::anyhow!(
                "invalid active Turn transition: {} -> {}",
                from_status.as_str(),
                to_status.as_str()
            )));
        }
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

    pub async fn settle_active_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: TurnId,
        outcome: ActiveTurnOutcome<'_>,
        now: &str,
    ) -> Result<Option<TurnTransition>, SessionsError> {
        let (
            from_status,
            terminal_status,
            completion_reason,
            cancellation_reason,
            summary,
            input_tokens,
            output_tokens,
        ) = match outcome {
            ActiveTurnOutcome::Completed {
                summary,
                input_tokens,
                output_tokens,
            } => (
                TurnStatus::Running,
                TurnStatus::Completed,
                Some("finish"),
                None,
                Some(summary.to_string()),
                Some(input_tokens),
                Some(output_tokens),
            ),
            ActiveTurnOutcome::Failed { reason, summary } => (
                TurnStatus::Running,
                TurnStatus::Failed,
                Some(reason),
                None,
                Some(summary.to_string()),
                None,
                None,
            ),
            ActiveTurnOutcome::Canceled { reason } => (
                TurnStatus::Canceling,
                TurnStatus::Canceled,
                None,
                Some(reason),
                None,
                None,
                None,
            ),
            ActiveTurnOutcome::Interrupted { reason } => (
                TurnStatus::Canceling,
                TurnStatus::Interrupted,
                Some(reason),
                None,
                None,
                None,
                None,
            ),
        };
        if !from_status.can_transition_to(terminal_status) {
            return Err(SessionsError::Internal(anyhow::anyhow!(
                "invalid terminal Turn transition: {} -> {}",
                from_status.as_str(),
                terminal_status.as_str()
            )));
        }
        let changed = sqlx::query(
            "UPDATE turns SET status = ?, completion_reason = COALESCE(?, completion_reason), \
                    cancellation_reason = COALESCE(?, cancellation_reason), \
                    completion_summary_json = COALESCE(?, completion_summary_json), \
                    input_tokens = COALESCE(?, input_tokens), \
                    output_tokens = COALESCE(?, output_tokens), updated_at = ? \
             WHERE id = ? AND session_id = ? AND status = ? \
               AND EXISTS(SELECT 1 FROM sessions WHERE id = ? AND active_turn_id = ?)",
        )
        .bind(terminal_status.as_str())
        .bind(completion_reason)
        .bind(cancellation_reason)
        .bind(summary)
        .bind(input_tokens)
        .bind(output_tokens)
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

    pub async fn accept_cancel_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: TurnId,
        reason: &str,
        expected_version: &str,
        now: &str,
    ) -> Result<Option<TurnTransition>, SessionsError> {
        let row = sqlx::query(
            "SELECT turn.status, session.version, session.active_turn_id FROM turns AS turn \
             JOIN sessions AS session ON session.id = turn.session_id \
             WHERE turn.id = ? AND turn.session_id = ?",
        )
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let current_version: String = row.try_get("version")?;
        if current_version != expected_version {
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.to_owned(),
                current: current_version,
            });
        }
        let from_status = row
            .try_get::<String, _>("status")?
            .parse::<TurnStatus>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
        if from_status == TurnStatus::Queued {
            if !from_status.can_transition_to(TurnStatus::Canceled) {
                return Err(SessionsError::Internal(anyhow::anyhow!(
                    "invalid queued Turn transition: {} -> {}",
                    from_status.as_str(),
                    TurnStatus::Canceled.as_str()
                )));
            }
            let changed = sqlx::query(
                "UPDATE turns SET status = 'canceled', cancellation_reason = ?, updated_at = ? \
                 WHERE id = ? AND session_id = ? AND status = 'queued'",
            )
            .bind(reason)
            .bind(now)
            .bind(turn_id.to_string())
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?;
            if changed.rows_affected() != 1 {
                return Ok(None);
            }
            let session_version = format!("v_{}", SessionId::new());
            let session_changed = sqlx::query(
                "UPDATE sessions SET version = ?, updated_at = ?, last_activity_at = ? \
                 WHERE id = ? AND version = ?",
            )
            .bind(&session_version)
            .bind(now)
            .bind(now)
            .bind(session_id.to_string())
            .bind(expected_version)
            .execute(&mut *tx)
            .await?;
            if session_changed.rows_affected() != 1 {
                return Err(SessionsError::Internal(anyhow::anyhow!(
                    "Session changed while canceling queued Turn"
                )));
            }
            return Ok(Some(TurnTransition {
                from_status,
                to_status: TurnStatus::Canceled,
                session_version,
            }));
        }
        let active_turn_id: Option<String> = row.try_get("active_turn_id")?;
        if active_turn_id.as_deref() != Some(turn_id.to_string().as_str()) {
            return Ok(None);
        }
        if !from_status.can_transition_to(TurnStatus::Canceling) {
            return Ok(None);
        }
        let changed = sqlx::query(
            "UPDATE turns SET status = 'canceling', cancellation_reason = ?, updated_at = ? \
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
        .await?;
        if changed.rows_affected() != 1 {
            return Ok(None);
        }
        let session_version = format!("v_{}", SessionId::new());
        let session_changed = sqlx::query(
            "UPDATE sessions SET version = ?, updated_at = ? \
             WHERE id = ? AND active_turn_id = ? AND version = ?",
        )
        .bind(&session_version)
        .bind(now)
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(expected_version)
        .execute(&mut *tx)
        .await?;
        if session_changed.rows_affected() != 1 {
            return Err(SessionsError::Internal(anyhow::anyhow!(
                "active Session changed while accepting Turn cancellation"
            )));
        }
        Ok(Some(TurnTransition {
            from_status,
            to_status: TurnStatus::Canceling,
            session_version,
        }))
    }

    pub async fn settle_cancel_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: TurnId,
        uncertain: bool,
        reason: &str,
        now: &str,
    ) -> Result<Option<TurnTransition>, SessionsError> {
        self.settle_active_turn_in_tx(
            tx,
            session_id,
            turn_id,
            if uncertain {
                ActiveTurnOutcome::Interrupted { reason }
            } else {
                ActiveTurnOutcome::Canceled { reason }
            },
            now,
        )
        .await
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

    pub async fn queued_turn_candidate_in_tx(
        &self,
        tx: &mut SqliteConnection,
        terminal_turn_id: TurnId,
        session_id: SessionId,
    ) -> Result<Option<QueuedTurnCandidate>, SessionsError> {
        let row = sqlx::query(
            "SELECT next_turn.id, next_turn.session_id, next_turn.model_snapshot_json \
             FROM turns AS terminal_turn \
             JOIN sessions AS session ON session.id = terminal_turn.session_id \
             JOIN turns AS next_turn ON next_turn.session_id = terminal_turn.session_id \
             WHERE terminal_turn.id = ? AND terminal_turn.session_id = ? \
               AND terminal_turn.status IN ('completed', 'canceled') \
               AND session.active_turn_id IS NULL \
               AND next_turn.status = 'queued' \
               AND next_turn.sequence = (SELECT MIN(sequence) FROM turns \
                                          WHERE session_id = terminal_turn.session_id \
                                            AND status = 'queued')",
        )
        .bind(terminal_turn_id.to_string())
        .bind(session_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        row.map(|row| {
            Ok(QueuedTurnCandidate {
                turn_id: row
                    .try_get::<String, _>("id")?
                    .parse::<TurnId>()
                    .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?,
                session_id: row
                    .try_get::<String, _>("session_id")?
                    .parse::<SessionId>()
                    .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?,
                model_snapshot: TurnModelSnapshot::parse(
                    &row.try_get::<String, _>("model_snapshot_json")?,
                )?,
            })
        })
        .transpose()
    }

    pub async fn activate_queued_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
        candidate: &QueuedTurnCandidate,
        model_snapshot: Option<&TurnModelSnapshot>,
        workspace_revision: &str,
        now: &str,
    ) -> Result<Option<String>, SessionsError> {
        let model_snapshot_json = serde_json::to_string(&model_snapshot)?;
        let promoted = sqlx::query(
            "UPDATE turns SET status = 'running', model_snapshot_json = ?, updated_at = ? \
             WHERE id = ? AND session_id = ? AND status = 'queued'",
        )
        .bind(model_snapshot_json)
        .bind(now)
        .bind(candidate.turn_id.to_string())
        .bind(candidate.session_id.to_string())
        .execute(&mut *tx)
        .await?;
        if promoted.rows_affected() != 1 {
            return Ok(None);
        }
        let session_version = format!("v_{}", SessionId::new());
        let claimed = sqlx::query(
            "UPDATE sessions SET state = 'active', active_turn_id = ?, version = ?, \
                    updated_at = ?, last_activity_at = ? \
             WHERE id = ? AND active_turn_id IS NULL",
        )
        .bind(candidate.turn_id.to_string())
        .bind(&session_version)
        .bind(now)
        .bind(now)
        .bind(candidate.session_id.to_string())
        .execute(&mut *tx)
        .await?;
        if claimed.rows_affected() != 1 {
            return Ok(None);
        }
        self.insert_checkpoint_for_turn_in_tx(
            tx,
            candidate.session_id,
            candidate.turn_id,
            workspace_revision,
            now,
        )
        .await?;
        Ok(Some(session_version))
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
