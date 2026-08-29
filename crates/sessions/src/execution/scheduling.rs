//! Turn scheduling and runnability: running/idle sets, blockers, queue candidates, and the session command lock.

use super::*;
use crate::interface::{opt_str, read_i64, read_str};
use futures_util::TryStreamExt;
use mongodb::{
    ClientSession,
    bson::{Bson, Document, doc},
};

impl SessionsInterface {
    pub async fn turn_is_runnable(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<bool, SessionsError> {
        let running = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {
                "_id": turn_id.to_string(),
                "session_id": session_id.to_string(),
                "status": "running",
            })
            .await?;
        if running.is_none() {
            return Ok(false);
        }
        let owns = self
            .pool
            .collection::<Document>("sessions")
            .find_one(doc! {
                "_id": session_id.to_string(),
                "active_turn_id": turn_id.to_string(),
            })
            .await?;
        Ok(owns.is_some())
    }

    pub async fn turn_is_runnable_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<bool, SessionsError> {
        let running = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {
                "_id": turn_id.to_string(),
                "session_id": session_id.to_string(),
                "status": "running",
            })
            .session(&mut *tx)
            .await?;
        if running.is_none() {
            return Ok(false);
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
        Ok(owns.is_some())
    }

    pub async fn running_turn_ids_in_tx(
        &self,
        tx: &mut ClientSession,
    ) -> Result<HashSet<TurnId>, SessionsError> {
        let mut rows = self
            .pool
            .collection::<Document>("turns")
            .find(doc! {"status": "running"})
            .session(&mut *tx)
            .await?;
        let mut ids = HashSet::new();
        while let Some(document) = rows.try_next().await? {
            let id = read_str(&document, "_id")?;
            ids.insert(
                id.parse::<TurnId>()
                    .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?,
            );
        }
        Ok(ids)
    }

    pub async fn interrupt_active_turns_in_tx(
        &self,
        tx: &mut ClientSession,
        now: &str,
        wake_required: &HashSet<TurnId>,
    ) -> Result<Vec<RecoveredTurn>, SessionsError> {
        let mut rows = self
            .pool
            .collection::<Document>("turns")
            .find(doc! {"status": {"$in": ["running", "canceling"]}})
            .sort(doc! {"session_id": 1, "sequence": 1})
            .session(&mut *tx)
            .await?;
        let mut recovered = Vec::new();
        while let Some(row) = rows.try_next().await? {
            let turn_id = read_str(&row, "_id")?
                .parse::<TurnId>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
            let session_id = read_str(&row, "session_id")?
                .parse::<SessionId>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
            let from_status = read_str(&row, "status")?
                .parse::<TurnStatus>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
            let stored_turn_version = read_str(&row, "version")?;
            if from_status == TurnStatus::Running && wake_required.contains(&turn_id) {
                recovered.push(RecoveredTurn {
                    turn_id,
                    session_id,
                    from_status,
                    turn_version: stored_turn_version,
                    session_version: None,
                    wake_required: true,
                });
                continue;
            }
            let turn_version = format!("v_{}", TurnId::new());
            let changed = self
                .pool
                .collection::<Document>("turns")
                .update_one(
                    doc! {
                        "_id": turn_id.to_string(),
                        "status": from_status.as_str(),
                    },
                    doc! {
                        "$set": {
                            "status": "interrupted",
                            "completion_reason": "control_plane_restart",
                            "version": &turn_version,
                            "updated_at": now,
                        }
                    },
                )
                .session(&mut *tx)
                .await?;
            if changed.matched_count != 1 {
                continue;
            }
            let next_session_version = format!("v_{}", SessionId::new());
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
                            "version": &next_session_version,
                            "updated_at": now,
                            "last_activity_at": now,
                        }
                    },
                )
                .session(&mut *tx)
                .await?;
            recovered.push(RecoveredTurn {
                turn_id,
                session_id,
                from_status,
                turn_version,
                session_version: (released.matched_count == 1).then_some(next_session_version),
                wake_required: false,
            });
        }
        Ok(recovered)
    }

    pub async fn lock_session_command_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
        expected_version: &str,
        model_preference: Option<Option<&SessionModelPreference>>,
        now: &str,
    ) -> Result<SessionCommandState, SessionsError> {
        let row = self
            .pool
            .collection::<Document>("sessions")
            .find_one(doc! {"_id": session_id.to_string()})
            .session(&mut *tx)
            .await?
            .ok_or(SessionsError::NotFound)?;
        let state = read_str(&row, "state")?;
        if state == "deleting" {
            return Err(SessionsError::SessionDeleting);
        }
        let current_version = read_str(&row, "version")?;
        if current_version != expected_version {
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.to_owned(),
                current: current_version,
            });
        }
        let session_version = format!("v_{}", SessionId::new());
        let stored_next_model_ref = opt_str(&row, "next_model_ref");
        let next_model_ref = match model_preference {
            Some(Some(preference)) => Some(serde_json::to_string(preference)?),
            Some(None) => None,
            None => stored_next_model_ref,
        };
        let mut set = Document::new();
        set.insert("version", &session_version);
        set.insert("updated_at", now);
        set.insert("last_activity_at", now);
        if model_preference.is_some() {
            let next_model_ref_bson = next_model_ref
                .as_ref()
                .cloned()
                .map(Bson::String)
                .unwrap_or(Bson::Null);
            set.insert("next_model_ref", next_model_ref_bson);
        }
        let updated = self
            .pool
            .collection::<Document>("sessions")
            .update_one(
                doc! {
                    "_id": session_id.to_string(),
                    "version": expected_version,
                    "state": {"$ne": "deleting"},
                },
                doc! {"$set": set},
            )
            .session(&mut *tx)
            .await?;
        if updated.matched_count != 1 {
            let current = self
                .pool
                .collection::<Document>("sessions")
                .find_one(doc! {"_id": session_id.to_string()})
                .session(&mut *tx)
                .await?
                .ok_or(SessionsError::NotFound)?;
            let current = read_str(&current, "version")?;
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.to_owned(),
                current,
            });
        }
        Ok(SessionCommandState {
            project_id: read_str(&row, "project_id")?,
            state,
            next_model_ref,
            active_turn_id: opt_str(&row, "active_turn_id"),
            session_version,
        })
    }

    pub async fn has_queued_turn_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
    ) -> Result<bool, SessionsError> {
        let exists = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {
                "session_id": session_id.to_string(),
                "status": "queued",
            })
            .session(&mut *tx)
            .await?;
        Ok(exists.is_some())
    }

    pub async fn queued_turn_candidate_in_tx(
        &self,
        tx: &mut ClientSession,
        terminal_turn_id: TurnId,
        session_id: SessionId,
    ) -> Result<Option<QueuedTurnCandidate>, SessionsError> {
        let terminal = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {
                "_id": terminal_turn_id.to_string(),
                "session_id": session_id.to_string(),
                "status": {"$in": ["completed", "failed", "canceled"]},
            })
            .session(&mut *tx)
            .await?;
        if terminal.is_none() {
            return Ok(None);
        }
        let session = self
            .pool
            .collection::<Document>("sessions")
            .find_one(doc! {
                "_id": session_id.to_string(),
                "active_turn_id": Bson::Null,
            })
            .session(&mut *tx)
            .await?;
        if session.is_none() {
            return Ok(None);
        }
        let min_sequence = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {
                "session_id": session_id.to_string(),
                "status": "queued",
            })
            .sort(doc! {"sequence": 1})
            .session(&mut *tx)
            .await?
            .map(|document| read_i64(&document, "sequence"))
            .transpose()?;
        let Some(min_sequence) = min_sequence else {
            return Ok(None);
        };
        let candidate = self
            .pool
            .collection::<Document>("turns")
            .find_one(doc! {
                "session_id": session_id.to_string(),
                "status": "queued",
                "sequence": min_sequence,
            })
            .session(&mut *tx)
            .await?;
        candidate
            .map(|document| {
                Ok(QueuedTurnCandidate {
                    turn_id: read_str(&document, "_id")?
                        .parse::<TurnId>()
                        .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?,
                    session_id: read_str(&document, "session_id")?
                        .parse::<SessionId>()
                        .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?,
                    model_snapshot: TurnModelSnapshot::parse(&read_str(
                        &document,
                        "model_snapshot_json",
                    )?)?,
                })
            })
            .transpose()
    }

    pub async fn activate_queued_turn_in_tx(
        &self,
        tx: &mut ClientSession,
        candidate: &QueuedTurnCandidate,
        model_snapshot: Option<&TurnModelSnapshot>,
        workspace_revision: &str,
        now: &str,
    ) -> Result<Option<String>, SessionsError> {
        let model_snapshot_json = serde_json::to_string(&model_snapshot)?;
        let promoted = self
            .pool
            .collection::<Document>("turns")
            .update_one(
                doc! {
                    "_id": candidate.turn_id.to_string(),
                    "session_id": candidate.session_id.to_string(),
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
            return Ok(None);
        }
        let session_version = format!("v_{}", SessionId::new());
        let claimed = self
            .pool
            .collection::<Document>("sessions")
            .update_one(
                doc! {
                    "_id": candidate.session_id.to_string(),
                    "active_turn_id": Bson::Null,
                },
                doc! {
                    "$set": {
                        "state": "active",
                        "active_turn_id": candidate.turn_id.to_string(),
                        "version": &session_version,
                        "updated_at": now,
                        "last_activity_at": now,
                    }
                },
            )
            .session(&mut *tx)
            .await?;
        if claimed.matched_count != 1 {
            return Ok(None);
        }
        self.insert_checkpoint_for_turn_in_tx(
            tx,
            candidate.session_id,
            candidate.turn_id,
            workspace_revision,
            now,
        )
        .await?;
        Ok(Some(session_version))
    }
}
