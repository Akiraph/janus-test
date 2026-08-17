use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tracing::{debug, error, warn};

use janus_execution::interface::{ExecutionError, ExecutionInterface};
use janus_infrastructure::unit_of_work::{UnitOfWork, UnitOfWorkTransaction};
use janus_infrastructure::{
    events::{EventType, NewEvent},
    id::{CorrelationId, TurnId},
    operations::OperationInterface,
};
use janus_models::interface::{ModelPreference, ModelsError, ModelsInterface};
use janus_projects::interface::{ProjectsError, ProjectsInterface};
use janus_runtime::interface::RuntimeError;
use janus_sessions::interface::{
    ReasoningEffort, SessionModelPreference, SessionsError, SessionsInterface,
    TurnModelCandidateSnapshot, TurnModelSnapshot, TurnStatus,
};
use janus_workspace::interface::{WorkspaceHandle, WorkspaceInterface};

use super::operation_kinds::KIND_TURN_WAKE;

#[derive(Debug, thiserror::Error)]
pub(crate) enum TurnExecutionError {
    #[error("session execution state failed: {0}")]
    Sessions(#[from] SessionsError),
    #[error("execution module failed: {0}")]
    Execution(#[from] ExecutionError),
    #[error("runtime execution state failed: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("model configuration failed: {0}")]
    Models(#[from] ModelsError),
    #[error("project configuration failed: {0}")]
    Projects(#[from] ProjectsError),
    #[error("stored model preference is invalid")]
    InvalidModelPreference,
    #[error("execution transaction failed: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("execution event failed: {0}")]
    Event(#[from] anyhow::Error),
}

#[derive(Clone)]
pub(crate) struct ExecutionCoordinator {
    models: ModelsInterface,
    projects: ProjectsInterface,
    sessions: SessionsInterface,
    workspace: WorkspaceInterface,
    execution: ExecutionInterface,
    unit_of_work: UnitOfWork,
    operations: OperationInterface,
    active_turns: Arc<Mutex<HashMap<TurnId, bool>>>,
}

impl ExecutionCoordinator {
    pub(crate) fn new(
        models: ModelsInterface,
        projects: ProjectsInterface,
        sessions: SessionsInterface,
        workspace: WorkspaceInterface,
        execution: ExecutionInterface,
        unit_of_work: UnitOfWork,
        operations: OperationInterface,
    ) -> Self {
        Self {
            models,
            projects,
            sessions,
            workspace,
            execution,
            unit_of_work,
            operations,
            active_turns: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn schedule(&self, turn_id: TurnId) {
        let _ = self.schedule_and_wait(turn_id);
    }

    /// Schedule a Turn and return its runner handle when this call acquired a
    /// new in-memory claim. A `None` result means an active runner recorded the
    /// wake as pending and will consume it before releasing its claim.
    pub(crate) fn schedule_and_wait(
        &self,
        turn_id: TurnId,
    ) -> Option<tokio::task::JoinHandle<Result<(), TurnExecutionError>>> {
        let Some(claim) = self.claim(turn_id) else {
            debug!(%turn_id, "Turn execution wake recorded for active runner");
            return None;
        };
        let coordinator = self.clone();
        Some(tokio::spawn(async move {
            let result = coordinator.run_claimed(claim).await;
            if let Err(error) = &result {
                error!(%error, %turn_id, "Execution coordinator failed");
            }
            result
        }))
    }

    async fn run_claimed(&self, mut claim: TurnClaim) -> Result<(), TurnExecutionError> {
        loop {
            let pass = self.run_claimed_pass(&claim).await;
            match pass {
                Ok(Some(next_turn)) => {
                    if self.release_claim(&claim) {
                        continue;
                    }
                    let Some(next_claim) = self.claim(next_turn) else {
                        debug!(%next_turn, "promoted Turn already has an execution owner");
                        return Ok(());
                    };
                    claim = next_claim;
                }
                Ok(None) => {
                    if self.release_claim(&claim) {
                        continue;
                    }
                    return Ok(());
                }
                Err(error) => {
                    if self.release_claim(&claim) {
                        continue;
                    }
                    return Err(error);
                }
            }
        }
    }

    async fn run_claimed_pass(
        &self,
        claim: &TurnClaim,
    ) -> Result<Option<TurnId>, TurnExecutionError> {
        let turn_id = claim.turn_id;
        let before = match self.sessions.execution_turn(turn_id).await {
            Ok(state) => state,
            Err(SessionsError::NotFound) => {
                debug!(%turn_id, "dropping execution wake for deleted Turn");
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if before.status.is_active() {
            if !before.active {
                return Ok(None);
            }
            match self.execution.execute_turn(turn_id).await {
                Ok(_) => {}
                Err(execution_error) => {
                    // Preserve the provider or tool error as the Turn's
                    // completion reason so operators can act on the real cause.
                    let reason = execution_error.to_string();
                    warn!(%execution_error, %turn_id, "settling unexpected execution failure");
                    self.execution
                        .settle_execution_failure(turn_id, &reason)
                        .await?;
                    return Err(execution_error.into());
                }
            }
        }

        let after = match self.sessions.execution_turn(turn_id).await {
            Ok(state) => state,
            Err(SessionsError::NotFound) => {
                debug!(%turn_id, "Turn deleted while execution was settling");
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if !after.status.advances_queue() {
            return Ok(None);
        }
        self.activate_next_queued_after(turn_id, after.session_id)
            .await
    }

    fn claim(&self, turn_id: TurnId) -> Option<TurnClaim> {
        let mut active = match self.active_turns.lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(pending) = active.get_mut(&turn_id) {
            *pending = true;
            return None;
        }
        active.insert(turn_id, false);
        Some(TurnClaim {
            turn_id,
            active_turns: self.active_turns.clone(),
        })
    }

    /// Release a runner unless a wake arrived while its final state was being
    /// observed. Returning true retains the claim so the caller can run again.
    fn release_claim(&self, claim: &TurnClaim) -> bool {
        let mut active = match self.active_turns.lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(pending) = active.get_mut(&claim.turn_id) else {
            return false;
        };
        if *pending {
            *pending = false;
            true
        } else {
            active.remove(&claim.turn_id);
            false
        }
    }

    async fn enqueue_turn_wake_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        turn_id: TurnId,
    ) -> Result<(), TurnExecutionError> {
        self.operations
            .enqueue_work_in_tx(
                work,
                KIND_TURN_WAKE,
                serde_json::json!({"turn_id": turn_id.to_string()}),
            )
            .await
            .map(|_| ())
            .map_err(|error| {
                TurnExecutionError::Event(anyhow::anyhow!(
                    "enqueue Turn wake for {turn_id}: {error}"
                ))
            })
    }

    pub(crate) async fn resolve_model_snapshot_in_tx(
        &self,
        tx: &mut sqlx::SqliteConnection,
        project_id: janus_infrastructure::id::ProjectId,
        expected_owner_id: Option<&str>,
        preference: Option<&SessionModelPreference>,
    ) -> Result<Option<TurnModelSnapshot>, TurnExecutionError> {
        let project = self.projects.model_preference_in_tx(tx, project_id).await?;
        if expected_owner_id.is_some_and(|owner_id| owner_id != project.owner_id) {
            return Err(ProjectsError::NotFound.into());
        }
        let provider_id = preference.map(|value| value.provider_id.as_str());
        let upstream_model_id = preference.map(|value| value.upstream_model_id.as_str());
        let model = self
            .models
            .resolve_for_turn_in_tx(
                tx,
                &project.owner_id,
                ModelPreference {
                    model_id: project.default_model_id.as_deref(),
                    provider_id,
                    upstream_model_id,
                },
            )
            .await?;
        if preference.is_some() && model.is_none() {
            return Err(TurnExecutionError::InvalidModelPreference);
        }
        let Some(model) = model else {
            return Ok(None);
        };
        let primary_model_id = model.model_id.clone();
        let failover = self
            .models
            .failover_candidates_in_tx(tx, &project.owner_id, &primary_model_id)
            .await?;
        let apply_preference =
            |mut parameters: serde_json::Value| -> Result<serde_json::Value, TurnExecutionError> {
                let Some(preference) = preference else {
                    return Ok(parameters);
                };
                let map = parameters
                    .as_object_mut()
                    .ok_or(TurnExecutionError::InvalidModelPreference)?;
                if preference.reasoning_effort == ReasoningEffort::None {
                    map.remove("reasoning_effort");
                } else {
                    map.insert(
                        "reasoning_effort".into(),
                        serde_json::Value::String(preference.reasoning_effort.as_str().to_owned()),
                    );
                }
                Ok(parameters)
            };
        let parameters = apply_preference(model.parameters)?;
        let failover = failover
            .into_iter()
            .map(|candidate| {
                Ok(TurnModelCandidateSnapshot {
                    model_id: candidate.model_id,
                    provider_id: candidate.provider_id,
                    display_name: candidate.display_name,
                    upstream_model_id: candidate.upstream_model_id,
                    context_limit: candidate.context_limit,
                    supports_images: candidate.supports_images,
                    supports_tools: candidate.supports_tools,
                    parameters: apply_preference(candidate.parameters)?,
                })
            })
            .collect::<Result<Vec<_>, TurnExecutionError>>()?;
        Ok(Some(TurnModelSnapshot {
            model_id: model.model_id,
            provider_id: model.provider_id,
            display_name: model.display_name,
            upstream_model_id: model.upstream_model_id,
            context_limit: model.context_limit,
            supports_images: model.supports_images,
            supports_tools: model.supports_tools,
            parameters,
            failover,
        }))
    }

    async fn activate_next_queued_after(
        &self,
        terminal_turn_id: TurnId,
        session_id: janus_infrastructure::id::SessionId,
    ) -> Result<Option<TurnId>, TurnExecutionError> {
        let session = self.sessions.get_session(session_id).await?;
        let workspace_revision = self
            .workspace
            .current_revision(&WorkspaceHandle::main(session.project_id))
            .await
            .map_err(SessionsError::from)?
            .0;
        let now = self.sessions.now();
        let mut work = self.unit_of_work.begin().await?;
        let candidate = self
            .sessions
            .queued_turn_candidate_in_tx(work.connection(), terminal_turn_id, session_id)
            .await?;
        let Some(candidate) = candidate else {
            work.rollback().await?;
            return Ok(None);
        };
        let session_version = self
            .sessions
            .activate_queued_turn_in_tx(
                work.connection(),
                &candidate,
                candidate.model_snapshot.as_ref(),
                &workspace_revision,
                &now,
            )
            .await?;
        let Some(session_version) = session_version else {
            work.rollback().await?;
            return Ok(None);
        };
        work.append_event(NewEvent {
            event_type: EventType::TurnStatusChanged,
            actor: serde_json::json!({"kind": "execution"}),
            resource: Some(serde_json::json!({
                "kind": "turn",
                "id": candidate.turn_id.to_string(),
            })),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: serde_json::json!({
                "turn_id": candidate.turn_id.to_string(),
                "from": TurnStatus::Queued.as_str(),
                "to": TurnStatus::Running.as_str(),
                "route": "queued_start",
                "model_id": candidate
                    .model_snapshot
                    .as_ref()
                    .map(|model| model.model_id.as_str()),
                "session_version": session_version,
            }),
        })
        .await?;
        self.enqueue_turn_wake_in_tx(&mut work, candidate.turn_id)
            .await?;
        work.commit().await?;
        Ok(Some(candidate.turn_id))
    }
}

struct TurnClaim {
    turn_id: TurnId,
    active_turns: Arc<Mutex<HashMap<TurnId, bool>>>,
}

impl Drop for TurnClaim {
    fn drop(&mut self) {
        let mut active = match self.active_turns.lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        active.remove(&self.turn_id);
    }
}
