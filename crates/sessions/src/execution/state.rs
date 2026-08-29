//! Turn lifecycle state machine: activation, settlement, and cancel transitions.

use super::*;
use crate::interface::{opt_str, read_str};
use mongodb::{
    ClientSession,
    bson::{Bson, Document, doc},
};

impl SessionsInterface {
    pub async fn activate_created_turn_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
        turn_id: &str,
        model_snapshot: Option<&TurnModelSnapshot>,
        now: &str,
    ) -> Result<bool, SessionsError> {
        let model_snapshot_json = serde_json::to_string(&model_snapshot)?;
        let promoted = self
            .pool
            .collection::<Document>("turns")
            .update_one(
                doc! {
                    "_id": turn_id,
                    "session_id": session_id.to_string(),
                    "status": "queued",
                },
                doc! {
                    "$set": {
                        "status": "running",
                        "model_snapshot_json": &model_snapshot_json,
                        "updated_at": now,
                    }
                },
            )
            .session(&mut *tx)
            .await?;
        if promoted.matched_count != 1 {
            return Ok(false);
        }
        let claimed = self
            .pool
            .collection::<Document>("sessions")
            .update_one(
                doc! {
                    "_id": session_id.to_string(),
                    "active_turn_id": Bson::Null,
                },
                doc! {
                    "$set": {
                        "state": "active",
                        "active_turn_id": turn_id,
                        "updated_at": now,
                    }
                },
            )
            .session(&mut *tx)
            .await?;
        Ok(claimed.matched_count == 1)
    }

    /// Rename a session that still carries its creation placeholder, guarded by
    /// "no turn other than `created_turn_id` exists" so only the first message
    /// can name it and a title the user set manually is never overwritten.
    /// Returns whether the row changed.
    pub async fn retitle_placeholder_session_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
        title: &str,
        placeholder_title: &str,
        created_turn_id: &str,
        now: &str,
    ) -> Result<bool, SessionsError> {
        let other_turn = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {
                "session_id": session_id.to_string(),
                "_id": {"$ne": created_turn_id},
            })
            .session(&mut *tx)
            .await?;
        if other_turn.is_some() {
            return Ok(false);
        }
        let changed = self
            .pool
            .collection::<Document>("sessions")
            .update_one(
                doc! {
                    "_id": session_id.to_string(),
                    "title": placeholder_title,
                },
                doc! {
                    "$set": {
                        "title": title,
                        "updated_at": now,
                    }
                },
            )
            .session(&mut *tx)
            .await?;
        Ok(changed.matched_count == 1)
    }

    pub async fn settle_active_turn_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
        turn_id: TurnId,
        outcome: ActiveTurnOutcome<'_>,
        now: &str,
    ) -> Result<Option<TurnTransition>, SessionsError> {
        let (
            from_status,
            terminal_status,
            completion_reason,
            cancellation_reason,
            summary,
            input_tokens,
            output_tokens,
        ) = match outcome {
            ActiveTurnOutcome::Completed {
                summary,
                input_tokens,
                output_tokens,
            } => (
                TurnStatus::Running,
                TurnStatus::Completed,
                Some("finish"),
                None,
                Some(summary.to_string()),
                Some(input_tokens),
                Some(output_tokens),
            ),
            ActiveTurnOutcome::Failed { reason, summary } => (
                TurnStatus::Running,
                TurnStatus::Failed,
                Some(reason),
                None,
                Some(summary.to_string()),
                None,
                None,
            ),
            ActiveTurnOutcome::Canceled { reason } => (
                TurnStatus::Canceling,
                TurnStatus::Canceled,
                None,
                Some(reason),
                None,
                None,
                None,
            ),
            ActiveTurnOutcome::Interrupted { reason } => (
                TurnStatus::Canceling,
                TurnStatus::Interrupted,
                Some(reason),
                None,
                None,
                None,
                None,
            ),
        };
        if !from_status.can_transition_to(terminal_status) {
            return Err(SessionsError::Internal(anyhow::anyhow!(
                "invalid terminal Turn transition: {} -> {}",
                from_status.as_str(),
                terminal_status.as_str()
            )));
        }
        let mut set = Document::new();
        set.insert("status", terminal_status.as_str());
        set.insert("updated_at", now);
        if let Some(reason) = completion_reason {
            set.insert("completion_reason", reason);
        }
        if let Some(reason) = cancellation_reason {
            set.insert("cancellation_reason", reason);
        }
        if let Some(summary) = summary {
            set.insert("completion_summary_json", summary);
        }
        if let Some(tokens) = input_tokens {
            set.insert("input_tokens", tokens);
        }
        if let Some(tokens) = output_tokens {
            set.insert("output_tokens", tokens);
        }
        let owns = self
            .pool
            .collection::<Document>("sessions")
            .find_one(doc! {
                "_id": session_id.to_string(),
                "active_turn_id": turn_id.to_string(),
            })
            .session(&mut *tx)
            .await?;
        if owns.is_none() {
            return Ok(None);
        }
        let changed = self
            .pool
            .collection::<Document>("turns")
            .update_one(
                doc! {
                    "_id": turn_id.to_string(),
                    "session_id": session_id.to_string(),
                    "status": from_status.as_str(),
                },
                doc! {"$set": set},
            )
            .session(&mut *tx)
            .await?;
        if changed.matched_count != 1 {
            return Ok(None);
        }
        let session_version = format!("v_{}", SessionId::new());
        let released = self
            .pool
            .collection::<Document>("sessions")
            .update_one(
                doc! {
                    "_id": session_id.to_string(),
                    "active_turn_id": turn_id.to_string(),
                },
                doc! {
                    "$set": {
                        "state": "ready",
                        "active_turn_id": Bson::Null,
                        "version": &session_version,
                        "updated_at": now,
                        "last_activity_at": now,
                    }
                },
            )
            .session(&mut *tx)
            .await?;
        if released.matched_count != 1 {
            return Err(SessionsError::Internal(anyhow::anyhow!(
                "terminal Turn lost Session ownership during settlement"
            )));
        }
        Ok(Some(TurnTransition {
            from_status,
            to_status: terminal_status,
            session_version,
        }))
    }

    pub async fn accept_cancel_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
        turn_id: TurnId,
        reason: &str,
        expected_version: &str,
        now: &str,
    ) -> Result<Option<TurnTransition>, SessionsError> {
        let turn = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {
                "_id": turn_id.to_string(),
                "session_id": session_id.to_string(),
            })
            .session(&mut *tx)
            .await?;
        let Some(turn) = turn else {
            return Ok(None);
        };
        let session = self
            .pool
            .collection::<Document>("sessions")
            .find_one(doc! {"_id": session_id.to_string()})
            .session(&mut *tx)
            .await?;
        let Some(session) = session else {
            return Ok(None);
        };
        let current_version = read_str(&session, "version")?;
        if current_version != expected_version {
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.to_owned(),
                current: current_version,
            });
        }
        let from_status = read_str(&turn, "status")?
            .parse::<TurnStatus>()
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
        if from_status == TurnStatus::Queued {
            if !from_status.can_transition_to(TurnStatus::Canceled) {
                return Err(SessionsError::Internal(anyhow::anyhow!(
                    "invalid queued Turn transition: {} -> {}",
                    from_status.as_str(),
                    TurnStatus::Canceled.as_str()
                )));
            }
            let changed = self
                .pool
                .collection::<Document>("turns")
                .update_one(
                    doc! {
                        "_id": turn_id.to_string(),
                        "session_id": session_id.to_string(),
                        "status": "queued",
                    },
                    doc! {
                        "$set": {
                            "status": "canceled",
                            "cancellation_reason": reason,
                            "updated_at": now,
                        }
                    },
                )
                .session(&mut *tx)
                .await?;
            if changed.matched_count != 1 {
                return Ok(None);
            }
            let session_version = format!("v_{}", SessionId::new());
            let session_changed = self
                .pool
                .collection::<Document>("sessions")
                .update_one(
                    doc! {
                        "_id": session_id.to_string(),
                        "version": expected_version,
                    },
                    doc! {
                        "$set": {
                            "version": &session_version,
                            "updated_at": now,
                            "last_activity_at": now,
                        }
                    },
                )
                .session(&mut *tx)
                .await?;
            if session_changed.matched_count != 1 {
                return Err(SessionsError::Internal(anyhow::anyhow!(
                    "Session changed while canceling queued Turn"
                )));
            }
            return Ok(Some(TurnTransition {
                from_status,
                to_status: TurnStatus::Canceled,
                session_version,
            }));
        }
        let active_turn_id = opt_str(&session, "active_turn_id");
        if active_turn_id.as_deref() != Some(turn_id.to_string().as_str()) {
            return Ok(None);
        }
        if !from_status.can_transition_to(TurnStatus::Canceling) {
            return Ok(None);
        }
        let changed = self
            .pool
            .collection::<Document>("turns")
            .update_one(
                doc! {
                    "_id": turn_id.to_string(),
                    "session_id": session_id.to_string(),
                    "status": from_status.as_str(),
                },
                doc! {
                    "$set": {
                        "status": "canceling",
                        "cancellation_reason": reason,
                        "updated_at": now,
                    }
                },
            )
            .session(&mut *tx)
            .await?;
        if changed.matched_count != 1 {
            return Ok(None);
        }
        let session_version = format!("v_{}", SessionId::new());
        let session_changed = self
            .pool
            .collection::<Document>("sessions")
            .update_one(
                doc! {
                    "_id": session_id.to_string(),
                    "active_turn_id": turn_id.to_string(),
                    "version": expected_version,
                },
                doc! {
                    "$set": {
                        "version": &session_version,
                        "updated_at": now,
                    }
                },
            )
            .session(&mut *tx)
            .await?;
        if session_changed.matched_count != 1 {
            return Err(SessionsError::Internal(anyhow::anyhow!(
                "active Session changed while accepting Turn cancellation"
            )));
        }
        Ok(Some(TurnTransition {
            from_status,
            to_status: TurnStatus::Canceling,
            session_version,
        }))
    }

    pub async fn settle_cancel_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
        turn_id: TurnId,
        uncertain: bool,
        reason: &str,
        now: &str,
    ) -> Result<Option<TurnTransition>, SessionsError> {
        self.settle_active_turn_in_tx(
            tx,
            session_id,
            turn_id,
            if uncertain {
                ActiveTurnOutcome::Interrupted { reason }
            } else {
                ActiveTurnOutcome::Canceled { reason }
            },
            now,
        )
        .await
    }
}
