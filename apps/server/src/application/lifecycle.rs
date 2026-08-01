//! Cross-module Session/Project deletion and bounded Runtime shutdown.
//!
//! Sessions and Projects modules must not depend on Runtime (architecture
//! module.toml). Deletion that stops live Jobs/Services/Terminals therefore
//! lives here: isolate the resource, stop Runtime-owned processes, then hand
//! the durable row/workspace removal back to the owning module.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::AppState;
use crate::modules::models::interface::{ModelsError, ModelsInterface};
use crate::modules::projects::interface::ProjectsError;
use crate::modules::runtime::interface::{
    JobStatus, RuntimeError, RuntimeInterface, RuntimeScope, ServiceStatus, TerminalStatus,
};
use crate::modules::sessions::interface::{SessionsError, SessionsInterface, TurnStatus};
use crate::modules::supervisor::interface::{SupervisorError, SupervisorInterface};
use crate::modules::workspace_sync::interface::WorkspaceSyncError;
use crate::platform::{
    events::NewEvent,
    id::{CorrelationId, ProjectId, SessionId},
    operations::{
        CreateOperation, CreateWork, IdempotencyRequest, KIND_CREATE_SESSION, KIND_DELETE_SESSION,
        OperationError, OperationInterface, OperationStatus, OperationView, StepState,
    },
    unit_of_work::UnitOfWork,
};

#[derive(Debug, thiserror::Error)]
pub enum SessionLifecycleError {
    #[error(transparent)]
    Models(#[from] ModelsError),
    #[error(transparent)]
    Operation(#[from] OperationError),
    #[error(transparent)]
    Projects(#[from] ProjectsError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Sessions(#[from] SessionsError),
    #[error(transparent)]
    Supervisor(#[from] SupervisorError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceSyncError),
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
    #[error("invalid lifecycle work item: {0}")]
    InvalidWork(String),
}

impl SessionLifecycleError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Projects(error) => error.code(),
            Self::Sessions(SessionsError::NotFound) => "SESSION_NOT_FOUND",
            Self::Sessions(SessionsError::VersionMismatch { .. }) => "RESOURCE_VERSION_MISMATCH",
            Self::Runtime(error) => error.code().as_str(),
            Self::Workspace(WorkspaceSyncError::RevisionMismatch { .. }) => {
                "RESOURCE_VERSION_MISMATCH"
            }
            Self::InvalidWork(_) => "INVALID_WORK_ITEM",
            _ => "INTERNAL_ERROR",
        }
    }

    fn requires_attention(&self) -> bool {
        !matches!(
            self,
            Self::Projects(ProjectsError::NotFound | ProjectsError::Validation(_))
                | Self::Sessions(SessionsError::NotFound | SessionsError::VersionMismatch { .. })
        )
    }
}

#[derive(Debug, Deserialize)]
struct CreateSessionWork {
    operation_id: String,
    session_id: String,
    project_id: String,
    owner_id: String,
    title: Option<String>,
    actor: Value,
}

#[derive(Debug, Deserialize)]
struct DeleteSessionWork {
    operation_id: String,
    session_id: String,
    expected_version: String,
    actor: Value,
}

pub(crate) async fn recover_execution_state(
    unit_of_work: &UnitOfWork,
    models: &ModelsInterface,
    runtime: &RuntimeInterface,
    sessions: &SessionsInterface,
    supervisor: &SupervisorInterface,
) -> anyhow::Result<usize> {
    let now = sessions.now();
    let mut work = unit_of_work.begin().await?;
    runtime
        .recover_uncertain_in_tx(work.connection(), &now)
        .await?;
    models
        .interrupt_running_attempts_in_tx(work.connection(), &now)
        .await?;
    supervisor
        .interrupt_execution_in_tx(work.connection(), &now)
        .await?;
    let recovered = sessions
        .interrupt_active_turns_in_tx(work.connection(), &now)
        .await?;
    let correlation_id = CorrelationId::new().to_string();
    let actor = json!({"kind": "system", "reason": "control_plane_restart"});
    for turn in &recovered {
        work.append_event(NewEvent {
            event_type: "turn.status_changed".into(),
            actor: actor.clone(),
            resource: Some(json!({"kind": "turn", "id": turn.turn_id.to_string()})),
            correlation_id: correlation_id.clone(),
            causation_id: None,
            payload: json!({
                "turn_id": turn.turn_id.to_string(),
                "session_id": turn.session_id.to_string(),
                "from": turn.from_status.as_str(),
                "to": TurnStatus::Interrupted.as_str(),
                "reason": "control_plane_restart",
                "version": turn.turn_version,
            }),
        })
        .await?;
        if let Some(session_version) = &turn.session_version {
            work.append_event(NewEvent {
                event_type: "session.changed".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "session", "id": turn.session_id.to_string()})),
                correlation_id: correlation_id.clone(),
                causation_id: None,
                payload: json!({
                    "session_id": turn.session_id.to_string(),
                    "state": "ready",
                    "active_turn_id": null,
                    "version": session_version,
                }),
            })
            .await?;
        }
    }
    work.commit().await?;
    info!(
        recovered_turns = recovered.len(),
        "execution recovery committed"
    );
    Ok(recovered.len())
}

pub async fn request_session_creation(
    state: &AppState,
    owner_id: &str,
    project_id: ProjectId,
    title: Option<String>,
    actor: Value,
    correlation_id: CorrelationId,
    idempotency: IdempotencyRequest,
) -> Result<OperationView, SessionLifecycleError> {
    let session_id = SessionId::new();
    let created = state
        .operations()
        .create_with_work(
            CreateOperation {
                kind: KIND_CREATE_SESSION,
                actor: actor.clone(),
                target_kind: "session",
                target_id: Some(&session_id.to_string()),
                conditions: json!({
                    "session_id": session_id,
                    "project_id": project_id,
                    "title": title,
                }),
                correlation_id,
                idempotency: Some(idempotency),
            },
            CreateWork {
                handler_kind: KIND_CREATE_SESSION,
                payload: json!({
                    "session_id": session_id,
                    "project_id": project_id,
                    "owner_id": owner_id,
                    "title": title,
                    "actor": actor,
                }),
            },
        )
        .await?;
    Ok(created.operation)
}

pub async fn request_session_deletion(
    state: &AppState,
    session_id: SessionId,
    expected_version: String,
    actor: Value,
    correlation_id: CorrelationId,
    idempotency: IdempotencyRequest,
) -> Result<OperationView, SessionLifecycleError> {
    let created = state
        .operations()
        .create_with_work(
            CreateOperation {
                kind: KIND_DELETE_SESSION,
                actor: actor.clone(),
                target_kind: "session",
                target_id: Some(&session_id.to_string()),
                conditions: json!({
                    "session_id": session_id,
                    "expected_version": expected_version,
                }),
                correlation_id,
                idempotency: Some(idempotency),
            },
            CreateWork {
                handler_kind: KIND_DELETE_SESSION,
                payload: json!({
                    "session_id": session_id,
                    "expected_version": expected_version,
                    "actor": actor,
                }),
            },
        )
        .await?;
    Ok(created.operation)
}

pub(crate) async fn run_session_creation_operation(
    state: &AppState,
    payload: &Value,
) -> Result<(), SessionLifecycleError> {
    let input: CreateSessionWork = serde_json::from_value(payload.clone())?;
    let Some((operation, correlation_id)) =
        active_operation(state.operations(), &input.operation_id).await?
    else {
        return Ok(());
    };
    let result = execute_session_creation(state, &input, correlation_id).await;
    finish_session_operation(
        state.operations(),
        &operation.id,
        correlation_id,
        result.map(|session_id| json!({"session_id": session_id})),
    )
    .await
}

pub(crate) async fn run_session_deletion_operation(
    state: &AppState,
    payload: &Value,
) -> Result<(), SessionLifecycleError> {
    let input: DeleteSessionWork = serde_json::from_value(payload.clone())?;
    let Some((operation, correlation_id)) =
        active_operation(state.operations(), &input.operation_id).await?
    else {
        return Ok(());
    };
    let result = execute_session_deletion(state, &input, correlation_id).await;
    finish_session_operation(
        state.operations(),
        &operation.id,
        correlation_id,
        result.map(|session_id| json!({"session_id": session_id})),
    )
    .await
}

async fn active_operation(
    operations: &OperationInterface,
    operation_id: &str,
) -> Result<Option<(OperationView, CorrelationId)>, SessionLifecycleError> {
    let operation = operations
        .get(operation_id)
        .await?
        .ok_or_else(|| SessionLifecycleError::InvalidWork("operation not found".into()))?;
    if matches!(
        operation.status.as_str(),
        "succeeded" | "failed" | "canceled" | "needs_attention"
    ) {
        return Ok(None);
    }
    let correlation_id = operation.correlation_id.parse().map_err(|error| {
        SessionLifecycleError::InvalidWork(format!("invalid correlation id: {error}"))
    })?;
    Ok(Some((operation, correlation_id)))
}

async fn finish_session_operation(
    operations: &OperationInterface,
    operation_id: &str,
    correlation_id: CorrelationId,
    result: Result<Value, SessionLifecycleError>,
) -> Result<(), SessionLifecycleError> {
    match result {
        Ok(result) => {
            operations
                .finish(
                    operation_id,
                    OperationStatus::Succeeded,
                    Some(result),
                    None,
                    correlation_id,
                )
                .await?;
        }
        Err(error) => {
            let status = if error.requires_attention() {
                OperationStatus::NeedsAttention
            } else {
                OperationStatus::Failed
            };
            operations
                .finish(
                    operation_id,
                    status,
                    None,
                    Some(json!({"code": error.code(), "detail": error.to_string()})),
                    correlation_id,
                )
                .await?;
        }
    }
    Ok(())
}

async fn execute_session_creation(
    state: &AppState,
    input: &CreateSessionWork,
    correlation_id: CorrelationId,
) -> Result<SessionId, SessionLifecycleError> {
    let session_id: SessionId = input
        .session_id
        .parse()
        .map_err(|error| SessionLifecycleError::InvalidWork(format!("session id: {error}")))?;
    let project_id: ProjectId = input
        .project_id
        .parse()
        .map_err(|error| SessionLifecycleError::InvalidWork(format!("project id: {error}")))?;
    if matches!(
        state
            .operations()
            .begin_step(
                &input.operation_id,
                "validate_project",
                json!({"project_id": project_id}),
            )
            .await?,
        StepState::Running
    ) {
        state
            .projects()
            .ensure_ready(&input.owner_id, project_id)
            .await?;
        state
            .operations()
            .complete_step(&input.operation_id, "validate_project", None)
            .await?;
    }

    let workspace_step = state
        .operations()
        .begin_step(
            &input.operation_id,
            "create_workspace",
            json!({"session_id": session_id, "project_id": project_id}),
        )
        .await?;
    let copy = state
        .workspace_sync()
        .ensure_session_copy(
            project_id,
            session_id,
            None,
            input.actor.clone(),
        )
        .await?;
    if matches!(workspace_step, StepState::Running) {
        state
            .operations()
            .complete_step(
                &input.operation_id,
                "create_workspace",
                Some(&copy.revision.0),
            )
            .await?;
    }

    if matches!(
        state
            .operations()
            .begin_step(
                &input.operation_id,
                "record_session",
                json!({"session_id": session_id}),
            )
            .await?,
        StepState::Running
    ) {
        let mut work = state.unit_of_work().begin().await?;
        let record = state
            .sessions()
            .create_session_in_tx(
                work.connection(),
                session_id,
                project_id,
                input.title.clone(),
                &copy.handle,
                &copy.source_main_revision.0,
            )
            .await?;
        if record.created {
            work.append_event(NewEvent {
                event_type: "session.changed".into(),
                actor: input.actor.clone(),
                resource: Some(json!({"kind": "session", "id": session_id})),
                correlation_id: correlation_id.to_string(),
                causation_id: Some(input.operation_id.clone()),
                payload: json!({
                    "session_id": session_id,
                    "project_id": project_id,
                    "state": "ready",
                    "version": record.version,
                    "workspace_revision": copy.revision.0,
                }),
            })
            .await?;
        }
        work.commit().await?;
        state
            .operations()
            .complete_step(&input.operation_id, "record_session", None)
            .await?;
    }
    Ok(session_id)
}

async fn execute_session_deletion(
    state: &AppState,
    input: &DeleteSessionWork,
    correlation_id: CorrelationId,
) -> Result<SessionId, SessionLifecycleError> {
    let session_id: SessionId = input
        .session_id
        .parse()
        .map_err(|error| SessionLifecycleError::InvalidWork(format!("session id: {error}")))?;
    execute_session_deletion_steps(
        state,
        &input.operation_id,
        session_id,
        &input.expected_version,
        &input.actor,
        correlation_id,
    )
    .await?;
    Ok(session_id)
}

async fn execute_session_deletion_steps(
    state: &AppState,
    operation_id: &str,
    session_id: SessionId,
    expected_version: &str,
    actor: &Value,
    correlation_id: CorrelationId,
) -> Result<(), SessionLifecycleError> {
    let step_key = |name: &str| format!("session:{session_id}:{name}");
    let step_input = || json!({"session_id": session_id});

    run_operation_step(
        state.operations(),
        operation_id,
        &step_key("mark_deleting"),
        step_input(),
        mark_session_deleting(
            state,
            session_id,
            expected_version,
            actor,
            correlation_id,
            Some(operation_id),
        ),
    )
    .await?;
    run_operation_step(
        state.operations(),
        operation_id,
        &step_key("stop_runtime"),
        step_input(),
        drain_session_runtime(state, session_id),
    )
    .await?;
    run_operation_step(
        state.operations(),
        operation_id,
        &step_key("delete_runtime_logs"),
        step_input(),
        async {
            state.runtime().delete_session_log_files(session_id).await?;
            Ok(())
        },
    )
    .await?;
    run_operation_step(
        state.operations(),
        operation_id,
        &step_key("delete_workspace"),
        step_input(),
        async {
            state
                .workspace_sync()
                .delete_session_copy(session_id)
                .await?;
            Ok(())
        },
    )
    .await?;
    run_operation_step(
        state.operations(),
        operation_id,
        &step_key("delete_records"),
        step_input(),
        delete_session_records(state, session_id, actor, correlation_id, Some(operation_id)),
    )
    .await
}

async fn run_operation_step(
    operations: &OperationInterface,
    operation_id: &str,
    step_key: &str,
    input_summary: Value,
    step: impl std::future::Future<Output = Result<(), SessionLifecycleError>>,
) -> Result<(), SessionLifecycleError> {
    if matches!(
        operations
            .begin_step(operation_id, step_key, input_summary)
            .await?,
        StepState::Running
    ) {
        step.await?;
        operations
            .complete_step(operation_id, step_key, None)
            .await?;
    }
    Ok(())
}

async fn mark_session_deleting(
    state: &AppState,
    session_id: SessionId,
    expected_version: &str,
    actor: &Value,
    correlation_id: CorrelationId,
    causation_id: Option<&str>,
) -> Result<(), SessionLifecycleError> {
    let mut work = state.unit_of_work().begin().await?;
    let deleting = state
        .sessions()
        .mark_session_deleting_in_tx(work.connection(), session_id, expected_version)
        .await?;
    if deleting.changed {
        work.append_event(NewEvent {
            event_type: "session.changed".into(),
            actor: actor.clone(),
            resource: Some(json!({"kind": "session", "id": session_id})),
            correlation_id: correlation_id.to_string(),
            causation_id: causation_id.map(str::to_owned),
            payload: json!({
                "session_id": session_id,
                "project_id": deleting.project_id,
                "state": "deleting",
                "version": deleting.version,
            }),
        })
        .await?;
    }
    work.commit().await?;
    Ok(())
}

async fn delete_session_records(
    state: &AppState,
    session_id: SessionId,
    actor: &Value,
    correlation_id: CorrelationId,
    causation_id: Option<&str>,
) -> Result<(), SessionLifecycleError> {
    let mut work = state.unit_of_work().begin().await?;
    let Some(plan) = state
        .sessions()
        .session_deletion_plan_in_tx(work.connection(), session_id)
        .await?
    else {
        work.commit().await?;
        return Ok(());
    };
    let round_ids = state
        .supervisor()
        .round_ids_for_turns_in_tx(work.connection(), &plan.turn_ids)
        .await?;
    let attempt_ids = state
        .models()
        .attempt_ids_for_rounds_in_tx(work.connection(), &round_ids)
        .await?;
    state
        .runtime()
        .delete_session_resources_in_tx(work.connection(), session_id)
        .await?;
    state
        .supervisor()
        .delete_session_execution_in_tx(work.connection(), session_id, &plan.turn_ids, &attempt_ids)
        .await?;
    state
        .models()
        .delete_attempts_for_rounds_in_tx(work.connection(), &round_ids)
        .await?;
    if !state
        .sessions()
        .delete_session_in_tx(work.connection(), session_id)
        .await?
    {
        return Err(SessionLifecycleError::InvalidWork(format!(
            "session {session_id} is not marked deleting"
        )));
    }
    work.append_event(NewEvent {
        event_type: "session.deleted".into(),
        actor: actor.clone(),
        resource: Some(json!({"kind": "session", "id": session_id})),
        correlation_id: correlation_id.to_string(),
        causation_id: causation_id.map(str::to_owned),
        payload: json!({
            "session_id": session_id,
            "project_id": plan.project_id,
            "version": plan.version,
        }),
    })
    .await?;
    work.commit().await?;
    Ok(())
}

pub async fn delete_project_with_runtime(
    state: &AppState,
    project_id: &str,
    operation_id: &str,
) -> Result<(), SessionLifecycleError> {
    let project: ProjectId = project_id
        .parse()
        .map_err(|error| SessionLifecycleError::InvalidWork(format!("project id: {error}")))?;
    let operation = state
        .operations()
        .get(operation_id)
        .await?
        .ok_or_else(|| SessionLifecycleError::InvalidWork("operation not found".into()))?;
    let correlation_id = operation.correlation_id.parse().map_err(|error| {
        SessionLifecycleError::InvalidWork(format!("invalid correlation id: {error}"))
    })?;
    let session_ids = state.sessions().project_session_ids(project).await?;
    for session_id in session_ids {
        let session = state.sessions().get_session(session_id).await?;
        let actor = json!({"kind": "system", "reason": "project_delete"});
        execute_session_deletion_steps(
            state,
            operation_id,
            session_id,
            &session.version,
            &actor,
            correlation_id,
        )
        .await?;
    }
    cleanup_project_terminals(state, project).await?;
    state.runtime().delete_project_log_files(project).await?;
    state.runtime().delete_project_resources(project).await?;
    state.projects().run_delete(project_id).await?;
    Ok(())
}

async fn drain_session_runtime(
    state: &AppState,
    session_id: SessionId,
) -> Result<(), SessionLifecycleError> {
    let runtime = state.runtime();
    for job in runtime.jobs(session_id).await? {
        if matches!(job.status, JobStatus::Queued | JobStatus::Running) {
            runtime.cancel_job(job.id).await?;
        }
    }
    for service in runtime.services(session_id).await? {
        if matches!(
            service.status,
            ServiceStatus::Starting | ServiceStatus::Running | ServiceStatus::Unhealthy
        ) {
            runtime.stop_service(service.id).await?;
        }
    }
    if let Some(current) = runtime
        .current_runtime(RuntimeScope::session(session_id))
        .await?
    {
        runtime.stop_runtime(current.id).await?;
    }
    Ok(())
}

/// Best-effort Runtime drain used only during bounded process shutdown.
pub async fn cleanup_session_runtime(state: &AppState, session_id: SessionId) {
    if let Err(error) = drain_session_runtime(state, session_id).await {
        warn!(%error, %session_id, "drain Runtime during graceful shutdown");
    }
}

async fn cleanup_project_terminals(
    state: &AppState,
    project_id: ProjectId,
) -> Result<(), SessionLifecycleError> {
    for terminal in state.runtime().list_terminals(project_id).await? {
        if matches!(
            terminal.status,
            TerminalStatus::Starting | TerminalStatus::Running
        ) {
            state.runtime().close_terminal(terminal.id).await?;
        }
    }
    if let Some(current) = state
        .runtime()
        .current_runtime(RuntimeScope::project(project_id))
        .await?
    {
        state.runtime().stop_runtime(current.id).await?;
    }
    Ok(())
}

/// Bounded graceful shutdown: stop every live Runtime we can see, then
/// return. The deadline is a hard wall-clock cap so a hung process group cannot
/// block process exit indefinitely.
pub async fn graceful_shutdown(state: &AppState, deadline: Duration) {
    info!(?deadline, "graceful shutdown: stopping live runtimes");
    let shutdown = async {
        match state.runtime().live_runtimes().await {
            Ok(runtimes) => {
                for runtime in runtimes {
                    match runtime.scope {
                        RuntimeScope::Session { session_id } => {
                            cleanup_session_runtime(state, session_id).await;
                        }
                        RuntimeScope::Project { project_id } => {
                            if let Err(error) = cleanup_project_terminals(state, project_id).await {
                                warn!(%error, %project_id, "drain Project Runtime during graceful shutdown");
                            }
                        }
                    }
                }
            }
            Err(error) => {
                warn!(%error, "enumerate live runtimes during graceful shutdown");
            }
        }
    };
    if tokio::time::timeout(deadline, shutdown).await.is_err() {
        warn!(
            ?deadline,
            "graceful shutdown deadline exceeded; exiting anyway"
        );
    }
}
