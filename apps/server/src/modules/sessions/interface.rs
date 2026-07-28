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
    CancelResult, MessageRoute, MessageRouteResult, QueuedTurnSummary, SessionSummary,
    SessionsError, SteerResult, TimelineItemView, TimelinePage, TurnSummary,
};

#[derive(Clone)]
pub struct SessionsInterface {
    pub(super) pool: SqlitePool,
    pub(super) events: EventStore,
    pub(super) workspace_sync: WorkspaceSyncInterface,
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

    /// Borrow the connection pool. Used by `application::session_flow` to open the
    /// shared Handoff/cancel transaction that spans sessions + supervisor + runtime.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
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

    /// Post a user message and return the durable routing result.
    ///
    /// M4 Session Control State Machine: an ordinary user message is never
    /// rejected for an already-active Turn. Instead it is routed as:
    /// - `started` when the Session is idle (a `running` Turn is promoted in the
    ///   same transaction);
    /// - `queued` when a Turn is already active in any active state
    ///   (`running`, `waiting_for_job`, `waiting_for_ask`, `waiting_for_model`,
    ///   `canceling`); the new Turn is appended without taking a workspace
    ///   checkpoint yet.
    ///
    /// Handoff routing (`waiting_for_job` → successor Turn) is handled at the
    /// application layer (`application::session_flow`) because it must
    /// atomically transfer Runtime jobs, which sessions cannot depend on.
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

        let route = if current.active_turn_id.is_none() {
            MessageRoute::Started
        } else {
            MessageRoute::Queued
        };
        // When the active Turn is blocked on finite Jobs, a new message should
        // take over via an atomic Handoff rather than wait in the FIFO queue.
        // `sessions` cannot perform the Handoff itself (it cannot depend on
        // runtime/supervisor), so it only flags `awaiting_handoff` and lets
        // `application::session_flow` promote the queued Turn to the successor.
        let awaiting_handoff = if route == MessageRoute::Queued {
            let active: Option<String> = current.active_turn_id.clone();
            match active {
                Some(turn_id_str) => {
                    let st: Option<String> =
                        sqlx::query_scalar("SELECT status FROM turns WHERE id = ?")
                            .bind(&turn_id_str)
                            .fetch_optional(&self.pool)
                            .await?;
                    st.as_deref() == Some("waiting_for_job")
                }
                None => false,
            }
        } else {
            false
        };

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

        // New Turns are always created `queued`; the `started` route is produced
        // by promoting the queued Turn to `running` inside the same transaction.
        let turn_status = "queued";

        let mut tx = self.pool.begin().await?;

        // turns.input_message_id is not an FK; messages.turn_id is. Insert turn first.
        sqlx::query(
            "INSERT INTO turns \
             (id, session_id, sequence, status, input_message_id, model_snapshot_json, \
              predecessor_turn_id, handoff_from_turn_id, handoff_to_turn_id, \
              completion_summary_json, completion_reason, cancellation_reason, \
              input_tokens, output_tokens, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL, NULL, 0, 0, ?, ?, ?)",
        )
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .bind(next_seq)
        .bind(turn_status)
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

        let promoted_to_running = route == MessageRoute::Started;
        let updated = if promoted_to_running {
            // Promote queued -> running and steal the active slot transactionally.
            sqlx::query(
                "UPDATE turns SET status = 'running', updated_at = ? WHERE id = ? AND status = 'queued'",
            )
            .bind(&now)
            .bind(turn_id.to_string())
            .execute(&mut *tx)
            .await?;
            sqlx::query(
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
            .await?
        } else {
            // Queue: advance session version + activity without stealing the active turn.
            sqlx::query(
                "UPDATE sessions SET version = ?, updated_at = ?, last_activity_at = ? WHERE id = ?",
            )
            .bind(&session_version)
            .bind(&now)
            .bind(&now)
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?
        };

        let claimed = if promoted_to_running {
            updated.rows_affected() == 1
        } else {
            true
        };
        if !claimed {
            // Another worker raced us to the idle slot — leave the Turn queued.
            let route = MessageRoute::Queued;
            tx.commit().await?;
            return Ok(MessageRouteResult {
                route: route.as_str().into(),
                message_id: message_id.to_string(),
                turn_id: turn_id.to_string(),
                session_version,
                handoff_from_turn_id: None,
            });
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
                    "status": route.as_str(),
                    "route": route.as_str(),
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
                    "active_turn_id": if route == MessageRoute::Started {
                        Some(turn_id.to_string())
                    } else {
                        None
                    },
                    "version": session_version,
                }),
            })
            .await;

        Ok(MessageRouteResult {
            route: route.as_str().into(),
            message_id: message_id.to_string(),
            turn_id: turn_id.to_string(),
            session_version,
            handoff_from_turn_id: None,
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

    // ----------------------------------------------------------------------
    // M4 Session Control State Machine primitives
    // ----------------------------------------------------------------------

    /// Return the current active Turn's status (one of the active statuses or a
    /// terminal status), or `None` if the Session is idle. Used by the HTTP layer
    /// and `application::session_flow` to route a new message (started vs queued vs
    /// handoff) and by the worker to decide whether a waiting Turn is actionable.
    pub async fn active_turn_status(
        &self,
        session_id: SessionId,
    ) -> Result<Option<(TurnId, String)>, SessionsError> {
        let session = self.get_session(session_id).await?;
        let Some(turn_id) = session.active_turn_id else {
            return Ok(None);
        };
        let turn_id: TurnId = turn_id
            .parse()
            .map_err(|_| SessionsError::Internal(anyhow::anyhow!("invalid active_turn_id")))?;
        let row = sqlx::query("SELECT status FROM turns WHERE id = ?")
            .bind(turn_id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(SessionsError::NotFound)?;
        Ok(Some((turn_id, row.try_get::<String, _>("status")?)))
    }

    /// List queued Turns in sequence order (FIFO). Includes orphaned start/head
    /// of queue. `source` is inferred from `predecessor_turn_id` presence: turns
    /// carrying a predecessor originate from a Handoff; otherwise from an
    /// ordinary `post_message`.
    pub async fn list_queued_turns(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<QueuedTurnSummary>, SessionsError> {
        let rows = sqlx::query(
            "SELECT id, session_id, sequence, input_message_id, \
                    CASE WHEN predecessor_turn_id IS NULL THEN 'message' ELSE 'handoff' END AS source, \
                    predecessor_turn_id, created_at \
             FROM turns WHERE session_id = ? AND status = 'queued' \
             ORDER BY sequence ASC",
        )
        .bind(session_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok(QueuedTurnSummary {
                    turn_id: r.try_get("id")?,
                    session_id: r.try_get("session_id")?,
                    sequence: r.try_get("sequence")?,
                    message_id: r.try_get("input_message_id")?,
                    source: r.try_get("source")?,
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect()
    }

    /// Steer: bind a user message to the running Turn so it becomes visible at
    /// the next safe Round boundary. The running Turn stays `running`; we only
    /// append a Steered user message and surface it as `incoming_input` so the
    /// supervisor includes it on the next round. Steer is rejected while the Turn
    /// is `waiting_for_model` (supervisor cannot inject mid-attempt safely).
    pub async fn steer(
        &self,
        session_id: SessionId,
        content: &str,
        expected_version: &str,
        actor: Value,
    ) -> Result<SteerResult, SessionsError> {
        let session = self.get_session(session_id).await?;
        if session.state == "deleting" {
            return Err(SessionsError::SessionDeleting);
        }
        if session.version != expected_version {
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.into(),
                current: session.version,
            });
        }
        let Some(active_turn) = session.active_turn_id.clone() else {
            return Err(SessionsError::TurnNotInteractive);
        };
        let turn_id: TurnId = active_turn
            .parse()
            .map_err(|_| SessionsError::Internal(anyhow::anyhow!("invalid active_turn_id")))?;
        let status: String = sqlx::query_scalar("SELECT status FROM turns WHERE id = ?")
            .bind(turn_id.to_string())
            .fetch_one(&self.pool)
            .await?;
        if status == "waiting_for_model" {
            return Err(SessionsError::SteerBlockedByModel);
        }
        if !matches!(
            status.as_str(),
            "running" | "waiting_for_job" | "waiting_for_ask"
        ) {
            return Err(SessionsError::TurnNotInteractive);
        }

        let message_id = MessageId::new();
        let now = format_utc(SystemClock.now());
        let next_order: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(display_order), 0) + 1 FROM timeline_items WHERE session_id = ?",
        )
        .bind(session_id.to_string())
        .fetch_one(&self.pool)
        .await?;
        let session_version = format!("v_{}", SessionId::new());

        let mut tx = self.pool.begin().await?;
        let body = json!({"parts": [{"type": "text", "text": content}], "steer": true});
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
        .bind(format!("v_{}", MessageId::new()))
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        let projection = json!({
            "kind": "steer",
            "message_id": message_id.to_string(),
            "turn_id": turn_id.to_string(),
            "text": content,
        });
        sqlx::query(
            "INSERT INTO timeline_items \
             (id, session_id, turn_id, kind, source_resource_id, display_order, \
              projection_json, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'steer', ?, ?, ?, 'active', ?, ?, ?)",
        )
        .bind(TimelineItemId::new().to_string())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(message_id.to_string())
        .bind(next_order)
        .bind(serde_json::to_string(&projection)?)
        .bind(format!("v_{}", TimelineItemId::new()))
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        sqlx::query("UPDATE sessions SET version = ?, updated_at = ? WHERE id = ?")
            .bind(&session_version)
            .bind(&now)
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        let _ = self
            .events
            .append(NewEvent {
                event_type: "timeline.item_created".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "timeline_item_id": "steer",
                    "kind": "steer",
                    "turn_id": turn_id.to_string(),
                }),
            })
            .await;
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
                    "version": session_version,
                    "steer": { "turn_id": turn_id.to_string() },
                }),
            })
            .await;

        Ok(SteerResult {
            turn_id: turn_id.to_string(),
            message_id: message_id.to_string(),
            session_version,
        })
    }

    /// Cancel: drive `running | waiting_for_* -> canceling`. Final state
    /// (`canceled` vs `interrupted`) is set by the supervisor/runtime after
    /// finite resources settle; sessions only records the transition to
    /// `canceling` here.
    pub async fn cancel_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        reason: &str,
        expected_version: &str,
        actor: Value,
    ) -> Result<CancelResult, SessionsError> {
        let session = self.get_session(session_id).await?;
        if session.version != expected_version {
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.into(),
                current: session.version,
            });
        }
        let turn = self.get_turn(session_id, turn_id).await?;
        if !matches!(
            turn.status.as_str(),
            "running" | "waiting_for_job" | "waiting_for_ask" | "waiting_for_model"
        ) {
            return Err(SessionsError::TurnTerminal);
        }
        let now = format_utc(SystemClock.now());
        let session_version = format!("v_{}", SessionId::new());
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE turns SET status = 'canceling', cancellation_reason = ?, updated_at = ? \
             WHERE id = ? AND status IN ('running', 'waiting_for_job', 'waiting_for_ask', 'waiting_for_model')",
        )
        .bind(reason)
        .bind(&now)
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(SessionsError::TurnTerminal);
        }
        sqlx::query("UPDATE sessions SET version = ?, updated_at = ? WHERE id = ?")
            .bind(&session_version)
            .bind(&now)
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        let _ = self
            .events
            .append(NewEvent {
                event_type: "turn.status_changed".into(),
                actor,
                resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "turn_id": turn_id.to_string(),
                    "from": turn.status,
                    "to": "canceling",
                    "reason": reason,
                }),
            })
            .await;
        Ok(CancelResult {
            turn_id: turn_id.to_string(),
            from_status: turn.status,
            to_status: "canceling".into(),
            session_version,
        })
    }

    // ----------------------------------------------------------------------
    // M4 waiting/resume primitives (Turn state machine is owned by sessions)
    // ----------------------------------------------------------------------

    /// Move the active Turn from `running` to a `waiting_for_*` status. The
    /// Turn keeps the active slot (it is not terminal); `pause_state` is one of
    /// `waiting_for_job`, `waiting_for_ask`, or `waiting_for_model`. The
    /// supervisor calls this before it blocks on a finite Job, an Ask, or a
    /// `waiting_for_model` retry-model reload. Returns the new Session version
    /// so the caller can prove its own row otherwise unchanged.
    pub async fn pause_turn_for(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        pause_state: &str,
        actor: Value,
    ) -> Result<String, SessionsError> {
        let prev = self.get_turn(session_id, turn_id).await?;
        if !matches!(prev.status.as_str(), "running") {
            // Idempotent: already paused/transitioning — return current version.
            let s = self.get_session(session_id).await?;
            return Ok(s.version);
        }
        let now = format_utc(SystemClock.now());
        let session_version = format!("v_{}", SessionId::new());
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE turns SET status = ?, updated_at = ? \
             WHERE id = ? AND status = 'running'",
        )
        .bind(pause_state)
        .bind(&now)
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE sessions SET version = ?, updated_at = ? WHERE id = ?")
            .bind(&session_version)
            .bind(&now)
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let _ = self
            .events
            .append(NewEvent {
                event_type: "turn.status_changed".into(),
                actor,
                resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "turn_id": turn_id.to_string(),
                    "from": "running",
                    "to": pause_state,
                }),
            })
            .await;
        Ok(session_version)
    }

    /// Resume a paused Turn: `waiting_for_job` | `waiting_for_ask` |
    /// `waiting_for_model` -> `running`. The Turn must still hold the active
    /// slot. Used by `application::session_flow` (Ask answer / expire resume
    /// with a default / runtime_events Job wake-up / retry-model reload). Returns
    /// the new Session version, or the current version unchanged when the Turn
    /// is already `running` or no longer holds the slot.
    pub async fn resume_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        from_state: &str,
        actor: Value,
    ) -> Result<String, SessionsError> {
        let prev = self.get_turn(session_id, turn_id).await?;
        if prev.status != from_state {
            let s = self.get_session(session_id).await?;
            return Ok(s.version);
        }
        let now = format_utc(SystemClock.now());
        let session_version = format!("v_{}", SessionId::new());
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE turns SET status = 'running', updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(&now)
        .bind(turn_id.to_string())
        .bind(from_state)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.commit().await?;
            return Ok(session_version);
        }
        sqlx::query("UPDATE sessions SET version = ?, updated_at = ? WHERE id = ?")
            .bind(&session_version)
            .bind(&now)
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let _ = self
            .events
            .append(NewEvent {
                event_type: "turn.status_changed".into(),
                actor,
                resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "turn_id": turn_id.to_string(),
                    "from": from_state,
                    "to": "running",
                }),
            })
            .await;
        Ok(session_version)
    }

    // ----------------------------------------------------------------------
    // M4 Handoff transactional primitives — called by application::session_flow
    // inside a shared transaction. Sessions owns turns/sessions/checkpoints; the
    // tx is opened and committed by the coordinator so Runtime job transfer and
    // Ask closing commit with the Turn state together.
    // ----------------------------------------------------------------------

    /// Insert the messages row backing a Handoff successor Turn (the user message
    /// that triggered the handoff) and its `pre_turn` checkpoint, plus a Turn row
    /// in `queued` status carrying `predecessor_turn_id`. Returns the new Turn id
    /// and message id. Does NOT touch the active slot or the predecessor — the
    /// coordinator decides promotion order. `workspace_revision` is the boundary
    /// revision snapshot the supervisor captured for the handoff.
    pub async fn create_handoff_successor_in_tx(
        &self,
        tx: &mut sqlx::sqlite::SqliteConnection,
        session_id: SessionId,
        predecessor_turn_id: TurnId,
        content: &str,
        model_snapshot_json: &str,
        workspace_revision: &str,
        actor: Value,
    ) -> Result<(TurnId, MessageId), SessionsError> {
        let next_seq: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM turns WHERE session_id = ?",
        )
        .bind(session_id.to_string())
        .fetch_one(&mut *tx)
        .await?;
        let message_id = MessageId::new();
        let turn_id = TurnId::new();
        let checkpoint_id = CheckpointId::new();
        let now = format_utc(SystemClock.now());
        let body = json!({"parts": [{"type": "text", "text": content}], "route": "handoff"});
        let next_order: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(display_order), 0) + 1 FROM timeline_items WHERE session_id = ?",
        )
        .bind(session_id.to_string())
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO turns \
             (id, session_id, sequence, status, input_message_id, model_snapshot_json, \
              predecessor_turn_id, handoff_from_turn_id, handoff_to_turn_id, \
              completion_summary_json, completion_reason, cancellation_reason, \
              input_tokens, output_tokens, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'queued', ?, ?, ?, NULL, NULL, NULL, NULL, NULL, 0, 0, ?, ?, ?)",
        )
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .bind(next_seq)
        .bind(message_id.to_string())
        .bind(model_snapshot_json)
        .bind(predecessor_turn_id.to_string())
        .bind(format!("v_{}", TurnId::new()))
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

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
        .bind(format!("v_{}", MessageId::new()))
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
        .bind(workspace_revision)
        .bind(message_id.to_string())
        .bind(turn_id.to_string())
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        Ok((turn_id, message_id))
    }

    /// Record the bidirectional handoff links between predecessor and successor.
    pub async fn record_handoff_links_in_tx(
        &self,
        tx: &mut sqlx::sqlite::SqliteConnection,
        predecessor_turn_id: TurnId,
        successor_turn_id: TurnId,
    ) -> Result<(), SessionsError> {
        let now = format_utc(SystemClock.now());
        sqlx::query("UPDATE turns SET handoff_to_turn_id = ?, updated_at = ? WHERE id = ?")
            .bind(successor_turn_id.to_string())
            .bind(&now)
            .bind(predecessor_turn_id.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE turns SET handoff_from_turn_id = ?, updated_at = ? WHERE id = ?")
            .bind(predecessor_turn_id.to_string())
            .bind(&now)
            .bind(successor_turn_id.to_string())
            .execute(&mut *tx)
            .await?;
        Ok(())
    }

    /// Attach a predecessor link to an already-queued Turn created by
    /// `post_message`, making it the Handoff successor. Also stamps the incoming
    /// `handoff_from_turn_id`. The Turn must still be `queued`.
    pub async fn attach_predecessor_in_tx(
        &self,
        tx: &mut sqlx::sqlite::SqliteConnection,
        successor_turn_id: TurnId,
        predecessor_turn_id: TurnId,
    ) -> Result<(), SessionsError> {
        let now = format_utc(SystemClock.now());
        sqlx::query(
            "UPDATE turns SET predecessor_turn_id = ?, handoff_from_turn_id = ?, updated_at = ? \
             WHERE id = ? AND status = 'queued'",
        )
        .bind(predecessor_turn_id.to_string())
        .bind(predecessor_turn_id.to_string())
        .bind(&now)
        .bind(successor_turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        Ok(())
    }

    /// Promote a specific queued Turn to `running` and claim the now-empty
    /// active slot. Used by `application::session_flow::handoff_message` to
    /// promote the successor after the predecessor is settled `handed_off` and
    /// the active slot released, all in the same tx. Returns the new active
    /// Turn id and the new session version on success, or `None` if another
    /// Turn already grabbed the slot.
    pub async fn promote_successor_in_tx(
        &self,
        tx: &mut sqlx::sqlite::SqliteConnection,
        session_id: SessionId,
        successor_turn_id: TurnId,
    ) -> Result<Option<String>, SessionsError> {
        let now = format_utc(SystemClock.now());
        let session_version = format!("v_{}", SessionId::new());
        let result = sqlx::query(
            "UPDATE turns SET status = 'running', updated_at = ? WHERE id = ? AND status = 'queued'",
        )
        .bind(&now)
        .bind(successor_turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        let claim = sqlx::query(
            "UPDATE sessions SET state = 'active', active_turn_id = ?, version = ?, \
             updated_at = ?, last_activity_at = ? WHERE id = ? AND active_turn_id IS NULL",
        )
        .bind(successor_turn_id.to_string())
        .bind(&session_version)
        .bind(&now)
        .bind(&now)
        .bind(session_id.to_string())
        .execute(&mut *tx)
        .await?;
        if claim.rows_affected() == 0 {
            // Slot not free; revert to queued.
            sqlx::query("UPDATE turns SET status = 'queued', updated_at = ? WHERE id = ?")
                .bind(&now)
                .bind(successor_turn_id.to_string())
                .execute(&mut *tx)
                .await?;
            return Ok(None);
        }
        Ok(Some(session_version))
    }

    /// Mark the predecessor Turn `handed_off` (terminal) and release the active
    /// slot. The successor promotion is done separately by
    /// `promote_oldest_queued` after the coordinator commits, so the successor
    /// only becomes active once the predecessor is fully settled.
    pub async fn mark_predecessor_handed_off_in_tx(
        &self,
        tx: &mut sqlx::sqlite::SqliteConnection,
        session_id: SessionId,
        predecessor_turn_id: TurnId,
        completion_reason: Option<&str>,
    ) -> Result<String, SessionsError> {
        let now = format_utc(SystemClock.now());
        let session_version = format!("v_{}", SessionId::new());
        sqlx::query(
            "UPDATE turns SET status = 'handed_off', completion_reason = COALESCE(?, completion_reason), \
             updated_at = ? WHERE id = ?",
        )
        .bind(completion_reason)
        .bind(&now)
        .bind(predecessor_turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE sessions SET state = 'ready', active_turn_id = NULL, version = ?, \
             updated_at = ?, last_activity_at = ? WHERE id = ? AND active_turn_id = ?",
        )
        .bind(&session_version)
        .bind(&now)
        .bind(&now)
        .bind(session_id.to_string())
        .bind(predecessor_turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        Ok(session_version)
    }

    /// Promote the oldest queued Turn to `running` and assign the active slot.
    /// Called by `application::session_flow` after a Turn becomes terminal
    /// (`completed`/`canceled`); `failed`/`interrupted` leave the queue paused
    /// (the caller checks the terminal status before calling this).
    /// Returns the promoted Turn id, or `NothingQueued` when no candidate exists.
    pub async fn promote_oldest_queued(
        &self,
        session_id: SessionId,
    ) -> Result<Option<TurnId>, SessionsError> {
        let now = format_utc(SystemClock.now());
        let session_version = format!("v_{}", SessionId::new());
        let mut tx = self.pool.begin().await?;

        let next: Option<String> = sqlx::query_scalar(
            "SELECT id FROM turns WHERE session_id = ? AND status = 'queued' \
             ORDER BY sequence ASC LIMIT 1",
        )
        .bind(session_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(turn_id_str) = next else {
            tx.commit().await?;
            return Ok(None);
        };

        let promote = sqlx::query(
            "UPDATE turns SET status = 'running', updated_at = ? WHERE id = ? AND status = 'queued'",
        )
        .bind(&now)
        .bind(&turn_id_str)
        .execute(&mut *tx)
        .await?;
        if promote.rows_affected() == 0 {
            tx.commit().await?;
            return Ok(None);
        }
        let claim = sqlx::query(
            "UPDATE sessions SET state = 'active', active_turn_id = ?, version = ?, \
             updated_at = ?, last_activity_at = ? \
             WHERE id = ? AND active_turn_id IS NULL",
        )
        .bind(&turn_id_str)
        .bind(&session_version)
        .bind(&now)
        .bind(&now)
        .bind(session_id.to_string())
        .execute(&mut *tx)
        .await?;
        if claim.rows_affected() == 0 {
            // An active Turn still holds the slot. Revert promotion to queued and
            // leave the message in the queue for the next settle.
            sqlx::query("UPDATE turns SET status = 'queued', updated_at = ? WHERE id = ?")
                .bind(&now)
                .bind(&turn_id_str)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(None);
        }
        tx.commit().await?;

        let turn_id: TurnId = turn_id_str
            .parse()
            .map_err(|_| SessionsError::Internal(anyhow::anyhow!("invalid turn id")))?;
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
                    "from": "queued",
                    "to": "running",
                    "route": "queued_start",
                }),
            })
            .await;
        Ok(Some(turn_id))
    }

    /// Settle a Turn into a terminal status (`canceled`, `interrupted`,
    /// `completed`, `failed`) and release the active slot. If the predecessor
    /// is `completed` or `canceled` the queue is advanced by promoting the
    /// oldest queued Turn; `failed`/`interrupted` leave the queue paused per
    /// the M4 state machine. Returns the promoted Turn id, if any.
    ///
    /// Called by `application::session_flow` once Runtime confirms that finite
    /// resources owned by the cancelling Turn have settled.
    pub async fn settle_terminal_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        terminal: &str,
        reason: Option<&str>,
        actor: Value,
    ) -> Result<Option<TurnId>, SessionsError> {
        debug_assert!(matches!(
            terminal,
            "completed" | "failed" | "canceled" | "interrupted"
        ));
        let now = format_utc(SystemClock.now());
        let session_version = format!("v_{}", SessionId::new());
        let mut tx = self.pool.begin().await?;
        match terminal {
            "canceled" => {
                // The cancel workflow is done; promote regardless of whether
                // the Turn was already in `canceling` or still `running` /
                // waiting (Runtime settle may arrive before the intermediate
                // `canceling` row was written by `cancel_turn`).
                sqlx::query(
                    "UPDATE turns SET status = 'canceled', cancellation_reason = COALESCE(?, cancellation_reason), \
                     updated_at = ? WHERE id = ? AND status IN ('canceling', 'running', 'waiting_for_job', 'waiting_for_ask', 'waiting_for_model')",
                )
                .bind(reason)
                .bind(&now)
                .bind(turn_id.to_string())
                .execute(&mut *tx)
                .await?;
            }
            "interrupted" => {
                sqlx::query(
                    "UPDATE turns SET status = 'interrupted', completion_reason = COALESCE(?, completion_reason), \
                     updated_at = ? WHERE id = ?",
                )
                .bind(reason)
                .bind(&now)
                .bind(turn_id.to_string())
                .execute(&mut *tx)
                .await?;
            }
            "completed" => {
                sqlx::query(
                    "UPDATE turns SET status = 'completed', completion_reason = COALESCE(?, completion_reason), \
                     updated_at = ? WHERE id = ?",
                )
                .bind(reason)
                .bind(&now)
                .bind(turn_id.to_string())
                .execute(&mut *tx)
                .await?;
            }
            "failed" => {
                sqlx::query(
                    "UPDATE turns SET status = 'failed', completion_reason = COALESCE(?, completion_reason), \
                     updated_at = ? WHERE id = ?",
                )
                .bind(reason)
                .bind(&now)
                .bind(turn_id.to_string())
                .execute(&mut *tx)
                .await?;
            }
            _ => {}
        }
        // Release the active slot only if this Turn still holds it.
        sqlx::query(
            "UPDATE sessions SET state = 'ready', active_turn_id = NULL, version = ?, updated_at = ? \
             WHERE id = ? AND active_turn_id = ?",
        )
        .bind(&session_version)
        .bind(&now)
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        let _ = self
            .events
            .append(NewEvent {
                event_type: "turn.status_changed".into(),
                actor,
                resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "turn_id": turn_id.to_string(),
                    "to": terminal,
                }),
            })
            .await;

        // completed/canceled advance the queue; failed/interrupted pause it.
        if matches!(terminal, "completed" | "canceled") {
            self.promote_oldest_queued(session_id).await
        } else {
            Ok(None)
        }
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
            "SELECT id, session_id, sequence, status, input_message_id, \
                    predecessor_turn_id, handoff_from_turn_id, handoff_to_turn_id, \
                    cancellation_reason, completion_reason, version, created_at, updated_at \
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
            predecessor_turn_id: row.try_get("predecessor_turn_id")?,
            handoff_from_turn_id: row.try_get("handoff_from_turn_id")?,
            handoff_to_turn_id: row.try_get("handoff_to_turn_id")?,
            cancellation_reason: row.try_get("cancellation_reason")?,
            completion_reason: row.try_get("completion_reason")?,
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
