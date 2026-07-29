//! Cross-module Turn coordination.
//!
//! Lives in `application` because it must update more than one module's owned
//! tables in one SQLite transaction. Sessions owns turns/sessions/checkpoints;
//! supervisor owns asks; runtime owns jobs. This module opens one transaction
//! against the shared pool and drives each owner's `*_in_tx` primitive inside
//! it, then commits and schedules further work only after commit.
//!
//! No persistent business state lives here: only orchestration and correlation.

use std::collections::HashMap;

use serde_json::json;

use crate::AppState;
use crate::application::turn_execution::ToolResultRecord;
use crate::modules::runtime::interface::JobStatus;
use crate::modules::sessions::interface::{
    CancelResult, MessageRoute, MessageRouteResult, RecordAskAnswer, SessionsError, TurnStatus,
};
use crate::modules::supervisor::interface::{AskAnswerDisposition, AskClosure, SupervisorError};
use crate::platform::clock::{Clock, SystemClock, format_utc};
use crate::platform::events::NewEvent;
use crate::platform::id::{AskId, CorrelationId, ProjectId, SessionId, TurnId};
use crate::platform::unit_of_work::UnitOfWorkTransaction;

struct SessionInput<'a> {
    owner_id: &'a str,
    session_id: SessionId,
    content: &'a str,
    expected_version: &'a str,
    actor: serde_json::Value,
    source_ask_id: Option<&'a str>,
    workspace_revision: &'a str,
    now: &'a str,
}

impl AppState {
    pub async fn post_session_message(
        &self,
        owner_id: &str,
        session_id: SessionId,
        content: &str,
        expected_version: &str,
        actor: serde_json::Value,
    ) -> Result<MessageRouteResult, SessionsError> {
        let workspace_revision = self
            .sessions()
            .current_workspace_revision(session_id)
            .await?;
        let now = format_utc(SystemClock.now());
        let mut work = self.unit_of_work().begin().await?;
        let result = self
            .route_session_input_in_tx(
                &mut work,
                SessionInput {
                    owner_id,
                    session_id,
                    content,
                    expected_version,
                    actor,
                    source_ask_id: None,
                    workspace_revision: &workspace_revision,
                    now: &now,
                },
            )
            .await?;
        work.commit().await?;
        Ok(result)
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
            source_ask_id,
            workspace_revision,
            now,
        } = input;
        let command = self
            .sessions()
            .lock_session_command_in_tx(work.connection(), session_id, expected_version, now)
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
            (Some(_), Some(TurnStatus::WaitingForJob), _) => MessageRoute::HandedOff,
            (Some(_), _, _) | (None, _, true) => MessageRoute::Queued,
            (None, _, false) => MessageRoute::Started,
        };
        let predecessor = (route == MessageRoute::HandedOff)
            .then_some(command.active_turn_id.as_deref())
            .flatten();
        let checkpoint_revision = matches!(route, MessageRoute::Started | MessageRoute::HandedOff)
            .then_some(workspace_revision);
        let model_snapshot = if route == MessageRoute::Queued {
            None
        } else {
            let project_id = command.project_id.parse::<ProjectId>().map_err(|error| {
                SessionsError::Internal(anyhow::anyhow!("invalid Project id: {error}"))
            })?;
            self.turn_runner()
                .resolve_model_snapshot_in_tx(
                    work.connection(),
                    project_id,
                    Some(owner_id),
                    command.next_model_ref.as_deref(),
                )
                .await
                .map_err(|error| {
                    SessionsError::Internal(anyhow::anyhow!("resolve Turn model: {error}"))
                })?
        };
        let created = self
            .sessions()
            .create_turn_input_in_tx(
                work.connection(),
                session_id,
                content,
                &actor,
                predecessor,
                source_ask_id,
                checkpoint_revision,
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
            MessageRoute::HandedOff => {
                let predecessor = predecessor.ok_or(SessionsError::HandoffNotApplicable)?;
                let predecessor_id: TurnId = predecessor.parse().map_err(|_| {
                    SessionsError::Internal(anyhow::anyhow!("invalid predecessor Turn id"))
                })?;
                self.turn_runner()
                    .settle_terminal_jobs_for_turn_in_tx(work, predecessor_id, now)
                    .await
                    .map_err(|error| {
                        SessionsError::Internal(anyhow::anyhow!(
                            "settle predecessor Jobs before Handoff: {error}"
                        ))
                    })?;
                let handed_off = self
                    .sessions()
                    .begin_handoff_in_tx(
                        work.connection(),
                        session_id,
                        predecessor,
                        &created.turn_id,
                        now,
                    )
                    .await?;
                if !handed_off {
                    return Err(SessionsError::HandoffNotApplicable);
                }
                self.supervisor()
                    .close_open_asks_in_tx(
                        work.connection(),
                        predecessor_id,
                        AskClosure::Handoff,
                        now,
                    )
                    .await
                    .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
                let successor_id: TurnId = created.turn_id.parse().map_err(|_| {
                    SessionsError::Internal(anyhow::anyhow!("invalid successor Turn id"))
                })?;
                self.runtime()
                    .transfer_unfinished_jobs_in_tx(work.connection(), predecessor_id, successor_id)
                    .await
                    .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
                let activated = self
                    .sessions()
                    .activate_handoff_successor_in_tx(
                        work.connection(),
                        session_id,
                        predecessor,
                        &created.turn_id,
                        model_snapshot.as_ref(),
                        now,
                    )
                    .await?;
                if !activated {
                    return Err(SessionsError::HandoffNotApplicable);
                }
            }
            MessageRoute::Queued => {}
            MessageRoute::AskAnswerSteer => {
                unreachable!("ordinary messages cannot use Ask routing")
            }
        }

        for event in
            Self::message_accepted_events(session_id, &created, &command, route, predecessor, actor)
        {
            work.append_event(event)
                .await
                .map_err(SessionsError::Internal)?;
        }
        Ok(MessageRouteResult {
            route: route.as_str().to_owned(),
            message_id: created.message_id.clone(),
            turn_id: created.turn_id.clone(),
            session_version: command.session_version.clone(),
            handoff_from_turn_id: predecessor.map(str::to_owned),
        })
    }

    fn message_accepted_events(
        session_id: SessionId,
        created: &crate::modules::sessions::interface::CreatedTurnInput,
        command: &crate::modules::sessions::interface::SessionCommandState,
        route: MessageRoute,
        predecessor: Option<&str>,
        actor: serde_json::Value,
    ) -> Vec<NewEvent> {
        let correlation_id = CorrelationId::new().to_string();
        let mut events = Vec::with_capacity(5);
        if let Some(checkpoint_id) = &created.checkpoint_id {
            events.push(NewEvent {
                event_type: "checkpoint.created".into(),
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
        if let Some(predecessor) = predecessor {
            events.push(NewEvent {
                event_type: "turn.status_changed".into(),
                actor: json!({"kind": "supervisor"}),
                resource: Some(json!({"kind": "turn", "id": predecessor})),
                correlation_id: correlation_id.clone(),
                causation_id: None,
                payload: json!({
                    "turn_id": predecessor,
                    "to": "handed_off",
                    "handoff_to_turn_id": created.turn_id,
                }),
            });
        }
        let status = if route == MessageRoute::Queued {
            "queued"
        } else {
            "running"
        };
        events.push(NewEvent {
            event_type: "turn.created".into(),
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
                "handoff_from_turn_id": predecessor,
            }),
        });
        events.push(NewEvent {
            event_type: "timeline.item_created".into(),
            actor: actor.clone(),
            resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
            correlation_id: correlation_id.clone(),
            causation_id: None,
            payload: json!({
                "timeline_item_id": created.timeline_item_id,
                "session_id": session_id.to_string(),
                "kind": "user_message",
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
            event_type: "session.changed".into(),
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

    // ------------------------------------------------------------------
    // Ask creation / answer / expiry
    // ------------------------------------------------------------------

    /// Answer an open Ask. One shared transaction writes the answer (supervisor
    /// owns `asks`) then resumes the Turn to `running` (sessions owns `turns`).
    /// If the Ask's Turn is no longer `waiting_for_ask` (already terminal /
    /// handed off / canceled), this is a LATE answer: the coordinator opens a
    /// successor queued Turn attributed to this answer (best-effort expires
    /// default use the Turn, a manual late answer enqueues a new Turn; per
    /// design, late answers convert to a new ordinary Turn with source
    /// attribution). Returns `turn_status_after` for the caller/UI.
    pub async fn answer_ask(
        &self,
        owner_id: &str,
        ask_id: AskId,
        answer: &serde_json::Value,
        actor: serde_json::Value,
    ) -> Result<(TurnId, String, String), SessionsError> {
        let now = format_utc(SystemClock.now());
        let mut work = self.unit_of_work().begin().await?;
        let answered = self
            .supervisor()
            .answer_ask_in_tx(work.connection(), ask_id, answer, &now)
            .await
            .map_err(|error| match error {
                SupervisorError::AskNotFound => SessionsError::AskNotFound,
                error => SessionsError::Internal(anyhow::anyhow!("answer Ask: {error}")),
            })?;
        let outcome = self
            .turn_runner()
            .inspect_and_reconcile_turn_blockers_in_tx(work.connection(), answered.turn_id, &now)
            .await
            .map_err(|error| {
                SessionsError::Internal(anyhow::anyhow!("reconcile Ask blockers: {error}"))
            })?;
        let correlation_id = CorrelationId::new().to_string();
        if let Some(settlement) = &answered.tool_call {
            self.turn_runner()
                .record_tool_result_in_tx(
                    &mut work,
                    ToolResultRecord {
                        session_id: outcome.session_id,
                        settlement,
                        actor: &actor,
                        correlation_id: &correlation_id,
                        job_id: None,
                        now: &now,
                    },
                )
                .await
                .map_err(|error| {
                    SessionsError::Internal(anyhow::anyhow!("record Ask Tool result: {error}"))
                })?;
        }
        let accepted_by_original = answered.disposition == AskAnswerDisposition::Accepted
            && outcome.active
            && matches!(
                outcome.status,
                TurnStatus::Running | TurnStatus::WaitingForAsk | TurnStatus::WaitingForJob
            );
        let answer_text = answer
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| answer.to_string());
        let source_ask_id = answered.ask_id.to_string();
        let recorded = if accepted_by_original {
            Some(
                self.sessions()
                    .record_ask_answer_in_tx(
                        work.connection(),
                        RecordAskAnswer {
                            session_id: outcome.session_id,
                            turn_id: answered.turn_id,
                            ask_id: answered.ask_id,
                            answer,
                            actor: &actor,
                            now: &now,
                        },
                    )
                    .await?,
            )
        } else {
            None
        };
        let mut late_steer = None;
        let mut late_message = None;
        if answered.disposition == AskAnswerDisposition::Late {
            if outcome.active && outcome.status.is_interactive() {
                late_steer = Some(
                    self.sessions()
                        .append_steer_in_tx(
                            work.connection(),
                            outcome.session_id,
                            Some(answered.turn_id),
                            &answer_text,
                            &outcome.session_version,
                            &actor,
                            Some(&source_ask_id),
                            &now,
                        )
                        .await?,
                );
            } else {
                let workspace_revision = self
                    .sessions()
                    .current_workspace_revision(outcome.session_id)
                    .await?;
                late_message = Some(
                    self.route_session_input_in_tx(
                        &mut work,
                        SessionInput {
                            owner_id,
                            session_id: outcome.session_id,
                            content: &answer_text,
                            expected_version: &outcome.session_version,
                            actor: actor.clone(),
                            source_ask_id: Some(&source_ask_id),
                            workspace_revision: &workspace_revision,
                            now: &now,
                        },
                    )
                    .await?,
                );
            }
        }
        if answered.disposition == AskAnswerDisposition::Accepted {
            work.append_event(NewEvent {
                event_type: "ask.changed".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "ask", "id": answered.ask_id.to_string()})),
                correlation_id: correlation_id.clone(),
                causation_id: None,
                payload: json!({
                    "ask_id": answered.ask_id.to_string(),
                    "turn_id": answered.turn_id.to_string(),
                    "status": "answered",
                }),
            })
            .await
            .map_err(SessionsError::Internal)?;
        }
        if let Some(recorded) = &recorded {
            work.append_event(NewEvent {
                event_type: "timeline.item_created".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "session", "id": outcome.session_id.to_string()})),
                correlation_id: correlation_id.clone(),
                causation_id: None,
                payload: json!({
                    "timeline_item_id": recorded.timeline_item_id,
                    "message_id": recorded.message_id,
                    "display_order": recorded.display_order,
                    "kind": "user_message",
                    "source_ask_id": answered.ask_id.to_string(),
                }),
            })
            .await
            .map_err(SessionsError::Internal)?;
        }
        if let Some(transition) = &outcome.transition {
            work.append_event(Self::blocker_transition_event(
                answered.turn_id,
                transition,
                actor.clone(),
                &correlation_id,
            ))
            .await
            .map_err(SessionsError::Internal)?;
        }
        if let Some((steered, timeline_item_id)) = &late_steer {
            work.append_event(NewEvent {
                event_type: "timeline.item_created".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "session", "id": outcome.session_id.to_string()})),
                correlation_id: correlation_id.clone(),
                causation_id: None,
                payload: json!({
                    "timeline_item_id": timeline_item_id,
                    "kind": "steer",
                    "turn_id": steered.turn_id,
                    "source_ask_id": source_ask_id,
                }),
            })
            .await
            .map_err(SessionsError::Internal)?;
            work.append_event(NewEvent {
                event_type: "session.changed".into(),
                actor: actor.clone(),
                resource: Some(json!({"kind": "session", "id": outcome.session_id.to_string()})),
                correlation_id: correlation_id.clone(),
                causation_id: None,
                payload: json!({
                    "session_id": outcome.session_id.to_string(),
                    "version": steered.session_version,
                    "steer": {
                        "turn_id": steered.turn_id,
                        "source_ask_id": source_ask_id,
                    },
                }),
            })
            .await
            .map_err(SessionsError::Internal)?;
        }
        let resumed_original = outcome
            .transition
            .as_ref()
            .is_some_and(|transition| transition.to_status == TurnStatus::Running);
        let (response, schedule) = if let Some((steered, _)) = late_steer {
            (
                (
                    answered.turn_id,
                    MessageRoute::AskAnswerSteer.as_str().to_owned(),
                    steered.session_version,
                ),
                resumed_original.then_some(answered.turn_id),
            )
        } else if let Some(result) = late_message {
            let turn_id = result
                .turn_id
                .parse::<TurnId>()
                .map_err(|_| SessionsError::Internal(anyhow::anyhow!("invalid turn id")))?;
            let schedule =
                matches!(result.route.as_str(), "started" | "handed_off").then_some(turn_id);
            ((turn_id, result.route, result.session_version), schedule)
        } else {
            (
                (
                    answered.turn_id,
                    outcome.status.as_str().to_owned(),
                    outcome.session_version,
                ),
                resumed_original.then_some(answered.turn_id),
            )
        };
        work.commit().await?;
        if let Some(turn_id) = schedule {
            self.turn_runner().schedule(turn_id);
        }
        Ok(response)
    }

    /// Expire any due best-effort Asks, applying their default answer and
    /// resuming the controlling Turn if it is still `waiting_for_ask`. Called by
    /// a periodic sweeper (and at startup). Stale notifications are harmless.
    pub async fn expire_asks(&self, _owner_id: &str) -> Result<u64, SessionsError> {
        let now = format_utc(SystemClock.now());
        let mut work = self.unit_of_work().begin().await?;
        let expired = self
            .supervisor()
            .expire_due_asks_in_tx(work.connection(), &now, 100)
            .await
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!("expire Asks: {error}")))?;
        let mut by_turn = HashMap::<TurnId, Vec<_>>::new();
        for ask in &expired {
            by_turn.entry(ask.turn_id).or_default().push(ask);
        }
        let actor = json!({"kind": "supervisor"});
        let correlation_id = CorrelationId::new().to_string();
        let mut schedule = Vec::new();
        for (turn_id, asks) in by_turn {
            let outcome = self
                .turn_runner()
                .inspect_and_reconcile_turn_blockers_in_tx(work.connection(), turn_id, &now)
                .await
                .map_err(|error| {
                    SessionsError::Internal(anyhow::anyhow!(
                        "reconcile expired Ask blockers: {error}"
                    ))
                })?;
            let original_can_consume = outcome.active
                && matches!(
                    outcome.status,
                    TurnStatus::Running | TurnStatus::WaitingForAsk | TurnStatus::WaitingForJob
                );
            for ask in asks {
                self.turn_runner()
                    .record_tool_result_in_tx(
                        &mut work,
                        ToolResultRecord {
                            session_id: outcome.session_id,
                            settlement: &ask.tool_call,
                            actor: &actor,
                            correlation_id: &correlation_id,
                            job_id: None,
                            now: &now,
                        },
                    )
                    .await
                    .map_err(|error| {
                        SessionsError::Internal(anyhow::anyhow!(
                            "record expired Ask Tool result: {error}"
                        ))
                    })?;
                work.append_event(NewEvent {
                    event_type: "ask.changed".into(),
                    actor: actor.clone(),
                    resource: Some(json!({"kind": "ask", "id": ask.ask_id.to_string()})),
                    correlation_id: correlation_id.clone(),
                    causation_id: None,
                    payload: json!({
                        "ask_id": ask.ask_id.to_string(),
                        "turn_id": turn_id.to_string(),
                        "status": "expired",
                    }),
                })
                .await
                .map_err(SessionsError::Internal)?;
                if original_can_consume && let Some(default) = &ask.default {
                    let recorded = self
                        .sessions()
                        .record_ask_answer_in_tx(
                            work.connection(),
                            RecordAskAnswer {
                                session_id: outcome.session_id,
                                turn_id,
                                ask_id: ask.ask_id,
                                answer: default,
                                actor: &actor,
                                now: &now,
                            },
                        )
                        .await?;
                    work.append_event(NewEvent {
                        event_type: "timeline.item_created".into(),
                        actor: actor.clone(),
                        resource: Some(json!({
                            "kind": "session",
                            "id": outcome.session_id.to_string(),
                        })),
                        correlation_id: correlation_id.clone(),
                        causation_id: None,
                        payload: json!({
                            "timeline_item_id": recorded.timeline_item_id,
                            "message_id": recorded.message_id,
                            "display_order": recorded.display_order,
                            "kind": "user_message",
                            "source_ask_id": ask.ask_id.to_string(),
                        }),
                    })
                    .await
                    .map_err(SessionsError::Internal)?;
                }
            }
            if let Some(transition) = &outcome.transition {
                work.append_event(Self::blocker_transition_event(
                    turn_id,
                    transition,
                    actor.clone(),
                    &correlation_id,
                ))
                .await
                .map_err(SessionsError::Internal)?;
                if transition.to_status == TurnStatus::Running {
                    schedule.push(turn_id);
                }
            }
        }
        work.commit().await?;
        for turn_id in schedule {
            self.turn_runner().schedule(turn_id);
        }
        Ok(expired.len() as u64)
    }

    fn blocker_transition_event(
        turn_id: TurnId,
        transition: &crate::modules::sessions::interface::TurnTransition,
        actor: serde_json::Value,
        correlation_id: &str,
    ) -> NewEvent {
        NewEvent {
            event_type: "turn.status_changed".into(),
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
    /// - `canceled` when every owned finite Job is terminal and no uncertainty
    ///   remains (the queue advances for completed/canceled);
    /// - `interrupted` when some Job is `lost` / Runtime cannot confirm the
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
        let now = format_utc(SystemClock.now());
        let mut work = self.unit_of_work().begin().await?;
        self.turn_runner()
            .settle_terminal_jobs_for_turn_in_tx(&mut work, turn_id, &now)
            .await
            .map_err(|error| {
                SessionsError::Internal(anyhow::anyhow!("settle canceled Turn Jobs: {error}"))
            })?;
        let round_ids = self
            .supervisor()
            .cancel_execution_in_tx(work.connection(), turn_id, &now)
            .await
            .map_err(|error| {
                SessionsError::Internal(anyhow::anyhow!(
                    "cancel Supervisor execution ledger: {error}"
                ))
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
            work.append_event(Self::blocker_transition_event(
                turn_id,
                transition,
                actor.clone(),
                &correlation_id,
            ))
            .await
            .map_err(SessionsError::Internal)?;
            work.append_event(NewEvent {
                event_type: "session.changed".into(),
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
        work.commit().await?;
        if transition.is_some_and(|transition| transition.to_status == TurnStatus::Canceled) {
            // Terminal canceled Turns advance the FIFO queue through the runner
            // without re-entering model execution for the canceled Turn.
            self.turn_runner().schedule(turn_id);
        }
        Ok(())
    }

    /// Accept Cancel, bound-cancel finite Jobs owned by the Turn, then settle the
    /// Turn as `canceled` or `interrupted`. Only `canceled` advances the queue.
    pub async fn cancel_active_turn(
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
            .unfinished_jobs_for_turn(turn_id)
            .await
            .map_err(|error| {
                SessionsError::Internal(anyhow::anyhow!("list unfinished Jobs for Cancel: {error}"))
            })?;
        let mut uncertain = false;
        for job in unfinished {
            match self.runtime().cancel_job(job.id).await {
                Ok(projection) => {
                    if matches!(projection.status, JobStatus::Lost) {
                        uncertain = true;
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        job_id = %job.id,
                        turn_id = %turn_id,
                        "bounded Job cancellation failed during Turn Cancel"
                    );
                    uncertain = true;
                }
            }
        }

        if !uncertain {
            let remaining =
                self.runtime()
                    .unfinished_job_count(turn_id)
                    .await
                    .map_err(|error| {
                        SessionsError::Internal(anyhow::anyhow!(
                            "recheck unfinished Jobs after Cancel: {error}"
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

    /// Resume a Turn parked on `waiting_for_model` and schedule execution through
    /// the application Turn runner. Returns whether a runnable wake was scheduled.
    pub async fn retry_waiting_model(&self, turn_id: TurnId) -> Result<bool, SessionsError> {
        let runnable =
            self.supervisor()
                .retry_model(turn_id)
                .await
                .map_err(|error| match error {
                    SupervisorError::TurnNotFound => SessionsError::NotFound,
                    error => {
                        SessionsError::Internal(anyhow::anyhow!("retry waiting model: {error}"))
                    }
                })?;
        if runnable {
            self.turn_runner().schedule(turn_id);
        }
        Ok(runnable)
    }
}
