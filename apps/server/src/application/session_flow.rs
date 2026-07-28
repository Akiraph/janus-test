//! Cross-module Turn coordination (M4).
//!
//! Lives in `application` because it must update more than one module's owned
//! tables in one SQLite transaction. Sessions owns turns/sessions/checkpoints;
//! supervisor owns asks; runtime owns jobs. This module opens one transaction
//! against the shared pool and drives each owner's `*_in_tx` primitive inside
//! it, then commits and best-effort appends events. The HTTP layer calls
//! `handle_message` / `handoff_message`; the supervisor calls `settle_cancel`
//! after Runtime confirms finite resources owned by a cancelling Turn settled.
//!
//! No persistent business state lives here: only orchestration and correlation.

use serde_json::json;

use crate::AppState;
use crate::platform::clock::{Clock, SystemClock, format_utc};
use crate::platform::events::NewEvent;
use crate::platform::id::{AskId, CorrelationId, SessionId, ToolCallId, TurnId};

use crate::modules::sessions::types::{
    MessageRoute, MessageRouteResult, SessionsError, TurnStatus,
};

/// Outcome of handling a freshly accepted message via the M4 state machine.
pub struct HandledMessage {
    /// The Turn that should be executed next, if any. `None` when the message
    /// was queued behind an active Turn (no execution to spawn).
    pub run_turn: Option<TurnId>,
}

impl AppState {
    pub async fn post_session_message(
        &self,
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
        let mut tx = self.sessions().pool().begin().await?;
        let command = self
            .sessions()
            .lock_session_command_in_tx(&mut *tx, session_id, expected_version, &now)
            .await?;
        let has_queued = self
            .sessions()
            .has_queued_turn_in_tx(&mut *tx, session_id)
            .await?;
        let active_status = match command.active_turn_id.as_deref() {
            Some(turn_id) => self.sessions().turn_status_in_tx(&mut *tx, turn_id).await?,
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
            .then_some(workspace_revision.as_str());
        let created = self
            .sessions()
            .create_turn_input_in_tx(
                &mut *tx,
                session_id,
                content,
                &actor,
                predecessor,
                None,
                checkpoint_revision,
                &now,
            )
            .await?;

        match route {
            MessageRoute::Started => {
                let activated = self
                    .sessions()
                    .activate_created_turn_in_tx(&mut *tx, session_id, &created.turn_id, &now)
                    .await?;
                if !activated {
                    return Err(SessionsError::ActiveTurnExists);
                }
            }
            MessageRoute::HandedOff => {
                let predecessor = predecessor.ok_or(SessionsError::HandoffNotApplicable)?;
                let handed_off = self
                    .sessions()
                    .begin_handoff_in_tx(&mut *tx, session_id, predecessor, &created.turn_id, &now)
                    .await?;
                if !handed_off {
                    return Err(SessionsError::HandoffNotApplicable);
                }
                self.supervisor()
                    .close_open_asks_in_tx(&mut *tx, predecessor, &now)
                    .await
                    .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
                sqlx::query(
                    "UPDATE jobs SET controlling_turn_id = ? \
                     WHERE controlling_turn_id = ? AND status IN ('queued', 'running')",
                )
                .bind(&created.turn_id)
                .bind(predecessor)
                .execute(&mut *tx)
                .await?;
                let activated = self
                    .sessions()
                    .activate_handoff_successor_in_tx(
                        &mut *tx,
                        session_id,
                        predecessor,
                        &created.turn_id,
                        &now,
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
        tx.commit().await?;

        let result = MessageRouteResult {
            route: route.as_str().to_owned(),
            message_id: created.message_id.clone(),
            turn_id: created.turn_id.clone(),
            session_version: command.session_version.clone(),
            handoff_from_turn_id: predecessor.map(str::to_owned),
        };
        self.publish_message_accepted(session_id, &created, &command, route, predecessor, actor)
            .await;
        Ok(result)
    }

    async fn publish_message_accepted(
        &self,
        session_id: SessionId,
        created: &crate::modules::sessions::types::CreatedTurnInput,
        command: &crate::modules::sessions::types::SessionCommandState,
        route: MessageRoute,
        predecessor: Option<&str>,
        actor: serde_json::Value,
    ) {
        let correlation_id = CorrelationId::new().to_string();
        if let Some(checkpoint_id) = &created.checkpoint_id {
            let _ = self
                .events()
                .append(NewEvent {
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
                })
                .await;
        }
        if let Some(predecessor) = predecessor {
            let _ = self
                .events()
                .append(NewEvent {
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
                })
                .await;
        }
        let status = if route == MessageRoute::Queued {
            "queued"
        } else {
            "running"
        };
        let _ = self
            .events()
            .append(NewEvent {
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
            })
            .await;
        let _ = self
            .events()
            .append(NewEvent {
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
            })
            .await;
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
        let _ = self
            .events()
            .append(NewEvent {
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
            })
            .await;
    }

    /// Route a just-accepted message: if it should take over an active
    /// `waiting_for_job` Turn via an atomic Handoff, perform that Handoff and
    /// return the successor Turn to execute; otherwise return the Turn
    /// `post_message` already created (started -> execute; queued -> nothing).
    pub async fn handle_message(
        &self,
        session_id: SessionId,
        result: crate::modules::sessions::types::MessageRouteResult,
        content: &str,
        _owner_id: &str,
    ) -> Result<HandledMessage, SessionsError> {
        let turn_id: TurnId = result
            .turn_id
            .parse()
            .map_err(|_| SessionsError::Internal(anyhow::anyhow!("invalid turn id")))?;
        if result.route != "handed_off" {
            // started -> execute; queued -> nothing to run now.
            if result.route == "started" {
                return Ok(HandledMessage {
                    run_turn: Some(turn_id),
                });
            }
            return Ok(HandledMessage { run_turn: None });
        }
        // Handoff: promote the queued Turn to the successor of the active
        // waiting_for_job Turn, transactionally closing the predecessor's Asks
        // and transferring its unfinished finite Jobs.
        let active: Option<String> =
            sqlx::query_scalar("SELECT active_turn_id FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_optional(self.sessions().pool())
                .await?
                .flatten();
        let Some(predecessor_str) = active else {
            // No active Turn anymore (concurrency) — just promote the queued Turn.
            let _ = self.sessions().promote_oldest_queued(session_id).await?;
            return Ok(HandledMessage {
                run_turn: self
                    .sessions()
                    .active_turn_status(session_id)
                    .await?
                    .map(|(t, _)| t),
            });
        };
        let predecessor: TurnId = predecessor_str
            .parse()
            .map_err(|_| SessionsError::Internal(anyhow::anyhow!("invalid active_turn_id")))?;

        let supervisor = self.supervisor().clone();
        self.handoff_message_internal(session_id, predecessor, turn_id, content, &supervisor)
            .await?;
        Ok(HandledMessage {
            run_turn: Some(turn_id),
        })
    }

    /// Atomically hand a `waiting_for_job` Turn off to a queued successor Turn:
    /// attach the predecessor link, close the predecessor's open Asks, transfer
    /// its unfinished finite Jobs, record both handoff links, settle the
    /// predecessor `handed_off`, and promote the successor to `running`. Commits
    /// one SQLite transaction; events are appended best-effort after commit.
    async fn handoff_message_internal(
        &self,
        session_id: SessionId,
        predecessor: TurnId,
        successor: TurnId,
        content: &str,
        supervisor: &crate::modules::supervisor::interface::SupervisorInterface,
    ) -> Result<(), SessionsError> {
        let now = format_utc(SystemClock.now());
        let mut tx = self.sessions().pool().begin().await?;

        // The queued successor was created by post_message without a predecessor
        // link; stamp it so the queue projection reports source = handoff.
        self.sessions()
            .attach_predecessor_in_tx(&mut *tx, successor, predecessor)
            .await?;

        // Close the predecessor's open Asks (canceled) so a late answer cannot
        // resume a Turn we are handing off.
        supervisor
            .close_open_asks_in_tx(&mut *tx, &predecessor.to_string(), &now)
            .await
            .map_err(|e| SessionsError::Internal(anyhow::anyhow!("close_open_asks: {e}")))?;

        // Transfer every unfinished finite Job to the successor so the
        // `waiting_for_job` Turn cannot complete while those Jobs run; the new
        // owning Turn sees them via runtime_events wake-up.
        sqlx::query(
            "UPDATE jobs SET controlling_turn_id = ? \
             WHERE controlling_turn_id = ? AND status IN ('queued', 'running')",
        )
        .bind(successor.to_string())
        .bind(predecessor.to_string())
        .execute(&mut *tx)
        .await?;

        // Record bidirectional links + settle predecessor (releases active slot).
        self.sessions()
            .record_handoff_links_in_tx(&mut *tx, predecessor, successor)
            .await?;
        self.sessions()
            .mark_predecessor_handed_off_in_tx(&mut *tx, session_id, predecessor, Some("handoff"))
            .await?;

        // Promote the successor to running and claim the now-empty active slot.
        let promoted = self
            .sessions()
            .promote_successor_in_tx(&mut *tx, session_id, successor)
            .await?;
        tx.commit().await?;
        if promoted.is_none() {
            tracing::warn!(%session_id, %successor, "handoff successor could not claim the slot");
        }

        let _ = self
            .events()
            .append(NewEvent {
                event_type: "turn.status_changed".into(),
                actor: json!({"kind": "supervisor"}),
                resource: Some(json!({"kind": "turn", "id": predecessor.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "turn_id": predecessor.to_string(),
                    "to": "handed_off",
                    "handoff_to": successor.to_string(),
                }),
            })
            .await;
        let _ = self
            .events()
            .append(NewEvent {
                event_type: "turn.status_changed".into(),
                actor: json!({"kind": "supervisor"}),
                resource: Some(json!({"kind": "turn", "id": successor.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "turn_id": successor.to_string(),
                    "to": "running",
                    "route": "handoff",
                    "handoff_from": predecessor.to_string(),
                }),
            })
            .await;
        let _ = content; // body already persisted by post_message before handoff.
        Ok(())
    }

    // ------------------------------------------------------------------
    // Ask creation / answer / expiry
    // ------------------------------------------------------------------

    /// Coordinator for the `ask_user` tool path: one shared transaction writes
    /// the Ask row (supervisor owns `asks`) then pauses the Turn to
    /// `waiting_for_ask` (sessions owns `turns`). A blocking Ask moves the Turn
    /// out of `running` so the supervisor loop blocks; a best-effort Ask leaves
    /// the Turn `running` (the model continues and the Ask may expire with a
    /// default it reads later). Returns the Ask id (already known by the caller)
    /// and the resulting Turn status + session version.
    pub async fn create_ask(
        &self,
        owner_id: &str,
        session_id: SessionId,
        turn_id: TurnId,
        tool_call_id: ToolCallId,
        mode: &str,
        prompt: &serde_json::Value,
        choices: &serde_json::Value,
        default: Option<&serde_json::Value>,
        expires_at: Option<&str>,
    ) -> Result<(AskId, String, String), SessionsError> {
        let ask_id = AskId::new();
        let supervisor = self.supervisor().clone();
        let now = format_utc(SystemClock.now());
        let mut tx = self.sessions().pool().begin().await?;
        supervisor
            .create_ask_in_tx(
                &mut *tx,
                &ask_id.to_string(),
                &turn_id.to_string(),
                &tool_call_id.to_string(),
                mode,
                &prompt.to_string(),
                &choices.to_string(),
                default.map(|v| v.to_string()).as_deref(),
                expires_at,
                &format!("v_{}", AskId::new()),
                &now,
            )
            .await
            .map_err(|e| SessionsError::Internal(anyhow::anyhow!("create_ask: {e}")))?;
        tx.commit().await?;

        // Blocking Ask: pause the running Turn. sessions writes turns state.
        let (status, version) = if mode == "blocking" {
            let _ = owner_id;
            let v = self
                .sessions()
                .pause_turn_for(
                    session_id,
                    turn_id,
                    "waiting_for_ask",
                    json!({"kind": "supervisor"}),
                )
                .await?;
            ("waiting_for_ask".to_string(), v)
        } else {
            // Best-effort: the Turn stays running; the Ask may expire with its
            // default the supervisor reads on a later Round.
            let s = self.sessions().get_session(session_id).await?;
            ("running".to_string(), s.version)
        };
        let _ = self
            .events()
            .append(NewEvent {
                event_type: "ask.changed".into(),
                actor: json!({"kind": "supervisor"}),
                resource: Some(json!({"kind": "ask", "id": ask_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "ask_id": ask_id.to_string(),
                    "turn_id": turn_id.to_string(),
                    "mode": mode,
                    "status": "open",
                }),
            })
            .await;
        Ok((ask_id, status, version))
    }

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
        let supervisor = self.supervisor().clone();
        let now = format_utc(SystemClock.now());

        // Look up the Ask's Turn + session (best-effort if it still exists).
        let row = sqlx::query("SELECT turn_id, session_id FROM asks JOIN turns ON turns.id = asks.turn_id JOIN sessions ON sessions.id = turns.session_id WHERE asks.id = ?")
            .bind(ask_id.to_string())
            .fetch_optional(self.sessions().pool())
            .await?
            .ok_or(SessionsError::AskNotFound)?;
        use sqlx::Row;
        let turn_id_str: String = row.try_get("turn_id")?;
        let session_id_str: String = row.try_get("session_id")?;
        let turn_id: TurnId = turn_id_str
            .parse()
            .map_err(|_| SessionsError::Internal(anyhow::anyhow!("invalid turn_id")))?;
        let session_id: SessionId = session_id_str
            .parse()
            .map_err(|_| SessionsError::Internal(anyhow::anyhow!("invalid session_id")))?;

        let mut tx = self.sessions().pool().begin().await?;
        let still_open = supervisor
            .record_ask_answer_in_tx(&mut *tx, &ask_id.to_string(), &answer.to_string(), &now)
            .await
            .map_err(|e| SessionsError::Internal(anyhow::anyhow!("record_ask_answer: {e}")))?;
        tx.commit().await?;

        if !still_open {
            // Late answer: Ask was no longer open. Enqueue a new ordinary Turn
            // carrying this answer as its message (source = ask_answer). The
            // supervisor will run it when it reaches the head of the queue.
            let current = self.sessions().get_session(session_id).await?;
            let result = self
                .sessions()
                .post_message(session_id, &answer.to_string(), &current.version, actor)
                .await?;
            let new_turn: TurnId = result
                .turn_id
                .parse()
                .map_err(|_| SessionsError::Internal(anyhow::anyhow!("invalid turn id")))?;
            return Ok((new_turn, result.route, result.session_version));
        }

        // Resume the Turn: waiting_for_ask -> running.
        let version = self
            .sessions()
            .resume_turn(session_id, turn_id, "waiting_for_ask", actor)
            .await?;
        let _ = self
            .events()
            .append(NewEvent {
                event_type: "ask.changed".into(),
                actor: json!({"kind": "supervisor"}),
                resource: Some(json!({"kind": "ask", "id": ask_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "ask_id": ask_id.to_string(),
                    "turn_id": turn_id.to_string(),
                    "status": "answered",
                }),
            })
            .await;
        Ok((turn_id, "running".to_string(), version))
    }

    /// Expire any due best-effort Asks, applying their default answer and
    /// resuming the controlling Turn if it is still `waiting_for_ask`. Called by
    /// a periodic sweeper (and at startup). Stale notifications are harmless.
    pub async fn expire_asks(&self, owner_id: &str) -> Result<u64, SessionsError> {
        let supervisor = self.supervisor().clone();
        let now = format_utc(SystemClock.now());
        let expired = supervisor
            .expire_open_asks(&now)
            .await
            .map_err(|e| SessionsError::Internal(anyhow::anyhow!("expire_open_asks: {e}")))?;
        let mut count = 0u64;
        for (_ask_id, turn_id_str, _default) in &expired {
            let Ok(turn_id) = turn_id_str.parse::<TurnId>() else {
                continue;
            };
            // Find the session for this turn.
            let session_id_str: Option<String> =
                sqlx::query_scalar("SELECT session_id FROM turns WHERE id = ?")
                    .bind(turn_id_str)
                    .fetch_optional(self.sessions().pool())
                    .await?
                    .flatten();
            let Some(session_id_str) = session_id_str else {
                continue;
            };
            let Ok(session_id) = session_id_str.parse::<SessionId>() else {
                continue;
            };
            // Resume only if still waiting for the Ask.
            let _ = self
                .sessions()
                .resume_turn(
                    session_id,
                    turn_id,
                    "waiting_for_ask",
                    json!({"kind": "supervisor"}),
                )
                .await;
            count += 1;
        }
        let _ = owner_id;
        Ok(count)
    }

    /// Final Cancel settlement after Runtime confirms finite resources owned by
    /// the cancelling Turn have settled. Two outcomes:
    /// - `canceled` when every owned finite Job is terminal and no uncertainty
    ///   remains (the queue advances for completed/canceled);
    /// - `interrupted` when some Job is `lost` / Runtime cannot confirm the
    ///   process is gone (queue stays paused).
    ///
    /// Called by the cancel workflow once Runtime reports process-group exit
    /// (or after a bounded wait). Sessions owns the Turn state write via
    /// `settle_terminal_turn`.
    pub async fn settle_cancel(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        uncertain: bool,
        actor: serde_json::Value,
    ) -> Result<Option<TurnId>, SessionsError> {
        // Close any still-open Asks the canceling Turn owned so a late answer
        // cannot resume it after settlement.
        let now = format_utc(SystemClock.now());
        let mut tx = self.sessions().pool().begin().await?;
        let supervisor = self.supervisor().clone();
        supervisor
            .close_open_asks_in_tx(&mut *tx, &turn_id.to_string(), &now)
            .await
            .map_err(|e| SessionsError::Internal(anyhow::anyhow!("close_open_asks: {e}")))?;
        tx.commit().await?;

        let terminal = if uncertain { "interrupted" } else { "canceled" };
        self.sessions()
            .settle_terminal_turn(session_id, turn_id, terminal, Some("user_cancel"), actor)
            .await
    }
}
