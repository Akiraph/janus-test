//! Turn scheduling and runnability: running/idle sets, blockers, queue candidates, and the session command lock.

use super::*;

const TURN_IS_RUNNABLE_SQL: &str = "SELECT EXISTS( \
        SELECT 1 FROM turns AS turn \
        JOIN sessions AS session ON session.id = turn.session_id \
        WHERE turn.id = ? AND turn.session_id = ? AND turn.status = 'running' \
          AND session.active_turn_id = turn.id \
     )";

impl SessionsInterface {
    pub async fn turn_is_runnable(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<bool, SessionsError> {
        let runnable: i64 = sqlx::query_scalar(TURN_IS_RUNNABLE_SQL)
            .bind(turn_id.to_string())
            .bind(session_id.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(runnable == 1)
    }

    pub async fn turn_is_runnable_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<bool, SessionsError> {
        let runnable: i64 = sqlx::query_scalar(TURN_IS_RUNNABLE_SQL)
            .bind(turn_id.to_string())
            .bind(session_id.to_string())
            .fetch_one(&mut *tx)
            .await?;
        Ok(runnable == 1)
    }

    pub async fn running_turn_ids_in_tx(
        &self,
        tx: &mut SqliteConnection,
    ) -> Result<HashSet<TurnId>, SessionsError> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM turns WHERE status = 'running'")
            .fetch_all(&mut *tx)
            .await?;
        rows.into_iter()
            .map(|(id,)| {
                id.parse::<TurnId>()
                    .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))
            })
            .collect()
    }

    pub async fn interrupt_active_turns_in_tx(
        &self,
        tx: &mut SqliteConnection,
        now: &str,
        wake_required: &HashSet<TurnId>,
    ) -> Result<Vec<RecoveredTurn>, SessionsError> {
        let rows = sqlx::query(
            "SELECT id, session_id, status, version FROM turns \
             WHERE status IN ('running', 'canceling') \
             ORDER BY session_id, sequence",
        )
        .fetch_all(&mut *tx)
        .await?;
        let mut recovered = Vec::with_capacity(rows.len());
        for row in rows {
            let turn_id = row
                .try_get::<String, _>("id")?
                .parse::<TurnId>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
            let session_id = row
                .try_get::<String, _>("session_id")?
                .parse::<SessionId>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
            let from_status = row
                .try_get::<String, _>("status")?
                .parse::<TurnStatus>()
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
            let stored_turn_version = row.try_get::<String, _>("version")?;
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
            let changed = sqlx::query(
                "UPDATE turns SET status = 'interrupted', \
                        completion_reason = 'control_plane_restart', version = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(&turn_version)
            .bind(now)
            .bind(turn_id.to_string())
            .bind(from_status.as_str())
            .execute(&mut *tx)
            .await?;
            if changed.rows_affected() != 1 {
                continue;
            }
            let next_session_version = format!("v_{}", SessionId::new());
            let released = sqlx::query(
                "UPDATE sessions SET state = 'ready', active_turn_id = NULL, version = ?, \
                        updated_at = ?, last_activity_at = ? \
                 WHERE id = ? AND active_turn_id = ?",
            )
            .bind(&next_session_version)
            .bind(now)
            .bind(now)
            .bind(session_id.to_string())
            .bind(turn_id.to_string())
            .execute(&mut *tx)
            .await?;
            recovered.push(RecoveredTurn {
                turn_id,
                session_id,
                from_status,
                turn_version,
                session_version: (released.rows_affected() == 1).then_some(next_session_version),
                wake_required: false,
            });
        }
        Ok(recovered)
    }

    pub async fn lock_session_command_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        expected_version: &str,
        model_preference: Option<Option<&SessionModelPreference>>,
        now: &str,
    ) -> Result<SessionCommandState, SessionsError> {
        let row = sqlx::query(
            "SELECT project_id, state, next_model_ref, active_turn_id, version \
             FROM sessions WHERE id = ?",
        )
        .bind(session_id.to_string())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(SessionsError::NotFound)?;
        let state: String = row.try_get("state")?;
        if state == "deleting" {
            return Err(SessionsError::SessionDeleting);
        }
        let current_version: String = row.try_get("version")?;
        if current_version != expected_version {
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.to_owned(),
                current: current_version,
            });
        }
        let session_version = format!("v_{}", SessionId::new());
        let stored_next_model_ref: Option<String> = row.try_get("next_model_ref")?;
        let next_model_ref = match model_preference {
            Some(Some(preference)) => Some(serde_json::to_string(preference)?),
            Some(None) => None,
            None => stored_next_model_ref,
        };
        let updated = if model_preference.is_some() {
            sqlx::query(
                "UPDATE sessions SET next_model_ref = ?, version = ?, updated_at = ?, \
                 last_activity_at = ? WHERE id = ? AND version = ? AND state != 'deleting'",
            )
            .bind(&next_model_ref)
            .bind(&session_version)
            .bind(now)
            .bind(now)
            .bind(session_id.to_string())
            .bind(expected_version)
            .execute(&mut *tx)
            .await?
        } else {
            sqlx::query(
                "UPDATE sessions SET version = ?, updated_at = ?, last_activity_at = ? \
                 WHERE id = ? AND version = ? AND state != 'deleting'",
            )
            .bind(&session_version)
            .bind(now)
            .bind(now)
            .bind(session_id.to_string())
            .bind(expected_version)
            .execute(&mut *tx)
            .await?
        };
        if updated.rows_affected() != 1 {
            let current =
                sqlx::query_scalar::<_, String>("SELECT version FROM sessions WHERE id = ?")
                    .bind(session_id.to_string())
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or(SessionsError::NotFound)?;
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.to_owned(),
                current,
            });
        }
        Ok(SessionCommandState {
            project_id: row.try_get("project_id")?,
            state,
            next_model_ref,
            active_turn_id: row.try_get("active_turn_id")?,
            session_version,
        })
    }

    pub async fn has_queued_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
    ) -> Result<bool, SessionsError> {
        let exists: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM turns WHERE session_id = ? AND status = 'queued')",
        )
        .bind(session_id.to_string())
        .fetch_one(&mut *tx)
        .await?;
        Ok(exists == 1)
    }

    pub async fn queued_turn_candidate_in_tx(
        &self,
        tx: &mut SqliteConnection,
        terminal_turn_id: TurnId,
        session_id: SessionId,
    ) -> Result<Option<QueuedTurnCandidate>, SessionsError> {
        let row = sqlx::query(
            "SELECT next_turn.id, next_turn.session_id, next_turn.model_snapshot_json \
             FROM turns AS terminal_turn \
             JOIN sessions AS session ON session.id = terminal_turn.session_id \
             JOIN turns AS next_turn ON next_turn.session_id = terminal_turn.session_id \
             WHERE terminal_turn.id = ? AND terminal_turn.session_id = ? \
               AND terminal_turn.status IN ('completed', 'failed', 'canceled') \
               AND session.active_turn_id IS NULL \
               AND next_turn.status = 'queued' \
               AND next_turn.sequence = (SELECT MIN(sequence) FROM turns \
                                          WHERE session_id = terminal_turn.session_id \
                                            AND status = 'queued')",
        )
        .bind(terminal_turn_id.to_string())
        .bind(session_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        row.map(|row| {
            Ok(QueuedTurnCandidate {
                turn_id: row
                    .try_get::<String, _>("id")?
                    .parse::<TurnId>()
                    .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?,
                session_id: row
                    .try_get::<String, _>("session_id")?
                    .parse::<SessionId>()
                    .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?,
                model_snapshot: TurnModelSnapshot::parse(
                    &row.try_get::<String, _>("model_snapshot_json")?,
                )?,
            })
        })
        .transpose()
    }

    pub async fn activate_queued_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
        candidate: &QueuedTurnCandidate,
        model_snapshot: Option<&TurnModelSnapshot>,
        workspace_revision: &str,
        now: &str,
    ) -> Result<Option<String>, SessionsError> {
        let model_snapshot_json = serde_json::to_string(&model_snapshot)?;
        let promoted = sqlx::query(
            "UPDATE turns SET status = 'running', model_snapshot_json = ?, updated_at = ? \
             WHERE id = ? AND session_id = ? AND status = 'queued'",
        )
        .bind(model_snapshot_json)
        .bind(now)
        .bind(candidate.turn_id.to_string())
        .bind(candidate.session_id.to_string())
        .execute(&mut *tx)
        .await?;
        if promoted.rows_affected() != 1 {
            return Ok(None);
        }
        let session_version = format!("v_{}", SessionId::new());
        let claimed = sqlx::query(
            "UPDATE sessions SET state = 'active', active_turn_id = ?, version = ?, \
                    updated_at = ?, last_activity_at = ? \
             WHERE id = ? AND active_turn_id IS NULL",
        )
        .bind(candidate.turn_id.to_string())
        .bind(&session_version)
        .bind(now)
        .bind(now)
        .bind(candidate.session_id.to_string())
        .execute(&mut *tx)
        .await?;
        if claimed.rows_affected() != 1 {
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
