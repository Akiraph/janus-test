//! Public Session lifecycle boundary (M3 Stage 2).
//!
//! Owns Session/Turn/Message/timeline/checkpoint projections. Does not execute
//! model rounds (supervisor) or own workspace bytes (workspace_sync).
//!
//! Stage 2 creates Turns in `running` but does not call supervisor.execute_turn
//! yet — Stage 4 wires the loop. Tests assert single-active-turn and create/delete.

use serde_json::{Value, json};
use sqlx::{Row, SqlitePool};

use crate::modules::workspace_sync::interface::{WorkspaceHandle, WorkspaceSyncInterface};
use crate::platform::{
    clock::{Clock, SystemClock, format_utc},
    events::{EventStore, NewEvent},
    id::{CheckpointId, CorrelationId, MessageId, ProjectId, SessionId, TimelineItemId, TurnId},
};

use super::types::{
    MessageRouteResult, SessionSummary, SessionsError, TimelineItemView, TimelinePage, TurnSummary,
};

#[derive(Clone)]
pub struct SessionsInterface {
    pool: SqlitePool,
    events: EventStore,
    workspace_sync: WorkspaceSyncInterface,
}

impl SessionsInterface {
    pub fn new(
        pool: SqlitePool,
        events: EventStore,
        workspace_sync: WorkspaceSyncInterface,
    ) -> Self {
        Self {
            pool,
            events,
            workspace_sync,
        }
    }

    /// Create a regular Session from Project Main (synchronous for Stage 2).
    /// Copies managed content via workspace_sync and records the sessions row.
    pub async fn create_session(
        &self,
        project_id: ProjectId,
        title: Option<String>,
        actor: Value,
    ) -> Result<SessionSummary, SessionsError> {
        let project = self.load_project(project_id).await?;
        if project.state != "ready" {
            return Err(SessionsError::ProjectNotReady);
        }

        let session_id = SessionId::new();
        let main_revision = self
            .workspace_sync
            .current_revision(&WorkspaceHandle::main(project_id))
            .await?;
        let copy = self
            .workspace_sync
            .ensure_session_copy(project_id, session_id, Some(&main_revision), actor.clone())
            .await?;

        let now = format_utc(SystemClock.now());
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
        .bind(copy.handle.as_str())
        .bind(&main_revision.0)
        .bind(&version)
        .bind(&now)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        let _ = self
            .events
            .append(NewEvent {
                event_type: "session.changed".into(),
                actor,
                resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "session_id": session_id.to_string(),
                    "project_id": project_id.to_string(),
                    "state": "ready",
                    "version": version,
                    "workspace_revision": copy.revision.0,
                }),
            })
            .await;

        self.get_session(session_id).await
    }

    pub async fn list_sessions(
        &self,
        project_id: ProjectId,
        limit: i64,
    ) -> Result<Vec<SessionSummary>, SessionsError> {
        let limit = limit.clamp(1, 100);
        let rows = sqlx::query(
            "SELECT id, project_id, kind, title, state, workspace_handle, \
                    active_turn_id, source_main_revision_id, version, \
                    created_at, updated_at, last_activity_at \
             FROM sessions \
             WHERE project_id = ? AND state != 'deleting' \
             ORDER BY last_activity_at DESC LIMIT ?",
        )
        .bind(project_id.to_string())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(self.row_to_summary(row).await?);
        }
        Ok(out)
    }

    pub async fn get_session(
        &self,
        session_id: SessionId,
    ) -> Result<SessionSummary, SessionsError> {
        let row = sqlx::query(
            "SELECT id, project_id, kind, title, state, workspace_handle, \
                    active_turn_id, source_main_revision_id, version, \
                    created_at, updated_at, last_activity_at \
             FROM sessions WHERE id = ?",
        )
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(SessionsError::NotFound)?;
        self.row_to_summary(row).await
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

        let now = format_utc(SystemClock.now());
        let new_version = format!("v_{}", SessionId::new());
        let title = title.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());

        let result = sqlx::query(
            "UPDATE sessions SET title = COALESCE(?, title), version = ?, updated_at = ? \
             WHERE id = ? AND version = ?",
        )
        .bind(&title)
        .bind(&new_version)
        .bind(&now)
        .bind(session_id.to_string())
        .bind(expected_version)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.into(),
                current: current.version,
            });
        }

        let _ = self
            .events
            .append(NewEvent {
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
            .await;

        self.get_session(session_id).await
    }

    /// Cascade-delete a Session: mark deleting, drop DB rows (FK cascade),
    /// delete Session workspace copy. Does not touch Main or Runtime.
    pub async fn delete_session(
        &self,
        session_id: SessionId,
        actor: Value,
    ) -> Result<(), SessionsError> {
        let current = self.get_session(session_id).await?;
        let now = format_utc(SystemClock.now());
        sqlx::query("UPDATE sessions SET state = 'deleting', updated_at = ? WHERE id = ?")
            .bind(&now)
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await?;

        // Drop projection rows (turns/messages/... cascade from sessions).
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await?;

        self.workspace_sync.delete_session_copy(session_id).await?;

        let _ = self
            .events
            .append(NewEvent {
                event_type: "session.deleted".into(),
                actor,
                resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "session_id": session_id.to_string(),
                    "project_id": current.project_id,
                    "version": current.version,
                }),
            })
            .await;
        Ok(())
    }

    /// Post a user message and start a Turn (M3: always `started`, no queue).
    /// Rejects with `ActiveTurnExists` if a running turn is already present.
    /// Does not invoke supervisor yet (Stage 4).
    pub async fn post_message(
        &self,
        session_id: SessionId,
        content: &str,
        expected_version: &str,
        actor: Value,
    ) -> Result<MessageRouteResult, SessionsError> {
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
            return Err(SessionsError::ActiveTurnExists);
        }

        let handle = WorkspaceHandle(current.workspace_handle.clone());
        let workspace_revision = self
            .workspace_sync
            .current_revision(&handle)
            .await
            .map(|r| r.0)
            .unwrap_or_else(|_| current.source_main_revision_id.clone());

        let next_seq: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM turns WHERE session_id = ?",
        )
        .bind(session_id.to_string())
        .fetch_one(&self.pool)
        .await?;

        let message_id = MessageId::new();
        let turn_id = TurnId::new();
        let checkpoint_id = CheckpointId::new();
        let timeline_id = TimelineItemId::new();
        let now = format_utc(SystemClock.now());
        let session_version = format!("v_{}", SessionId::new());
        let turn_version = format!("v_{}", TurnId::new());
        let message_version = format!("v_{}", MessageId::new());
        let timeline_version = format!("v_{}", TimelineItemId::new());

        let model_snapshot = json!({
            "provider_id": null,
            "upstream_model_id": null,
            "note": "model snapshot filled when supervisor runs (Stage 4)"
        });
        let body = json!({"parts": [{"type": "text", "text": content}]});
        let next_order: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(display_order), 0) + 1 FROM timeline_items WHERE session_id = ?",
        )
        .bind(session_id.to_string())
        .fetch_one(&self.pool)
        .await?;

        let mut tx = self.pool.begin().await?;

        // turns.input_message_id is not an FK; messages.turn_id is. Insert turn first.
        sqlx::query(
            "INSERT INTO turns \
             (id, session_id, sequence, status, input_message_id, model_snapshot_json, \
              completion_summary_json, completion_reason, input_tokens, output_tokens, \
              version, created_at, updated_at) \
             VALUES (?, ?, ?, 'running', ?, ?, NULL, NULL, 0, 0, ?, ?, ?)",
        )
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .bind(next_seq)
        .bind(message_id.to_string())
        .bind(serde_json::to_string(&model_snapshot)?)
        .bind(&turn_version)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db)
                if db.message().contains("UNIQUE") || db.code().as_deref() == Some("2067") =>
            {
                SessionsError::ActiveTurnExists
            }
            _ => SessionsError::Storage(e),
        })?;

        sqlx::query(
            "INSERT INTO messages \
             (id, session_id, turn_id, actor_json, kind, body_json, status, \
              timeline_sequence, version, created_at) \
             VALUES (?, ?, ?, ?, 'user', ?, 'active', ?, ?, ?)",
        )
        .bind(message_id.to_string())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(serde_json::to_string(&actor)?)
        .bind(serde_json::to_string(&body)?)
        .bind(next_order)
        .bind(&message_version)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO checkpoints \
             (id, session_id, kind, timeline_position, workspace_revision_id, \
              source_message_id, source_turn_id, created_at) \
             VALUES (?, ?, 'pre_turn', ?, ?, ?, ?, ?)",
        )
        .bind(checkpoint_id.to_string())
        .bind(session_id.to_string())
        .bind(next_order)
        .bind(&workspace_revision)
        .bind(message_id.to_string())
        .bind(turn_id.to_string())
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        let projection = json!({
            "kind": "user_message",
            "message_id": message_id.to_string(),
            "turn_id": turn_id.to_string(),
            "text": content,
        });
        sqlx::query(
            "INSERT INTO timeline_items \
             (id, session_id, turn_id, kind, source_resource_id, display_order, \
              projection_json, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'user_message', ?, ?, ?, 'active', ?, ?, ?)",
        )
        .bind(timeline_id.to_string())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(message_id.to_string())
        .bind(next_order)
        .bind(serde_json::to_string(&projection)?)
        .bind(&timeline_version)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        let updated = sqlx::query(
            "UPDATE sessions SET state = 'active', active_turn_id = ?, version = ?, \
             updated_at = ?, last_activity_at = ? \
             WHERE id = ? AND version = ? AND active_turn_id IS NULL",
        )
        .bind(turn_id.to_string())
        .bind(&session_version)
        .bind(&now)
        .bind(&now)
        .bind(session_id.to_string())
        .bind(expected_version)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(SessionsError::ActiveTurnExists);
        }

        tx.commit().await?;

        let correlation = CorrelationId::new().to_string();
        let _ = self
            .events
            .append(NewEvent {
                event_type: "checkpoint.created".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
                correlation_id: correlation.clone(),
                causation_id: None,
                payload: json!({
                    "checkpoint_id": checkpoint_id.to_string(),
                    "session_id": session_id.to_string(),
                    "kind": "pre_turn",
                    "workspace_revision_id": workspace_revision,
                }),
            })
            .await;
        let _ = self
            .events
            .append(NewEvent {
                event_type: "turn.created".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
                correlation_id: correlation.clone(),
                causation_id: None,
                payload: json!({
                    "turn_id": turn_id.to_string(),
                    "session_id": session_id.to_string(),
                    "sequence": next_seq,
                    "status": "running",
                    "route": "started",
                }),
            })
            .await;
        let _ = self
            .events
            .append(NewEvent {
                event_type: "timeline.item_created".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
                correlation_id: correlation.clone(),
                causation_id: None,
                payload: json!({
                    "timeline_item_id": timeline_id.to_string(),
                    "session_id": session_id.to_string(),
                    "kind": "user_message",
                    "display_order": next_order,
                }),
            })
            .await;
        let _ = self
            .events
            .append(NewEvent {
                event_type: "session.changed".into(),
                actor,
                resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
                correlation_id: correlation,
                causation_id: None,
                payload: json!({
                    "session_id": session_id.to_string(),
                    "state": "active",
                    "active_turn_id": turn_id.to_string(),
                    "version": session_version,
                }),
            })
            .await;

        Ok(MessageRouteResult {
            route: "started".into(),
            message_id: message_id.to_string(),
            turn_id: turn_id.to_string(),
            session_version,
        })
    }

    /// Test helper: mark a running turn completed and clear active_turn.
    pub async fn force_complete_turn_for_test(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<(), SessionsError> {
        let now = format_utc(SystemClock.now());
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE turns SET status = 'completed', updated_at = ? WHERE id = ? AND session_id = ?",
        )
        .bind(&now)
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
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
        Ok(())
    }

    pub async fn timeline(
        &self,
        session_id: SessionId,
        before: Option<&str>,
        after: Option<&str>,
        limit: i64,
    ) -> Result<TimelinePage, SessionsError> {
        let _ = self.get_session(session_id).await?;
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
                "SELECT id, session_id, turn_id, kind, source_resource_id, display_order, \
                        projection_json, status, version, created_at \
                 FROM timeline_items \
                 WHERE session_id = ? AND display_order < ? \
                 ORDER BY display_order DESC LIMIT ?",
            )
            .bind(session_id.to_string())
            .bind(order)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else if let Some(after) = after {
            let order = parse_cursor(after)?;
            sqlx::query(
                "SELECT id, session_id, turn_id, kind, source_resource_id, display_order, \
                        projection_json, status, version, created_at \
                 FROM timeline_items \
                 WHERE session_id = ? AND display_order > ? \
                 ORDER BY display_order ASC LIMIT ?",
            )
            .bind(session_id.to_string())
            .bind(order)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT id, session_id, turn_id, kind, source_resource_id, display_order, \
                        projection_json, status, version, created_at \
                 FROM timeline_items \
                 WHERE session_id = ? \
                 ORDER BY display_order DESC LIMIT ?",
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
            "SELECT id, session_id, sequence, status, input_message_id, version, created_at, updated_at \
             FROM turns WHERE id = ? AND session_id = ?",
        )
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(SessionsError::NotFound)?;
        Ok(TurnSummary {
            id: row.try_get("id")?,
            session_id: row.try_get("session_id")?,
            sequence: row.try_get("sequence")?,
            status: row.try_get("status")?,
            input_message_id: row.try_get("input_message_id")?,
            version: row.try_get("version")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    async fn load_project(&self, project_id: ProjectId) -> Result<ProjectRow, SessionsError> {
        let row = sqlx::query("SELECT id, state FROM projects WHERE id = ?")
            .bind(project_id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(SessionsError::ProjectNotFound)?;
        Ok(ProjectRow {
            state: row.try_get("state")?,
        })
    }

    async fn row_to_summary(
        &self,
        row: sqlx::sqlite::SqliteRow,
    ) -> Result<SessionSummary, SessionsError> {
        let workspace_handle: String = row.try_get("workspace_handle")?;
        let workspace_revision = {
            let handle = WorkspaceHandle(workspace_handle.clone());
            self.workspace_sync
                .current_revision(&handle)
                .await
                .ok()
                .map(|r| r.0)
        };
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
            version: row.try_get("version")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            last_activity_at: row.try_get("last_activity_at")?,
        })
    }
}

struct ProjectRow {
    state: String,
}

fn timeline_row(row: sqlx::sqlite::SqliteRow) -> Result<TimelineItemView, SessionsError> {
    let projection_json: String = row.try_get("projection_json")?;
    let projection: Value = serde_json::from_str(&projection_json)?;
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
    })
}
