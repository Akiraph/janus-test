//! Job/round scheduling and turn interruption: waiting sets, job
//! settlement, interrupt and cancel transactions.
use super::*;

impl ExecutionInterface {
    pub async fn waiting_job_ids(&self, limit: i64) -> Result<Vec<JobId>, ExecutionError> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT job_id FROM tool_calls \
             WHERE status = 'waiting' AND job_id IS NOT NULL \
             ORDER BY started_at ASC LIMIT ?",
        )
        .bind(limit.clamp(1, 100))
        .fetch_all(&self.pool)
        .await?;
        ids.into_iter()
            .map(|id| {
                id.parse()
                    .map_err(|_| ExecutionError::Internal(anyhow::anyhow!("invalid Job id")))
            })
            .collect()
    }

    pub async fn settle_job_tool_call_in_tx(
        &self,
        tx: &mut sqlx::SqliteConnection,
        job: &JobProjection,
        now: &str,
    ) -> Result<Option<ToolCallSettlement>, ExecutionError> {
        if !job.status.is_terminal() {
            return Ok(None);
        }
        let row: Option<(String, Option<String>, String, String)> = sqlx::query_as(
            "SELECT call.tool_name, call.provider_call_id, round.turn_id, call.input_json \
             FROM tool_calls AS call \
             JOIN rounds AS round ON round.id = call.round_id \
             WHERE call.id = ? AND call.status = 'waiting' \
               AND (call.job_id IS NULL OR call.job_id = ?)",
        )
        .bind(job.initiated_by_tool_call_id.to_string())
        .bind(job.id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let Some((tool_name, provider_call_id, source_turn_id, input_json)) = row else {
            return Ok(None);
        };
        let provider_call_id = provider_call_id.ok_or_else(|| {
            ExecutionError::Internal(anyhow::anyhow!("waiting Tool Call has no Provider call id"))
        })?;
        let (status, error_code, disposition) = match job.status {
            JobStatus::Succeeded => (
                ToolCallStatus::Succeeded,
                None,
                ToolExecutionDisposition::Succeeded,
            ),
            JobStatus::Failed => (
                ToolCallStatus::Failed,
                Some("JOB_FAILED"),
                ToolExecutionDisposition::Failed,
            ),
            JobStatus::Canceled => (
                ToolCallStatus::Canceled,
                Some("JOB_CANCELED"),
                ToolExecutionDisposition::Failed,
            ),
            JobStatus::Lost => (
                ToolCallStatus::Lost,
                Some("JOB_LOST"),
                ToolExecutionDisposition::Failed,
            ),
            JobStatus::Queued | JobStatus::Running => return Ok(None),
        };
        let summary = json!({
            "job_id": job.id.to_string(),
            "status": job.status.as_str(),
            "exit": job.exit,
            "usage": job.usage,
            "log_stream_id": job.log_stream_id.to_string(),
            "command_summary": job.command_summary,
        });
        let mut outcome = ToolOutcome {
            disposition,
            parts: vec![ToolResultPart::Text {
                text: format!(
                    "job {} {} (exit={:?}, log_stream={})",
                    job.id,
                    job.status.as_str(),
                    job.exit.as_ref().and_then(|exit| exit.exit_code),
                    job.log_stream_id,
                ),
            }],
            summary,
            error_code: error_code.map(str::to_owned),
            finish_summary: None,
            wait: None,
        };
        let input = serde_json::from_str::<Value>(&input_json).unwrap_or_else(|_| json!({}));
        attach_tool_display(&tool_name, &input, &mut outcome);
        let summary = outcome.summary.clone();
        let (_, model_parts) = tool_result_message(&outcome, &provider_call_id);
        let changed = sqlx::query(
            "UPDATE tool_calls SET status = ?, result_summary_json = ?, error_code = ?, \
                    job_id = ?, ended_at = ?, version = ? \
             WHERE id = ? AND status = 'waiting' \
               AND (job_id IS NULL OR job_id = ?)",
        )
        .bind(status.as_str())
        .bind(summary.to_string())
        .bind(&outcome.error_code)
        .bind(job.id.to_string())
        .bind(now)
        .bind(format!("v_{}", ToolCallId::new()))
        .bind(job.initiated_by_tool_call_id.to_string())
        .bind(job.id.to_string())
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() != 1 {
            return Ok(None);
        }
        Ok(Some(ToolCallSettlement {
            tool_call_id: job.initiated_by_tool_call_id.to_string(),
            source_turn_id,
            provider_call_id,
            tool_name,
            status,
            summary,
            model_parts,
        }))
    }

    pub async fn interrupt_execution_in_tx(
        &self,
        tx: &mut SqliteConnection,
        now: &str,
    ) -> Result<(), ExecutionError> {
        sqlx::query(
            "UPDATE rounds SET status = 'interrupted', stop_reason = 'control_plane_restart', \
                    version = ?, updated_at = ? WHERE status = 'running'",
        )
        .bind(format!("v_{}", RoundId::new()))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE tool_calls SET status = 'canceled', error_code = 'CONTROL_PLANE_RESTART', \
                    ended_at = ?, version = ? WHERE status = 'requested'",
        )
        .bind(now)
        .bind(format!("v_{}", ToolCallId::new()))
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE tool_calls SET status = 'lost', error_code = 'CONTROL_PLANE_RESTART', \
                    ended_at = ?, version = ? WHERE status IN ('running', 'waiting')",
        )
        .bind(now)
        .bind(format!("v_{}", ToolCallId::new()))
        .execute(&mut *tx)
        .await?;
        self.close_asks_in_tx(tx, None, AskClosure::ControlPlaneRestart, now)
            .await?;
        Ok(())
    }

    /// Return the subset of candidate Turns that never created an
    /// Execution-owned Round. The candidate list is supplied by the Sessions
    /// owner so this capability never reads Session tables directly.
    ///
    /// Recovery uses this query to distinguish a crash before execution began
    /// from a crash in an already materialized Round. The Sessions capability
    /// receives the result instead of reading the `rounds` table itself.
    pub async fn unstarted_active_turn_ids_in_tx(
        &self,
        tx: &mut SqliteConnection,
        candidate_turn_ids: &HashSet<TurnId>,
    ) -> Result<HashSet<TurnId>, ExecutionError> {
        let mut unstarted = HashSet::new();
        for turn_id in candidate_turn_ids {
            let has_round: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM rounds WHERE turn_id = ?)")
                    .bind(turn_id.to_string())
                    .fetch_one(&mut *tx)
                    .await?;
            if !has_round {
                unstarted.insert(*turn_id);
            }
        }
        Ok(unstarted)
    }

    pub async fn cancel_execution_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: TurnId,
        now: &str,
    ) -> Result<Vec<RoundId>, ExecutionError> {
        let round_ids = self.round_ids_for_turns_in_tx(tx, &[turn_id]).await?;
        sqlx::query(
            "UPDATE tool_calls SET status = 'canceled', error_code = 'USER_CANCEL', \
                    ended_at = ?, version = ? \
             WHERE status IN ('requested', 'running', 'waiting') \
               AND round_id IN (SELECT id FROM rounds WHERE turn_id = ?)",
        )
        .bind(now)
        .bind(format!("v_{}", ToolCallId::new()))
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE rounds SET status = 'canceled', stop_reason = 'user_cancel', \
                    version = ?, updated_at = ? WHERE turn_id = ? AND status = 'running'",
        )
        .bind(format!("v_{}", RoundId::new()))
        .bind(now)
        .bind(turn_id.to_string())
        .execute(&mut *tx)
        .await?;
        self.close_asks_in_tx(tx, Some(turn_id), AskClosure::UserCancel, now)
            .await?;
        Ok(round_ids)
    }

    pub async fn round_ids_for_turns_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_ids: &[TurnId],
    ) -> Result<Vec<RoundId>, ExecutionError> {
        let mut rounds = Vec::new();
        for turn_id in turn_ids {
            let rows = sqlx::query_scalar::<_, String>(
                "SELECT id FROM rounds WHERE turn_id = ? ORDER BY sequence",
            )
            .bind(turn_id.to_string())
            .fetch_all(&mut *tx)
            .await?;
            for id in rows {
                rounds.push(
                    id.parse::<RoundId>()
                        .map_err(|error| ExecutionError::Internal(anyhow::anyhow!(error)))?,
                );
            }
        }
        Ok(rounds)
    }

}

#[cfg(test)]
mod tests {
    use super::{ExecutionDependencies, ExecutionInterface, ToolCallSettlement, ToolCallStatus};
    use futures_util::future::BoxFuture;
    use janus_infrastructure::{
        clock::{format_utc, now_utc, now_utc_str},
        events::EventStore,
        id::{AskId, JobId, LogStreamId, RoundId, RuntimeId, ServiceId, SessionId, TerminalId, ToolCallId, TurnId},
        managed_storage::BlobStore,
        operations::OperationInterface,
        secrets::SecretCipher,
        state_broadcaster::StateBroadcaster,
    };
    use janus_models::interface::ModelsInterface;
    use janus_projects::interface::ProjectsInterface;
    use janus_runtime::interface::{
        ExecutorProcessHandle, ExecutorRuntimeHandle, ExecutionResult, ExecutionSpec, ExitSummary,
        JobProjection, JobSpec, JobStatus, ProcessCompletion, ResourceUsage, RuntimeError,
        RuntimeExecutor, RuntimeInterface, RuntimeSpec, ServiceSpec, TerminalSignal, TerminalSize,
        TerminalSpec,
    };
    use janus_sessions::interface::SessionsInterface;
    use janus_workspace::interface::WorkspaceInterface;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::{collections::HashSet, str::FromStr, sync::Arc};
    use tempfile::TempDir;

    /// Stub executor: scheduling transactions never invoke the runtime, so every
    /// entry point fails fast instead of touching a live process.
    struct StubExecutor;

    impl RuntimeExecutor for StubExecutor {
        fn ensure_runtime<'a>(
            &'a self,
            _spec: &'a RuntimeSpec,
        ) -> BoxFuture<'a, Result<ExecutorRuntimeHandle, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn stop_runtime<'a>(
            &'a self,
            _runtime_id: RuntimeId,
            _executor_nonce: &'a str,
        ) -> BoxFuture<'a, Result<(), RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn execute_sync<'a>(
            &'a self,
            _spec: ExecutionSpec,
            _log_stream_id: LogStreamId,
        ) -> BoxFuture<'a, Result<ExecutionResult, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn start_job<'a>(
            &'a self,
            _spec: JobSpec,
            _log_stream_id: LogStreamId,
        ) -> BoxFuture<'a, Result<ExecutorProcessHandle, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn wait_job<'a>(
            &'a self,
            _id: JobId,
            _executor_nonce: &'a str,
        ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn write_job_stdin<'a>(
            &'a self,
            _id: JobId,
            _executor_nonce: &'a str,
            _input: Vec<u8>,
        ) -> BoxFuture<'a, Result<(), RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn cancel_job<'a>(
            &'a self,
            _id: JobId,
            _executor_nonce: &'a str,
        ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn start_service<'a>(
            &'a self,
            _spec: ServiceSpec,
            _log_stream_id: LogStreamId,
        ) -> BoxFuture<'a, Result<ExecutorProcessHandle, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn wait_service<'a>(
            &'a self,
            _id: ServiceId,
            _executor_nonce: &'a str,
        ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn stop_service<'a>(
            &'a self,
            _id: ServiceId,
            _executor_nonce: &'a str,
        ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn start_terminal<'a>(
            &'a self,
            _spec: TerminalSpec,
            _scrollback_stream_id: LogStreamId,
        ) -> BoxFuture<'a, Result<ExecutorProcessHandle, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn write_terminal_input<'a>(
            &'a self,
            _id: TerminalId,
            _executor_nonce: &'a str,
            _input: Vec<u8>,
        ) -> BoxFuture<'a, Result<(), RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn resize_terminal<'a>(
            &'a self,
            _id: TerminalId,
            _executor_nonce: &'a str,
            _size: TerminalSize,
        ) -> BoxFuture<'a, Result<(), RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn signal_terminal<'a>(
            &'a self,
            _id: TerminalId,
            _executor_nonce: &'a str,
            _signal: TerminalSignal,
        ) -> BoxFuture<'a, Result<(), RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn close_terminal<'a>(
            &'a self,
            _id: TerminalId,
            _executor_nonce: &'a str,
        ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn await_terminal_exit<'a>(
            &'a self,
            _id: TerminalId,
            _executor_nonce: &'a str,
        ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
    }

    /// In-memory SQLite with the real server migration and the full
    /// ExecutionInterface dependency bundle wired around a stub executor.
    async fn test_execution() -> (sqlx::SqlitePool, ExecutionInterface, TempDir) {
        let temp = TempDir::new().unwrap();
        let data_root = temp.path();
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("../../apps/server/migrations")
            .run(&pool)
            .await
            .unwrap();

        let events = EventStore::new(pool.clone());
        let state_broadcaster = StateBroadcaster::new();
        let cipher = SecretCipher::load(data_root, false).unwrap();
        let blobs = BlobStore::new(pool.clone(), data_root).unwrap();
        let operations = OperationInterface::new(pool.clone(), events.clone());
        let workspace = WorkspaceInterface::new(pool.clone(), data_root, blobs.clone());
        let projects = ProjectsInterface::new(
            pool.clone(),
            cipher.clone(),
            operations,
            workspace.clone(),
            events.clone(),
            data_root,
        );
        let models = ModelsInterface::new(pool.clone(), cipher, events.clone()).unwrap();
        let sessions = SessionsInterface::new(pool.clone(), events.clone(), workspace.clone(), blobs.clone());
        let runtime = RuntimeInterface::new(
            pool.clone(),
            events.clone(),
            data_root,
            Arc::new(StubExecutor),
        );

        let execution = ExecutionInterface::new(ExecutionDependencies {
            pool: pool.clone(),
            events,
            state_broadcaster,
            models,
            projects,
            workspace,
            sessions,
            blobs,
            runtime,
        });
        (pool, execution, temp)
    }

    async fn seed_round(
        pool: &sqlx::SqlitePool,
        id: &RoundId,
        turn_id: TurnId,
        sequence: i64,
        status: &str,
        now: &str,
    ) {
        sqlx::query(
            "INSERT INTO rounds (id, turn_id, sequence, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 'v1', ?, ?)",
        )
        .bind(id.to_string())
        .bind(turn_id.to_string())
        .bind(sequence)
        .bind(status)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_tool_call(
        pool: &sqlx::SqlitePool,
        id: &ToolCallId,
        round_id: &RoundId,
        ord: i64,
        status: &str,
        now: &str,
    ) {
        sqlx::query(
            "INSERT INTO tool_calls \
             (id, round_id, ord, tool_name, schema_version, input_json, status, actor_json, version, started_at) \
             VALUES (?, ?, ?, 'bash', 1, '{}', ?, '{\"kind\":\"model\"}', 'v1', ?)",
        )
        .bind(id.to_string())
        .bind(round_id.to_string())
        .bind(ord)
        .bind(status)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_open_ask(
        pool: &sqlx::SqlitePool,
        id: &AskId,
        turn_id: TurnId,
        tool_call_id: &ToolCallId,
        now: &str,
    ) {
        sqlx::query(
            "INSERT INTO asks \
             (id, turn_id, tool_call_id, mode, prompt_json, choices_json, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'blocking', '{}', '{}', 'open', 'v1', ?, ?)",
        )
        .bind(id.to_string())
        .bind(turn_id.to_string())
        .bind(tool_call_id.to_string())
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
    }

    fn job_projection(
        id: JobId,
        initiated_by_tool_call_id: ToolCallId,
        turn_id: TurnId,
        status: JobStatus,
        now: &str,
    ) -> JobProjection {
        JobProjection {
            id,
            runtime_id: RuntimeId::new(),
            session_id: SessionId::new(),
            controlling_turn_id: turn_id,
            cli_kind: None,
            initiated_by_tool_call_id,
            cli_session_id: None,
            status,
            command_summary: "ls".to_owned(),
            log_stream_id: LogStreamId::new(),
            exit: if matches!(status, JobStatus::Succeeded) {
                Some(ExitSummary {
                    exit_code: Some(0),
                    signal: None,
                })
            } else {
                None
            },
            usage: ResourceUsage::default(),
            version: "v1".to_owned(),
            created_at: now.to_owned(),
            started_at: Some(now.to_owned()),
            ended_at: Some(now.to_owned()),
        }
    }

    async fn tool_call_status_and_error(
        pool: &sqlx::SqlitePool,
        id: &ToolCallId,
    ) -> (String, Option<String>) {
        sqlx::query_as("SELECT status, error_code FROM tool_calls WHERE id = ?")
            .bind(id.to_string())
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn interrupt_marks_running_rounds_and_loses_live_tool_calls() {
        let (pool, execution, _temp) = test_execution().await;
        let now = now_utc_str();
        let turn = TurnId::new();
        let round = RoundId::new();
        seed_round(&pool, &round, turn, 1, "running", &now).await;
        let requested = ToolCallId::new();
        let running = ToolCallId::new();
        let waiting = ToolCallId::new();
        seed_tool_call(&pool, &requested, &round, 1, "requested", &now).await;
        seed_tool_call(&pool, &running, &round, 2, "running", &now).await;
        seed_tool_call(&pool, &waiting, &round, 3, "waiting", &now).await;
        let ask = AskId::new();
        seed_open_ask(&pool, &ask, turn, &waiting, &now).await;

        let mut tx = pool.begin().await.unwrap();
        execution
            .interrupt_execution_in_tx(&mut tx, &now)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let (status, stop): (String, Option<String>) =
            sqlx::query_as("SELECT status, stop_reason FROM rounds WHERE id = ?")
                .bind(round.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "interrupted");
        assert_eq!(stop.as_deref(), Some("control_plane_restart"));

        let (status, error) = tool_call_status_and_error(&pool, &requested).await;
        assert_eq!(status, "canceled");
        assert_eq!(error.as_deref(), Some("CONTROL_PLANE_RESTART"));
        for id in [running, waiting] {
            let (status, error) = tool_call_status_and_error(&pool, &id).await;
            assert_eq!(status, "lost");
            assert_eq!(error.as_deref(), Some("CONTROL_PLANE_RESTART"));
        }

        let (status, reason): (String, Option<String>) =
            sqlx::query_as("SELECT status, closure_reason FROM asks WHERE id = ?")
                .bind(ask.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "canceled");
        assert_eq!(reason.as_deref(), Some("control_plane_restart"));
    }

    #[tokio::test]
    async fn cancel_marks_turn_tool_calls_and_rounds_only() {
        let (pool, execution, _temp) = test_execution().await;
        let now = now_utc_str();
        let turn = TurnId::new();
        let round_a = RoundId::new();
        let round_b = RoundId::new();
        seed_round(&pool, &round_a, turn, 1, "running", &now).await;
        seed_round(&pool, &round_b, turn, 2, "running", &now).await;
        let tc_a = ToolCallId::new();
        let tc_b = ToolCallId::new();
        seed_tool_call(&pool, &tc_a, &round_a, 1, "running", &now).await;
        seed_tool_call(&pool, &tc_b, &round_b, 1, "waiting", &now).await;
        let ask = AskId::new();
        seed_open_ask(&pool, &ask, turn, &tc_b, &now).await;
        // Another turn stays untouched.
        let other_turn = TurnId::new();
        let other_round = RoundId::new();
        let other_tc = ToolCallId::new();
        seed_round(&pool, &other_round, other_turn, 1, "running", &now).await;
        seed_tool_call(&pool, &other_tc, &other_round, 1, "running", &now).await;

        let mut tx = pool.begin().await.unwrap();
        let round_ids = execution
            .cancel_execution_in_tx(&mut tx, turn, &now)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(round_ids.len(), 2);
        for id in [tc_a, tc_b] {
            let (status, error) = tool_call_status_and_error(&pool, &id).await;
            assert_eq!(status, "canceled");
            assert_eq!(error.as_deref(), Some("USER_CANCEL"));
        }
        let (status, stop): (String, Option<String>) =
            sqlx::query_as("SELECT status, stop_reason FROM rounds WHERE id = ?")
                .bind(round_a.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "canceled");
        assert_eq!(stop.as_deref(), Some("user_cancel"));
        let (status, reason): (String, Option<String>) =
            sqlx::query_as("SELECT status, closure_reason FROM asks WHERE id = ?")
                .bind(ask.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "canceled");
        assert_eq!(reason.as_deref(), Some("user_cancel"));
        // The unrelated turn is untouched.
        let (status, _): (String, Option<String>) =
            sqlx::query_as("SELECT status, stop_reason FROM rounds WHERE id = ?")
                .bind(other_round.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "running");
        let (status, error) = tool_call_status_and_error(&pool, &other_tc).await;
        assert_eq!(status, "running");
        assert_eq!(error, None);
    }

    #[tokio::test]
    async fn unstarted_turns_are_those_without_any_round() {
        let (pool, execution, _temp) = test_execution().await;
        let now = now_utc_str();
        let started = TurnId::new();
        let unstarted = TurnId::new();
        seed_round(&pool, &RoundId::new(), started, 1, "running", &now).await;

        let candidates = HashSet::from([started, unstarted]);
        let mut tx = pool.begin().await.unwrap();
        let result = execution
            .unstarted_active_turn_ids_in_tx(&mut tx, &candidates)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        assert!(result.contains(&unstarted));
        assert!(!result.contains(&started));
    }

    #[tokio::test]
    async fn waiting_job_ids_orders_by_started_at() {
        let (pool, execution, _temp) = test_execution().await;
        let now = now_utc_str();
        let turn = TurnId::new();
        let round = RoundId::new();
        seed_round(&pool, &round, turn, 1, "running", &now).await;
        let job_b = JobId::new();
        let job_a = JobId::new();
        let tc_a = ToolCallId::new();
        let tc_b = ToolCallId::new();
        // job_b started before job_a; both are waiting.
        sqlx::query(
            "INSERT INTO tool_calls \
             (id, round_id, ord, tool_name, schema_version, input_json, status, actor_json, version, job_id, started_at) \
             VALUES (?, ?, 1, 'bash', 1, '{}', 'waiting', '{\"kind\":\"model\"}', 'v1', ?, ?)",
        )
        .bind(tc_a.to_string())
        .bind(round.to_string())
        .bind(job_a.to_string())
        .bind(format_utc(now_utc() + chrono::Duration::seconds(10)))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tool_calls \
             (id, round_id, ord, tool_name, schema_version, input_json, status, actor_json, version, job_id, started_at) \
             VALUES (?, ?, 2, 'bash', 1, '{}', 'waiting', '{\"kind\":\"model\"}', 'v1', ?, ?)",
        )
        .bind(tc_b.to_string())
        .bind(round.to_string())
        .bind(job_b.to_string())
        .bind(now_utc_str())
        .execute(&pool)
        .await
        .unwrap();

        let ids = execution.waiting_job_ids(10).await.unwrap();
        assert_eq!(ids, vec![job_b, job_a]);
    }

    #[tokio::test]
    async fn settle_marks_waiting_tool_call_from_terminal_job() {
        let (pool, execution, _temp) = test_execution().await;
        let now = now_utc_str();
        let turn = TurnId::new();
        let round = RoundId::new();
        seed_round(&pool, &round, turn, 1, "running", &now).await;
        let tool_call = ToolCallId::new();
        sqlx::query(
            "INSERT INTO tool_calls \
             (id, round_id, ord, tool_name, schema_version, input_json, status, actor_json, version, provider_call_id, started_at) \
             VALUES (?, ?, 1, 'bash', 1, '{\"command\":\"ls\"}', 'waiting', '{\"kind\":\"model\"}', 'v1', 'prov-1', ?)",
        )
        .bind(tool_call.to_string())
        .bind(round.to_string())
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();

        let job = job_projection(JobId::new(), tool_call, turn, JobStatus::Succeeded, &now);
        let mut tx = pool.begin().await.unwrap();
        let settlement: ToolCallSettlement = execution
            .settle_job_tool_call_in_tx(&mut tx, &job, &now)
            .await
            .unwrap()
            .expect("terminal job should settle the waiting call");
        tx.commit().await.unwrap();

        assert_eq!(settlement.tool_call_id, tool_call.to_string());
        assert_eq!(settlement.source_turn_id, turn.to_string());
        assert_eq!(settlement.provider_call_id, "prov-1");
        assert_eq!(settlement.tool_name, "bash");
        assert_eq!(settlement.status, ToolCallStatus::Succeeded);
        let (status, job_id): (String, Option<String>) =
            sqlx::query_as("SELECT status, job_id FROM tool_calls WHERE id = ?")
                .bind(tool_call.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "succeeded");
        assert_eq!(job_id, Some(job.id.to_string()));
    }

    #[tokio::test]
    async fn settle_ignores_nonterminal_and_job_id_mismatch() {
        let (pool, execution, _temp) = test_execution().await;
        let now = now_utc_str();
        let turn = TurnId::new();
        let round = RoundId::new();
        seed_round(&pool, &round, turn, 1, "running", &now).await;
        let tool_call = ToolCallId::new();
        sqlx::query(
            "INSERT INTO tool_calls \
             (id, round_id, ord, tool_name, schema_version, input_json, status, actor_json, version, provider_call_id, started_at) \
             VALUES (?, ?, 1, 'bash', 1, '{}', 'waiting', '{\"kind\":\"model\"}', 'v1', 'prov-1', ?)",
        )
        .bind(tool_call.to_string())
        .bind(round.to_string())
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();

        // A non-terminal job is never settled.
        let running_job = job_projection(JobId::new(), tool_call, turn, JobStatus::Running, &now);
        let mut tx = pool.begin().await.unwrap();
        let settled = execution
            .settle_job_tool_call_in_tx(&mut tx, &running_job, &now)
            .await
            .unwrap();
        assert!(settled.is_none());
        tx.commit().await.unwrap();
        let (status, _): (String, Option<String>) =
            sqlx::query_as("SELECT status, job_id FROM tool_calls WHERE id = ?")
                .bind(tool_call.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "waiting");

        // A terminal job for a different job id cannot settle the call.
        let recorded_job = JobId::new();
        let other_job = JobId::new();
        sqlx::query("UPDATE tool_calls SET job_id = ? WHERE id = ?")
            .bind(recorded_job.to_string())
            .bind(tool_call.to_string())
            .execute(&pool)
            .await
            .unwrap();
        let mismatched_job = job_projection(other_job, tool_call, turn, JobStatus::Succeeded, &now);
        let mut tx = pool.begin().await.unwrap();
        let settled = execution
            .settle_job_tool_call_in_tx(&mut tx, &mismatched_job, &now)
            .await
            .unwrap();
        assert!(settled.is_none());
        tx.commit().await.unwrap();
        let (status, _): (String, Option<String>) =
            sqlx::query_as("SELECT status, job_id FROM tool_calls WHERE id = ?")
                .bind(tool_call.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "waiting");
    }
}

