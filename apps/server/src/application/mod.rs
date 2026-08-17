//! Cross-capability workflows live here. They have no persistent business state;
//! they record correlation/operation journals and call capability interfaces.
//!
//! The background worker leases durable `work_items` and dispatches them to the
//! owning capability's handler, so clone/delete/git operations survive HTTP
//! disconnects and process restarts.

use janus_execution::interface::ExecutionInterface;
use janus_infrastructure::{
    events::EventStore,
    id::{SessionId, TurnId},
    operations::OperationInterface,
    state_broadcaster::StateBroadcaster,
    unit_of_work::{UnitOfWork, UnitOfWorkTransaction},
};
use janus_models::interface::ModelsInterface;
use janus_notifications::interface::NotificationsInterface;
use janus_projects::interface::ProjectsInterface;
use janus_runtime::interface::RuntimeInterface;
use janus_sessions::interface::SessionsInterface;
use janus_source_control::SourceControlInterface;
use janus_workspace::interface::{WorkspaceHandle, WorkspaceInterface};
use serde_json::json;

pub(crate) mod async_tasks;
pub(crate) mod automation;
pub(crate) mod context;
pub(crate) mod execution;
pub mod lifecycle;
pub mod notification_worker;
pub(crate) mod operation_kinds;
pub(crate) mod project_terminal;
pub(crate) mod session_flow;
pub mod state_worker;
pub mod workers;

use execution::ExecutionCoordinator;

/// Cross-capability workflow interface owned by the server composition root.
///
/// Capability interfaces remain the only owners of durable business state. This
/// module owns the ordering, transaction scope, recovery, and scheduling rules
/// that necessarily combine more than one capability.
#[derive(Clone)]
pub struct Application {
    unit_of_work: UnitOfWork,
    operations: OperationInterface,
    workspace: WorkspaceInterface,
    models: ModelsInterface,
    projects: ProjectsInterface,
    source_control: SourceControlInterface,
    runtime: RuntimeInterface,
    sessions: SessionsInterface,
    execution: ExecutionInterface,
    execution_coordinator: ExecutionCoordinator,
    state_broadcaster: StateBroadcaster,
    events: EventStore,
    notifications: NotificationsInterface,
}

pub(crate) struct ApplicationDependencies {
    pub(crate) unit_of_work: UnitOfWork,
    pub(crate) operations: OperationInterface,
    pub(crate) workspace: WorkspaceInterface,
    pub(crate) models: ModelsInterface,
    pub(crate) projects: ProjectsInterface,
    pub(crate) source_control: SourceControlInterface,
    pub(crate) runtime: RuntimeInterface,
    pub(crate) sessions: SessionsInterface,
    pub(crate) execution: ExecutionInterface,
    pub(crate) execution_coordinator: ExecutionCoordinator,
    pub(crate) state_broadcaster: StateBroadcaster,
    pub(crate) events: EventStore,
    pub(crate) notifications: NotificationsInterface,
}

impl Application {
    pub(crate) fn new(dependencies: ApplicationDependencies) -> Self {
        Self {
            unit_of_work: dependencies.unit_of_work,
            operations: dependencies.operations,
            workspace: dependencies.workspace,
            models: dependencies.models,
            projects: dependencies.projects,
            source_control: dependencies.source_control,
            runtime: dependencies.runtime,
            sessions: dependencies.sessions,
            execution: dependencies.execution,
            execution_coordinator: dependencies.execution_coordinator,
            state_broadcaster: dependencies.state_broadcaster,
            events: dependencies.events,
            notifications: dependencies.notifications,
        }
    }

    pub(crate) fn unit_of_work(&self) -> &UnitOfWork {
        &self.unit_of_work
    }

    pub(crate) fn operations(&self) -> &OperationInterface {
        &self.operations
    }

    pub(crate) fn workspace(&self) -> &WorkspaceInterface {
        &self.workspace
    }

    pub(crate) async fn current_workspace_revision(
        &self,
        session_id: SessionId,
    ) -> Result<String, janus_sessions::interface::SessionsError> {
        let session = self.sessions.get_session(session_id).await?;
        Ok(self
            .workspace
            .current_revision(&WorkspaceHandle::main(session.project_id))
            .await?
            .0)
    }

    pub(crate) fn models(&self) -> &ModelsInterface {
        &self.models
    }

    pub(crate) fn projects(&self) -> &ProjectsInterface {
        &self.projects
    }

    pub(crate) fn source_control(&self) -> &SourceControlInterface {
        &self.source_control
    }

    pub(crate) fn runtime(&self) -> &RuntimeInterface {
        &self.runtime
    }

    pub(crate) fn sessions(&self) -> &SessionsInterface {
        &self.sessions
    }

    pub(crate) fn execution(&self) -> &ExecutionInterface {
        &self.execution
    }

    pub(crate) fn execution_coordinator(&self) -> &ExecutionCoordinator {
        &self.execution_coordinator
    }

    pub(crate) fn events(&self) -> &EventStore {
        &self.events
    }

    pub(crate) fn notifications(&self) -> &NotificationsInterface {
        &self.notifications
    }

    pub(crate) fn state_broadcaster(&self) -> &StateBroadcaster {
        &self.state_broadcaster
    }

    pub(crate) async fn enqueue_turn_wake_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        turn_id: TurnId,
    ) -> anyhow::Result<()> {
        self.operations
            .enqueue_work_in_tx(
                work,
                operation_kinds::KIND_TURN_WAKE,
                json!({"turn_id": turn_id.to_string()}),
            )
            .await
            .map(|_| ())
            .map_err(Into::into)
    }
}
