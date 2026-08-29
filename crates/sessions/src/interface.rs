//! Public Session lifecycle boundary.
//!
//! Owns Session/Turn/Message/timeline/checkpoint projections. Does not execute
//! model rounds (execution) or own workspace bytes (workspace).
//!
//! This crate persists Session-owned state only; the application layer decides
//! when a newly active Turn is scheduled for execution.

use std::collections::HashMap;

use futures_util::TryStreamExt;
use janus_infrastructure::clock::now_utc_str;
use mongodb::{
    ClientSession,
    bson::{Bson, Document, doc},
};
use serde_json::{Value, json};

pub use super::types::{
    ActiveTurnOutcome, AppendAssistantMessage, AppendSteerInput, AppendToolResultInput,
    AttachmentResource, AttachmentView, CancelResult, ContextMessage, CreateTurnInput,
    CreatedTurnInput, ExecutionTurn, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS, MAX_MESSAGE_BYTES,
    MessageRoute, MessageRouteResult, ModelAttemptStatus, QueuedTurnCandidate, QueuedTurnItem,
    ReasoningEffort, RecordedTurnInput, RecoveredTurn, ReplaceToolResultInput, SessionCommandState,
    SessionModelPreference, SessionSummary, SessionsError, SteerResult, TerminalSettlement,
    TimelineItemView, TimelinePage, TimelineTurnStatus, TurnModelAttempt,
    TurnModelCandidateSnapshot, TurnModelSnapshot, TurnStatus, TurnSummary, TurnTokenExchange,
    TurnTransition, UploadAttachmentInput,
};
use janus_infrastructure::{
    events::{EventStore, EventType, NewEvent},
    id::{AttachmentId, CorrelationId, ProjectId, SessionId, TurnId},
    managed_storage::{BlobReference, BlobStore},
    unit_of_work::UnitOfWork,
};

#[derive(Clone)]
pub struct SessionsInterface {
    pub(super) pool: mongodb::Database,
    pub(super) unit_of_work: UnitOfWork,
    pub(super) blobs: BlobStore,
}

#[derive(Debug, Clone)]
pub struct CreatedSessionRecord {
    pub created: bool,
    pub version: String,
}

#[derive(Debug, Clone)]
pub struct DeletingSession {
    pub changed: bool,
    pub project_id: ProjectId,
    pub version: String,
}

#[derive(Debug, Clone)]
pub struct SessionDeletionPlan {
    pub project_id: ProjectId,
    pub version: String,
    pub turn_ids: Vec<TurnId>,
}

#[derive(Debug, Clone, Copy)]
pub struct ContextCompactedTimelineInput<'a> {
    pub session_id: SessionId,
    pub compact_summary_id: &'a str,
    pub source_first_timeline_id: Option<&'a str>,
    pub source_last_timeline_id: &'a str,
    pub summary: &'a Value,
    pub now: &'a str,
}

struct PersistUploadAttachment<'a> {
    owner_id: &'a str,
    session_id: SessionId,
    upload_id: janus_infrastructure::id::UploadId,
    attachment_id: AttachmentId,
    name: &'a str,
    mime: &'a str,
    byte_size: u64,
    blob_sha: &'a str,
}

pub(super) fn opt_str(document: &Document, key: &str) -> Option<String> {
    document.get(key).and_then(Bson::as_str).map(str::to_owned)
}

pub(super) fn read_str(document: &Document, key: &str) -> Result<String, SessionsError> {
    document
        .get_str(key)
        .map(str::to_owned)
        .map_err(accessor_error)
}

pub(super) fn read_i64(document: &Document, key: &str) -> Result<i64, SessionsError> {
    document.get_i64(key).map_err(accessor_error)
}

fn accessor_error(error: impl std::error::Error + Send + Sync + 'static) -> SessionsError {
    SessionsError::Internal(anyhow::anyhow!(error))
}

impl SessionsInterface {
    pub fn new(pool: mongodb::Database, events: EventStore, blobs: BlobStore) -> Self {
        let unit_of_work = UnitOfWork::new(pool.clone(), events);
        Self {
            pool,
            unit_of_work,
            blobs,
        }
    }

    pub async fn create_session_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
        project_id: ProjectId,
        title: Option<String>,
    ) -> Result<CreatedSessionRecord, SessionsError> {
        let existing = self
            .pool
            .collection::<Document>("sessions")
            .find_one(doc! {"_id": session_id.to_string()})
            .session(&mut *tx)
            .await?;
        if let Some(document) = existing {
            let stored_project_id = read_str(document, "project_id")?;
            let version = read_str(document, "version")?;
            if stored_project_id != project_id.to_string() {
                return Err(SessionsError::Internal(anyhow::anyhow!(
                    "session id already belongs to another project"
                )));
            }
            return Ok(CreatedSessionRecord {
                created: false,
                version,
            });
        }
        let now = now_utc_str();
        let version = format!("v_{}", SessionId::new());
        let title = title.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());

        self.pool
            .collection::<Document>("sessions")
            .insert_one(doc! {
                "_id": session_id.to_string(),
                "project_id": project_id.to_string(),
                "title": title.map(Bson::String).unwrap_or(Bson::Null),
                "state": "ready",
                "next_model_ref": Bson::Null,
                "active_turn_id": Bson::Null,
                "version": &version,
                "created_at": &now,
                "updated_at": &now,
                "last_activity_at": &now,
            })
            .session(&mut *tx)
            .await?;
        Ok(CreatedSessionRecord {
            created: true,
            version,
        })
    }

    pub async fn list_sessions(
        &self,
        project_id: ProjectId,
        limit: i64,
    ) -> Result<Vec<SessionSummary>, SessionsError> {
        let limit = limit.clamp(1, 100);
        let mut rows = self
            .pool
            .collection::<Document>("sessions")
            .find(doc! {"project_id": project_id.to_string(), "state": {"$ne": "deleting"}})
            .sort(doc! {"last_activity_at": -1})
            .limit(limit)
            .await?;

        let mut out = Vec::with_capacity(limit as usize);
        while let Some(document) = rows.try_next().await? {
            out.push(Self::summary_from_row(&document)?);
        }
        Ok(out)
    }

    pub async fn active_sessions(
        &self,
        project_id: ProjectId,
        limit: i64,
    ) -> Result<Vec<SessionSummary>, SessionsError> {
        let limit = limit.clamp(1, 100);
        let mut rows = self
            .pool
            .collection::<Document>("sessions")
            .find(doc! {
                "state": {"$ne": "deleting"},
                "active_turn_id": {"$ne": Bson::Null},
                "project_id": project_id.to_string(),
            })
            .sort(doc! {"last_activity_at": -1})
            .limit(limit)
            .await?;
        let mut out = Vec::with_capacity(limit as usize);
        while let Some(document) = rows.try_next().await? {
            out.push(Self::summary_from_row(&document)?);
        }
        Ok(out)
    }

    pub async fn project_session_ids(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<SessionId>, SessionsError> {
        let mut rows = self
            .pool
            .collection::<Document>("sessions")
            .find(doc! {"project_id": project_id.to_string()})
            .sort(doc! {"created_at": 1})
            .await?;
        let mut ids = Vec::new();
        while let Some(document) = rows.try_next().await? {
            let id = read_str(document, "_id")?;
            ids.push(id.parse::<SessionId>().map_err(|error| {
                SessionsError::Internal(anyhow::anyhow!(error))
            })?);
        }
        Ok(ids)
    }

    pub async fn ready_session_ids(&self) -> Result<Vec<SessionId>, SessionsError> {
        let mut candidates = Vec::new();
        let mut rows = self
            .pool
            .collection::<Document>("sessions")
            .find(doc! {"state": "ready", "active_turn_id": Bson::Null})
            .sort(doc! {"last_activity_at": 1})
            .await?;
        while let Some(document) = rows.try_next().await? {
            candidates.push(read_str(document, "_id")?);
        }
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let mut queued: std::collections::HashSet<String> = Default::default();
        let mut turns = self
            .pool
            .collection::<Document>("turns")
            .find(doc! {"status": "queued"})
            .await?;
        while let Some(document) = turns.try_next().await? {
            queued.insert(read_str(document, "session_id")?);
        }
        candidates.retain(|id| !queued.contains(id));
        candidates
            .into_iter()
            .map(|id| {
                id.parse::<SessionId>()
                    .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))
            })
            .collect()
    }

    pub async fn get_session(
        &self,
        session_id: SessionId,
    ) -> Result<SessionSummary, SessionsError> {
        let document = self
            .pool
            .collection::<Document>("sessions")
            .find_one(doc! {"_id": session_id.to_string()})
            .await?
            .ok_or(SessionsError::NotFound)?;
        self.row_to_summary(&document).await
    }

    pub async fn create_upload_attachment(
        &self,
        input: UploadAttachmentInput<'_>,
    ) -> Result<AttachmentView, SessionsError> {
        let UploadAttachmentInput {
            owner_id,
            session_id,
            upload_id,
            attachment_id,
            name,
            mime,
            byte_size,
            bytes,
        } = input;
        let reference = BlobReference::new(
            "sessions",
            "attachment",
            &attachment_id.to_string(),
            "content",
        );
        let blob_sha = self
            .blobs
            .write(bytes, reference.clone())
            .await
            .map_err(SessionsError::Internal)?;
        let result = self
            .persist_upload_attachment(PersistUploadAttachment {
                owner_id,
                session_id,
                upload_id,
                attachment_id,
                name,
                mime,
                byte_size,
                blob_sha: blob_sha.as_str(),
            })
            .await;
        if result.is_err()
            && let Err(error) = self.blobs.drop_reference(&reference).await
        {
            tracing::error!(
                %error,
                attachment_id = %attachment_id,
                "blob reference compensation was deferred or failed"
            );
        }
        result
    }

    async fn persist_upload_attachment(
        &self,
        input: PersistUploadAttachment<'_>,
    ) -> Result<AttachmentView, SessionsError> {
        let PersistUploadAttachment {
            owner_id,
            session_id,
            upload_id,
            attachment_id,
            name,
            mime,
            byte_size,
            blob_sha,
        } = input;
        let available = self
            .pool
            .collection::<Document>("sessions")
            .find_one(doc! {
                "_id": session_id.to_string(),
                "state": {"$ne": "deleting"},
            })
            .await?;
        if available.is_none() {
            return Err(SessionsError::NotFound);
        }
        let now = now_utc_str();
        let version = format!("v_{attachment_id}");
        let byte_size = i64::try_from(byte_size).map_err(|error| SessionsError::Internal(error.into()))?;
        let mut work = self.unit_of_work.begin().await?;
        self.pool
            .collection::<Document>("uploads")
            .insert_one(doc! {
                "_id": upload_id.to_string(),
                "owner_id": owner_id,
                "original_name": name,
                "mime": mime,
                "byte_size": byte_size,
                "blob_sha": blob_sha,
                "scan_status": "accepted",
                "created_at": &now,
            })
            .session(work.connection())
            .await?;
        self.pool
            .collection::<Document>("attachments")
            .insert_one(doc! {
                "_id": attachment_id.to_string(),
                "session_id": session_id.to_string(),
                "source_kind": "upload",
                "upload_id": upload_id.to_string(),
                "name": name,
                "mime": mime,
                "byte_size": byte_size,
                "blob_sha": blob_sha,
                "lifecycle": "draft",
                "version": &version,
                "created_at": &now,
            })
            .session(work.connection())
            .await?;
        work.commit().await?;
        Ok(AttachmentView {
            id: attachment_id.to_string(),
            session_id: session_id.to_string(),
            name: name.to_owned(),
            mime: mime.to_owned(),
            byte_size: u64::try_from(byte_size)
                .map_err(|error| SessionsError::Internal(error.into()))?,
            lifecycle: "draft".into(),
            version,
            created_at: now,
        })
    }

    pub async fn delete_draft_attachment(
        &self,
        session_id: SessionId,
        attachment_id: AttachmentId,
    ) -> Result<(), SessionsError> {
        let mut work = self.unit_of_work.begin().await?;
        let document = self
            .pool
            .collection::<Document>("attachments")
            .find_one(doc! {
                "_id": attachment_id.to_string(),
                "session_id": session_id.to_string(),
                "lifecycle": "draft",
            })
            .session(&mut *work.connection())
            .await?;
        let Some(document) = document else {
            work.rollback().await?;
            return Err(SessionsError::Validation(
                "attachment is missing or is already referenced by a message".into(),
            ));
        };
        let referenced = self
            .pool
            .collection::<Document>("message_attachments")
            .find_one(doc! {"attachment_id": attachment_id.to_string()})
            .session(&mut *work.connection())
            .await?;
        if referenced.is_some() {
            work.rollback().await?;
            return Err(SessionsError::Validation(
                "attachment is missing or is already referenced by a message".into(),
            ));
        }
        let upload_id = opt_str(&document, "upload_id");
        self.pool
            .collection::<Document>("attachments")
            .delete_one(doc! {"_id": attachment_id.to_string()})
            .session(&mut *work.connection())
            .await?;
        if let Some(upload_id) = upload_id {
            self.pool
                .collection::<Document>("uploads")
                .delete_one(doc! {"_id": upload_id})
                .session(&mut *work.connection())
                .await?;
        }
        work.commit().await?;
        let reference = BlobReference::new(
            "sessions",
            "attachment",
            &attachment_id.to_string(),
            "content",
        );
        // Database ownership is authoritative. If reference cleanup is
        // interrupted, BlobStore persists a retryable cleanup intent.
        if let Err(error) = self.blobs.drop_reference(&reference).await {
            tracing::error!(
                %error,
                attachment_id = %attachment_id,
                "blob reference cleanup was deferred or failed"
            );
        }
        Ok(())
    }

    pub async fn list_attachments(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<AttachmentResource>, SessionsError> {
        let mut rows = self
            .pool
            .collection::<Document>("attachments")
            .find(doc! {
                "session_id": session_id.to_string(),
                "lifecycle": "attached",
            })
            .sort(doc! {"created_at": 1, "_id": 1})
            .await?;
        let mut out = Vec::new();
        while let Some(document) = rows.try_next().await? {
            out.push(attachment_resource(&document)?);
        }
        Ok(out)
    }

    pub async fn get_attachment(
        &self,
        session_id: SessionId,
        attachment_id: AttachmentId,
    ) -> Result<AttachmentResource, SessionsError> {
        let document = self
            .pool
            .collection::<Document>("attachments")
            .find_one(doc! {
                "_id": attachment_id.to_string(),
                "session_id": session_id.to_string(),
                "lifecycle": "attached",
            })
            .await?
            .ok_or(SessionsError::NotFound)?;
        attachment_resource(&document)
    }

    pub async fn patch_session(
        &self,
        session_id: SessionId,
        title: Option<String>,
        expected_version: &str,
        actor: Value,
    ) -> Result<SessionSummary, SessionsError> {
        let current = self.get_session(session_id).await?;
        if current.state == "deleting" {
            return Err(SessionsError::SessionDeleting);
        }
        if current.version != expected_version {
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.into(),
                current: current.version,
            });
        }
        if current.active_turn_id.is_some() {
            // Idle-only rename per API docs.
            return Err(SessionsError::ActiveTurnExists);
        }

        let now = now_utc_str();
        let new_version = format!("v_{}", SessionId::new());
        let title = title.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());

        let mut set = Document::new();
        set.insert("updated_at", &now);
        set.insert("version", &new_version);
        if let Some(title) = &title {
            set.insert("title", title.clone());
        }

        let mut work = self.unit_of_work.begin().await?;
        let result = self
            .pool
            .collection::<Document>("sessions")
            .update_one(
                doc! {"_id": session_id.to_string(), "version": expected_version},
                doc! {"$set": set},
            )
            .session(work.connection())
            .await?;
        if result.matched_count == 0 {
            work.rollback().await?;
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.into(),
                current: current.version,
            });
        }

        work.append_event(NewEvent {
            event_type: EventType::SessionChanged,
            actor,
            resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "session_id": session_id.to_string(),
                "version": new_version,
                "state": current.state,
            }),
        })
        .await?;
        work.commit().await?;

        self.get_session(session_id).await
    }

    pub async fn mark_session_deleting_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
        expected_version: &str,
    ) -> Result<DeletingSession, SessionsError> {
        let document = self
            .pool
            .collection::<Document>("sessions")
            .find_one(doc! {"_id": session_id.to_string()})
            .session(&mut *tx)
            .await?
            .ok_or(SessionsError::NotFound)?;
        let project_id = read_str(document, "project_id")?
            .parse::<ProjectId>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
        let state = read_str(document, "state")?;
        let current_version = read_str(document, "version")?;
        if state == "deleting" {
            return Ok(DeletingSession {
                changed: false,
                project_id,
                version: current_version,
            });
        }
        if current_version != expected_version {
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.into(),
                current: current_version,
            });
        }
        let now = now_utc_str();
        let version = format!("v_{}", SessionId::new());
        self.pool
            .collection::<Document>("sessions")
            .update_one(
                doc! {"_id": session_id.to_string()},
                doc! {
                    "$set": {
                        "state": "deleting",
                        "version": &version,
                        "updated_at": &now,
                    }
                },
            )
            .session(&mut *tx)
            .await?;
        Ok(DeletingSession {
            changed: true,
            project_id,
            version,
        })
    }

    pub async fn session_deletion_plan_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
    ) -> Result<Option<SessionDeletionPlan>, SessionsError> {
        let document = self
            .pool
            .collection::<Document>("sessions")
            .find_one(doc! {"_id": session_id.to_string()})
            .session(&mut *tx)
            .await?;
        let Some(document) = document else {
            return Ok(None);
        };
        let project_id = read_str(document, "project_id")?
            .parse::<ProjectId>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
        let version = read_str(document, "version")?;
        let mut turns = self
            .pool
            .collection::<Document>("turns")
            .find(doc! {"session_id": session_id.to_string()})
            .sort(doc! {"sequence": 1})
            .session(&mut *tx)
            .await?;
        let mut turn_ids = Vec::new();
        while let Some(document) = turns.try_next().await? {
            let id = read_str(document, "_id")?;
            turn_ids.push(id.parse::<TurnId>().map_err(|error| {
                SessionsError::Internal(anyhow::anyhow!(error))
            })?);
        }
        Ok(Some(SessionDeletionPlan {
            project_id,
            version,
            turn_ids,
        }))
    }

    pub async fn delete_session_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
    ) -> Result<bool, SessionsError> {
        let deleted = self
            .pool
            .collection::<Document>("sessions")
            .delete_one(doc! {"_id": session_id.to_string(), "state": "deleting"})
            .session(&mut *tx)
            .await?;
        Ok(deleted.deleted_count > 0)
    }

    // ----------------------------------------------------------------------
    // Session Control State Machine primitives
    // ----------------------------------------------------------------------

    /// Steer: bind a user message to the active Turn so it becomes visible at
    /// the next safe Round boundary.
    pub async fn steer(
        &self,
        session_id: SessionId,
        content: &str,
        expected_version: &str,
        actor: Value,
    ) -> Result<SteerResult, SessionsError> {
        let now = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        let (result, timeline_item_id) = self
            .append_steer_in_tx(
                work.connection(),
                AppendSteerInput {
                    session_id,
                    expected_turn_id: None,
                    content,
                    expected_version,
                    actor: &actor,
                    now: &now,
                },
            )
            .await?;
        let correlation_id = CorrelationId::new().to_string();
        work.append_event(NewEvent {
            event_type: EventType::TimelineItemCreated,
            actor: actor.clone(),
            resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
            correlation_id: correlation_id.clone(),
            causation_id: None,
            payload: json!({
                "timeline_item_id": timeline_item_id,
                "kind": "steer",
                "turn_id": result.turn_id,
            }),
        })
        .await?;
        work.append_event(NewEvent {
            event_type: EventType::SessionChanged,
            actor,
            resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
            correlation_id,
            causation_id: None,
            payload: json!({
                "session_id": session_id.to_string(),
                "version": result.session_version,
                "steer": { "turn_id": result.turn_id },
            }),
        })
        .await?;
        work.commit().await?;

        Ok(result)
    }

    /// Cancel an active or queued Turn. Active Turns first enter `canceling`
    /// and are settled by execution/runtime; queued Turns have no owned
    /// resources and move directly to terminal `canceled`.
    pub async fn cancel_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        reason: &str,
        expected_version: &str,
        actor: Value,
    ) -> Result<CancelResult, SessionsError> {
        let now = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        let transition = self
            .accept_cancel_in_tx(
                work.connection(),
                session_id,
                turn_id,
                reason,
                expected_version,
                &now,
            )
            .await?;
        let Some(transition) = transition else {
            work.rollback().await?;
            return Err(SessionsError::TurnTerminal);
        };
        work.append_event(NewEvent {
            event_type: EventType::TurnStatusChanged,
            actor,
            resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "session_id": session_id.to_string(),
                "turn_id": turn_id.to_string(),
                "from": transition.from_status.as_str(),
                "to": transition.to_status.as_str(),
                "reason": reason,
                "session_version": transition.session_version,
            }),
        })
        .await?;
        work.commit().await?;
        Ok(CancelResult {
            turn_id: turn_id.to_string(),
            from_status: transition.from_status.as_str().to_owned(),
            to_status: transition.to_status.as_str().to_owned(),
            session_version: transition.session_version,
        })
    }

    pub async fn timeline(
        &self,
        session_id: SessionId,
        before: Option<&str>,
        after: Option<&str>,
        limit: i64,
    ) -> Result<TimelinePage, SessionsError> {
        let exists = self
            .pool
            .collection::<Document>("sessions")
            .find_one(doc! {"_id": session_id.to_string()})
            .await?;
        if exists.is_none() {
            return Err(SessionsError::NotFound);
        }
        if before.is_some() && after.is_some() {
            return Err(SessionsError::TimelineCursorInvalid);
        }
        let limit = limit.clamp(1, 100);

        let parse_cursor = |c: &str| -> Result<i64, SessionsError> {
            c.parse::<i64>()
                .map_err(|_| SessionsError::TimelineCursorInvalid)
        };

        let mut rows = if let Some(before) = before {
            let order = parse_cursor(before)?;
            self.pool
                .collection::<Document>("timeline_items")
                .find(doc! {
                    "session_id": session_id.to_string(),
                    "display_order": {"$lt": order},
                })
                .sort(doc! {"display_order": -1})
                .limit(limit)
                .await?
        } else if let Some(after) = after {
            let order = parse_cursor(after)?;
            self.pool
                .collection::<Document>("timeline_items")
                .find(doc! {
                    "session_id": session_id.to_string(),
                    "display_order": {"$gt": order},
                })
                .sort(doc! {"display_order": 1})
                .limit(limit)
                .await?
        } else {
            self.pool
                .collection::<Document>("timeline_items")
                .find(doc! {"session_id": session_id.to_string()})
                .sort(doc! {"display_order": -1})
                .limit(limit)
                .await?
        };

        let mut documents = Vec::with_capacity(limit as usize);
        let mut turn_ids = Vec::new();
        while let Some(document) = rows.try_next().await? {
            if let Some(turn_id) = opt_str(&document, "turn_id") {
                turn_ids.push(turn_id);
            }
            documents.push(document);
        }
        let mut turns: HashMap<String, Document> = HashMap::new();
        if !turn_ids.is_empty() {
            let mut cursor = self
                .pool
                .collection::<Document>("turns")
                .find(doc! {
                    "_id": {"$in": &turn_ids},
                    "session_id": session_id.to_string(),
                })
                .await?;
            while let Some(turn) = cursor.try_next().await? {
                if let Ok(id) = turn.get_str("_id") {
                    turns.insert(id.to_owned(), turn);
                }
            }
        }
        let mut items = Vec::with_capacity(documents.len());
        for document in documents {
            items.push(timeline_row(&document, &turns)?);
        }
        items.sort_by_key(|i| i.display_order);

        let oldest = items.first().map(|i| i.display_order.to_string());
        let newest = items.last().map(|i| i.display_order.to_string());

        let has_older = if let Some(o) = oldest.as_ref().and_then(|s| s.parse::<i64>().ok()) {
            let count: u64 = self
                .pool
                .collection::<Document>("timeline_items")
                .count_documents(doc! {
                    "session_id": session_id.to_string(),
                    "display_order": {"$lt": o},
                })
                .await?;
            count > 0
        } else {
            false
        };
        let has_newer = if let Some(n) = newest.as_ref().and_then(|s| s.parse::<i64>().ok()) {
            let count: u64 = self
                .pool
                .collection::<Document>("timeline_items")
                .count_documents(doc! {
                    "session_id": session_id.to_string(),
                    "display_order": {"$gt": n},
                })
                .await?;
            count > 0
        } else {
            false
        };

        Ok(TimelinePage {
            items,
            oldest_cursor: oldest,
            newest_cursor: newest,
            has_older,
            has_newer,
        })
    }

    pub async fn timeline_bounds(
        &self,
        session_id: SessionId,
    ) -> Result<(Option<String>, Option<String>, i64), SessionsError> {
        let exists = self
            .pool
            .collection::<Document>("sessions")
            .find_one(doc! {"_id": session_id.to_string()})
            .await?;
        if exists.is_none() {
            return Err(SessionsError::NotFound);
        }
        let oldest = self
            .pool
            .collection::<Document>("timeline_items")
            .find_one(doc! {"session_id": session_id.to_string()})
            .sort(doc! {"display_order": 1})
            .await?
            .map(|document| read_str(&document, "_id"))
            .transpose()?;
        let newest = self
            .pool
            .collection::<Document>("timeline_items")
            .find_one(doc! {"session_id": session_id.to_string()})
            .sort(doc! {"display_order": -1})
            .await?
            .map(|document| read_str(&document, "_id"))
            .transpose()?;
        let count = i64::try_from(
            self.pool
                .collection::<Document>("timeline_items")
                .count_documents(doc! {"session_id": session_id.to_string()})
                .await?,
        )
        .map_err(|error| SessionsError::Internal(error.into()))?;
        Ok((oldest, newest, count))
    }

    pub async fn get_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<TurnSummary, SessionsError> {
        let document = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {"_id": turn_id.to_string(), "session_id": session_id.to_string()})
            .await?
            .ok_or(SessionsError::NotFound)?;
        let model_snapshot_json = read_str(document, "model_snapshot_json")?;
        Ok(TurnSummary {
            id: read_str(document, "_id")?,
            session_id: read_str(document, "session_id")?,
            sequence: read_i64(document, "sequence")?,
            status: read_str(document, "status")?,
            input_message_id: opt_str(&document, "input_message_id"),
            goal_mode: read_i64(document, "goal_mode")? != 0,
            model_snapshot: TurnModelSnapshot::parse(&model_snapshot_json)?,
            predecessor_turn_id: opt_str(&document, "predecessor_turn_id"),
            cancellation_reason: opt_str(&document, "cancellation_reason"),
            completion_reason: opt_str(&document, "completion_reason"),
            model_attempt: None,
            token_exchange: None,
            version: read_str(document, "version")?,
            created_at: read_str(document, "created_at")?,
            updated_at: read_str(document, "updated_at")?,
        })
    }

    /// Resolve a Turn id to its owning Session for application-level
    /// coordination. The caller still uses `get_session` to cross the
    /// Session-to-Project ownership boundary.
    pub async fn session_id_for_turn(&self, turn_id: TurnId) -> Result<SessionId, SessionsError> {
        let id = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {"_id": turn_id.to_string()})
            .await?
            .map(|document| read_str(&document, "session_id"))
            .transpose()?
            .ok_or(SessionsError::NotFound)?;
        id.parse::<SessionId>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))
    }

    /// Queued Turns for the conversation's QueuedMessagesBar. Each row pairs a
    /// queued turn with its user message text so the bar can render a
    /// preview and a delete (cancel) affordance. Cheap index seek on
    /// `(session_id, status='queued')`; no projection walk.
    pub async fn queued_turns(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<QueuedTurnItem>, SessionsError> {
        let _ = self.get_session(session_id).await?;
        let mut rows = self
            .pool
            .collection::<Document>("turns")
            .find(doc! {"session_id": session_id.to_string(), "status": "queued"})
            .sort(doc! {"sequence": 1})
            .await?;
        let mut items = Vec::new();
        while let Some(turn) = rows.try_next().await? {
            let turn_id = read_str(&turn, "_id")?;
            let sequence = read_i64(&turn, "sequence")?;
            let version = read_str(&turn, "version")?;
            let body_json = if let Some(message_id) = opt_str(&turn, "input_message_id") {
                self.pool
                    .collection::<Document>("messages")
                    .find_one(doc! {"_id": message_id})
                    .await?
                    .map(|message| read_str(&message, "body_json"))
                    .transpose()?
                    .unwrap_or_default()
            } else {
                String::new()
            };
            items.push(QueuedTurnItem {
                turn_id,
                sequence,
                version,
                message_text: Self::extract_message_text(&body_json),
            });
        }
        Ok(items)
    }

    /// Best-effort extraction of user-facing text from a message body_json.
    /// Handles both the flat `{"text": "..."}` form and the parts-based
    /// `{"parts": [{"type": "text", "text": "..."}]}` form used by the
    /// answer/steer code paths.
    fn extract_message_text(body_json: &str) -> String {
        if body_json.is_empty() {
            return String::new();
        }
        let Ok(val) = serde_json::from_str::<Value>(body_json) else {
            return String::new();
        };
        if let Some(t) = val.get("text").and_then(Value::as_str) {
            return t.to_owned();
        }
        if let Some(parts) = val.get("parts").and_then(Value::as_array) {
            let text: String = parts
                .iter()
                .filter_map(|p| {
                    if p.get("type").and_then(Value::as_str) == Some("text") {
                        p.get("text").and_then(Value::as_str).map(str::to_owned)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                return text;
            }
        }
        String::new()
    }

    async fn row_to_summary(
        &self,
        document: &Document,
    ) -> Result<SessionSummary, SessionsError> {
        Self::summary_from_row(document)
    }

    fn summary_from_row(document: &Document) -> Result<SessionSummary, SessionsError> {
        Ok(SessionSummary {
            id: read_str(document, "_id")?,
            project_id: read_str(document, "project_id")?,
            title: opt_str(document, "title"),
            state: read_str(document, "state")?,
            active_turn_id: opt_str(document, "active_turn_id"),
            model_preference: opt_str(document, "next_model_ref")
                .map(|raw| serde_json::from_str(&raw))
                .transpose()?,
            version: read_str(document, "version")?,
            created_at: read_str(document, "created_at")?,
            updated_at: read_str(document, "updated_at")?,
            last_activity_at: read_str(document, "last_activity_at")?,
        })
    }
}

fn timeline_row(
    document: &Document,
    turns: &HashMap<String, Document>,
) -> Result<TimelineItemView, SessionsError> {
    let projection_json = read_str(document, "projection_json")?;
    let projection: Value = serde_json::from_str(&projection_json)?;
    let turn_status = opt_str(document, "turn_id")
        .and_then(|id| turns.get(&id))
        .map(|turn| {
            Ok::<TimelineTurnStatus, SessionsError>(TimelineTurnStatus {
                id: read_str(&turn, "_id")?,
                status: read_str(&turn, "status")?,
                cancellation_reason: opt_str(turn, "cancellation_reason"),
                completion_reason: opt_str(turn, "completion_reason"),
                created_at: read_str(&turn, "created_at")?,
                updated_at: read_str(&turn, "updated_at")?,
            })
        })
        .transpose()?;
    Ok(TimelineItemView {
        id: read_str(document, "_id")?,
        session_id: read_str(document, "session_id")?,
        turn_id: opt_str(document, "turn_id"),
        kind: read_str(document, "kind")?,
        source_resource_id: opt_str(document, "source_resource_id"),
        display_order: read_i64(document, "display_order")?,
        projection,
        status: read_str(document, "status")?,
        version: read_str(document, "version")?,
        created_at: read_str(document, "created_at")?,
        turn_status,
    })
}

fn attachment_resource(
    document: &Document,
) -> Result<AttachmentResource, SessionsError> {
    let id = read_str(document, "_id")?
        .parse::<AttachmentId>()
        .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
    let byte_size = u64::try_from(read_i64(document, "byte_size")?)
        .map_err(|error| SessionsError::Internal(error.into()))?;
    Ok(AttachmentResource {
        id,
        name: read_str(document, "name")?,
        mime: read_str(document, "mime")?,
        byte_size,
        blob_sha: opt_str(document, "blob_sha"),
    })
}
