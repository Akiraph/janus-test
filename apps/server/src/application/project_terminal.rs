use crate::AppState;
use crate::modules::projects::interface::ProjectsError;
use crate::modules::runtime::interface::{
    ExecutionEnvironment, ExecutorKind, NetworkPolicy, RelativeWorkingDirectory, ResourceLimits,
    RuntimeError, RuntimeScope, RuntimeSpec, RuntimeStatus, TerminalProjection, TerminalSize,
    TerminalSpec,
};
use crate::platform::id::{ProjectId, RuntimeId, TerminalId};

#[derive(Debug, thiserror::Error)]
pub enum ProjectTerminalError {
    #[error(transparent)]
    Projects(#[from] ProjectsError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

impl AppState {
    pub async fn create_project_terminal(
        &self,
        owner_id: &str,
        project_id: ProjectId,
        working_directory: RelativeWorkingDirectory,
        environment: ExecutionEnvironment,
        size: TerminalSize,
    ) -> Result<TerminalProjection, ProjectTerminalError> {
        let workspace_root = self
            .projects()
            .main_workspace_root(owner_id, project_id)
            .await?;
        let scope = RuntimeScope::project(project_id);
        let runtime = match self.runtime().current_runtime(scope).await? {
            Some(runtime) if runtime.status == RuntimeStatus::Ready => runtime,
            Some(_) => return Err(RuntimeError::RuntimeUnavailable.into()),
            None => {
                let config = self
                    .projects()
                    .runtime_config(owner_id, &project_id.to_string())
                    .await?;
                let (executor, limits, network_policy) = config.map_or_else(
                    || {
                        (
                            ExecutorKind::Local,
                            ResourceLimits {
                                timeout_ms: 30_000,
                                memory_bytes: 256 * 1024 * 1024,
                                cpu_millis: 1_000,
                                pids: 64,
                                temporary_disk_bytes: 128 * 1024 * 1024,
                                open_files: 128,
                            },
                            NetworkPolicy::DenyAll,
                        )
                    },
                    |config| {
                        (
                            config.executor,
                            config.default_limits,
                            config.network_policy,
                        )
                    },
                );
                let spec = RuntimeSpec::new(
                    RuntimeId::new(),
                    scope,
                    executor,
                    workspace_root,
                    limits,
                    network_policy,
                )?;
                self.runtime().ensure_runtime(&spec).await?
            }
        };
        self.runtime()
            .create_terminal(TerminalSpec {
                id: TerminalId::new(),
                runtime_id: runtime.id,
                project_id,
                working_directory,
                environment,
                size,
            })
            .await
            .map_err(Into::into)
    }
}
