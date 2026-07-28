//! Cross-module Session/Project deletion and bounded Runtime shutdown.
//!
//! Sessions and Projects modules must not depend on Runtime (architecture
//! module.toml). Deletion that stops live Jobs/Services/Terminals therefore
//! lives here: isolate the resource, stop Runtime-owned processes, then hand
//! the durable row/workspace removal back to the owning module.

use std::time::Duration;

use serde_json::json;
use tracing::{info, warn};

use crate::AppState;
use crate::modules::runtime::interface::{JobStatus, ServiceStatus, TerminalOwner, TerminalStatus};
use crate::modules::sessions::types::SessionsError;
use crate::platform::id::{ProjectId, SessionId};

/// Stop every live Runtime resource owned by a Session, then drop the Session
/// row and its workspace copy. Failures stopping individual resources are
/// logged and do not block the durable delete — a later restart recovery will
/// mark any leftover rows `lost`.
pub async fn delete_session_with_runtime(
    state: &AppState,
    session_id: SessionId,
    actor: serde_json::Value,
) -> Result<(), SessionsError> {
    cleanup_session_runtime(state, session_id).await;
    state.sessions().delete_session(session_id, actor).await
}

/// Project cascade: for every non-deleting Session under the Project, stop its
/// Runtime resources and drop the Session, then hand the Project row + Main
/// workspace removal to `ProjectsInterface::run_delete`.
pub async fn delete_project_with_runtime(
    state: &AppState,
    project_id: &str,
) -> anyhow::Result<()> {
    let project: ProjectId = project_id
        .parse()
        .map_err(|error| anyhow::anyhow!("project id: {error}"))?;
    // Include a generous limit so a Project with many Sessions still cleans up.
    let sessions = state
        .sessions()
        .list_sessions(project, 500)
        .await
        .map_err(|error| anyhow::anyhow!("list sessions for delete: {error}"))?;
    for session in sessions {
        let session_id: SessionId = session
            .id
            .parse()
            .map_err(|error| anyhow::anyhow!("session id: {error}"))?;
        let actor = json!({"kind": "system", "reason": "project_delete"});
        if let Err(error) = delete_session_with_runtime(state, session_id, actor).await {
            warn!(%error, session_id = %session.id, "session cleanup during project delete failed");
        }
    }
    // Project-owned Terminals (Main Terminal) are not Session-scoped; close them
    // against the Project owner before the Main workspace is removed.
    if let Ok(project_id_typed) = project_id.parse::<ProjectId>() {
        cleanup_project_terminals(state, project_id_typed).await;
    }
    state
        .projects()
        .run_delete(project_id)
        .await
        .map_err(|error| anyhow::anyhow!("project run_delete: {error}"))
}

/// Best-effort stop of Jobs / Services / Terminals / Runtime for one Session.
pub async fn cleanup_session_runtime(state: &AppState, session_id: SessionId) {
    let runtime = state.runtime();

    // Cancel running/queued Jobs first so they do not race a later stop_runtime.
    if let Ok(jobs) = runtime.jobs(session_id).await {
        for job in jobs {
            if matches!(job.status, JobStatus::Queued | JobStatus::Running) {
                if let Err(error) = runtime.cancel_job(job.id).await {
                    warn!(%error, job_id = %job.id, "cancel job during session cleanup");
                }
            }
        }
    }

    if let Ok(services) = runtime.services(session_id).await {
        for service in services {
            if matches!(
                service.status,
                ServiceStatus::Starting
                    | ServiceStatus::Running
                    | ServiceStatus::Unhealthy
                    | ServiceStatus::Stopping
            ) {
                if let Err(error) = runtime.stop_service(service.id).await {
                    warn!(%error, service_id = %service.id, "stop service during session cleanup");
                }
            }
        }
    }

    if let Ok(terminals) = runtime
        .list_terminals(TerminalOwner::Session(session_id))
        .await
    {
        for terminal in terminals {
            if matches!(
                terminal.status,
                TerminalStatus::Starting | TerminalStatus::Running | TerminalStatus::Closing
            ) {
                if let Err(error) = runtime.close_terminal(terminal.id).await {
                    warn!(%error, terminal_id = %terminal.id, "close terminal during session cleanup");
                }
            }
        }
    }

    match runtime.current_runtime(session_id).await {
        Ok(Some(current)) => {
            if let Err(error) = runtime.stop_runtime(current.id).await {
                warn!(%error, runtime_id = %current.id, "stop runtime during session cleanup");
            }
        }
        Ok(None) => {}
        Err(error) => warn!(%error, %session_id, "lookup runtime during session cleanup"),
    }
}

async fn cleanup_project_terminals(state: &AppState, project_id: ProjectId) {
    let runtime = state.runtime();
    if let Ok(terminals) = runtime
        .list_terminals(TerminalOwner::Project(project_id))
        .await
    {
        for terminal in terminals {
            if matches!(
                terminal.status,
                TerminalStatus::Starting | TerminalStatus::Running | TerminalStatus::Closing
            ) {
                if let Err(error) = runtime.close_terminal(terminal.id).await {
                    warn!(%error, terminal_id = %terminal.id, "close project terminal during project delete");
                }
            }
        }
    }
}

/// Bounded graceful shutdown: stop every live Session Runtime we can see, then
/// return. The deadline is a hard wall-clock cap so a hung process group cannot
/// block process exit indefinitely.
pub async fn graceful_shutdown(state: &AppState, deadline: Duration) {
    info!(?deadline, "graceful shutdown: stopping live runtimes");
    let shutdown = async {
        // Enumerate ready/starting/stopping runtimes directly so we do not
        // depend on Session list completeness during teardown.
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, session_id FROM runtimes \
             WHERE status IN ('starting', 'ready', 'stopping')",
        )
        .fetch_all(state.database().pool())
        .await
        .unwrap_or_default();
        for (runtime_id, session_id) in rows {
            if let Ok(sid) = session_id.parse::<SessionId>() {
                cleanup_session_runtime(state, sid).await;
            } else if let Ok(rid) = runtime_id.parse::<crate::platform::id::RuntimeId>() {
                let _ = state.runtime().stop_runtime(rid).await;
            }
        }
    };
    if tokio::time::timeout(deadline, shutdown).await.is_err() {
        warn!(?deadline, "graceful shutdown deadline exceeded; exiting anyway");
    }
}
