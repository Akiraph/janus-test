//! React to durable Runtime terminal-state changes (M4 "Async Resource Wake-up").
//!
//! Job completion is what unblocks a `waiting_for_job` Turn. The Runtime module
//! already persists the Job's terminal state and emits a `job.changed` event; it
//! does NOT own Turn state. This module is the application-level bridge: given a
//! job that settled, it loads the controlling Turn, counts the Turn's remaining
//! unfinished finite Jobs, and either resumes the Turn (`waiting_for_job` ->
//! `running`) and schedules exactly one next Round, or leaves it blocked when
//! other unfinished Jobs remain. Stale/duplicate notifications are harmless: a
//! Turn that is no longer `waiting_for_job` is a no-op.
//!
//! The single-flight "exactly one next Round" guarantee is provided by the
//! supervisor re-running `execute_turn`, which is idempotent on a non-`running`
//! Turn; resume + execute together start one Round chain. This module never
//! infers state from Job log text — it only reads the durable Job/Turn rows.

use serde_json::json;

use crate::AppState;
use crate::platform::events::NewEvent;
use crate::platform::id::{CorrelationId, JobId, SessionId, TurnId};

use crate::modules::sessions::types::SessionsError;

impl AppState {
    /// Called after a Job reaches a terminal status. Resume the controlling
    /// Turn if all its finite Jobs have now settled, and schedule one next
    /// Round. Returns the Turn that became runnable, if any. Idempotent: a Turn
    /// not in `waiting_for_job` (already resumed, canceled, or handed off) is a
    /// no-op and returns `None`.
    pub async fn on_job_settled(&self, job_id: JobId) -> Result<Option<TurnId>, SessionsError> {
        // Load the settled Job's controlling Turn (the Job row is terminal, so
        // its controlling_turn_id is the durable owner even after a handoff
        // transferred it).
        let controlling: Option<String> =
            sqlx::query_scalar("SELECT controlling_turn_id FROM jobs WHERE id = ?")
                .bind(job_id.to_string())
                .fetch_optional(self.sessions().pool())
                .await?
                .flatten();
        let Some(turn_id_str) = controlling else {
            return Ok(None);
        };
        let turn_id: TurnId = turn_id_str
            .parse()
            .map_err(|_| SessionsError::Internal(anyhow::anyhow!("invalid controlling_turn_id")))?;

        // Only a Turn still waiting for Jobs is resumable from this path.
        let turn_status: Option<String> =
            sqlx::query_scalar("SELECT status FROM turns WHERE id = ?")
                .bind(turn_id.to_string())
                .fetch_optional(self.sessions().pool())
                .await?;
        let Some(status) = turn_status else {
            return Ok(None);
        };
        if status != "waiting_for_job" {
            return Ok(None);
        }

        // Count the Turn's remaining unfinished finite Jobs (queued/running).
        // If any remain, the Turn legitimately stays paused.
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(1) FROM jobs \
             WHERE controlling_turn_id = ? AND status IN ('queued', 'running')",
        )
        .bind(turn_id.to_string())
        .fetch_one(self.sessions().pool())
        .await?;
        if remaining > 0 {
            return Ok(None);
        }

        // Load the session id for the Turn (resume_turn + supervisor need it).
        let session_id_str: String =
            sqlx::query_scalar("SELECT session_id FROM turns WHERE id = ?")
                .bind(turn_id.to_string())
                .fetch_one(self.sessions().pool())
                .await?;
        let session_id: SessionId = session_id_str
            .parse()
            .map_err(|_| SessionsError::Internal(anyhow::anyhow!("invalid session_id")))?;

        let _ = self
            .events()
            .append(NewEvent {
                event_type: "job.wake".into(),
                actor: json!({"kind": "supervisor"}),
                resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "turn_id": turn_id.to_string(),
                    "settled_job_id": job_id.to_string(),
                    "from": "waiting_for_job",
                    "to": "running",
                }),
            })
            .await;

        let version = self
            .sessions()
            .resume_turn(
                session_id,
                turn_id,
                "waiting_for_job",
                json!({"kind": "supervisor"}),
            )
            .await?;
        let _ = version;
        Ok(Some(turn_id))
    }
}
