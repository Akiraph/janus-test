//! Round scheduling and Turn interruption/cancel transactions.
use super::*;

impl ExecutionInterface {
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
                    ended_at = ?, version = ? WHERE status = 'running'",
        )
        .bind(now)
        .bind(format!("v_{}", ToolCallId::new()))
        .execute(&mut *tx)
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
             WHERE status IN ('requested', 'running') \
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{ExecutionDependencies, ExecutionInterface};
    use futures_util::future::BoxFuture;
    use janus_infrastructure::{
        clock::now_utc_str,
        events::EventStore,
        id::{AsyncTaskId, LogStreamId, RoundId, RuntimeId, TerminalId, ToolCallId, TurnId},
        managed_storage::BlobStore,
        operations::OperationInterface,
        secrets::SecretCipher,
        state_broadcaster::StateBroadcaster,
    };
    use janus_models::interface::ModelsInterface;
    use janus_projects::interface::ProjectsInterface;
    use janus_runtime::interface::{
        AsyncTaskSpec, ExecutionResult, ExecutionSpec, ExecutorProcessHandle,
        ExecutorRuntimeHandle, ProcessCompletion, RuntimeError, RuntimeExecutor, RuntimeInterface,
        RuntimeSpec, TerminalSignal, TerminalSize, TerminalSpec,
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
        fn start_async_task<'a>(
            &'a self,
            _spec: AsyncTaskSpec,
            _log_stream_id: LogStreamId,
        ) -> BoxFuture<'a, Result<ExecutorProcessHandle, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn wait_async_task<'a>(
            &'a self,
            _id: AsyncTaskId,
            _executor_nonce: &'a str,
        ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn write_async_task_stdin<'a>(
            &'a self,
            _id: AsyncTaskId,
            _executor_nonce: &'a str,
            _input: Vec<u8>,
        ) -> BoxFuture<'a, Result<(), RuntimeError>> {
            Box::pin(async { Err(RuntimeError::RuntimeUnavailable) })
        }
        fn cancel_async_task<'a>(
            &'a self,
            _id: AsyncTaskId,
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
        let sessions = SessionsInterface::new(pool.clone(), events.clone(), blobs.clone());
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
        let second_running = ToolCallId::new();
        seed_tool_call(&pool, &requested, &round, 1, "requested", &now).await;
        seed_tool_call(&pool, &running, &round, 2, "running", &now).await;
        seed_tool_call(&pool, &second_running, &round, 3, "running", &now).await;

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
        for id in [running, second_running] {
            let (status, error) = tool_call_status_and_error(&pool, &id).await;
            assert_eq!(status, "lost");
            assert_eq!(error.as_deref(), Some("CONTROL_PLANE_RESTART"));
        }
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
        seed_tool_call(&pool, &tc_b, &round_b, 1, "running", &now).await;
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
}
