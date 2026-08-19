//! Cross-module Turn coordination.
//!
//! Lives in `application` because it must update more than one module's owned
//! tables in one SQLite transaction. Sessions owns turns/sessions/checkpoints;
//! runtime owns global async tasks. This module opens one transaction
//! against the shared pool and drives each owner's `*_in_tx` primitive inside
//! it, then commits and schedules further work only after commit.
//!
//! No persistent business state lives here: only orchestration and correlation.

use serde_json::json;

use crate::application::Application;
use crate::application::execution::TurnExecutionError;
use janus_execution::interface::ContextUsageView;
use janus_infrastructure::{
    clock::now_utc_str,
    command_idempotency,
    events::{EventType, NewEvent},
    id::{AttachmentId, CorrelationId, ProjectId, SessionId, TurnId},
    operations::IdempotencyRequest,
    unit_of_work::UnitOfWorkTransaction,
};
use janus_runtime::interface::AsyncTaskStatus;
use janus_sessions::interface::{
    CancelResult, CreateTurnInput, MessageRoute, MessageRouteResult, SessionModelPreference,
    SessionsError, TurnStatus, TurnSummary,
};

struct SessionInput<'a> {
    owner_id: &'a str,
    session_id: SessionId,
    content: &'a str,
    expected_version: &'a str,
    actor: serde_json::Value,
    goal_mode: bool,
    model_preference: Option<Option<&'a SessionModelPreference>>,
    attachment_ids: &'a [AttachmentId],
    workspace_revision: &'a str,
    now: &'a str,
}

pub(crate) struct PostSessionMessage<'a> {
    pub(crate) owner_id: &'a str,
    pub(crate) session_id: SessionId,
    pub(crate) content: &'a str,
    pub(crate) expected_version: &'a str,
    pub(crate) model_preference: Option<Option<&'a SessionModelPreference>>,
    pub(crate) attachment_ids: &'a [AttachmentId],
    pub(crate) actor: serde_json::Value,
    pub(crate) goal_mode: bool,
    pub(crate) idempotency: Option<IdempotencyRequest>,
}

impl Application {
    pub(crate) async fn post_session_message(
        &self,
        input: PostSessionMessage<'_>,
    ) -> Result<MessageRouteResult, SessionsError> {
        let PostSessionMessage {
            owner_id,
            session_id,
            content,
            expected_version,
            model_preference,
            attachment_ids,
            actor,
            goal_mode,
            idempotency,
        } = input;
        let workspace_revision = self.current_workspace_revision(session_id).await?;
        let now = now_utc_str();
        let mut work = self.unit_of_work().begin().await?;
        if let Some(request) = idempotency.as_ref()
            && let Some(response) = command_idempotency::lookup_in_tx(work.connection(), request)
                .await
                .map_err(SessionsError::Internal)?
        {
            return serde_json::from_value(response).map_err(SessionsError::Serde);
        }
        let result = self
            .route_session_input_in_tx(
                &mut work,
                SessionInput {
                    owner_id,
                    session_id,
                    content,
                    expected_version,
                    actor,
                    goal_mode,
                    model_preference,
                    attachment_ids,
                    workspace_revision: &workspace_revision,
                    now: &now,
                },
            )
            .await?;
        let scheduled_turn = (result.route == "started")
            .then(|| {
                result
                    .turn_id
                    .parse::<TurnId>()
                    .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))
            })
            .transpose()?;
        if let Some(turn_id) = scheduled_turn {
            self.enqueue_turn_wake_in_tx(&mut work, turn_id)
                .await
                .map_err(SessionsError::Internal)?;
        }
        if let Some(request) = idempotency.as_ref() {
            let response = serde_json::to_value(&result)?;
            command_idempotency::record_in_tx(work.connection(), request, &response)
                .await
                .map_err(SessionsError::Internal)?;
        }
        work.commit().await?;
        if let Some(turn_id) = scheduled_turn {
            self.execution_coordinator().schedule(turn_id);
        }
        Ok(result)
    }

    pub(crate) async fn session_context_usage(
        &self,
        session_id: SessionId,
    ) -> Result<Option<ContextUsageView>, SessionsError> {
        self.sessions().get_session(session_id).await?;
        self.execution()
            .latest_context_usage(session_id)
            .await
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))
    }

    pub(crate) async fn turn_summary(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<TurnSummary, SessionsError> {
        let mut data = self.sessions().get_turn(session_id, turn_id).await?;
        data.model_attempt = self
            .execution()
            .latest_model_attempt_for_turn(turn_id)
            .await
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
        data.token_exchange = Some(
            self.execution()
                .turn_token_exchange(turn_id)
                .await
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?,
        );
        Ok(data)
    }

    async fn route_session_input_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        input: SessionInput<'_>,
    ) -> Result<MessageRouteResult, SessionsError> {
        let SessionInput {
            owner_id,
            session_id,
            content,
            expected_version,
            actor,
            goal_mode,
            model_preference,
            attachment_ids,
            workspace_revision,
            now,
        } = input;
        let command = self
            .sessions()
            .lock_session_command_in_tx(
                work.connection(),
                session_id,
                expected_version,
                model_preference,
                now,
            )
            .await?;
        let has_queued = self
            .sessions()
            .has_queued_turn_in_tx(work.connection(), session_id)
            .await?;
        let active_status = match command.active_turn_id.as_deref() {
            Some(turn_id) => {
                self.sessions()
                    .turn_status_in_tx(work.connection(), turn_id)
                    .await?
            }
            None => None,
        };
        let route = match (command.active_turn_id.as_deref(), active_status, has_queued) {
            (Some(_), _, _) | (None, _, true) => MessageRoute::Queued,
            (None, _, false) => MessageRoute::Started,
        };
        let checkpoint_revision = (route == MessageRoute::Started).then_some(workspace_revision);
        let project_id = command.project_id.parse::<ProjectId>().map_err(|error| {
            SessionsError::Internal(anyhow::anyhow!("invalid Project id: {error}"))
        })?;
        let preference = command
            .next_model_ref
            .as_deref()
            .map(serde_json::from_str::<SessionModelPreference>)
            .transpose()?;
        let model_snapshot = self
            .execution_coordinator()
            .resolve_model_snapshot_in_tx(
                work.connection(),
                project_id,
                Some(owner_id),
                preference.as_ref(),
            )
            .await
            .map_err(|error| match error {
                TurnExecutionError::InvalidModelPreference => SessionsError::InvalidModelPreference,
                other => SessionsError::Internal(anyhow::anyhow!("resolve Turn model: {other}")),
            })?;
        let created = self
            .sessions()
            .create_turn_input_in_tx(
                work.connection(),
                CreateTurnInput {
                    session_id,
                    content,
                    actor: &actor,
                    message_kind: "user",
                    timeline_kind: "user_message",
                    metadata: None,
                    goal_mode,
                    predecessor_turn_id: None,
                    attachment_ids,
                    model_snapshot: model_snapshot.as_ref(),
                    checkpoint_revision,
                    now,
                },
            )
            .await?;

        // First message names the session. Sessions are created with a
        // placeholder title ("New session") and nothing else ever set it, so
        // the list showed the placeholder forever. Derive the title from this
        // first user message in the same transaction: the appended
        // SessionChanged event re-projects the row, so the UI renames live.
        Self::maybe_derive_session_title_in_tx(
            work,
            session_id,
            &created.turn_id,
            content,
            &actor,
            now,
        )
        .await?;

        match route {
            MessageRoute::Started => {
                let activated = self
                    .sessions()
                    .activate_created_turn_in_tx(
                        work.connection(),
                        session_id,
                        &created.turn_id,
                        model_snapshot.as_ref(),
                        now,
                    )
                    .await?;
                if !activated {
                    return Err(SessionsError::ActiveTurnExists);
                }
            }
            MessageRoute::Queued => {}
        }

        for event in Self::message_accepted_events(
            session_id,
            &created,
            &command,
            route,
            "user_message",
            actor,
        ) {
            work.append_event(event)
                .await
                .map_err(SessionsError::Internal)?;
        }
        Ok(MessageRouteResult {
            route: route.as_str().to_owned(),
            message_id: created.message_id.clone(),
            turn_id: created.turn_id.clone(),
            session_version: command.session_version.clone(),
        })
    }

    /// Set the session title from its first user message if the title is
    /// still the creation placeholder. Best-effort and cheap: one UPDATE
    /// guarded by "no user turns yet", so second and later messages never
    /// rewrite a name the user may have set manually.
    async fn maybe_derive_session_title_in_tx(
        work: &mut UnitOfWorkTransaction<'_>,
        session_id: SessionId,
        created_turn_id: &str,
        content: &str,
        actor: &serde_json::Value,
        now: &str,
    ) -> Result<(), SessionsError> {
        const PLACEHOLDER: &str = "New session";
        const MAX_TITLE_CHARS: usize = 80;
        let derived = derive_session_title(content, MAX_TITLE_CHARS);
        if derived.is_empty() {
            return Ok(());
        }
        let changed = sqlx::query(
            "UPDATE sessions SET title = ?, updated_at = ? \
             WHERE id = ? AND title = ? AND NOT EXISTS \
             (SELECT 1 FROM turns WHERE turns.session_id = sessions.id AND turns.id != ?)",
        )
        .bind(&derived)
        .bind(now)
        .bind(session_id.to_string())
        .bind(PLACEHOLDER)
        // The turn carrying this very message was inserted a moment ago;
        // exclude it so "first turn" means "no earlier turn exists".
        .bind(created_turn_id)
        .execute(work.connection())
        .await
        .map_err(SessionsError::from)?;
        if changed.rows_affected() != 1 {
            return Ok(());
        }
        work.append_event(NewEvent {
            event_type: EventType::SessionChanged,
            actor: actor.clone(),
            resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "session_id": session_id.to_string(),
                "state": "titled",
                "detail": "first message",
            }),
        })
        .await
        .map_err(SessionsError::Internal)?;
        Ok(())
    }

    pub(crate) fn message_accepted_events(
        session_id: SessionId,
        created: &janus_sessions::interface::CreatedTurnInput,
        command: &janus_sessions::interface::SessionCommandState,
        route: MessageRoute,
        timeline_kind: &str,
        actor: serde_json::Value,
    ) -> Vec<NewEvent> {
        let correlation_id = CorrelationId::new().to_string();
        let mut events = Vec::with_capacity(5);
        if let Some(checkpoint_id) = &created.checkpoint_id {
            events.push(NewEvent {
                event_type: EventType::CheckpointCreated,
                actor: actor.clone(),
                resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
                correlation_id: correlation_id.clone(),
                causation_id: None,
                payload: json!({
                    "checkpoint_id": checkpoint_id,
                    "session_id": session_id.to_string(),
                    "kind": "pre_turn",
                }),
            });
        }
        let status = if route == MessageRoute::Queued {
            "queued"
        } else {
            "running"
        };
        events.push(NewEvent {
            event_type: EventType::TurnCreated,
            actor: actor.clone(),
            resource: Some(json!({"kind": "turn", "id": created.turn_id})),
            correlation_id: correlation_id.clone(),
            causation_id: None,
            payload: json!({
                "turn_id": created.turn_id,
                "session_id": session_id.to_string(),
                "sequence": created.sequence,
                "status": status,
                "route": route.as_str(),
            }),
        });
        events.push(NewEvent {
            event_type: EventType::TimelineItemCreated,
            actor: actor.clone(),
            resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
            correlation_id: correlation_id.clone(),
            causation_id: None,
            payload: json!({
                "timeline_item_id": created.timeline_item_id,
                "session_id": session_id.to_string(),
                "kind": timeline_kind,
                "display_order": created.display_order,
            }),
        });
        let session_state = if route == MessageRoute::Queued {
            command.state.as_str()
        } else {
            "active"
        };
        let active_turn_id = if route == MessageRoute::Queued {
            command.active_turn_id.as_deref()
        } else {
            Some(created.turn_id.as_str())
        };
        events.push(NewEvent {
            event_type: EventType::SessionChanged,
            actor,
            resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
            correlation_id,
            causation_id: None,
            payload: json!({
                "session_id": session_id.to_string(),
                "state": session_state,
                "active_turn_id": active_turn_id,
                "version": command.session_version,
            }),
        });
        events
    }

    fn turn_transition_event(
        turn_id: TurnId,
        transition: &janus_sessions::interface::TurnTransition,
        actor: serde_json::Value,
        correlation_id: &str,
    ) -> NewEvent {
        NewEvent {
            event_type: EventType::TurnStatusChanged,
            actor,
            resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
            correlation_id: correlation_id.to_owned(),
            causation_id: None,
            payload: json!({
                "turn_id": turn_id.to_string(),
                "from": transition.from_status.as_str(),
                "to": transition.to_status.as_str(),
                "session_version": transition.session_version,
            }),
        }
    }

    /// Final Cancel settlement after Runtime confirms finite resources owned by
    /// the cancelling Turn have settled. Two outcomes:
    /// - `canceled` when every owned finite AsyncTask is terminal and no uncertainty
    ///   remains (the queue advances for completed/canceled);
    /// - `interrupted` when some AsyncTask is `lost` / Runtime cannot confirm the
    ///   process is gone (queue stays paused).
    ///
    /// Sessions owns the Turn state write via `settle_cancel_in_tx`.
    async fn settle_cancel(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        uncertain: bool,
        actor: serde_json::Value,
    ) -> Result<(), SessionsError> {
        let now = now_utc_str();
        let mut work = self.unit_of_work().begin().await?;
        let round_ids = self
            .execution()
            .cancel_execution_in_tx(work.connection(), turn_id, &now)
            .await
            .map_err(|error| {
                SessionsError::Internal(anyhow::anyhow!("cancel execution ledger: {error}"))
            })?;
        self.models()
            .cancel_running_attempts_for_rounds_in_tx(work.connection(), &round_ids, &now)
            .await
            .map_err(|error| {
                SessionsError::Internal(anyhow::anyhow!("cancel Model Attempts: {error}"))
            })?;
        let transition = self
            .sessions()
            .settle_cancel_in_tx(
                work.connection(),
                session_id,
                turn_id,
                uncertain,
                "user_cancel",
                &now,
            )
            .await?;
        if let Some(transition) = &transition {
            let correlation_id = CorrelationId::new().to_string();
            work.append_event(Self::turn_transition_event(
                turn_id,
                transition,
                actor.clone(),
                &correlation_id,
            ))
            .await
            .map_err(SessionsError::Internal)?;
            work.append_event(NewEvent {
                event_type: EventType::SessionChanged,
                actor,
                resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
                correlation_id,
                causation_id: None,
                payload: json!({
                    "session_id": session_id.to_string(),
                    "state": "ready",
                    "active_turn_id": null,
                    "version": transition.session_version,
                }),
            })
            .await
            .map_err(SessionsError::Internal)?;
        }
        if transition
            .as_ref()
            .is_some_and(|transition| transition.to_status == TurnStatus::Canceled)
        {
            self.enqueue_turn_wake_in_tx(&mut work, turn_id)
                .await
                .map_err(SessionsError::Internal)?;
        }
        work.commit().await?;
        if transition
            .as_ref()
            .is_some_and(|transition| transition.to_status == TurnStatus::Canceled)
        {
            // Terminal canceled Turns advance the FIFO queue through the runner
            // without re-entering model execution for the canceled Turn.
            self.execution_coordinator().schedule(turn_id);
        }
        Ok(())
    }

    /// Cancel a queued Turn immediately, or accept Cancel for an active Turn,
    /// bound-cancel its finite AsyncTasks, then settle it as `canceled`/`interrupted`.
    pub(crate) async fn cancel_active_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        reason: &str,
        expected_version: &str,
        actor: serde_json::Value,
    ) -> Result<CancelResult, SessionsError> {
        let initial = self.sessions().execution_turn(turn_id).await?;
        if initial.session_id != session_id {
            return Err(SessionsError::NotFound);
        }
        let mut from_status = initial.status.as_str().to_owned();
        if matches!(
            initial.status,
            TurnStatus::Canceled | TurnStatus::Interrupted
        ) {
            let session = self.sessions().get_session(session_id).await?;
            return Ok(CancelResult {
                turn_id: turn_id.to_string(),
                from_status,
                to_status: initial.status.as_str().to_owned(),
                session_version: session.version,
            });
        }
        if initial.status != TurnStatus::Canceling {
            match self
                .sessions()
                .cancel_turn(session_id, turn_id, reason, expected_version, actor.clone())
                .await
            {
                Ok(accepted) => from_status = accepted.from_status,
                Err(error) => {
                    let current = self.sessions().execution_turn(turn_id).await?;
                    if current.session_id != session_id
                        || !matches!(
                            current.status,
                            TurnStatus::Canceling | TurnStatus::Canceled | TurnStatus::Interrupted
                        )
                    {
                        return Err(error);
                    }
                }
            }
        } else if !initial.active {
            return Err(SessionsError::Internal(anyhow::anyhow!(
                "canceling Turn no longer owns its Session"
            )));
        }

        let current = self.sessions().execution_turn(turn_id).await?;
        if matches!(
            current.status,
            TurnStatus::Canceled | TurnStatus::Interrupted
        ) {
            let session = self.sessions().get_session(session_id).await?;
            return Ok(CancelResult {
                turn_id: turn_id.to_string(),
                from_status,
                to_status: current.status.as_str().to_owned(),
                session_version: session.version,
            });
        }

        let unfinished = self
            .runtime()
            .unfinished_async_tasks_for_turn(turn_id)
            .await
            .map_err(|error| {
                SessionsError::Internal(anyhow::anyhow!(
                    "list unfinished AsyncTasks for Cancel: {error}"
                ))
            })?;
        let mut uncertain = false;
        for async_task in unfinished {
            match self.runtime().cancel_async_task(async_task.id).await {
                Ok(projection) => {
                    if matches!(projection.status, AsyncTaskStatus::Lost) {
                        uncertain = true;
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        async_task_id = %async_task.id,
                        turn_id = %turn_id,
                        "bounded AsyncTask cancellation failed during Turn Cancel"
                    );
                    uncertain = true;
                }
            }
        }

        if !uncertain {
            let remaining = self
                .runtime()
                .unfinished_async_task_count(turn_id)
                .await
                .map_err(|error| {
                    SessionsError::Internal(anyhow::anyhow!(
                        "recheck unfinished AsyncTasks after Cancel: {error}"
                    ))
                })?;
            uncertain = remaining > 0;
        }

        self.settle_cancel(session_id, turn_id, uncertain, actor)
            .await?;

        let turn = self.sessions().get_turn(session_id, turn_id).await?;
        let session = self.sessions().get_session(session_id).await?;
        Ok(CancelResult {
            turn_id: turn_id.to_string(),
            from_status,
            to_status: turn.status,
            session_version: session.version,
        })
    }
}

/// Collapse a user message into a session title: take the first line, trim
/// decoration (common markdown headings and surrounding quotes), collapse
/// runs of whitespace, and cap the length on a word boundary.
fn derive_session_title(content: &str, max_chars: usize) -> String {
    let first_line = content.lines().next().unwrap_or("").trim();
    let stripped = first_line
        .trim_start_matches('#')
        .trim_matches(|c: char| c == '"' || c == '`')
        .trim();
    let mut normalized = String::new();
    let mut pending_space = false;
    for ch in stripped.chars() {
        if ch.is_whitespace() {
            if !normalized.is_empty() {
                pending_space = true;
            }
            continue;
        }
        if pending_space {
            normalized.push(' ');
            pending_space = false;
        }
        normalized.push(ch);
    }
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    // Cut on a word boundary and mark the truncation.
    let mut cut = max_chars;
    while cut > 0 && !normalized.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut truncated = normalized[..cut].trim_end().to_owned();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::derive_session_title;

    #[test]
    fn first_line_becomes_the_title() {
        assert_eq!(derive_session_title("Fix the login bug", 80), "Fix the login bug");
    }

    #[test]
    fn later_lines_are_dropped() {
        assert_eq!(
            derive_session_title("Summarize this\n\nand that", 80),
            "Summarize this"
        );
    }

    #[test]
    fn whitespace_runs_collapse() {
        assert_eq!(derive_session_title("  hello   world  ", 80), "hello world");
    }

    #[test]
    fn markdown_heading_is_stripped() {
        assert_eq!(derive_session_title("## Ship the release", 80), "Ship the release");
    }

    #[test]
    fn surrounding_quotes_are_stripped() {
        assert_eq!(derive_session_title("\"Ship the release\"", 80), "Ship the release");
    }

    #[test]
    fn long_input_is_truncated_on_a_word_boundary() {
        let title = derive_session_title(&"word ".repeat(40), 20);
        assert!(title.chars().count() <= 21, "got {title}");
        assert!(title.ends_with('…'));
    }

    #[test]
    fn blank_input_yields_no_title() {
        assert_eq!(derive_session_title("   \n\t", 80), "");
    }
}
