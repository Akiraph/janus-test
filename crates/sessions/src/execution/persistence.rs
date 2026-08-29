//! Turn content persistence: read queries and transactional writes for turn inputs, messages, tool results, and checkpoints.

use super::*;
use crate::interface::{ContextCompactedTimelineInput, opt_str, read_i64, read_str};
use futures_util::TryStreamExt;
use mongodb::{
    ClientSession,
    bson::{Bson, Document, doc, oid::ObjectId},
    options::ReturnDocument,
};
use serde_json::Value;

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
        let turn = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {"_id": turn_id.to_string()})
            .await?
            .ok_or(SessionsError::NotFound)?;
        let session_id = read_str(&turn, "session_id")?;
        let session = self
            .pool
            .collection::<Document>("sessions")
            .find_one(doc! {"_id": &session_id})
            .await?
            .ok_or(SessionsError::NotFound)?;
        let id: TurnId = read_str(&turn, "_id")?
            .parse::<TurnId>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
        let active_turn_id = opt_str(&session, "active_turn_id");
        let active = active_turn_id.as_deref() == Some(id.to_string().as_str());
        Ok(ExecutionTurn {
            active,
            id,
            session_id: session_id
                .parse::<SessionId>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?,
            project_id: read_str(&session, "project_id")?
                .parse::<ProjectId>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?,
            status: read_str(&turn, "status")?
                .parse()
                .map_err(|error: String| SessionsError::Internal(anyhow::anyhow!(error)))?,
            sequence: read_i64(&turn, "sequence")?,
            goal_mode: read_i64(&turn, "goal_mode")? != 0,
            model_snapshot: TurnModelSnapshot::parse(&read_str(&turn, "model_snapshot_json")?)?,
        })
    }

    pub async fn context_messages(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<ContextMessage>, SessionsError> {
        self.context_messages_since(session_id, turn_id, None).await
    }

    pub async fn context_messages_after_timeline(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        after_timeline_id: &str,
    ) -> Result<Vec<ContextMessage>, SessionsError> {
        self.context_messages_since(session_id, turn_id, Some(after_timeline_id))
            .await
    }

    async fn context_messages_since(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        after_timeline_id: Option<&str>,
    ) -> Result<Vec<ContextMessage>, SessionsError> {
        let current_turn = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {"_id": turn_id.to_string()})
            .await?
            .ok_or(SessionsError::NotFound)?;
        if read_str(&current_turn, "session_id")? != session_id.to_string() {
            return Err(SessionsError::NotFound);
        }
        let current_sequence = read_i64(&current_turn, "sequence")?;
        let mut terminal_ids = Vec::new();
        let mut terminal_turns = self
            .pool
            .collection::<Document>("turns")
            .find(doc! {
                "session_id": session_id.to_string(),
                "status": {"$in": ["completed", "failed", "canceled", "interrupted"]},
                "sequence": {"$lt": current_sequence},
            })
            .await?;
        while let Some(document) = terminal_turns.try_next().await? {
            terminal_ids.push(read_str(&document, "_id")?);
        }
        let after_order = if let Some(after_timeline_id) = after_timeline_id {
            self.pool
                .collection::<Document>("timeline_items")
                .find_one(doc! {
                    "_id": after_timeline_id,
                    "session_id": session_id.to_string(),
                })
                .await?
                .map(|document| read_i64(&document, "display_order"))
                .transpose()?
                .unwrap_or(0)
        } else {
            0
        };
        let mut filter = doc! {
            "session_id": session_id.to_string(),
            "status": "active",
            "$or": [
                {"turn_id": Bson::Null},
                {"turn_id": turn_id.to_string()},
                {"turn_id": {"$in": terminal_ids}},
            ],
        };
        if after_timeline_id.is_some() {
            filter.insert("timeline_sequence", doc! {"$gt": after_order});
        }
        let mut rows = self
            .pool
            .collection::<Document>("messages")
            .find(filter)
            .sort(doc! {"timeline_sequence": 1, "created_at": 1, "_id": 1})
            .await?;
        let mut out = Vec::new();
        while let Some(document) = rows.try_next().await? {
            let body_json = read_str(&document, "body_json")?;
            out.push(ContextMessage {
                turn_id: opt_str(&document, "turn_id"),
                kind: read_str(&document, "kind")?,
                body: serde_json::from_str(&body_json)?,
                timeline_sequence: read_i64(&document, "timeline_sequence").unwrap_or(0),
            });
        }
        Ok(out)
    }

    pub async fn append_context_compacted_in_tx(
        &self,
        tx: &mut ClientSession,
        input: ContextCompactedTimelineInput<'_>,
    ) -> Result<(String, bool), SessionsError> {
        let ContextCompactedTimelineInput {
            session_id,
            compact_summary_id,
            source_first_timeline_id: source_first,
            source_last_timeline_id: source_last,
            summary,
            now,
        } = input;
        if let Some(existing) = self
            .pool
            .collection::<Document>("timeline_items")
            .find_one(doc! {
                "session_id": session_id.to_string(),
                "kind": "context_compacted",
                "source_resource_id": compact_summary_id,
            })
            .session(&mut *tx)
            .await?
        {
            let existing = read_str(&existing, "_id")?;
            return Ok((existing, false));
        }

        let timeline_item_id = TimelineItemId::new().to_string();
        let display_order = self.next_timeline_position_in_tx(tx, session_id).await?;
        let item_count = summary.get("item_count").cloned();
        let projection = json!({
            "kind": "context_compacted",
            "title": "Context Compacted",
            "compact_summary_id": compact_summary_id,
            "source_first_timeline_id": source_first,
            "source_last_timeline_id": source_last,
            "item_count": item_count,
            "summary": summary,
        });
        self.pool
            .collection::<Document>("timeline_items")
            .insert_one(doc! {
                "_id": &timeline_item_id,
                "session_id": session_id.to_string(),
                "turn_id": Bson::Null,
                "kind": "context_compacted",
                "source_resource_id": compact_summary_id,
                "display_order": display_order,
                "projection_json": projection.to_string(),
                "status": "active",
                "version": format!("v_{}", TimelineItemId::new()),
                "created_at": now,
                "updated_at": now,
            })
            .session(&mut *tx)
            .await?;
        Ok((timeline_item_id, true))
    }

    pub async fn turn_inputs_after(
        &self,
        turn_id: TurnId,
        after_sequence: i64,
    ) -> Result<Vec<ContextMessage>, SessionsError> {
        let mut rows = self
            .pool
            .collection::<Document>("messages")
            .find(doc! {
                "turn_id": turn_id.to_string(),
                "status": "active",
                "kind": "user",
                "timeline_sequence": {"$gt": after_sequence},
            })
            .sort(doc! {"timeline_sequence": 1, "created_at": 1, "_id": 1})
            .await?;
        let mut out = Vec::new();
        while let Some(document) = rows.try_next().await? {
            let body_json = read_str(&document, "body_json")?;
            out.push(ContextMessage {
                turn_id: opt_str(&document, "turn_id"),
                kind: read_str(&document, "kind")?,
                body: serde_json::from_str(&body_json)?,
                timeline_sequence: read_i64(&document, "timeline_sequence").unwrap_or(0),
            });
        }
        Ok(out)
    }

    pub async fn turn_status_in_tx(
        &self,
        tx: &mut ClientSession,
        turn_id: &str,
    ) -> Result<Option<TurnStatus>, SessionsError> {
        let status = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {"_id": turn_id})
            .session(&mut *tx)
            .await?
            .map(|document| read_str(&document, "status"))
            .transpose()?;
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
        tx: &mut ClientSession,
        input: CreateTurnInput<'_>,
    ) -> Result<CreatedTurnInput, SessionsError> {
        let CreateTurnInput {
            session_id,
            content,
            actor,
            message_kind,
            timeline_kind,
            metadata,
            goal_mode,
            predecessor_turn_id,
            attachment_ids,
            model_snapshot,
            checkpoint_revision,
            now,
        } = input;
        if !matches!(message_kind, "user" | "system") || timeline_kind.trim().is_empty() {
            return Err(SessionsError::Validation(
                "invalid message or timeline kind".into(),
            ));
        }
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
            let attachment = self
                .pool
                .collection::<Document>("attachments")
                .find_one(doc! {
                    "_id": attachment_id.to_string(),
                    "session_id": session_id.to_string(),
                    "lifecycle": {"$in": ["draft", "attached"]},
                })
                .session(&mut *tx)
                .await?;
            let Some(attachment) = attachment else {
                return Err(SessionsError::Validation(
                    "attachment is missing or belongs to another session".into(),
                ));
            };
            let byte_size = u64::try_from(read_i64(&attachment, "byte_size")?)
                .map_err(|error| SessionsError::Internal(error.into()))?;
            message_bytes = message_bytes
                .checked_add(byte_size)
                .ok_or_else(|| SessionsError::Validation("message is too large".into()))?;
            attachments.push(MessageAttachment {
                id: *attachment_id,
                name: read_str(&attachment, "name")?,
                mime: read_str(&attachment, "mime")?,
                byte_size,
            });
        }
        if message_bytes > MAX_MESSAGE_BYTES {
            return Err(SessionsError::Validation(format!(
                "message content and attachments exceed {MAX_MESSAGE_BYTES} bytes"
            )));
        }
        let next_sequence: i64 = {
            let max_sequence = self
                .pool
                .collection::<Document>("turns")
                .find_one(doc! {"session_id": session_id.to_string()})
                .sort(doc! {"sequence": -1})
                .session(&mut *tx)
                .await?
                .map(|document| read_i64(&document, "sequence"))
                .transpose()?
                .unwrap_or(0);
            max_sequence + 1
        };
        let display_order = self.next_timeline_position_in_tx(tx, session_id).await?;
        let turn_id = TurnId::new();
        let message_id = MessageId::new();
        let timeline_item_id = TimelineItemId::new();
        let model_snapshot_json = serde_json::to_string(&model_snapshot)?;
        let predecessor_turn_id = predecessor_turn_id
            .map(|id| Bson::String(id.to_owned()))
            .unwrap_or(Bson::Null);
        self.pool
            .collection::<Document>("turns")
            .insert_one(doc! {
                "_id": turn_id.to_string(),
                "session_id": session_id.to_string(),
                "sequence": next_sequence,
                "status": "queued",
                "input_message_id": message_id.to_string(),
                "model_snapshot_json": &model_snapshot_json,
                "goal_mode": i64::from(goal_mode),
                "predecessor_turn_id": predecessor_turn_id,
                "completion_summary_json": Bson::Null,
                "completion_reason": Bson::Null,
                "cancellation_reason": Bson::Null,
                "input_tokens": 0i64,
                "output_tokens": 0i64,
                "version": format!("v_{}", TurnId::new()),
                "created_at": now,
                "updated_at": now,
            })
            .session(&mut *tx)
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
        if let Some(metadata) = metadata.and_then(Value::as_object)
            && let Some(body_object) = body.as_object_mut()
        {
            body_object.extend(metadata.clone());
        }
        self.pool
            .collection::<Document>("messages")
            .insert_one(doc! {
                "_id": message_id.to_string(),
                "session_id": session_id.to_string(),
                "turn_id": turn_id.to_string(),
                "actor_json": actor.to_string(),
                "kind": message_kind,
                "body_json": body.to_string(),
                "status": "active",
                "timeline_sequence": display_order,
                "version": format!("v_{}", MessageId::new()),
                "created_at": now,
            })
            .session(&mut *tx)
            .await?;

        for (ordinal, attachment) in attachments.iter().enumerate() {
            let ord =
                i64::try_from(ordinal).map_err(|error| SessionsError::Internal(error.into()))?;
            self.pool
                .collection::<Document>("message_attachments")
                .insert_one(doc! {
                    "_id": ObjectId::new(),
                    "message_id": message_id.to_string(),
                    "attachment_id": attachment.id.to_string(),
                    "ord": ord,
                })
                .session(&mut *tx)
                .await?;
            self.pool
                .collection::<Document>("attachments")
                .update_one(
                    doc! {
                        "_id": attachment.id.to_string(),
                        "lifecycle": "draft",
                    },
                    doc! {
                        "$set": {
                            "lifecycle": "attached",
                            "version": format!("v_{}", AttachmentId::new()),
                        }
                    },
                )
                .session(&mut *tx)
                .await?;
        }

        let mut projection = json!({
            "kind": timeline_kind,
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
        if let Some(metadata) = metadata.and_then(Value::as_object)
            && let Some(projection_object) = projection.as_object_mut()
        {
            projection_object.extend(metadata.clone());
        }
        self.pool
            .collection::<Document>("timeline_items")
            .insert_one(doc! {
                "_id": timeline_item_id.to_string(),
                "session_id": session_id.to_string(),
                "turn_id": turn_id.to_string(),
                "kind": timeline_kind,
                "source_resource_id": message_id.to_string(),
                "display_order": display_order,
                "projection_json": projection.to_string(),
                "status": "active",
                "version": format!("v_{}", TimelineItemId::new()),
                "created_at": now,
                "updated_at": now,
            })
            .session(&mut *tx)
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
        tx: &mut ClientSession,
        input: AppendSteerInput<'_>,
    ) -> Result<(crate::types::SteerResult, String), SessionsError> {
        let AppendSteerInput {
            session_id,
            expected_turn_id,
            content,
            expected_version,
            actor,
            now,
        } = input;
        let session = self
            .pool
            .collection::<Document>("sessions")
            .find_one(doc! {"_id": session_id.to_string()})
            .session(&mut *tx)
            .await?
            .ok_or(SessionsError::NotFound)?;
        if read_str(&session, "state")? == "deleting" {
            return Err(SessionsError::SessionDeleting);
        }
        let current_version = read_str(&session, "version")?;
        if current_version != expected_version {
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.to_owned(),
                current: current_version,
            });
        }
        let active_turn_id = opt_str(&session, "active_turn_id")
            .ok_or(SessionsError::TurnNotInteractive)?
            .parse::<TurnId>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
        if expected_turn_id.is_some_and(|expected| expected != active_turn_id) {
            return Err(SessionsError::TurnNotInteractive);
        }
        let turn = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {"_id": active_turn_id.to_string()})
            .session(&mut *tx)
            .await?;
        let status = turn
            .map(|document| read_str(&document, "status"))
            .transpose()?
            .ok_or(SessionsError::TurnNotInteractive)?
            .parse::<TurnStatus>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
        if !status.is_interactive() {
            return Err(SessionsError::TurnNotInteractive);
        }

        let display_order = self.next_timeline_position_in_tx(tx, session_id).await?;
        let message_id = MessageId::new();
        let timeline_item_id = TimelineItemId::new();
        let body = json!({"parts": [{"type": "text", "text": content}], "steer": true});
        self.pool
            .collection::<Document>("messages")
            .insert_one(doc! {
                "_id": message_id.to_string(),
                "session_id": session_id.to_string(),
                "turn_id": active_turn_id.to_string(),
                "actor_json": actor.to_string(),
                "kind": "user",
                "body_json": body.to_string(),
                "status": "active",
                "timeline_sequence": display_order,
                "version": format!("v_{}", MessageId::new()),
                "created_at": now,
            })
            .session(&mut *tx)
            .await?;

        let projection = json!({
            "kind": "steer",
            "message_id": message_id.to_string(),
            "turn_id": active_turn_id.to_string(),
            "text": content,
        });
        self.pool
            .collection::<Document>("timeline_items")
            .insert_one(doc! {
                "_id": timeline_item_id.to_string(),
                "session_id": session_id.to_string(),
                "turn_id": active_turn_id.to_string(),
                "kind": "steer",
                "source_resource_id": message_id.to_string(),
                "display_order": display_order,
                "projection_json": projection.to_string(),
                "status": "active",
                "version": format!("v_{}", TimelineItemId::new()),
                "created_at": now,
                "updated_at": now,
            })
            .session(&mut *tx)
            .await?;

        let session_version = format!("v_{}", SessionId::new());
        let changed = self
            .pool
            .collection::<Document>("sessions")
            .update_one(
                doc! {
                    "_id": session_id.to_string(),
                    "version": expected_version,
                    "active_turn_id": active_turn_id.to_string(),
                },
                doc! {
                    "$set": {
                        "version": &session_version,
                        "updated_at": now,
                    }
                },
            )
            .session(&mut *tx)
            .await?;
        if changed.matched_count != 1 {
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
        tx: &mut ClientSession,
        input: AppendAssistantMessage<'_>,
    ) -> Result<(String, Option<String>, i64), SessionsError> {
        let AppendAssistantMessage {
            session_id,
            turn_id,
            round_id,
            text,
            reasoning,
            reasoning_content,
            duration_ms,
            tool_calls,
            actor,
            now,
        } = input;
        let display_order = self.next_timeline_position_in_tx(tx, session_id).await?;
        let message_id = MessageId::new();
        let mut body = json!({
            "parts": [{"type": "text", "text": text}],
            "reasoning": reasoning,
            "tool_calls": tool_calls,
        });
        if let Some(raw) = reasoning_content {
            body["reasoning_content"] = json!(raw);
        }
        self.pool
            .collection::<Document>("messages")
            .insert_one(doc! {
                "_id": message_id.to_string(),
                "session_id": session_id.to_string(),
                "turn_id": turn_id.to_string(),
                "actor_json": actor.to_string(),
                "kind": "assistant",
                "body_json": body.to_string(),
                "status": "active",
                "timeline_sequence": display_order,
                "version": format!("v_{}", MessageId::new()),
                "created_at": now,
            })
            .session(&mut *tx)
            .await?;
        if text.is_empty() && reasoning.is_empty() {
            return Ok((message_id.to_string(), None, display_order));
        }
        let timeline_item_id = TimelineItemId::new();
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
        self.pool
            .collection::<Document>("timeline_items")
            .insert_one(doc! {
                "_id": timeline_item_id.to_string(),
                "session_id": session_id.to_string(),
                "turn_id": turn_id.to_string(),
                "kind": "assistant_message",
                "source_resource_id": message_id.to_string(),
                "display_order": display_order,
                "projection_json": proj.to_string(),
                "status": "active",
                "version": format!("v_{}", TimelineItemId::new()),
                "created_at": now,
                "updated_at": now,
            })
            .session(&mut *tx)
            .await?;
        Ok((
            message_id.to_string(),
            Some(timeline_item_id.to_string()),
            display_order,
        ))
    }

    pub async fn append_tool_result_in_tx(
        &self,
        tx: &mut ClientSession,
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
        self.pool
            .collection::<Document>("timeline_items")
            .insert_one(doc! {
                "_id": timeline_item_id.to_string(),
                "session_id": session_id.to_string(),
                "turn_id": turn_id.to_string(),
                "kind": "tool_call",
                "source_resource_id": tool_call_id,
                "display_order": display_order,
                "projection_json": json!({
                    "kind": "tool_call",
                    "tool_call_id": tool_call_id,
                    "tool_name": tool_name,
                    "status": status,
                    "summary": summary,
                })
                .to_string(),
                "status": "active",
                "version": format!("v_{}", TimelineItemId::new()),
                "created_at": now,
                "updated_at": now,
            })
            .session(&mut *tx)
            .await?;

        let message_id = MessageId::new();
        self.pool
            .collection::<Document>("messages")
            .insert_one(doc! {
                "_id": message_id.to_string(),
                "session_id": session_id.to_string(),
                "turn_id": turn_id.to_string(),
                "actor_json": actor.to_string(),
                "kind": "tool_result_ref",
                "body_json": json!({
                    "parts": model_parts,
                    "tool_call_id": provider_call_id,
                    "resource_tool_call_id": tool_call_id,
                })
                .to_string(),
                "status": "active",
                "timeline_sequence": display_order,
                "version": format!("v_{}", MessageId::new()),
                "created_at": now,
            })
            .session(&mut *tx)
            .await?;
        Ok((
            message_id.to_string(),
            timeline_item_id.to_string(),
            display_order,
        ))
    }

    pub async fn replace_tool_result_in_tx(
        &self,
        tx: &mut ClientSession,
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
        let timeline_item = self
            .pool
            .collection::<Document>("timeline_items")
            .find_one_and_update(
                doc! {
                    "session_id": session_id.to_string(),
                    "turn_id": source_turn_id.to_string(),
                    "kind": "tool_call",
                    "source_resource_id": tool_call_id,
                    "status": "active",
                },
                doc! {
                    "$set": {
                        "projection_json": projection.to_string(),
                        "version": format!("v_{}", TimelineItemId::new()),
                        "updated_at": now,
                    }
                },
            )
            .return_document(ReturnDocument::After)
            .session(&mut *tx)
            .await?;
        let timeline_item_id = timeline_item
            .map(|document| read_str(&document, "_id"))
            .transpose()?
            .ok_or_else(|| {
                SessionsError::Internal(anyhow::anyhow!("Tool Call timeline projection is missing"))
            })?;

        let message = json!({
            "parts": model_parts,
            "tool_call_id": provider_call_id,
            "resource_tool_call_id": tool_call_id,
        });
        let mut candidates = self
            .pool
            .collection::<Document>("messages")
            .find(doc! {
                "session_id": session_id.to_string(),
                "turn_id": source_turn_id.to_string(),
                "kind": "tool_result_ref",
                "status": "active",
            })
            .session(&mut *tx)
            .await?;
        let mut matched_ids = Vec::new();
        while let Some(document) = candidates.next(&mut *tx).await.transpose()? {
            let body_json = read_str(&document, "body_json")?;
            let resource_tool_call_id = serde_json::from_str::<Value>(&body_json)
                .ok()
                .and_then(|value| value.get("resource_tool_call_id").and_then(Value::as_str))
                .map(str::to_owned);
            if resource_tool_call_id.as_deref() == Some(tool_call_id) {
                matched_ids.push(read_str(&document, "_id")?);
            }
        }
        if matched_ids.len() != 1 {
            return Err(SessionsError::Internal(anyhow::anyhow!(
                "Tool Call protocol result is missing or duplicated"
            )));
        }
        self.pool
            .collection::<Document>("messages")
            .update_one(
                doc! {"_id": &matched_ids[0]},
                doc! {
                    "$set": {
                        "body_json": message.to_string(),
                        "version": format!("v_{}", MessageId::new()),
                    }
                },
            )
            .session(&mut *tx)
            .await?;
        Ok(timeline_item_id)
    }

    pub async fn insert_checkpoint_for_turn_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
        turn_id: TurnId,
        workspace_revision: &str,
        now: &str,
    ) -> Result<(), SessionsError> {
        let turn = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {"_id": turn_id.to_string(), "session_id": session_id.to_string()})
            .session(&mut *tx)
            .await?
            .ok_or(SessionsError::NotFound)?;
        let message_id = opt_str(&turn, "input_message_id").ok_or(SessionsError::NotFound)?;
        let timeline_position = self
            .pool
            .collection::<Document>("messages")
            .find_one(doc! {"_id": &message_id})
            .session(&mut *tx)
            .await?
            .map(|document| read_i64(&document, "timeline_sequence"))
            .transpose()?
            .unwrap_or(0);
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
        tx: &mut ClientSession,
        session_id: SessionId,
    ) -> Result<i64, SessionsError> {
        let items_max = self
            .pool
            .collection::<Document>("timeline_items")
            .find_one(doc! {"session_id": session_id.to_string()})
            .sort(doc! {"display_order": -1})
            .session(&mut *tx)
            .await?
            .map(|document| read_i64(&document, "display_order"))
            .transpose()?
            .unwrap_or(0);
        let messages_max = self
            .pool
            .collection::<Document>("messages")
            .find_one(doc! {
                "session_id": session_id.to_string(),
                "timeline_sequence": {"$ne": Bson::Null},
            })
            .sort(doc! {"timeline_sequence": -1})
            .session(&mut *tx)
            .await?
            .map(|document| read_i64(&document, "timeline_sequence"))
            .transpose()?
            .unwrap_or(0);
        Ok(items_max.max(messages_max) + 1)
    }

    async fn insert_checkpoint_in_tx(
        &self,
        tx: &mut ClientSession,
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
        self.pool
            .collection::<Document>("checkpoints")
            .insert_one(doc! {
                "_id": &checkpoint_id,
                "session_id": session_id.to_string(),
                "kind": "pre_turn",
                "timeline_position": timeline_position,
                "workspace_revision_id": workspace_revision,
                "source_message_id": message_id,
                "source_turn_id": turn_id,
                "created_at": now,
            })
            .session(&mut *tx)
            .await?;
        Ok(checkpoint_id)
    }
}
