use janus_infrastructure::{
    events::NewEvent,
    id::{CorrelationId, SessionId},
};
use janus_sessions::interface::{SessionSummary, SessionsError};
use janus_workspace::interface::{
    PropagationConflict, PropagationDirection, PropagationError, PropagationResult, WorkspaceError,
};
use serde_json::{Value, json};

use super::Application;

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkspacePropagationError {
    #[error(transparent)]
    Sessions(#[from] SessionsError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error("workspace propagation conflict")]
    Conflict(PropagationConflict),
    #[error("workspace propagation event failed: {0}")]
    Internal(#[from] anyhow::Error),
}

impl Application {
    pub(crate) async fn sync_session_workspace(
        &self,
        session_id: SessionId,
        actor: Value,
    ) -> Result<PropagationResult, WorkspacePropagationError> {
        self.propagate_session_workspace(session_id, PropagationDirection::Sync, actor)
            .await
    }

    pub(crate) async fn apply_session_workspace(
        &self,
        session_id: SessionId,
        actor: Value,
    ) -> Result<PropagationResult, WorkspacePropagationError> {
        self.propagate_session_workspace(session_id, PropagationDirection::Apply, actor)
            .await
    }

    async fn propagate_session_workspace(
        &self,
        session_id: SessionId,
        direction: PropagationDirection,
        actor: Value,
    ) -> Result<PropagationResult, WorkspacePropagationError> {
        let session = self.sessions().get_session(session_id).await?;
        ensure_session_idle(&session)?;

        let propagation = self
            .workspace()
            .propagate(session_id, direction, actor.clone())
            .await;
        match propagation {
            Ok(result) => {
                self.append_propagation_events(&session, direction, &actor, &result)
                    .await?;
                Ok(result)
            }
            Err(PropagationError::Conflict(conflict)) => {
                self.append_conflict_event(&session, &actor, &conflict)
                    .await?;
                Err(WorkspacePropagationError::Conflict(conflict))
            }
            Err(PropagationError::Workspace(error)) => Err(error.into()),
        }
    }

    async fn append_propagation_events(
        &self,
        session: &SessionSummary,
        direction: PropagationDirection,
        actor: &Value,
        result: &PropagationResult,
    ) -> Result<(), WorkspacePropagationError> {
        let correlation_id = CorrelationId::new().to_string();
        let mut work = self
            .unit_of_work()
            .begin()
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        work.append_event(NewEvent {
            event_type: "workspace.diff_changed".into(),
            actor: actor.clone(),
            resource: Some(json!({"kind": "session", "id": session.id})),
            correlation_id: correlation_id.clone(),
            causation_id: None,
            payload: json!({
                "session_id": session.id,
                "project_id": session.project_id,
                "direction": direction.as_str(),
                "changed_paths": result.changed_paths,
                "session_revision": result.session_revision,
                "main_revision": result.main_revision,
            }),
        })
        .await
        .map_err(WorkspacePropagationError::Internal)?;
        work.append_event(NewEvent {
            event_type: "session.revision_changed".into(),
            actor: actor.clone(),
            resource: Some(json!({"kind": "session", "id": session.id})),
            correlation_id,
            causation_id: None,
            payload: json!({
                "session_id": session.id,
                "project_id": session.project_id,
                "direction": direction.as_str(),
                "workspace_revision": result.session_revision,
                "main_revision": result.main_revision,
            }),
        })
        .await
        .map_err(WorkspacePropagationError::Internal)?;
        work.commit()
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        Ok(())
    }

    async fn append_conflict_event(
        &self,
        session: &SessionSummary,
        actor: &Value,
        conflict: &PropagationConflict,
    ) -> Result<(), WorkspacePropagationError> {
        let mut work = self
            .unit_of_work()
            .begin()
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        work.append_event(NewEvent {
            event_type: "workspace.diff_changed".into(),
            actor: actor.clone(),
            resource: Some(json!({"kind": "session", "id": session.id})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "session_id": session.id,
                "project_id": session.project_id,
                "direction": conflict.direction.as_str(),
                "conflict": conflict,
            }),
        })
        .await
        .map_err(WorkspacePropagationError::Internal)?;
        work.commit()
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        Ok(())
    }
}

fn ensure_session_idle(session: &SessionSummary) -> Result<(), SessionsError> {
    if session.state == "deleting" {
        return Err(SessionsError::SessionDeleting);
    }
    if session.active_turn_id.is_some() || session.state == "active" {
        return Err(SessionsError::ActiveTurnExists);
    }
    Ok(())
}
