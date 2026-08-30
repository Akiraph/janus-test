//! Cross-module Session/Project deletion and bounded Runtime shutdown.
//!
//! Sessions and Projects declare no Runtime dependency in their architecture
//! manifests. Deletion that stops live async tasks or Terminals therefore
//! lives here: isolate the resource, stop Runtime-owned processes, then hand
//! durable row and workspace removal back to the owning capability.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::application::Application;
use crate::application::operation_kinds::{KIND_CREATE_SESSION, KIND_DELETE_SESSION};
use janus_execution::interface::ExecutionError;
use janus_infrastructure::{
    events::{EventType, NewEvent},
    id::{CorrelationId, ProjectId, SessionId},
    operations::{
        CreateOperation, CreateWork, IdempotencyRequest, OperationCompletion, OperationError,
        OperationInterface, OperationStatus, OperationView, StepState, WorkClaim,
    },
};
use janus_models::interface::ModelsError;
use janus_projects::interface::ProjectsError;
use janus_runtime::interface::{AsyncTaskStatus, RuntimeError, RuntimeScope, TerminalStatus};
use janus_sessions::interface::{SessionsError, TurnStatus};
use janus_workspace::interface::WorkspaceError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionLifecycleError {
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
    Execution(#[from] ExecutionError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
    #[error(transparent)]
    Storage(#[from] mongodb::error::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
    #[error("invalid lifecycle work item: {0}")]
    InvalidWork(String),
    #[error("operation step requires external-effect reconciliation: {0}")]
    StepNeedsReconciliation(String),
}

impl SessionLifecycleError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Projects(error) => error.code(),
            Self::Sessions(SessionsError::NotFound) => "SESSION_NOT_FOUND",
            Self::Sessions(SessionsError::VersionMismatch { .. }) => "RESOURCE_VERSION_MISMATCH",
            Self::Runtime(error) => error.code().as_str(),
            Self::Workspace(WorkspaceError::RevisionMismatch { .. }) => "RESOURCE_VERSION_MISMATCH",
            Self::InvalidWork(_) => "INVALID_WORK_ITEM",
            Self::StepNeedsReconciliation(_) => "OPERATION_STEP_NEEDS_RECONCILIATION",
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

pub(crate) async fn recover_execution_state(application: &Application) -> anyhow::Result<usize> {
    let now = application.sessions().now();
    let mut work = application.unit_of_work().begin().await?;
    application
        .runtime()
        .recover_uncertain_in_tx(&mut work, &now)
        .await?;
    application
        .models()
        .interrupt_running_attempts_in_tx(work.connection(), &now)
        .await?;
    application
        .execution()
        .interrupt_execution_in_tx(work.connection(), &now)
        .await?;
    let running_turns = application
        .sessions()
        .running_turn_ids_in_tx(work.connection())
        .await?;
    let wake_required = application
        .execution()
        .unstarted_active_turn_ids_in_tx(work.connection(), &running_turns)
        .await?;
    let recovered = application
        .sessions()
        .interrupt_active_turns_in_tx(work.connection(), &now, &wake_required)
        .await?;
    let correlation_id = CorrelationId::new().to_string();
    let actor = json!({"kind": "system", "reason": "control_plane_restart"});
    for turn in &recovered {
        if turn.wake_required {
            application
                .enqueue_turn_wake_in_tx(&mut work, turn.turn_id)
                .await?;
            continue;
        }
        work.append_event(NewEvent {
            event_type: EventType::TurnStatusChanged,
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
                event_type: EventType::SessionChanged,
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

pub(crate) async fn recover_workspace_mutations(
    application: &Application,
) -> anyhow::Result<usize> {
    let recovered = application
        .workspace()
        .recover_uncertain_file_mutations()
        .await?;
    let count = recovered.len();
    for mutation in recovered {
        let mut payload = mutation.event.payload;
        if mutation.event.event_type == EventType::ProjectMainRevisionChanged
            && let serde_json::Value::Object(ref mut object) = payload
        {
            object.insert(
                "main_revision".into(),
                serde_json::Value::String(mutation.revision.0.clone()),
            );
        }
        let mut work = application.unit_of_work().begin().await?;
        work.append_event(NewEvent {
            event_type: mutation.event.event_type,
            actor: mutation.event.actor,
            resource: Some(mutation.event.resource),
            correlation_id: mutation.event.correlation_id,
            causation_id: mutation.event.causation_id,
            payload,
        })
        .await?;
        application
            .workspace()
            .acknowledge_file_mutation_event_in_tx(
                work.connection(),
                &mutation.intent_id,
                &mutation.revision,
            )
            .await?;
        work.commit().await?;
    }
    Ok(count)
}

pub(crate) async fn request_session_creation(
    state: &OperationInterface,
    owner_id: &str,
    project_id: ProjectId,
    title: Option<String>,
    actor: Value,
    correlation_id: CorrelationId,
    idempotency: IdempotencyRequest,
) -> Result<OperationView, SessionLifecycleError> {
    let session_id = SessionId::new();
    let created = state
        .create(
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
            Some(CreateWork {
                handler_kind: KIND_CREATE_SESSION,
                payload: json!({
                    "session_id": session_id,
                    "project_id": project_id,
                    "owner_id": owner_id,
                    "title": title,
                    "actor": actor,
                }),
            }),
        )
        .await?;
    Ok(created.operation)
}

pub(crate) async fn request_session_deletion(
    state: &OperationInterface,
    session_id: SessionId,
    expected_version: String,
    actor: Value,
    correlation_id: CorrelationId,
    idempotency: IdempotencyRequest,
) -> Result<OperationView, SessionLifecycleError> {
    let created = state
        .create(
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
            Some(CreateWork {
                handler_kind: KIND_DELETE_SESSION,
                payload: json!({
                    "session_id": session_id,
                    "expected_version": expected_version,
                    "actor": actor,
                }),
            }),
        )
        .await?;
    Ok(created.operation)
}

pub(crate) async fn run_session_creation_operation(
    state: &Application,
    payload: &Value,
    work_id: &str,
    work_nonce: &str,
) -> Result<(), SessionLifecycleError> {
    let input: CreateSessionWork = serde_json::from_value(payload.clone())?;
    let Some((operation, correlation_id)) =
        active_operation(state.operations(), &input.operation_id).await?
    else {
        return Ok(());
    };
    let result = execute_session_creation(
        state,
        &input,
        correlation_id,
        WorkClaim {
            id: work_id,
            nonce: work_nonce,
        },
    )
    .await;
    finish_session_operation(
        state.operations(),
        &operation.id,
        correlation_id,
        result.map(|session_id| json!({"session_id": session_id})),
        work_id,
        work_nonce,
    )
    .await
}

pub(crate) async fn run_session_deletion_operation(
    state: &Application,
    payload: &Value,
    work_id: &str,
    work_nonce: &str,
) -> Result<(), SessionLifecycleError> {
    let input: DeleteSessionWork = serde_json::from_value(payload.clone())?;
    let Some((operation, correlation_id)) =
        active_operation(state.operations(), &input.operation_id).await?
    else {
        return Ok(());
    };
    let result = execute_session_deletion(
        state,
        &input,
        correlation_id,
        WorkClaim {
            id: work_id,
            nonce: work_nonce,
        },
    )
    .await;
    finish_session_operation(
        state.operations(),
        &operation.id,
        correlation_id,
        result.map(|session_id| json!({"session_id": session_id})),
        work_id,
        work_nonce,
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
    work_id: &str,
    work_nonce: &str,
) -> Result<(), SessionLifecycleError> {
    match result {
        Ok(result) => {
            let applied = operations
                .finish_claimed(
                    operation_id,
                    work_id,
                    work_nonce,
                    OperationCompletion {
                        status: OperationStatus::Succeeded,
                        result: Some(result),
                        problem: None,
                        correlation_id,
                    },
                )
                .await?;
            if !applied {
                warn!(%operation_id, %work_id, "stale worker could not finish Session Operation");
            }
        }
        Err(error) => {
            let status = if error.requires_attention() {
                OperationStatus::NeedsAttention
            } else {
                OperationStatus::Failed
            };
            let applied = operations
                .finish_claimed(
                    operation_id,
                    work_id,
                    work_nonce,
                    OperationCompletion {
                        status,
                        result: None,
                        problem: Some(json!({"code": error.code(), "detail": error.to_string()})),
                        correlation_id,
                    },
                )
                .await?;
            if !applied {
                warn!(%operation_id, %work_id, "stale worker could not fail Session Operation");
            }
        }
    }
    Ok(())
}

async fn execute_session_creation(
    state: &Application,
    input: &CreateSessionWork,
    correlation_id: CorrelationId,
    claim: WorkClaim<'_>,
) -> Result<SessionId, SessionLifecycleError> {
    let session_id: SessionId = input
        .session_id
        .parse()
        .map_err(|error| SessionLifecycleError::InvalidWork(format!("session id: {error}")))?;
    let project_id: ProjectId = input
        .project_id
        .parse()
        .map_err(|error| SessionLifecycleError::InvalidWork(format!("project id: {error}")))?;
    let validation_step = state
        .operations()
        .begin_step_claimed(
            claim,
            &input.operation_id,
            "validate_project",
            json!({"project_id": project_id}),
        )
        .await?;
    match validation_step {
        StepState::Running => {
            state.operations().assert_claimed(claim).await?;
            state
                .projects()
                .ensure_ready(&input.owner_id, project_id)
                .await?;
            state
                .operations()
                .complete_step_claimed(claim, &input.operation_id, "validate_project", None)
                .await?;
        }
        StepState::AlreadySucceeded => {}
        StepState::NeedsReconciliation => {
            return Err(SessionLifecycleError::StepNeedsReconciliation(
                "validate_project".into(),
            ));
        }
    }

    let record_step = state
        .operations()
        .begin_step_claimed(
            claim,
            &input.operation_id,
            "record_session",
            json!({"session_id": session_id}),
        )
        .await?;
    match record_step {
        StepState::NeedsReconciliation => {
            return Err(SessionLifecycleError::StepNeedsReconciliation(
                "record_session".into(),
            ));
        }
        StepState::AlreadySucceeded => {}
        StepState::Running => {
            state.operations().assert_claimed(claim).await?;
            let mut work = state.unit_of_work().begin().await?;
            let record = state
                .sessions()
                .create_session_in_tx(
                    work.connection(),
                    session_id,
                    project_id,
                    input.title.clone(),
                )
                .await?;
            if record.created {
                work.append_event(NewEvent {
                    event_type: EventType::SessionChanged,
                    actor: input.actor.clone(),
                    resource: Some(json!({"kind": "session", "id": session_id})),
                    correlation_id: correlation_id.to_string(),
                    causation_id: Some(input.operation_id.clone()),
                    payload: json!({
                        "session_id": session_id,
                        "project_id": project_id,
                        "state": "ready",
                        "version": record.version,
                    }),
                })
                .await?;
            }
            work.commit().await?;
            state
                .operations()
                .complete_step_claimed(claim, &input.operation_id, "record_session", None)
                .await?;
        }
    }
    Ok(session_id)
}

async fn execute_session_deletion(
    state: &Application,
    input: &DeleteSessionWork,
    correlation_id: CorrelationId,
    claim: WorkClaim<'_>,
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
        claim,
    )
    .await?;
    Ok(session_id)
}

async fn execute_session_deletion_steps(
    state: &Application,
    operation_id: &str,
    session_id: SessionId,
    expected_version: &str,
    actor: &Value,
    correlation_id: CorrelationId,
    claim: WorkClaim<'_>,
) -> Result<(), SessionLifecycleError> {
    let step_key = |name: &str| format!("session:{session_id}:{name}");
    let step_input = || json!({"session_id": session_id});

    run_operation_step(
        state.operations(),
        claim,
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
        claim,
        operation_id,
        &step_key("stop_processes"),
        step_input(),
        drain_session_processes(state, session_id),
    )
    .await?;
    run_operation_step(
        state.operations(),
        claim,
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
        claim,
        operation_id,
        &step_key("delete_records"),
        step_input(),
        delete_session_records(state, session_id, actor, correlation_id, Some(operation_id)),
    )
    .await
}

async fn run_operation_step(
    operations: &OperationInterface,
    claim: WorkClaim<'_>,
    operation_id: &str,
    step_key: &str,
    input_summary: Value,
    step: impl std::future::Future<Output = Result<(), SessionLifecycleError>>,
) -> Result<(), SessionLifecycleError> {
    match operations
        .begin_step_claimed(claim, operation_id, step_key, input_summary)
        .await?
    {
        StepState::AlreadySucceeded => {}
        StepState::NeedsReconciliation => {
            return Err(SessionLifecycleError::StepNeedsReconciliation(
                step_key.to_owned(),
            ));
        }
        StepState::Running => {
            operations.assert_claimed(claim).await?;
            step.await?;
            operations
                .complete_step_claimed(claim, operation_id, step_key, None)
                .await?;
        }
    }
    Ok(())
}

async fn mark_session_deleting(
    state: &Application,
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
            event_type: EventType::SessionChanged,
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
    state: &Application,
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
        .execution()
        .round_ids_for_turns_in_tx(work.connection(), &plan.turn_ids)
        .await?;
    state
        .runtime()
        .delete_session_resources_in_tx(work.connection(), session_id)
        .await?;
    state
        .execution()
        .delete_session_execution_in_tx(work.connection(), session_id, &plan.turn_ids)
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
        event_type: EventType::SessionDeleted,
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
    state
        .sessions()
        .drop_session_attachment_blobs(&plan.attachment_ids)
        .await;
    Ok(())
}

pub(crate) async fn delete_project_with_runtime(
    state: &Application,
    project_id: &str,
    operation_id: &str,
    work_id: &str,
    work_nonce: &str,
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
            WorkClaim {
                id: work_id,
                nonce: work_nonce,
            },
        )
        .await?;
    }
    let claim = WorkClaim {
        id: work_id,
        nonce: work_nonce,
    };
    run_operation_step(
        state.operations(),
        claim,
        operation_id,
        &format!("project:{project_id}:delete_terminals"),
        json!({"project_id": project_id}),
        cleanup_project_terminals(state, project),
    )
    .await?;
    run_operation_step(
        state.operations(),
        claim,
        operation_id,
        &format!("project:{project_id}:delete_runtime_logs"),
        json!({"project_id": project_id}),
        async {
            state.runtime().delete_project_log_files(project).await?;
            Ok(())
        },
    )
    .await?;
    run_operation_step(
        state.operations(),
        claim,
        operation_id,
        &format!("project:{project_id}:delete_runtime_resources"),
        json!({"project_id": project_id}),
        async {
            state.runtime().delete_project_resources(project).await?;
            Ok(())
        },
    )
    .await?;
    run_operation_step(
        state.operations(),
        claim,
        operation_id,
        &format!("project:{project_id}:delete_workspace"),
        json!({"project_id": project_id}),
        async {
            state.projects().run_delete(project_id).await?;
            Ok::<(), SessionLifecycleError>(())
        },
    )
    .await?;
    Ok(())
}

async fn drain_session_processes(
    state: &Application,
    session_id: SessionId,
) -> Result<(), SessionLifecycleError> {
    let runtime = state.runtime();
    for async_task in runtime
        .async_tasks(1000)
        .await?
        .into_iter()
        .filter(|async_task| async_task.session_id == session_id)
    {
        if matches!(
            async_task.status,
            AsyncTaskStatus::Queued | AsyncTaskStatus::Running
        ) {
            runtime.cancel_async_task(async_task.id).await?;
        }
    }
    Ok(())
}

async fn cleanup_project_terminals(
    state: &Application,
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
pub async fn graceful_shutdown(state: &Application, deadline: Duration) {
    info!(?deadline, "graceful shutdown: stopping live runtimes");
    let shutdown = async {
        match state.runtime().live_runtimes().await {
            Ok(runtimes) => {
                for runtime in runtimes {
                    let RuntimeScope::Project { project_id } = runtime.scope;
                    if let Err(error) = cleanup_project_terminals(state, project_id).await {
                        warn!(%error, %project_id, "drain Project Runtime during graceful shutdown");
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
