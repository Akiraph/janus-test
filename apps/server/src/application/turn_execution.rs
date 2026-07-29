use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use tracing::{debug, error, warn};

use crate::modules::models::interface::{ModelPreference, ModelsError, ModelsInterface};
use crate::modules::projects::interface::{ProjectsError, ProjectsInterface};
use crate::modules::runtime::interface::{JobProjection, RuntimeError, RuntimeInterface};
use crate::modules::sessions::interface::{
    SessionsError, SessionsInterface, TurnBlockers, TurnModelSnapshot, TurnStatus,
};
use crate::modules::supervisor::interface::{SupervisorError, SupervisorInterface, TurnWait};
use crate::platform::events::NewEvent;
use crate::platform::id::{CorrelationId, JobId, TurnId};
use crate::platform::unit_of_work::{UnitOfWork, UnitOfWorkTransaction};

#[derive(Debug, thiserror::Error)]
pub enum TurnExecutionError {
    #[error("session execution state failed: {0}")]
    Sessions(#[from] SessionsError),
    #[error("supervisor execution failed: {0}")]
    Supervisor(#[from] SupervisorError),
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
pub struct TurnRunner {
    models: ModelsInterface,
    projects: ProjectsInterface,
    sessions: SessionsInterface,
    supervisor: SupervisorInterface,
    runtime: RuntimeInterface,
    unit_of_work: UnitOfWork,
    active_turns: Arc<Mutex<HashSet<TurnId>>>,
}

impl TurnRunner {
    pub fn new(
        models: ModelsInterface,
        projects: ProjectsInterface,
        sessions: SessionsInterface,
        supervisor: SupervisorInterface,
        runtime: RuntimeInterface,
        unit_of_work: UnitOfWork,
    ) -> Self {
        Self {
            models,
            projects,
            sessions,
            supervisor,
            runtime,
            unit_of_work,
            active_turns: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn schedule(&self, turn_id: TurnId) {
        let Some(claim) = self.claim(turn_id) else {
            debug!(%turn_id, "Turn execution wake coalesced");
            return;
        };
        let runner = self.clone();
        tokio::spawn(async move {
            if let Err(error) = runner.run_claimed(claim).await {
                error!(%error, %turn_id, "Turn runner failed");
            }
        });
    }

    pub async fn run(&self, initial_turn_id: TurnId) -> Result<(), TurnExecutionError> {
        let Some(claim) = self.claim(initial_turn_id) else {
            debug!(%initial_turn_id, "Turn execution wake coalesced");
            return Ok(());
        };
        self.run_claimed(claim).await
    }

    async fn run_claimed(&self, mut claim: TurnClaim) -> Result<(), TurnExecutionError> {
        loop {
            let turn_id = claim.turn_id;
            let before = self.sessions.execution_turn(turn_id).await?;
            if before.status.is_active() {
                if !before.active {
                    return Ok(());
                }
                let execution = match self.supervisor.execute_turn(turn_id).await {
                    Ok(execution) => execution,
                    Err(execution_error) => {
                        warn!(%execution_error, %turn_id, "settling unexpected Supervisor failure");
                        self.supervisor
                            .settle_execution_failure(
                                turn_id,
                                "unexpected supervisor execution error",
                            )
                            .await?;
                        return Err(execution_error.into());
                    }
                };
                if let Some(wait) = execution.coordination {
                    let session_id = before.session_id;
                    if self.coordinate_wait(session_id, turn_id, wait).await? {
                        continue;
                    }
                    return Ok(());
                }
            }

            let after = self.sessions.execution_turn(turn_id).await?;
            let next_turn = if after.status.advances_queue() {
                self.activate_next_queued_after(turn_id, after.session_id)
                    .await?
            } else {
                None
            };
            let Some(next_turn) = next_turn else {
                return Ok(());
            };
            let Some(next_claim) = self.claim(next_turn) else {
                debug!(%next_turn, "promoted Turn already has an execution owner");
                return Ok(());
            };
            drop(claim);
            claim = next_claim;
        }
    }

    fn claim(&self, turn_id: TurnId) -> Option<TurnClaim> {
        let mut active = match self.active_turns.lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        active.insert(turn_id).then(|| TurnClaim {
            turn_id,
            active_turns: self.active_turns.clone(),
        })
    }

    pub(crate) async fn resolve_model_snapshot_in_tx(
        &self,
        tx: &mut sqlx::SqliteConnection,
        project_id: crate::platform::id::ProjectId,
        expected_owner_id: Option<&str>,
        next_model_ref: Option<&str>,
    ) -> Result<Option<TurnModelSnapshot>, TurnExecutionError> {
        let project = self.projects.model_preference_in_tx(tx, project_id).await?;
        if expected_owner_id.is_some_and(|owner_id| owner_id != project.owner_id) {
            return Err(ProjectsError::NotFound.into());
        }
        let configured = next_model_ref
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()
            .map_err(|_| TurnExecutionError::InvalidModelPreference)?;
        let (provider_id, upstream_model_id) = match configured.as_ref() {
            Some(configured) => {
                let provider_id = configured
                    .get("provider_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(TurnExecutionError::InvalidModelPreference)?;
                let upstream_model_id = configured
                    .get("upstream_model_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(TurnExecutionError::InvalidModelPreference)?;
                (Some(provider_id), Some(upstream_model_id))
            }
            None => (None, None),
        };
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
        Ok(model.map(|model| TurnModelSnapshot {
            model_id: model.model_id,
            provider_id: model.provider_id,
            display_name: model.display_name,
            upstream_model_id: model.upstream_model_id,
            context_limit: model.context_limit,
            supports_images: model.supports_images,
            supports_tools: model.supports_tools,
            parameters: model.parameters,
        }))
    }

    async fn activate_next_queued_after(
        &self,
        terminal_turn_id: TurnId,
        session_id: crate::platform::id::SessionId,
    ) -> Result<Option<TurnId>, TurnExecutionError> {
        let workspace_revision = self.sessions.current_workspace_revision(session_id).await?;
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
        let model_snapshot = self
            .resolve_model_snapshot_in_tx(
                work.connection(),
                candidate.project_id,
                None,
                candidate.next_model_ref.as_deref(),
            )
            .await?;
        let session_version = self
            .sessions
            .activate_queued_turn_in_tx(
                work.connection(),
                &candidate,
                model_snapshot.as_ref(),
                &workspace_revision,
                &now,
            )
            .await?;
        let Some(session_version) = session_version else {
            work.rollback().await?;
            return Ok(None);
        };
        work.append_event(NewEvent {
            event_type: "turn.status_changed".into(),
            actor: serde_json::json!({"kind": "supervisor"}),
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
                "model_id": model_snapshot.as_ref().map(|model| model.model_id.as_str()),
                "session_version": session_version,
            }),
        })
        .await?;
        work.commit().await?;
        Ok(Some(candidate.turn_id))
    }

    async fn coordinate_wait(
        &self,
        session_id: crate::platform::id::SessionId,
        turn_id: TurnId,
        wait: TurnWait,
    ) -> Result<bool, TurnExecutionError> {
        let now = self.sessions.now();
        let mut work = self.unit_of_work.begin().await?;
        let correlation_id = CorrelationId::new().to_string();
        let actor = serde_json::json!({"kind": "supervisor"});
        if !self
            .sessions
            .turn_is_runnable_in_tx(work.connection(), session_id, turn_id)
            .await?
        {
            work.rollback().await?;
            return Ok(false);
        }
        for ask in wait.asks() {
            if !self
                .supervisor
                .create_ask_in_tx(work.connection(), ask, &now)
                .await?
            {
                work.rollback().await?;
                return Ok(false);
            }
            work.append_event(NewEvent {
                event_type: "ask.changed".into(),
                actor: actor.clone(),
                resource: Some(serde_json::json!({
                    "kind": "ask",
                    "id": ask.id.to_string(),
                })),
                correlation_id: correlation_id.clone(),
                causation_id: None,
                payload: serde_json::json!({
                    "ask_id": ask.id.to_string(),
                    "turn_id": ask.turn_id.to_string(),
                    "tool_call_id": ask.tool_call_id.to_string(),
                    "mode": ask.mode.as_str(),
                    "status": "open",
                    "expires_at": ask.expires_at,
                }),
            })
            .await?;
        }

        let blockers = TurnBlockers {
            open_ask: wait.has_ask(),
            unfinished_job: wait.waits_for_job(),
        };
        let target_status = blockers.status();
        if target_status != TurnStatus::Running {
            let outcome = self
                .sessions
                .reconcile_turn_blockers_in_tx(work.connection(), turn_id, blockers, &now)
                .await?;
            let Some(transition) = outcome.transition else {
                work.rollback().await?;
                return Ok(false);
            };
            work.append_event(NewEvent {
                event_type: "turn.status_changed".into(),
                actor,
                resource: Some(serde_json::json!({
                    "kind": "turn",
                    "id": turn_id.to_string(),
                })),
                correlation_id,
                causation_id: None,
                payload: serde_json::json!({
                    "turn_id": turn_id.to_string(),
                    "from": transition.from_status.as_str(),
                    "to": transition.to_status.as_str(),
                    "session_version": transition.session_version,
                }),
            })
            .await?;
        }
        work.commit().await?;
        Ok(target_status == TurnStatus::Running)
    }

    pub async fn settle_job(&self, job_id: JobId) -> Result<Option<TurnId>, TurnExecutionError> {
        let job = self.runtime.job(job_id).await?;
        if !job.status.is_terminal() {
            return Ok(None);
        }
        let now = self.sessions.now();
        let mut work = self.unit_of_work.begin().await?;
        if !self.record_job_result_in_tx(&mut work, &job, &now).await? {
            work.rollback().await?;
            return Ok(None);
        }
        let open_ask = self
            .supervisor
            .has_open_asks_in_tx(work.connection(), job.controlling_turn_id)
            .await?;
        let unfinished_job = self
            .runtime
            .has_unfinished_jobs_in_tx(work.connection(), job.controlling_turn_id)
            .await?;
        let blocker_outcome = self
            .sessions
            .reconcile_turn_blockers_in_tx(
                work.connection(),
                job.controlling_turn_id,
                TurnBlockers {
                    open_ask,
                    unfinished_job,
                },
                &now,
            )
            .await?;
        let transition = blocker_outcome.transition;

        if let Some(transition) = transition.as_ref() {
            let correlation_id = CorrelationId::new().to_string();
            work.append_event(NewEvent {
                event_type: "turn.status_changed".into(),
                actor: serde_json::json!({"kind": "runtime"}),
                resource: Some(serde_json::json!({
                    "kind": "turn",
                    "id": job.controlling_turn_id.to_string(),
                })),
                correlation_id,
                causation_id: None,
                payload: serde_json::json!({
                    "turn_id": job.controlling_turn_id.to_string(),
                    "from": transition.from_status.as_str(),
                    "to": transition.to_status.as_str(),
                    "session_version": transition.session_version,
                    "settled_job_id": job.id.to_string(),
                }),
            })
            .await?;
        }
        let runnable_turn = transition
            .filter(|transition| transition.to_status == TurnStatus::Running)
            .map(|_| job.controlling_turn_id);
        work.commit().await?;
        Ok(runnable_turn)
    }

    pub(crate) async fn settle_terminal_jobs_for_turn_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        turn_id: TurnId,
        now: &str,
    ) -> Result<u64, TurnExecutionError> {
        let jobs = self
            .runtime
            .terminal_jobs_for_turn_in_tx(work.connection(), turn_id)
            .await?;
        let mut settled = 0;
        for job in jobs {
            if self.record_job_result_in_tx(work, &job, now).await? {
                settled += 1;
            }
        }
        Ok(settled)
    }

    async fn record_job_result_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        job: &JobProjection,
        now: &str,
    ) -> Result<bool, TurnExecutionError> {
        let Some(settlement) = self
            .supervisor
            .settle_job_tool_call_in_tx(work.connection(), job, now)
            .await?
        else {
            return Ok(false);
        };
        let source_turn_id: TurnId = settlement
            .source_turn_id
            .parse()
            .map_err(|_| SessionsError::Internal(anyhow::anyhow!("invalid source Turn id")))?;
        let timeline_item_id = self
            .sessions
            .replace_tool_result_in_tx(
                work.connection(),
                job.session_id,
                source_turn_id,
                &settlement.tool_call_id,
                &settlement.provider_call_id,
                &settlement.tool_name,
                settlement.status.as_str(),
                &settlement.summary,
                &settlement.model_parts,
                now,
            )
            .await?;
        let correlation_id = CorrelationId::new().to_string();
        let actor = serde_json::json!({"kind": "runtime"});
        work.append_event(NewEvent {
            event_type: "tool_call.changed".into(),
            actor: actor.clone(),
            resource: Some(serde_json::json!({
                "kind": "tool_call",
                "id": settlement.tool_call_id,
            })),
            correlation_id: correlation_id.clone(),
            causation_id: None,
            payload: serde_json::json!({
                "tool_call_id": settlement.tool_call_id,
                "provider_call_id": settlement.provider_call_id,
                "tool_name": settlement.tool_name,
                "status": settlement.status.as_str(),
                "summary": settlement.summary,
                "timeline_item_id": timeline_item_id,
                "job_id": job.id.to_string(),
            }),
        })
        .await?;
        work.append_event(NewEvent {
            event_type: "timeline.item_updated".into(),
            actor,
            resource: Some(serde_json::json!({
                "kind": "session",
                "id": job.session_id.to_string(),
            })),
            correlation_id,
            causation_id: None,
            payload: serde_json::json!({
                "timeline_item_id": timeline_item_id,
                "tool_call_id": job.initiated_by_tool_call_id.to_string(),
            }),
        })
        .await?;
        Ok(true)
    }

    pub async fn reconcile_waiting_jobs(&self) -> Result<u64, TurnExecutionError> {
        let mut resumed = 0;
        for job_id in self.supervisor.waiting_job_ids(100).await? {
            if self.settle_job(job_id).await?.is_some() {
                resumed += 1;
            }
        }
        Ok(resumed)
    }
}

struct TurnClaim {
    turn_id: TurnId,
    active_turns: Arc<Mutex<HashSet<TurnId>>>,
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
