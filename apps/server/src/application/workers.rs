//! Background worker loop: leases durable work items and dispatches them to the
//! owning Module handler. Single control-plane task is enough for Phase 1, but
//! the lease nonce + TTL still protects against canceled tasks and restarted
//! processes leaving a stale attempt (`DAT-OP-02`).
//!
//! Handlers map `handler_kind` to a function. M2 handles `project.clone` and
//! `project.delete`; git fetch/update/push are enqueued as Operations but the
//! short ones also run inline in the request path (see `projects`), so the
//! worker focuses on the long external side effects.

use std::time::Duration;

use serde_json::Value;
use tracing::{error, info, warn};

use crate::AppState;
use crate::platform::operations::{KIND_CLONE, KIND_DELETE_PROJECT, OperationStatus};

/// Lease TTL for a claimed work item: short enough that a dead worker's lease
/// is reclaimed quickly, long enough for a clone to finish.
const LEASE_TTL_SECONDS: i64 = 120;

/// The kinds the worker claims. Short git commands run inline in the request
/// path; only the long external side effects go through the queue.
const HANDLED_KINDS: &[&str] = &[KIND_CLONE, KIND_DELETE_PROJECT];

/// Spawn the background worker. Runs until the runtime shuts down.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        info!("janus worker started");
        loop {
            if let Err(error) = run_once(&state).await {
                error!(%error, "worker iteration failed");
            }
            // Idle pause between sweeps; claim_work returns None when the queue
            // is empty, so this keeps CPU flat without missing new items.
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}

/// Spawn the Job-settled wake-up loop. Subscribes to Runtime's broadcast of
/// terminal Job ids and resumes any `waiting_for_job` Turn that no longer has
/// unfinished finite Jobs. Single-flight: each resume schedules one next
/// Supervisor Round via `execute_turn`.
pub fn spawn_job_wake(state: AppState) {
    let mut rx = state.runtime().subscribe_job_settled();
    tokio::spawn(async move {
        info!("janus job-wake worker started");
        loop {
            match rx.recv().await {
                Ok(job_id) => {
                    match state.on_job_settled(job_id).await {
                        Ok(Some(turn_id)) => {
                            // Resume schedules one next Round. Owner is the
                            // bootstrap supervisor; HTTP request-scoped owner
                            // binding is not available on this path.
                            let supervisor = state.supervisor().clone();
                            tokio::spawn(async move {
                                if let Err(error) = supervisor.execute_turn(turn_id).await {
                                    warn!(%error, %turn_id, "job-wake execute_turn failed");
                                }
                            });
                        }
                        Ok(None) => {}
                        Err(error) => {
                            warn!(%error, %job_id, "on_job_settled failed");
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(lagged = n, "job-wake receiver lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

async fn run_once(state: &AppState) -> anyhow::Result<()> {
    for kind in HANDLED_KINDS {
        let Some(claimed) = state
            .operations()
            .claim_work(kind, LEASE_TTL_SECONDS)
            .await?
        else {
            continue;
        };
        let outcome = dispatch(state, kind, &claimed.payload).await;
        match outcome {
            Ok(()) => {
                state
                    .operations()
                    .complete_work(&claimed.id, &claimed.nonce)
                    .await?;
            }
            Err(error) => {
                warn!(%error, kind, "work item failed");
                // Non-fatal errors stay claimable for retry; fatal ones are
                // marked dead so a poison item does not loop forever.
                let dead = is_fatal(&error);
                state
                    .operations()
                    .fail_work(&claimed.id, &claimed.nonce, dead)
                    .await?;
            }
        }
    }
    Ok(())
}

async fn dispatch(state: &AppState, kind: &str, payload: &Value) -> anyhow::Result<()> {
    let project_id = payload
        .get("project_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("work item missing project_id"))?;
    match kind {
        KIND_CLONE => match state.projects().run_clone(project_id).await {
            Ok(()) => {
                record_operation_success(state, kind, project_id, payload).await;
                Ok(())
            }
            Err(error) => {
                record_operation_failure(state, kind, project_id, payload, &error).await;
                Err(anyhow::anyhow!("clone failed: {error}"))
            }
        },
        KIND_DELETE_PROJECT => {
            match crate::application::lifecycle::delete_project_with_runtime(state, project_id).await
            {
                Ok(()) => {
                    record_operation_success(state, kind, project_id, payload).await;
                    Ok(())
                }
                Err(error) => {
                    // record_operation_failure expects a ProjectsError-shaped
                    // display; wrap as a generic failure so the Operation still
                    // lands in needs_attention / failed.
                    record_operation_failure(
                        state,
                        kind,
                        project_id,
                        payload,
                        &crate::modules::projects::interface::ProjectsError::Validation(
                            error.to_string(),
                        ),
                    )
                    .await;
                    Err(error)
                }
            }
        }
        other => Err(anyhow::anyhow!("no handler for kind {other}")),
    }
}

/// Resolve the Operation id from the work payload when present; otherwise fall
/// back to the latest in-flight Operation for the target. Preferring the
/// payload id avoids racing a later retry's Operation when two clones share a
/// project_id (retry path).
async fn resolve_operation_id(
    state: &AppState,
    kind: &str,
    project_id: &str,
    payload: &Value,
) -> Option<String> {
    if let Some(op_id) = payload.get("operation_id").and_then(|v| v.as_str()) {
        return Some(op_id.to_owned());
    }
    sqlx::query_scalar(
        "SELECT id FROM operations WHERE kind = ? AND target_kind = 'project' AND target_id = ? AND status IN ('queued', 'running') ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(kind)
    .bind(project_id)
    .fetch_optional(state.database().pool())
    .await
    .ok()
    .flatten()
}

/// Mark the Operation backing this work item as succeeded so clients polling
/// `GET /operations/{id}` leave the waiting state.
async fn record_operation_success(state: &AppState, kind: &str, project_id: &str, payload: &Value) {
    let Some(op_id) = resolve_operation_id(state, kind, project_id, payload).await else {
        return;
    };
    let correlation = crate::platform::id::CorrelationId::new();
    let _ = state
        .operations()
        .finish(
            &op_id,
            OperationStatus::Succeeded,
            Some(serde_json::json!({"project_id": project_id})),
            None,
            correlation,
        )
        .await;
}

/// Mark the Operation backing this work item as failed so the client can read
/// the failure from `GET /operations/{id}`.
async fn record_operation_failure(
    state: &AppState,
    kind: &str,
    project_id: &str,
    payload: &Value,
    error: &crate::modules::projects::interface::ProjectsError,
) {
    // A miss is not fatal: the Project state itself already records the error
    // (clone -> `error` state).
    let Some(op_id) = resolve_operation_id(state, kind, project_id, payload).await else {
        return;
    };
    let correlation = crate::platform::id::CorrelationId::new();
    let _ = state
        .operations()
        .finish(
            &op_id,
            OperationStatus::Failed,
            None,
            Some(serde_json::json!({"code": error.code(), "detail": error.to_string()})),
            correlation,
        )
        .await;
}

fn is_fatal(error: &anyhow::Error) -> bool {
    // Auth/unreachable failures are recorded on the Project (`error` state) and
    // should not be retried blindly; the user retries explicitly. Validation
    // is fatal. Transient issues (none modeled in M2) would be retried.
    let s = error.to_string();
    s.contains("validation") || s.contains("GIT_AUTH_FAILED") || s.contains("not creating")
}

#[cfg(test)]
mod tests {
    use super::is_fatal;
    use anyhow::anyhow;

    #[test]
    fn validation_and_auth_are_fatal() {
        assert!(is_fatal(&anyhow!("validation failed: name is required")));
        assert!(is_fatal(&anyhow!("clone failed: GIT_AUTH_FAILED")));
        assert!(!is_fatal(&anyhow!("transient timeout")));
    }
}
