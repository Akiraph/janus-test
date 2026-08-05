//! Public Session lifecycle boundary.
//!
//! Owns Session/Turn/Message/timeline/checkpoint projections. Does not execute
//! model rounds (execution) or own workspace bytes (workspace).
//!
//! This crate persists Session-owned state only; the application layer decides
//! when a newly active Turn is scheduled for execution.

use janus_infrastructure::clock::now_utc_str;
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection, SqlitePool};

use janus_infrastructure::{
    events::{EventStore, NewEvent},
    id::{AttachmentId, CorrelationId, ProjectId, SessionId, TurnId},
    managed_storage::{BlobReference, BlobStore},
    unit_of_work::UnitOfWork,
};
use janus_workspace::interface::{WorkspaceHandle, WorkspaceInterface};

pub use super::types::{
    ActiveTurnOutcome, AppendAssistantMessage, AppendSteerInput, AppendToolResultInput,
    AskAnswerResult, AskSummary, AttachmentResource, AttachmentView, CancelResult, ContextMessage,
    CreateTurnInput, CreatedTurnInput, ExecutionTurn, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS,
    MAX_MESSAGE_BYTES, MessageRoute, MessageRouteResult, ModelAttemptStatus, QueuedTurnCandidate,
    QueuedTurnItem, ReasoningEffort, RecordedTurnInput, RecoveredTurn, ReplaceToolResultInput,
    SessionCommandState, SessionModelPreference, SessionSummary, SessionsError, SteerResult,
    TerminalSettlement, TimelineItemView, TimelinePage, TimelineTurnStatus, TurnBlockerOutcome,
    TurnBlockers, TurnModelAttempt, TurnModelCandidateSnapshot, TurnModelSnapshot, TurnStatus,
    TurnSummary, TurnTransition, UploadAttachmentInput,
};

#[derive(Clone)]
pub struct SessionsInterface {
    pub(super) pool: SqlitePool,
    pub(super) unit_of_work: UnitOfWork,
    pub(super) workspace: WorkspaceInterface,
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

impl SessionsInterface {
    pub fn new(
        pool: SqlitePool,
        events: EventStore,
        workspace: WorkspaceInterface,
        blobs: BlobStore,
    ) -> Self {
        let unit_of_work = UnitOfWork::new(pool.clone(), events);
        Self {
            pool,
            unit_of_work,
            workspace,
            blobs,
        }
    }

    pub async fn create_session_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        project_id: ProjectId,
        title: Option<String>,
        workspace_handle: &WorkspaceHandle,
        source_main_revision: &str,
    ) -> Result<CreatedSessionRecord, SessionsError> {
        let existing: Option<(String, String)> =
            sqlx::query_as("SELECT project_id, version FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_optional(&mut *tx)
                .await?;
        if let Some((stored_project_id, version)) = existing {
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

        sqlx::query(
            "INSERT INTO sessions \
             (id, project_id, kind, parent_session_id, forked_from_checkpoint_id, \
              resolver_conflict_id, title, state, workspace_handle, next_model_ref, \
              active_turn_id, source_main_revision_id, version, created_at, updated_at, \
              last_activity_at) \
             VALUES (?, ?, 'regular', NULL, NULL, NULL, ?, 'ready', ?, NULL, NULL, ?, ?, ?, ?, ?)",
        )
        .bind(session_id.to_string())
        .bind(project_id.to_string())
        .bind(&title)
        .bind(workspace_handle.as_str())
        .bind(source_main_revision)
        .bind(&version)
        .bind(&now)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
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
        let rows = sqlx::query(
            "SELECT id, project_id, kind, title, state, workspace_handle, \
                    active_turn_id, next_model_ref, source_main_revision_id, version, \
                    created_at, updated_at, last_activity_at \
             FROM sessions \
             WHERE project_id = ? AND state != 'deleting' \
             ORDER BY last_activity_at DESC LIMIT ?",
        )
        .bind(project_id.to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let handles = rows
            .iter()
            .map(|row| {
                row.try_get::<String, _>("workspace_handle")
                    .map(WorkspaceHandle)
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        let workspace_revisions = self.workspace.current_revisions(&handles).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let handle: String = row.try_get("workspace_handle")?;
            let revision = workspace_revisions
                .get(&handle)
                .map(|revision| revision.0.clone());
            out.push(Self::summary_from_row(row, revision)?);
        }
        Ok(out)
    }

    pub async fn project_session_ids(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<SessionId>, SessionsError> {
        sqlx::query_scalar::<_, String>(
            "SELECT id FROM sessions WHERE project_id = ? ORDER BY created_at",
        )
        .bind(project_id.to_string())
        .fetch_all(&self.pool)
        .await?
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
        let row = sqlx::query(
            "SELECT id, project_id, kind, title, state, workspace_handle, \
                    active_turn_id, next_model_ref, source_main_revision_id, version, \
                    created_at, updated_at, last_activity_at \
             FROM sessions WHERE id = ?",
        )
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(SessionsError::NotFound)?;
        self.row_to_summary(row).await
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
        let available: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ? AND state != 'deleting')",
        )
        .bind(session_id.to_string())
        .fetch_one(&self.pool)
        .await?;
        if available != 1 {
            return Err(SessionsError::NotFound);
        }
        let now = now_utc_str();
        let version = format!("v_{attachment_id}");
        let mut work = self.unit_of_work.begin().await?;
        sqlx::query(
            "INSERT INTO uploads \
             (id, owner_id, original_name, mime, byte_size, blob_sha, scan_status, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'accepted', ?)",
        )
        .bind(upload_id.to_string())
        .bind(owner_id)
        .bind(name)
        .bind(mime)
        .bind(i64::try_from(byte_size).map_err(|error| SessionsError::Internal(error.into()))?)
        .bind(blob_sha)
        .bind(&now)
        .execute(work.connection())
        .await?;
        sqlx::query(
            "INSERT INTO attachments \
             (id, session_id, source_kind, upload_id, name, mime, byte_size, blob_sha, \
              lifecycle, version, created_at) \
             VALUES (?, ?, 'upload', ?, ?, ?, ?, ?, 'draft', ?, ?)",
        )
        .bind(attachment_id.to_string())
        .bind(session_id.to_string())
        .bind(upload_id.to_string())
        .bind(name)
        .bind(mime)
        .bind(i64::try_from(byte_size).map_err(|error| SessionsError::Internal(error.into()))?)
        .bind(blob_sha)
        .bind(&version)
        .bind(&now)
        .execute(work.connection())
        .await?;
        work.commit().await?;
        Ok(AttachmentView {
            id: attachment_id.to_string(),
            session_id: session_id.to_string(),
            name: name.to_owned(),
            mime: mime.to_owned(),
            byte_size,
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
        let row: Option<Option<String>> = sqlx::query_scalar(
            "SELECT attachment.upload_id FROM attachments AS attachment \
             WHERE attachment.id = ? AND attachment.session_id = ? \
               AND attachment.lifecycle = 'draft' \
               AND NOT EXISTS (SELECT 1 FROM message_attachments AS message_attachment \
                               WHERE message_attachment.attachment_id = attachment.id)",
        )
        .bind(attachment_id.to_string())
        .bind(session_id.to_string())
        .fetch_optional(work.connection())
        .await?;
        let Some(upload_id) = row else {
            work.rollback().await?;
            return Err(SessionsError::Validation(
                "attachment is missing or is already referenced by a message".into(),
            ));
        };
        sqlx::query("DELETE FROM attachments WHERE id = ?")
            .bind(attachment_id.to_string())
            .execute(work.connection())
            .await?;
        if let Some(upload_id) = upload_id {
            sqlx::query("DELETE FROM uploads WHERE id = ?")
                .bind(upload_id)
                .execute(work.connection())
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
        let rows = sqlx::query(
            "SELECT id, name, mime, byte_size, blob_sha FROM attachments \
             WHERE session_id = ? AND lifecycle = 'attached' \
             ORDER BY created_at, id",
        )
        .bind(session_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(attachment_resource).collect()
    }

    pub async fn get_attachment(
        &self,
        session_id: SessionId,
        attachment_id: AttachmentId,
    ) -> Result<AttachmentResource, SessionsError> {
        let row = sqlx::query(
            "SELECT id, name, mime, byte_size, blob_sha FROM attachments \
             WHERE id = ? AND session_id = ? AND lifecycle = 'attached'",
        )
        .bind(attachment_id.to_string())
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(SessionsError::NotFound)?;
        attachment_resource(row)
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

        let mut work = self.unit_of_work.begin().await?;
        let result = sqlx::query(
            "UPDATE sessions SET title = COALESCE(?, title), version = ?, updated_at = ? \
             WHERE id = ? AND version = ?",
        )
        .bind(&title)
        .bind(&new_version)
        .bind(&now)
        .bind(session_id.to_string())
        .bind(expected_version)
        .execute(work.connection())
        .await?;
        if result.rows_affected() == 0 {
            work.rollback().await?;
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.into(),
                current: current.version,
            });
        }

        work.append_event(NewEvent {
            event_type: "session.changed".into(),
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
        tx: &mut SqliteConnection,
        session_id: SessionId,
        expected_version: &str,
    ) -> Result<DeletingSession, SessionsError> {
        let row: Option<(String, String, String)> =
            sqlx::query_as("SELECT project_id, state, version FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_optional(&mut *tx)
                .await?;
        let (project_id, state, current_version) = row.ok_or(SessionsError::NotFound)?;
        let project_id = project_id
            .parse::<ProjectId>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
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
        sqlx::query(
            "UPDATE sessions SET state = 'deleting', version = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&version)
        .bind(&now)
        .bind(session_id.to_string())
        .execute(&mut *tx)
        .await?;
        Ok(DeletingSession {
            changed: true,
            project_id,
            version,
        })
    }

    pub async fn session_deletion_plan_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
    ) -> Result<Option<SessionDeletionPlan>, SessionsError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT project_id, version FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_optional(&mut *tx)
                .await?;
        let Some((project_id, version)) = row else {
            return Ok(None);
        };
        let project_id = project_id
            .parse::<ProjectId>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
        let turn_ids = sqlx::query_scalar::<_, String>(
            "SELECT id FROM turns WHERE session_id = ? ORDER BY sequence",
        )
        .bind(session_id.to_string())
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|id| {
            id.parse::<TurnId>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))
        })
        .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(SessionDeletionPlan {
            project_id,
            version,
            turn_ids,
        }))
    }

    pub async fn delete_session_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
    ) -> Result<bool, SessionsError> {
        let deleted = sqlx::query("DELETE FROM sessions WHERE id = ? AND state = 'deleting'")
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected();
        Ok(deleted > 0)
    }

    // ----------------------------------------------------------------------
    // Session Control State Machine primitives
    // ----------------------------------------------------------------------

    /// Steer: bind a user message to the active interactive Turn so it becomes
    /// visible at the next safe Round boundary. Accepted while the Turn is
    /// `running`, waiting on Job/Ask, or parked on `waiting_for_model` (durable
    /// input only; no mid-stream provider injection).
    pub async fn steer(
        &self,
        session_id: SessionId,
        content: &str,
        expected_version: &str,
        actor: Value,
    ) -> Result<SteerResult, SessionsError> {
        self.steer_with_source(session_id, content, expected_version, actor, None)
            .await
    }

    /// Steer with optional Ask attribution for late Ask answers that still bind
    /// to an active original Turn.
    pub async fn steer_with_source(
        &self,
        session_id: SessionId,
        content: &str,
        expected_version: &str,
        actor: Value,
        source_ask_id: Option<&str>,
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
                    source_ask_id,
                    now: &now,
                },
            )
            .await?;
        let correlation_id = CorrelationId::new().to_string();
        work.append_event(NewEvent {
            event_type: "timeline.item_created".into(),
            actor: actor.clone(),
            resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
            correlation_id: correlation_id.clone(),
            causation_id: None,
            payload: json!({
                "timeline_item_id": timeline_item_id,
                "kind": "steer",
                "turn_id": result.turn_id,
                "source_ask_id": source_ask_id,
            }),
        })
        .await?;
        work.append_event(NewEvent {
            event_type: "session.changed".into(),
            actor,
            resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
            correlation_id,
            causation_id: None,
            payload: json!({
                "session_id": session_id.to_string(),
                "version": result.session_version,
                "steer": { "turn_id": result.turn_id, "source_ask_id": source_ask_id },
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
            event_type: "turn.status_changed".into(),
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
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM sessions WHERE id = ?")
            .bind(session_id.to_string())
            .fetch_optional(&self.pool)
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

        let rows = if let Some(before) = before {
            let order = parse_cursor(before)?;
            sqlx::query(
                "SELECT timeline_items.id, timeline_items.session_id, timeline_items.turn_id, \
                        timeline_items.kind, timeline_items.source_resource_id, \
                        timeline_items.display_order, timeline_items.projection_json, \
                        timeline_items.status, timeline_items.version, timeline_items.created_at, \
                        turns.id AS turn_status_id, turns.status AS turn_status, \
                        turns.cancellation_reason AS turn_cancellation_reason, \
                        turns.completion_reason AS turn_completion_reason, \
                        turns.created_at AS turn_created_at, turns.updated_at AS turn_updated_at \
                 FROM timeline_items \
                 LEFT JOIN turns ON turns.id = timeline_items.turn_id \
                    AND turns.session_id = timeline_items.session_id \
                 WHERE timeline_items.session_id = ? AND timeline_items.display_order < ? \
                 ORDER BY timeline_items.display_order DESC LIMIT ?",
            )
            .bind(session_id.to_string())
            .bind(order)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else if let Some(after) = after {
            let order = parse_cursor(after)?;
            sqlx::query(
                "SELECT timeline_items.id, timeline_items.session_id, timeline_items.turn_id, \
                        timeline_items.kind, timeline_items.source_resource_id, \
                        timeline_items.display_order, timeline_items.projection_json, \
                        timeline_items.status, timeline_items.version, timeline_items.created_at, \
                        turns.id AS turn_status_id, turns.status AS turn_status, \
                        turns.cancellation_reason AS turn_cancellation_reason, \
                        turns.completion_reason AS turn_completion_reason, \
                        turns.created_at AS turn_created_at, turns.updated_at AS turn_updated_at \
                 FROM timeline_items \
                 LEFT JOIN turns ON turns.id = timeline_items.turn_id \
                    AND turns.session_id = timeline_items.session_id \
                 WHERE timeline_items.session_id = ? AND timeline_items.display_order > ? \
                 ORDER BY timeline_items.display_order ASC LIMIT ?",
            )
            .bind(session_id.to_string())
            .bind(order)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT timeline_items.id, timeline_items.session_id, timeline_items.turn_id, \
                        timeline_items.kind, timeline_items.source_resource_id, \
                        timeline_items.display_order, timeline_items.projection_json, \
                        timeline_items.status, timeline_items.version, timeline_items.created_at, \
                        turns.id AS turn_status_id, turns.status AS turn_status, \
                        turns.cancellation_reason AS turn_cancellation_reason, \
                        turns.completion_reason AS turn_completion_reason, \
                        turns.created_at AS turn_created_at, turns.updated_at AS turn_updated_at \
                 FROM timeline_items \
                 LEFT JOIN turns ON turns.id = timeline_items.turn_id \
                    AND turns.session_id = timeline_items.session_id \
                 WHERE timeline_items.session_id = ? \
                 ORDER BY timeline_items.display_order DESC LIMIT ?",
            )
            .bind(session_id.to_string())
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        };

        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            items.push(timeline_row(row)?);
        }
        items.sort_by_key(|i| i.display_order);

        let oldest = items.first().map(|i| i.display_order.to_string());
        let newest = items.last().map(|i| i.display_order.to_string());

        let has_older = if let Some(o) = oldest.as_ref().and_then(|s| s.parse::<i64>().ok()) {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(1) FROM timeline_items WHERE session_id = ? AND display_order < ?",
            )
            .bind(session_id.to_string())
            .bind(o)
            .fetch_one(&self.pool)
            .await?;
            count > 0
        } else {
            false
        };
        let has_newer = if let Some(n) = newest.as_ref().and_then(|s| s.parse::<i64>().ok()) {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(1) FROM timeline_items WHERE session_id = ? AND display_order > ?",
            )
            .bind(session_id.to_string())
            .bind(n)
            .fetch_one(&self.pool)
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

    pub async fn get_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<TurnSummary, SessionsError> {
        let row = sqlx::query(
            "SELECT id, session_id, sequence, status, input_message_id, model_snapshot_json, \
                    predecessor_turn_id, handoff_from_turn_id, handoff_to_turn_id, \
                    cancellation_reason, completion_reason, version, created_at, updated_at \
             FROM turns WHERE id = ? AND session_id = ?",
        )
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(SessionsError::NotFound)?;
        let model_snapshot_json: String = row.try_get("model_snapshot_json")?;
        Ok(TurnSummary {
            id: row.try_get("id")?,
            session_id: row.try_get("session_id")?,
            sequence: row.try_get("sequence")?,
            status: row.try_get("status")?,
            input_message_id: row.try_get("input_message_id")?,
            model_snapshot: TurnModelSnapshot::parse(&model_snapshot_json)?,
            predecessor_turn_id: row.try_get("predecessor_turn_id")?,
            handoff_from_turn_id: row.try_get("handoff_from_turn_id")?,
            handoff_to_turn_id: row.try_get("handoff_to_turn_id")?,
            cancellation_reason: row.try_get("cancellation_reason")?,
            completion_reason: row.try_get("completion_reason")?,
            model_attempt: None,
            version: row.try_get("version")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
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
        let rows = sqlx::query(
            "SELECT turn.id AS turn_id, turn.sequence, turn.version, \
                    COALESCE(message.body_json, '') AS body_json \
             FROM turns AS turn \
             LEFT JOIN messages AS message ON message.id = turn.input_message_id \
             WHERE turn.session_id = ? AND turn.status = 'queued' \
             ORDER BY turn.sequence ASC",
        )
        .bind(session_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let turn_id: String = row.try_get("turn_id")?;
            let sequence: i64 = row.try_get("sequence")?;
            let version: String = row.try_get("version")?;
            let body_json: String = row.try_get("body_json")?;
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
        row: sqlx::sqlite::SqliteRow,
    ) -> Result<SessionSummary, SessionsError> {
        let workspace_handle: String = row.try_get("workspace_handle")?;
        let workspace_revision = {
            let handle = WorkspaceHandle(workspace_handle.clone());
            self.workspace
                .current_revision(&handle)
                .await
                .ok()
                .map(|r| r.0)
        };
        Self::summary_from_row(row, workspace_revision)
    }

    fn summary_from_row(
        row: sqlx::sqlite::SqliteRow,
        workspace_revision: Option<String>,
    ) -> Result<SessionSummary, SessionsError> {
        let workspace_handle: String = row.try_get("workspace_handle")?;
        Ok(SessionSummary {
            id: row.try_get("id")?,
            project_id: row.try_get("project_id")?,
            kind: row.try_get("kind")?,
            title: row.try_get("title")?,
            state: row.try_get("state")?,
            workspace_handle,
            workspace_revision,
            source_main_revision_id: row.try_get("source_main_revision_id")?,
            active_turn_id: row.try_get("active_turn_id")?,
            model_preference: row
                .try_get::<Option<String>, _>("next_model_ref")?
                .map(|raw| serde_json::from_str(&raw))
                .transpose()?,
            version: row.try_get("version")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            last_activity_at: row.try_get("last_activity_at")?,
        })
    }
}

fn timeline_row(row: sqlx::sqlite::SqliteRow) -> Result<TimelineItemView, SessionsError> {
    let projection_json: String = row.try_get("projection_json")?;
    let projection: Value = serde_json::from_str(&projection_json)?;
    let turn_status = row
        .try_get::<Option<String>, _>("turn_status_id")?
        .map(|id| {
            Ok::<TimelineTurnStatus, SessionsError>(TimelineTurnStatus {
                id,
                status: row.try_get("turn_status")?,
                cancellation_reason: row.try_get("turn_cancellation_reason")?,
                completion_reason: row.try_get("turn_completion_reason")?,
                created_at: row.try_get("turn_created_at")?,
                updated_at: row.try_get("turn_updated_at")?,
            })
        })
        .transpose()?;
    Ok(TimelineItemView {
        id: row.try_get("id")?,
        session_id: row.try_get("session_id")?,
        turn_id: row.try_get("turn_id")?,
        kind: row.try_get("kind")?,
        source_resource_id: row.try_get("source_resource_id")?,
        display_order: row.try_get("display_order")?,
        projection,
        status: row.try_get("status")?,
        version: row.try_get("version")?,
        created_at: row.try_get("created_at")?,
        turn_status,
    })
}

fn attachment_resource(row: sqlx::sqlite::SqliteRow) -> Result<AttachmentResource, SessionsError> {
    let id = row
        .try_get::<String, _>("id")?
        .parse::<AttachmentId>()
        .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
    let byte_size = u64::try_from(row.try_get::<i64, _>("byte_size")?)
        .map_err(|error| SessionsError::Internal(error.into()))?;
    Ok(AttachmentResource {
        id,
        name: row.try_get("name")?,
        mime: row.try_get("mime")?,
        byte_size,
        blob_sha: row.try_get("blob_sha")?,
    })
}
