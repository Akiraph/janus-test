//! Background worker loop: leases durable work items and dispatches them to the
//! owning capability handler. The current deployment uses one control-plane
//! task; the lease nonce and TTL still protect against canceled tasks and
//! restarted processes leaving a stale attempt.
//!
//! Handlers map `handler_kind` to a function. Project clone/delete and Git
//! operations are durable Operations; the worker focuses on their external
//! side effects so an HTTP disconnect cannot lose the work.

use std::{sync::Arc, time::Duration};

use serde_json::Value;
use tokio::{sync::Semaphore, task::JoinHandle, time::MissedTickBehavior};
use tracing::{error, info, warn};

use crate::application::Application;
use crate::application::operation_kinds::{
    KIND_CLONE, KIND_CREATE_SESSION, KIND_DELETE_PROJECT, KIND_DELETE_SESSION, KIND_TURN_WAKE,
};
use janus_infrastructure::id::TurnId;
use janus_infrastructure::operations::{
    OperationCompletion, OperationInterface, OperationStatus, WorkFailureDisposition,
};

/// Lease TTL for a claimed work item: short enough that a dead worker's lease
/// is reclaimed quickly, long enough for a clone to finish.
const LEASE_TTL_SECONDS: i64 = 120;
const MAX_CONCURRENT_WORK: usize = 4;
const LEASE_RENEW_INTERVAL_SECONDS: u64 = 30;

/// Session creation and deletion both update Git worktree administration and
/// the same workspace-owned tables. Keep those filesystem operations single
/// flight so a burst of sidebar actions cannot interleave half-created trees
/// with cleanup or another worktree registration.
const SESSION_LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The kinds the worker claims. Short git commands run inline in the request
/// path; only the long external side effects go through the queue.
const HANDLED_KINDS: &[&str] = &[
    KIND_CLONE,
    KIND_DELETE_PROJECT,
    KIND_CREATE_SESSION,
    KIND_DELETE_SESSION,
    KIND_TURN_WAKE,
];

#[derive(Debug)]
struct WorkFailure {
    error: anyhow::Error,
    disposition: WorkFailureDisposition,
}

impl WorkFailure {
    fn retry(error: impl Into<anyhow::Error>) -> Self {
        Self {
            error: error.into(),
            disposition: WorkFailureDisposition::Retry,
        }
    }

    fn dead_letter(error: impl Into<anyhow::Error>) -> Self {
        Self {
            error: error.into(),
            disposition: WorkFailureDisposition::DeadLetter,
        }
    }
}

impl From<anyhow::Error> for WorkFailure {
    fn from(error: anyhow::Error) -> Self {
        Self::retry(error)
    }
}

/// Spawn the background worker. Runs until the runtime shuts down.
pub fn spawn(state: Application) {
    let session_lifecycle = Arc::new(Semaphore::new(1));
    let work_concurrency = Arc::new(Semaphore::new(MAX_CONCURRENT_WORK));
    tokio::spawn(async move {
        info!("janus worker started");
        loop {
            if let Err(error) = run_once(&state, &session_lifecycle, &work_concurrency).await {
                error!(%error, "worker iteration failed");
            }
            // Idle pause between sweeps; claim_work returns None when the queue
            // is empty, so this keeps CPU flat without missing new items.
            tokio::time::sleep(SESSION_LIFECYCLE_POLL_INTERVAL).await;
        }
    });
}

/// Spawn the Job-settled wake-up loop. Subscribes to Runtime's broadcast of
/// terminal Job ids and resumes any `waiting_for_job` Turn that no longer has
/// unfinished finite Jobs. Single-flight: each resume schedules one next
/// Execution Round via the application coordinator.
pub fn spawn_job_wake(state: Application) {
    let mut rx = state.runtime().subscribe_job_settled();
    tokio::spawn(async move {
        info!("janus job-wake worker started");
        let mut reconciliation = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                notification = rx.recv() => match notification {
                    Ok(job_id) => match state.execution_coordinator().settle_job(job_id).await {
                        Ok(Some(turn_id)) => state.execution_coordinator().schedule(turn_id),
                        Ok(None) => {}
                        Err(error) => {
                            warn!(%error, %job_id, "settle Job notification failed");
                        }
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(lagged = n, "job-wake receiver lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                _ = reconciliation.tick() => {
                    if let Err(error) = state.execution_coordinator().reconcile_waiting_jobs().await {
                        warn!(%error, "waiting Job reconciliation failed");
                    }
                }
            }
        }
    });
}

/// Periodically expire due best-effort Asks through the application command so
/// defaults become durable Turn input and only runnable Turns are scheduled.
pub fn spawn_ask_expiry(state: Application) {
    tokio::spawn(async move {
        info!("janus ask-expiry worker started");
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            if let Err(error) = state.expire_asks("system").await {
                warn!(%error, "Ask expiry sweep failed");
            }
        }
    });
}

async fn run_once(
    state: &Application,
    session_lifecycle: &Arc<Semaphore>,
    work_concurrency: &Arc<Semaphore>,
) -> anyhow::Result<()> {
    for kind in HANDLED_KINDS {
        let work_permit = match work_concurrency.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => continue,
        };
        let session_permit = if matches!(*kind, KIND_CREATE_SESSION | KIND_DELETE_SESSION) {
            // Do not claim work while the single session lifecycle slot is
            // occupied. Leaving the item queued preserves FIFO ordering and
            // lets the next sweep retry it without a lease timeout.
            match session_lifecycle.clone().try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    drop(work_permit);
                    continue;
                }
            }
        } else {
            None
        };
        let Some(claimed) = state
            .operations()
            .claim_work(kind, LEASE_TTL_SECONDS)
            .await?
        else {
            drop(session_permit);
            continue;
        };
        let worker_state = state.clone();
        let worker_kind = (*kind).to_owned();
        let work_id = claimed.id.clone();
        let work_nonce = claimed.nonce.clone();
        tokio::spawn(async move {
            let _work_permit = work_permit;
            let _session_permit = session_permit;
            let lease_heartbeat = spawn_lease_heartbeat(
                worker_state.operations().clone(),
                work_id.clone(),
                work_nonce.clone(),
            );
            let outcome = dispatch(
                &worker_state,
                &worker_kind,
                &claimed.payload,
                &work_id,
                &work_nonce,
            )
            .await;
            lease_heartbeat.abort();
            let _ = lease_heartbeat.await;
            match outcome {
                Ok(()) => {
                    if let Err(error) = worker_state
                        .operations()
                        .complete_work(&work_id, &work_nonce)
                        .await
                    {
                        warn!(%error, kind = %worker_kind, "complete work item failed");
                    }
                }
                Err(failure) => {
                    warn!(error = %failure.error, kind = %worker_kind, "work item failed");
                    let will_dead_letter = match worker_state
                        .operations()
                        .work_will_dead_letter(&work_id, &work_nonce, failure.disposition)
                        .await
                    {
                        Ok(value) => value,
                        Err(error) => {
                            warn!(%error, kind = %worker_kind, "work item attempt check failed");
                            false
                        }
                    };
                    if will_dead_letter {
                        mark_operation_needs_attention(
                            &worker_state,
                            &claimed.payload,
                            &work_id,
                            &work_nonce,
                            &failure.error,
                        )
                        .await;
                    }
                    if let Err(fail_error) = worker_state
                        .operations()
                        .fail_work(&work_id, &work_nonce, failure.disposition)
                        .await
                    {
                        warn!(%fail_error, kind = %worker_kind, "fail work item update failed");
                    }
                }
            }
        });
    }
    Ok(())
}

async fn dispatch(
    state: &Application,
    kind: &str,
    payload: &Value,
    work_id: &str,
    work_nonce: &str,
) -> Result<(), WorkFailure> {
    match kind {
        KIND_CLONE => {
            let project_id = payload_string(payload, "project_id")?;
            match state.projects().run_clone(project_id).await {
                Ok(()) => {
                    record_operation_success(state, kind, project_id, payload, work_id, work_nonce)
                        .await;
                    Ok(())
                }
                Err(error) => Err(WorkFailure {
                    error: anyhow::anyhow!("clone failed: {error}"),
                    disposition: project_failure_disposition(&error),
                }),
            }
        }
        KIND_DELETE_PROJECT => {
            let project_id = payload_string(payload, "project_id")?;
            let operation_id = payload
                .get("operation_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    WorkFailure::dead_letter(anyhow::anyhow!(
                        "project delete work item missing operation_id"
                    ))
                })?;
            match crate::application::lifecycle::delete_project_with_runtime(
                state,
                project_id,
                operation_id,
                work_id,
                work_nonce,
            )
            .await
            {
                Ok(()) => {
                    record_operation_success(state, kind, project_id, payload, work_id, work_nonce)
                        .await;
                    Ok(())
                }
                Err(error) => Err(WorkFailure::retry(error)),
            }
        }
        KIND_CREATE_SESSION => {
            crate::application::lifecycle::run_session_creation_operation(
                state, payload, work_id, work_nonce,
            )
            .await
            .map_err(WorkFailure::retry)?;
            Ok(())
        }
        KIND_DELETE_SESSION => {
            crate::application::lifecycle::run_session_deletion_operation(
                state, payload, work_id, work_nonce,
            )
            .await
            .map_err(WorkFailure::retry)?;
            Ok(())
        }
        KIND_TURN_WAKE => {
            let turn_id = payload_string(payload, "turn_id")?
                .parse::<TurnId>()
                .map_err(|error| {
                    WorkFailure::dead_letter(anyhow::anyhow!("invalid Turn id in wake: {error}"))
                })?;
            if let Some(handle) = state.execution_coordinator().schedule_and_wait(turn_id) {
                handle
                    .await
                    .map_err(|error| anyhow::anyhow!("Turn runner task failed: {error}"))?
                    .map_err(|error| anyhow::anyhow!("Turn execution failed: {error}"))?;
            }
            Ok(())
        }
        other => Err(WorkFailure::dead_letter(anyhow::anyhow!(
            "no handler for kind {other}"
        ))),
    }
}

fn payload_string<'a>(payload: &'a Value, field: &str) -> Result<&'a str, WorkFailure> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| WorkFailure::dead_letter(anyhow::anyhow!("work item missing {field}")))
}

/// Resolve the Operation id from the work payload when present; otherwise fall
/// back to the latest in-flight Operation for the target. Preferring the
/// payload id avoids racing a later retry's Operation when two clones share a
/// project_id (retry path).
async fn resolve_operation_id(
    state: &Application,
    kind: &str,
    project_id: &str,
    payload: &Value,
) -> Option<String> {
    if let Some(op_id) = payload.get("operation_id").and_then(|v| v.as_str()) {
        return Some(op_id.to_owned());
    }
    state
        .operations()
        .in_flight_for_target(kind, "project", project_id)
        .await
        .ok()
        .flatten()
}

/// Mark the Operation backing this work item as succeeded so clients polling
/// `GET /operations/{id}` leave the waiting state.
async fn record_operation_success(
    state: &Application,
    kind: &str,
    project_id: &str,
    payload: &Value,
    work_id: &str,
    work_nonce: &str,
) {
    let Some(op_id) = resolve_operation_id(state, kind, project_id, payload).await else {
        return;
    };
    let correlation = janus_infrastructure::id::CorrelationId::new();
    match state
        .operations()
        .finish_claimed(
            &op_id,
            work_id,
            work_nonce,
            OperationCompletion {
                status: OperationStatus::Succeeded,
                result: Some(serde_json::json!({"project_id": project_id})),
                problem: None,
                correlation_id: correlation,
            },
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => warn!(%op_id, %work_id, "stale worker could not finish Operation"),
        Err(error) => warn!(%error, %op_id, "finish Operation failed"),
    }
}

fn project_failure_disposition(
    error: &janus_projects::interface::ProjectsError,
) -> WorkFailureDisposition {
    use janus_projects::interface::{GitError, ProjectsError};

    match error {
        ProjectsError::Git(
            GitError::RemoteUnavailable | GitError::CommandFailed(_) | GitError::BadOutput(_),
        )
        | ProjectsError::Workspace(_)
        | ProjectsError::Operation(_)
        | ProjectsError::Storage(_)
        | ProjectsError::Serde(_)
        | ProjectsError::Io(_)
        | ProjectsError::Internal(_) => WorkFailureDisposition::Retry,
        ProjectsError::Validation(_)
        | ProjectsError::NotFound
        | ProjectsError::CredentialNotFound
        | ProjectsError::ConflictNotFound
        | ProjectsError::RevisionMismatch { .. }
        | ProjectsError::InvalidPath(_)
        | ProjectsError::NotEditable(_)
        | ProjectsError::Git(_) => WorkFailureDisposition::DeadLetter,
    }
}

fn spawn_lease_heartbeat(
    operations: OperationInterface,
    work_id: String,
    work_nonce: String,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(LEASE_RENEW_INTERVAL_SECONDS));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match operations
                .renew_work(&work_id, &work_nonce, LEASE_TTL_SECONDS)
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    warn!(%work_id, "work lease could not be renewed");
                    break;
                }
                Err(error) => {
                    warn!(%error, %work_id, "work lease renewal failed");
                }
            }
        }
    })
}

async fn mark_operation_needs_attention(
    state: &Application,
    payload: &Value,
    work_id: &str,
    work_nonce: &str,
    error: &anyhow::Error,
) {
    let Some(operation_id) = payload.get("operation_id").and_then(Value::as_str) else {
        return;
    };
    match state
        .operations()
        .finish_claimed(
            operation_id,
            work_id,
            work_nonce,
            OperationCompletion {
                status: OperationStatus::NeedsAttention,
                result: None,
                problem: Some(serde_json::json!({
                    "code": "WORK_ITEM_DEAD_LETTERED",
                    "detail": error.to_string(),
                })),
                correlation_id: janus_infrastructure::id::CorrelationId::new(),
            },
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => warn!(%operation_id, %work_id, "dead work item could not update Operation"),
        Err(finish_error) => {
            warn!(%finish_error, %operation_id, "dead work item Operation update failed")
        }
    }
}
