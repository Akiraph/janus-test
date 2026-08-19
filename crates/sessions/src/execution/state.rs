//! Turn lifecycle state machine: activation, settlement, and cancel transitions.

use super::*;

impl SessionsInterface {
    pub async fn activate_created_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: &str,
        model_snapshot: Option<&TurnModelSnapshot>,
        now: &str,
    ) -> Result<bool, SessionsError> {
        let model_snapshot_json = serde_json::to_string(&model_snapshot)?;
        let promoted = sqlx::query(
            "UPDATE turns SET status = 'running', model_snapshot_json = ?, updated_at = ? \
             WHERE id = ? AND session_id = ? AND status = 'queued'",
        )
        .bind(model_snapshot_json)
        .bind(now)
        .bind(turn_id)
        .bind(session_id.to_string())
        .execute(&mut *tx)
        .await?;
        if promoted.rows_affected() != 1 {
            return Ok(false);
        }
        let claimed = sqlx::query(
            "UPDATE sessions SET state = 'active', active_turn_id = ?, updated_at = ? \
             WHERE id = ? AND active_turn_id IS NULL",
        )
        .bind(turn_id)
        .bind(now)
        .bind(session_id.to_string())
        .execute(&mut *tx)
        .await?;
        Ok(claimed.rows_affected() == 1)
    }

    /// Rename a session that still carries its creation placeholder, guarded by
    /// "no turn other than `created_turn_id` exists" so only the first message
    /// can name it and a title the user set manually is never overwritten.
    /// Returns whether the row changed.
    pub async fn retitle_placeholder_session_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        title: &str,
        placeholder_title: &str,
        created_turn_id: &str,
        now: &str,
    ) -> Result<bool, SessionsError> {
        let changed = sqlx::query(
            "UPDATE sessions SET title = ?, updated_at = ? \
             WHERE id = ? AND title = ? AND NOT EXISTS \
             (SELECT 1 FROM turns WHERE turns.session_id = sessions.id AND turns.id != ?)",
        )
        .bind(title)
        .bind(now)
        .bind(session_id.to_string())
        .bind(placeholder_title)
        .bind(created_turn_id)
        .execute(&mut *tx)
        .await?;
        Ok(changed.rows_affected() == 1)
    }

    pub async fn settle_active_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
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
        let changed = sqlx::query(
            "UPDATE turns SET status = ?, completion_reason = COALESCE(?, completion_reason), \
                    cancellation_reason = COALESCE(?, cancellation_reason), \
                    completion_summary_json = COALESCE(?, completion_summary_json), \
                    input_tokens = COALESCE(?, input_tokens), \
                    output_tokens = COALESCE(?, output_tokens), updated_at = ? \
             WHERE id = ? AND session_id = ? AND status = ? \
               AND EXISTS(SELECT 1 FROM sessions WHERE id = ? AND active_turn_id = ?)",
        )
        .bind(terminal_status.as_str())
        .bind(completion_reason)
        .bind(cancellation_reason)
        .bind(summary)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(now)
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .bind(from_status.as_str())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() != 1 {
            return Ok(None);
        }
        let session_version = format!("v_{}", SessionId::new());
        let released = sqlx::query(
            "UPDATE sessions SET state = 'ready', active_turn_id = NULL, version = ?, \
                    updated_at = ?, last_activity_at = ? \
             WHERE id = ? AND active_turn_id = ?",
        )
        .bind(&session_version)
        .bind(now)
        .bind(now)
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        if released.rows_affected() != 1 {
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
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_id: TurnId,
        reason: &str,
        expected_version: &str,
        now: &str,
    ) -> Result<Option<TurnTransition>, SessionsError> {
        let row = sqlx::query(
            "SELECT turn.status, session.version, session.active_turn_id FROM turns AS turn \
             JOIN sessions AS session ON session.id = turn.session_id \
             WHERE turn.id = ? AND turn.session_id = ?",
        )
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let current_version: String = row.try_get("version")?;
        if current_version != expected_version {
            return Err(SessionsError::VersionMismatch {
                expected: expected_version.to_owned(),
                current: current_version,
            });
        }
        let from_status = row
            .try_get::<String, _>("status")?
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
            let changed = sqlx::query(
                "UPDATE turns SET status = 'canceled', cancellation_reason = ?, updated_at = ? \
                 WHERE id = ? AND session_id = ? AND status = 'queued'",
            )
            .bind(reason)
            .bind(now)
            .bind(turn_id.to_string())
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?;
            if changed.rows_affected() != 1 {
                return Ok(None);
            }
            let session_version = format!("v_{}", SessionId::new());
            let session_changed = sqlx::query(
                "UPDATE sessions SET version = ?, updated_at = ?, last_activity_at = ? \
                 WHERE id = ? AND version = ?",
            )
            .bind(&session_version)
            .bind(now)
            .bind(now)
            .bind(session_id.to_string())
            .bind(expected_version)
            .execute(&mut *tx)
            .await?;
            if session_changed.rows_affected() != 1 {
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
        let active_turn_id: Option<String> = row.try_get("active_turn_id")?;
        if active_turn_id.as_deref() != Some(turn_id.to_string().as_str()) {
            return Ok(None);
        }
        if !from_status.can_transition_to(TurnStatus::Canceling) {
            return Ok(None);
        }
        let changed = sqlx::query(
            "UPDATE turns SET status = 'canceling', cancellation_reason = ?, updated_at = ? \
             WHERE id = ? AND session_id = ? AND status = ? \
               AND EXISTS(SELECT 1 FROM sessions WHERE id = ? AND active_turn_id = ?)",
        )
        .bind(reason)
        .bind(now)
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .bind(from_status.as_str())
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() != 1 {
            return Ok(None);
        }
        let session_version = format!("v_{}", SessionId::new());
        let session_changed = sqlx::query(
            "UPDATE sessions SET version = ?, updated_at = ? \
             WHERE id = ? AND active_turn_id = ? AND version = ?",
        )
        .bind(&session_version)
        .bind(now)
        .bind(session_id.to_string())
        .bind(turn_id.to_string())
        .bind(expected_version)
        .execute(&mut *tx)
        .await?;
        if session_changed.rows_affected() != 1 {
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
        tx: &mut SqliteConnection,
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
