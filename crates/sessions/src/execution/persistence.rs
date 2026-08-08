//! Turn content persistence: read queries and transactional writes for turn inputs, messages, tool results, and checkpoints.

use super::*;

struct MessageAttachment {
    id: AttachmentId,
    name: String,
    mime: String,
    byte_size: u64,
}

struct CheckpointInput<'a> {
    session_id: SessionId,
    turn_id: &'a str,
    message_id: &'a str,
    timeline_position: i64,
    workspace_revision: &'a str,
    now: &'a str,
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
                .parse::<ProjectId>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?,
            status: status
                .parse()
                .map_err(|error: String| SessionsError::Internal(anyhow::anyhow!(error)))?,
            sequence: row.try_get("sequence")?,
            model_snapshot: TurnModelSnapshot::parse(&model_snapshot_json)?,
        })
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
                     source_turn.status IN ('completed', 'failed', 'canceled', 'interrupted', 'handed_off'))) \
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
            .workspace
            .current_revision(&WorkspaceHandle(session.workspace_handle))
            .await?
            .0)
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

    pub async fn create_turn_input_in_tx(
        &self,
        tx: &mut SqliteConnection,
        input: CreateTurnInput<'_>,
    ) -> Result<CreatedTurnInput, SessionsError> {
        let CreateTurnInput {
            session_id,
            content,
            actor,
            predecessor_turn_id,
            source_ask_id,
            attachment_ids,
            model_snapshot,
            checkpoint_revision,
            now,
        } = input;
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
            projection["ask_answer"] = json!(false);
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
                    CheckpointInput {
                        session_id,
                        turn_id: &turn_id.to_string(),
                        message_id: &message_id.to_string(),
                        timeline_position: display_order,
                        workspace_revision,
                        now,
                    },
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

    pub async fn append_steer_in_tx(
        &self,
        tx: &mut SqliteConnection,
        input: AppendSteerInput<'_>,
    ) -> Result<(crate::types::SteerResult, String), SessionsError> {
        let AppendSteerInput {
            session_id,
            expected_turn_id,
            content,
            expected_version,
            actor,
            source_ask_id,
            now,
        } = input;
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
            crate::types::SteerResult {
                turn_id: active_turn_id.to_string(),
                message_id: message_id.to_string(),
                session_version,
            },
            timeline_item_id.to_string(),
        ))
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

    pub async fn append_tool_result_in_tx(
        &self,
        tx: &mut SqliteConnection,
        input: AppendToolResultInput<'_>,
    ) -> Result<(String, String, i64), SessionsError> {
        let AppendToolResultInput {
            session_id,
            turn_id,
            tool_call_id,
            provider_call_id,
            tool_name,
            status,
            summary,
            model_parts,
            actor,
            now,
        } = input;
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

    pub async fn replace_tool_result_in_tx(
        &self,
        tx: &mut SqliteConnection,
        input: ReplaceToolResultInput<'_>,
    ) -> Result<String, SessionsError> {
        let ReplaceToolResultInput {
            session_id,
            source_turn_id,
            tool_call_id,
            provider_call_id,
            tool_name,
            status,
            summary,
            model_parts,
            now,
        } = input;
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
            CheckpointInput {
                session_id,
                turn_id: &turn_id.to_string(),
                message_id: &message_id,
                timeline_position,
                workspace_revision,
                now,
            },
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

    async fn insert_checkpoint_in_tx(
        &self,
        tx: &mut SqliteConnection,
        input: CheckpointInput<'_>,
    ) -> Result<String, SessionsError> {
        let CheckpointInput {
            session_id,
            turn_id,
            message_id,
            timeline_position,
            workspace_revision,
            now,
        } = input;
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

}
