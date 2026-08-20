use std::{path::Path, sync::Arc, time::Duration};

use janus_infrastructure::clock::{format_utc, now_utc, now_utc_str};
use serde_json::json;
use sqlx::{FromRow, SqliteConnection, SqlitePool};

use super::{
    interface::{
        AsyncTaskProjection, AsyncTaskSpec, AsyncTaskStatus, ExecutionResult, ExecutionSpec,
        ExitSummary, LogCursor, LogRange, ProcessCompletion, ResourceLimits, ResourceUsage,
        RuntimeError, RuntimeExecutor, RuntimeProjection, RuntimeScope, RuntimeSpec, RuntimeStatus,
        TerminalProjection, TerminalSignal, TerminalSize, TerminalSpec, TerminalStatus,
        TerminalTicket, TerminalTicketRequest,
    },
    log_store::LogStore,
};
use janus_infrastructure::{
    events::{EventStore, EventType, NewEvent},
    id::{AsyncTaskId, LogStreamId, ProjectId, RuntimeId, SessionId, TerminalId, TurnId},
    secrets::{purpose_hash, random_token},
    unit_of_work::{UnitOfWork, UnitOfWorkTransaction},
};

#[derive(Clone)]
pub struct RuntimeInterface {
    pool: SqlitePool,
    unit_of_work: UnitOfWork,
    logs: LogStore,
    executor: Arc<dyn RuntimeExecutor>,
    /// Broadcast of AsyncTask ids that just reached a durable terminal status.
    /// The application delivery worker subscribes and opens a new Turn.
    async_task_settled_tx: tokio::sync::broadcast::Sender<AsyncTaskId>,
}

#[derive(FromRow)]
struct RuntimeRow {
    id: String,
    scope_kind: String,
    scope_id: String,
    executor_nonce: String,
    limits_json: String,
    status: String,
    version: String,
    created_at: String,
    updated_at: String,
    stopped_at: Option<String>,
}

#[derive(FromRow)]
struct AsyncTaskRow {
    id: String,
    runtime_id: String,
    session_id: String,
    initiated_by_tool_call_id: String,
    controlling_turn_id: String,
    command_summary: String,
    executor_nonce: String,
    log_stream_id: String,
    status: String,
    exit_json: Option<String>,
    usage_json: String,
    version: String,
    created_at: String,
    started_at: Option<String>,
    ended_at: Option<String>,
}

#[derive(FromRow)]
struct TerminalRow {
    id: String,
    runtime_id: String,
    owner_kind: String,
    owner_id: String,
    executor_nonce: String,
    cols: i64,
    rows: i64,
    scrollback_stream_id: String,
    status: String,
    exit_json: Option<String>,
    version: String,
    created_at: String,
    updated_at: String,
    ended_at: Option<String>,
}

#[derive(FromRow)]
struct TerminalTicketRow {
    terminal_id: String,
    actor_id: String,
    origin: String,
    expires_at: String,
    consumed_at: Option<String>,
    revoked_at: Option<String>,
}

impl RuntimeInterface {
    pub fn new(
        pool: SqlitePool,
        events: EventStore,
        data_root: &Path,
        executor: Arc<dyn RuntimeExecutor>,
    ) -> Self {
        let (async_task_settled_tx, _) = tokio::sync::broadcast::channel(64);
        let unit_of_work = UnitOfWork::new(pool.clone(), events);
        Self {
            logs: LogStore::new(pool.clone(), data_root),
            pool,
            unit_of_work,
            executor,
            async_task_settled_tx,
        }
    }

    /// Subscribe to durable AsyncTask terminal-state notifications.
    pub fn subscribe_async_task_settled(&self) -> tokio::sync::broadcast::Receiver<AsyncTaskId> {
        self.async_task_settled_tx.subscribe()
    }

    pub async fn has_unfinished_async_tasks_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: TurnId,
    ) -> Result<bool, RuntimeError> {
        Ok(self.unfinished_async_task_count_in_tx(tx, turn_id).await? > 0)
    }

    pub async fn unfinished_async_task_count(&self, turn_id: TurnId) -> Result<i64, RuntimeError> {
        sqlx::query_scalar(
            "SELECT COUNT(1) FROM async_tasks \
             WHERE controlling_turn_id = ? AND status IN ('queued', 'running')",
        )
        .bind(turn_id.to_string())
        .fetch_one(&self.pool)
        .await
        .map_err(storage_error)
    }

    /// Finite AsyncTasks still controlled by `turn_id` that have not reached a terminal
    /// status. Used by application Cancel to bound Runtime cancellation before
    /// settling the Turn.
    pub async fn unfinished_async_tasks_for_turn(
        &self,
        turn_id: TurnId,
    ) -> Result<Vec<AsyncTaskProjection>, RuntimeError> {
        sqlx::query_as::<_, AsyncTaskRow>(&format!(
            "{} WHERE controlling_turn_id = ? \
             AND status IN ('queued', 'running') \
             ORDER BY created_at, id",
            ASYNC_TASK_SELECT
        ))
        .bind(turn_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?
        .into_iter()
        .map(async_task_projection)
        .collect()
    }

    pub async fn unfinished_async_task_count_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: TurnId,
    ) -> Result<i64, RuntimeError> {
        sqlx::query_scalar(
            "SELECT COUNT(1) FROM async_tasks \
             WHERE controlling_turn_id = ? AND status IN ('queued', 'running')",
        )
        .bind(turn_id.to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_error)
    }

    pub async fn transfer_unfinished_async_tasks_in_tx(
        &self,
        tx: &mut SqliteConnection,
        from_turn_id: TurnId,
        to_turn_id: TurnId,
    ) -> Result<u64, RuntimeError> {
        sqlx::query(
            "UPDATE async_tasks SET controlling_turn_id = ?, version = ? \
             WHERE controlling_turn_id = ? AND status IN ('queued', 'running')",
        )
        .bind(to_turn_id.to_string())
        .bind(new_version())
        .bind(from_turn_id.to_string())
        .execute(&mut *tx)
        .await
        .map(|result| result.rows_affected())
        .map_err(storage_error)
    }

    pub async fn terminal_async_tasks_for_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: TurnId,
    ) -> Result<Vec<AsyncTaskProjection>, RuntimeError> {
        sqlx::query_as::<_, AsyncTaskRow>(&format!(
            "{} WHERE controlling_turn_id = ? \
             AND status IN ('succeeded', 'failed', 'canceled', 'lost') \
             ORDER BY created_at, id",
            ASYNC_TASK_SELECT
        ))
        .bind(turn_id.to_string())
        .fetch_all(&mut *tx)
        .await
        .map_err(storage_error)?
        .into_iter()
        .map(async_task_projection)
        .collect()
    }

    pub async fn ensure_runtime(
        &self,
        spec: &RuntimeSpec,
    ) -> Result<RuntimeProjection, RuntimeError> {
        if let Some(existing) = self.current_runtime(spec.scope()).await? {
            if existing.id != spec.id() {
                return Err(RuntimeError::RuntimeUnavailable);
            }
            if matches!(
                existing.status,
                RuntimeStatus::Ready | RuntimeStatus::Starting
            ) {
                let handle = self.executor.ensure_runtime(spec).await?;
                let same_nonce = existing.status == RuntimeStatus::Ready
                    && self.runtime_nonce(existing.id).await.ok().as_deref()
                        == Some(handle.executor_nonce.as_str());
                if same_nonce {
                    return Ok(existing);
                }
                sqlx::query(
                    "UPDATE runtimes SET executor_identity = ?, executor_nonce = ?, status = 'ready', \
                     version = ?, updated_at = ? WHERE id = ? AND status IN ('starting', 'ready')",
                )
                .bind(handle.executor_identity)
                .bind(handle.executor_nonce)
                .bind(new_version())
                .bind(now_utc_str())
                .bind(existing.id.to_string())
                .execute(&self.pool)
                .await
                .map_err(storage_error)?;
                return self.runtime(existing.id).await;
            }
            return Err(RuntimeError::RuntimeUnavailable);
        }
        let now = now_utc_str();
        let placeholder_nonce = format!("pending-{}", spec.id());
        sqlx::query(
            "INSERT INTO runtimes \
             (id, scope_kind, scope_id, executor_nonce, limits_json, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, 'starting', ?, ?, ?)",
        )
        .bind(spec.id().to_string())
        .bind(spec.scope().kind())
        .bind(spec.scope().id())
        .bind(placeholder_nonce)
        .bind(serde_json::to_string(spec.limits()).map_err(storage_error)?)
        .bind(new_version())
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;

        match self.executor.ensure_runtime(spec).await {
            Ok(handle) => {
                sqlx::query(
                    "UPDATE runtimes SET executor_identity = ?, executor_nonce = ?, status = 'ready', \
                     version = ?, updated_at = ? WHERE id = ? AND status = 'starting'",
                )
                .bind(handle.executor_identity)
                .bind(handle.executor_nonce)
                .bind(new_version())
                .bind(now_utc_str())
                .bind(spec.id().to_string())
                .execute(&self.pool)
                .await
                .map_err(storage_error)?;
                self.runtime(spec.id()).await
            }
            Err(error) => {
                let _ = sqlx::query(
                    "UPDATE runtimes SET status = 'failed', stop_reason = ?, version = ?, \
                     updated_at = ?, stopped_at = ? WHERE id = ?",
                )
                .bind(error.code().as_str())
                .bind(new_version())
                .bind(now_utc_str())
                .bind(now_utc_str())
                .bind(spec.id().to_string())
                .execute(&self.pool)
                .await;
                Err(error)
            }
        }
    }

    pub async fn stop_runtime(&self, id: RuntimeId) -> Result<RuntimeProjection, RuntimeError> {
        let nonce = self.runtime_nonce(id).await?;
        let now = now_utc_str();
        sqlx::query(
            "UPDATE runtimes SET status = 'stopping', version = ?, updated_at = ? \
             WHERE id = ? AND status IN ('starting', 'ready')",
        )
        .bind(new_version())
        .bind(&now)
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        self.executor.stop_runtime(id, &nonce).await?;
        sqlx::query(
            "UPDATE runtimes SET status = 'stopped', stop_reason = 'requested', version = ?, \
             updated_at = ?, stopped_at = ? WHERE id = ?",
        )
        .bind(new_version())
        .bind(&now)
        .bind(&now)
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        self.runtime(id).await
    }

    pub async fn execute_sync(&self, spec: ExecutionSpec) -> Result<ExecutionResult, RuntimeError> {
        let stream = self
            .logs
            .create(
                super::interface::LogOwnerKind::Sync,
                &spec.runtime_id().to_string(),
            )
            .await?;
        self.executor.execute_sync(spec, stream.id).await
    }

    pub async fn start_async_task(
        &self,
        spec: AsyncTaskSpec,
    ) -> Result<AsyncTaskProjection, RuntimeError> {
        let async_task_id = spec.id;
        let runtime_nonce = self.runtime_nonce(spec.execution.runtime_id()).await?;
        let log = self
            .logs
            .create(
                super::interface::LogOwnerKind::AsyncTask,
                &spec.id.to_string(),
            )
            .await?;
        let now = now_utc_str();
        sqlx::query(
            "INSERT INTO async_tasks \
             (id, runtime_id, session_id, initiated_by_tool_call_id, controlling_turn_id, \
              command_summary, executor_nonce, log_stream_id, status, usage_json, version, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'queued', ?, ?, ?)",
        )
        .bind(async_task_id.to_string())
        .bind(spec.execution.runtime_id().to_string())
        .bind(spec.session_id.to_string())
        .bind(spec.initiated_by_tool_call_id.to_string())
        .bind(spec.controlling_turn_id.to_string())
        .bind(command_summary(&spec))
        .bind(&runtime_nonce)
        .bind(log.id.to_string())
        .bind(serde_json::to_string(&ResourceUsage::default()).map_err(storage_error)?)
        .bind(new_version())
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        if self
            .async_task_cancellation_requested(async_task_id)
            .await?
        {
            self.finalize_async_task(
                async_task_id,
                ProcessCompletion {
                    exit: ExitSummary {
                        exit_code: None,
                        signal: Some("canceled_before_start".into()),
                    },
                    duration_ms: 0,
                    usage: ResourceUsage::default(),
                },
                Some(AsyncTaskStatus::Canceled),
            )
            .await?;
            return self.async_task(async_task_id).await;
        }
        match self.executor.start_async_task(spec, log.id).await {
            Ok(handle) if handle.executor_nonce == runtime_nonce => {
                let mut work = self.unit_of_work.begin().await.map_err(storage_error)?;
                let started_at = now_utc_str();
                let changed = sqlx::query(
                    "UPDATE async_tasks SET executor_process_identity = ?, status = 'running', \
                     version = ?, started_at = ? WHERE id = ? AND status = 'queued' \
                       AND cancellation_requested_at IS NULL",
                )
                .bind(handle.process_identity)
                .bind(new_version())
                .bind(&started_at)
                .bind(async_task_id.to_string())
                .execute(work.connection())
                .await
                .map_err(storage_error)?
                .rows_affected();
                if changed == 0 {
                    work.rollback().await.map_err(storage_error)?;
                    return self
                        .cancel_started_async_task(async_task_id, &runtime_nonce)
                        .await;
                }
                let async_task = self
                    .append_async_task_changed_in_tx(&mut work, async_task_id)
                    .await?;
                work.commit().await.map_err(storage_error)?;
                let this = self.clone();
                let async_task_id = async_task.id;
                tokio::spawn(async move {
                    match this
                        .executor
                        .wait_async_task(async_task_id, &runtime_nonce)
                        .await
                    {
                        Ok(completion) => {
                            if let Err(error) = this
                                .finalize_async_task(async_task_id, completion, None)
                                .await
                            {
                                tracing::warn!(%error, %async_task_id, "finalize AsyncTask after wait failed");
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, %async_task_id, "wait AsyncTask failed; marking it lost");
                            if let Err(mark_error) = this
                                .mark_async_task_lost(async_task_id, "wait_failed")
                                .await
                            {
                                tracing::error!(
                                    %mark_error,
                                    %async_task_id,
                                    "mark AsyncTask lost after wait failure failed"
                                );
                            }
                        }
                    }
                });
                Ok(async_task)
            }
            Ok(handle) => {
                let _ = tokio::time::timeout(
                    ASYNC_TASK_CANCEL_TIMEOUT,
                    self.executor
                        .cancel_async_task(async_task_id, &handle.executor_nonce),
                )
                .await;
                self.mark_async_task_lost(async_task_id, "executor_nonce_mismatch")
                    .await?;
                Err(RuntimeError::RuntimeUnavailable)
            }
            Err(error) => {
                self.mark_async_task_lost(async_task_id, "start_failed")
                    .await?;
                Err(error)
            }
        }
    }

    pub async fn write_async_task_stdin(
        &self,
        id: AsyncTaskId,
        input: Vec<u8>,
    ) -> Result<(), RuntimeError> {
        let nonce = self.async_task_nonce(id).await?;
        self.executor
            .write_async_task_stdin(id, &nonce, input)
            .await
    }

    pub async fn cancel_async_task(
        &self,
        id: AsyncTaskId,
    ) -> Result<AsyncTaskProjection, RuntimeError> {
        sqlx::query(
            "UPDATE async_tasks SET cancellation_requested_at = ?, version = ? \
             WHERE id = ? AND status IN ('queued', 'running') \
               AND cancellation_requested_at IS NULL",
        )
        .bind(now_utc_str())
        .bind(new_version())
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        let deadline = tokio::time::Instant::now() + ASYNC_TASK_CANCEL_TIMEOUT;
        loop {
            let async_task = self.async_task(id).await?;
            match async_task.status {
                AsyncTaskStatus::Running => {
                    let nonce = self.async_task_nonce(id).await?;
                    return self.cancel_started_async_task(id, &nonce).await;
                }
                status if status.is_terminal() => return Ok(async_task),
                AsyncTaskStatus::Queued if tokio::time::Instant::now() >= deadline => {
                    self.mark_async_task_lost(id, "cancel_before_start_unconfirmed")
                        .await?;
                    return self.async_task(id).await;
                }
                AsyncTaskStatus::Queued => {
                    tokio::time::sleep(ASYNC_TASK_CANCEL_POLL_INTERVAL).await
                }
                _ => unreachable!("all AsyncTask statuses are covered"),
            }
        }
    }

    pub async fn runtime(&self, id: RuntimeId) -> Result<RuntimeProjection, RuntimeError> {
        runtime_projection(self.runtime_row(id).await?)
    }

    pub async fn current_runtime(
        &self,
        scope: RuntimeScope,
    ) -> Result<Option<RuntimeProjection>, RuntimeError> {
        let row = sqlx::query_as::<_, RuntimeRow>(
            "SELECT id, scope_kind, scope_id, executor_nonce, limits_json, \
             status, version, created_at, updated_at, stopped_at \
             FROM runtimes WHERE scope_kind = ? AND scope_id = ? \
             AND status IN ('starting', 'ready', 'stopping')",
        )
        .bind(scope.kind())
        .bind(scope.id())
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        row.map(runtime_projection).transpose()
    }

    pub async fn live_runtimes(&self) -> Result<Vec<RuntimeProjection>, RuntimeError> {
        sqlx::query_as::<_, RuntimeRow>(
            "SELECT id, scope_kind, scope_id, executor_nonce, limits_json, \
             status, version, created_at, updated_at, stopped_at \
             FROM runtimes WHERE status IN ('starting', 'ready', 'stopping') \
             ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?
        .into_iter()
        .map(runtime_projection)
        .collect()
    }

    pub async fn delete_session_log_files(
        &self,
        session_id: SessionId,
    ) -> Result<(), RuntimeError> {
        let ids = sqlx::query_scalar::<_, String>(SESSION_LOG_STREAMS)
            .bind(session_id.to_string())
            .bind(session_id.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(storage_error)?
            .into_iter()
            .map(|id| id.parse().map_err(storage_error))
            .collect::<Result<Vec<LogStreamId>, _>>()?;
        self.logs.delete_files(&ids).await
    }

    pub async fn delete_project_log_files(
        &self,
        project_id: ProjectId,
    ) -> Result<(), RuntimeError> {
        let ids = sqlx::query_scalar::<_, String>(PROJECT_LOG_STREAMS)
            .bind(project_id.to_string())
            .bind(project_id.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(storage_error)?
            .into_iter()
            .map(|id| id.parse().map_err(storage_error))
            .collect::<Result<Vec<LogStreamId>, _>>()?;
        self.logs.delete_files(&ids).await
    }

    pub async fn delete_project_resources(
        &self,
        project_id: ProjectId,
    ) -> Result<(), RuntimeError> {
        let project_id = project_id.to_string();
        let log_ids = sqlx::query_scalar::<_, String>(PROJECT_LOG_STREAMS)
            .bind(&project_id)
            .bind(&project_id)
            .fetch_all(&self.pool)
            .await
            .map_err(storage_error)?;
        let mut work = self.unit_of_work.begin().await.map_err(storage_error)?;
        sqlx::query(
            "DELETE FROM terminals WHERE runtime_id IN \
             (SELECT id FROM runtimes WHERE scope_kind = 'project' AND scope_id = ?)",
        )
        .bind(&project_id)
        .execute(work.connection())
        .await
        .map_err(storage_error)?;
        sqlx::query("DELETE FROM runtimes WHERE scope_kind = 'project' AND scope_id = ?")
            .bind(&project_id)
            .execute(work.connection())
            .await
            .map_err(storage_error)?;
        for log_id in log_ids {
            sqlx::query("DELETE FROM log_streams WHERE id = ?")
                .bind(log_id)
                .execute(work.connection())
                .await
                .map_err(storage_error)?;
        }
        work.commit().await.map_err(storage_error)
    }

    pub async fn delete_session_resources_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
    ) -> Result<(), RuntimeError> {
        let session_id = session_id.to_string();
        let log_ids = sqlx::query_scalar::<_, String>(SESSION_LOG_STREAMS)
            .bind(&session_id)
            .bind(&session_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(storage_error)?;
        sqlx::query("DELETE FROM async_tasks WHERE session_id = ?")
            .bind(&session_id)
            .execute(&mut *tx)
            .await
            .map_err(storage_error)?;
        for log_id in log_ids {
            sqlx::query("DELETE FROM log_streams WHERE id = ?")
                .bind(log_id)
                .execute(&mut *tx)
                .await
                .map_err(storage_error)?;
        }
        Ok(())
    }

    pub async fn async_task(&self, id: AsyncTaskId) -> Result<AsyncTaskProjection, RuntimeError> {
        async_task_projection(self.async_task_row(id).await?)
    }

    pub async fn async_tasks(
        &self,
        limit: usize,
    ) -> Result<Vec<AsyncTaskProjection>, RuntimeError> {
        sqlx::query_as::<_, AsyncTaskRow>(&format!(
            "{} ORDER BY created_at DESC, id DESC LIMIT ?",
            ASYNC_TASK_SELECT
        ))
        .bind(i64::try_from(limit.clamp(1, 1000)).unwrap_or(1000))
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?
        .into_iter()
        .map(async_task_projection)
        .collect()
    }

    pub async fn undelivered_terminal_task_ids(
        &self,
        limit: usize,
    ) -> Result<Vec<AsyncTaskId>, RuntimeError> {
        sqlx::query_scalar::<_, String>(
            "SELECT id FROM async_tasks \
             WHERE status IN ('succeeded', 'failed', 'canceled', 'lost') \
               AND delivery_completed_at IS NULL \
             ORDER BY ended_at, id LIMIT ?",
        )
        .bind(i64::try_from(limit.clamp(1, 1000)).unwrap_or(1000))
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?
        .into_iter()
        .map(|id| id.parse().map_err(storage_error))
        .collect()
    }

    pub async fn claim_task_delivery_in_tx(
        &self,
        tx: &mut SqliteConnection,
        id: AsyncTaskId,
    ) -> Result<bool, RuntimeError> {
        let changed = sqlx::query(
            "UPDATE async_tasks SET delivery_claimed_at = ? \
             WHERE id = ? AND status IN ('succeeded', 'failed', 'canceled', 'lost') \
               AND delivery_completed_at IS NULL AND delivery_claimed_at IS NULL",
        )
        .bind(now_utc_str())
        .bind(id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?
        .rows_affected();
        Ok(changed == 1)
    }

    pub async fn complete_task_delivery_in_tx(
        &self,
        tx: &mut SqliteConnection,
        id: AsyncTaskId,
    ) -> Result<(), RuntimeError> {
        sqlx::query(
            "UPDATE async_tasks SET delivery_completed_at = ? \
             WHERE id = ? AND delivery_completed_at IS NULL",
        )
        .bind(now_utc_str())
        .bind(id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        Ok(())
    }

    pub async fn log_range(
        &self,
        id: LogStreamId,
        after: LogCursor,
        limit_bytes: usize,
    ) -> Result<LogRange, RuntimeError> {
        self.logs.read(id, after, limit_bytes).await
    }

    pub async fn create_terminal(
        &self,
        spec: TerminalSpec,
    ) -> Result<TerminalProjection, RuntimeError> {
        let runtime_nonce = self.runtime_nonce(spec.runtime_id).await?;
        let scrollback = self
            .logs
            .create(
                super::interface::LogOwnerKind::Terminal,
                &spec.id.to_string(),
            )
            .await?;
        let now = now_utc_str();
        sqlx::query(
            "INSERT INTO terminals \
             (id, runtime_id, owner_kind, owner_id, executor_nonce, cols, rows, \
              scrollback_stream_id, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'starting', ?, ?, ?)",
        )
        .bind(spec.id.to_string())
        .bind(spec.runtime_id.to_string())
        .bind("project")
        .bind(spec.project_id.to_string())
        .bind(&runtime_nonce)
        .bind(i64::from(spec.size.cols))
        .bind(i64::from(spec.size.rows))
        .bind(scrollback.id.to_string())
        .bind(new_version())
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        let terminal_id = spec.id;
        match self.executor.start_terminal(spec, scrollback.id).await {
            Ok(handle) if handle.executor_nonce == runtime_nonce => {
                let mut work = self.unit_of_work.begin().await.map_err(storage_error)?;
                let changed = sqlx::query(
                    "UPDATE terminals SET executor_pty_identity = ?, status = 'running', \
                     version = ?, updated_at = ? WHERE id = ? AND status = 'starting'",
                )
                .bind(&handle.process_identity)
                .bind(new_version())
                .bind(now_utc_str())
                .bind(terminal_id.to_string())
                .execute(work.connection())
                .await
                .map_err(storage_error)?
                .rows_affected();
                let terminal = if changed != 0 {
                    let terminal = self
                        .append_terminal_changed_in_tx(&mut work, terminal_id)
                        .await?;
                    work.commit().await.map_err(storage_error)?;
                    terminal
                } else {
                    work.rollback().await.map_err(storage_error)?;
                    self.terminal(terminal_id).await?
                };
                let this = self.clone();
                let terminal_id = terminal.id;
                let nonce = runtime_nonce;
                tokio::spawn(async move {
                    match this.executor.await_terminal_exit(terminal_id, &nonce).await {
                        Ok(completion) => {
                            if let Err(error) =
                                this.finalize_terminal(terminal_id, completion).await
                            {
                                tracing::warn!(
                                    %error,
                                    %terminal_id,
                                    "finalize Terminal after wait failed"
                                );
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                %terminal_id,
                                "wait Terminal failed; marking it lost"
                            );
                            if let Err(mark_error) =
                                this.mark_terminal_lost(terminal_id, "wait_failed").await
                            {
                                tracing::error!(
                                    %mark_error,
                                    %terminal_id,
                                    "mark Terminal lost after wait failure failed"
                                );
                            }
                        }
                    }
                });
                Ok(terminal)
            }
            Ok(_) => {
                self.mark_terminal_failed(terminal_id).await?;
                Err(RuntimeError::RuntimeUnavailable)
            }
            Err(error) => {
                self.mark_terminal_failed(terminal_id).await?;
                Err(error)
            }
        }
    }

    pub async fn terminal(&self, id: TerminalId) -> Result<TerminalProjection, RuntimeError> {
        let mut projection = terminal_projection(self.terminal_row(id).await?)?;
        self.attach_scrollback_cursors(&mut projection).await;
        Ok(projection)
    }

    pub async fn list_terminals(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<TerminalProjection>, RuntimeError> {
        let mut projections = sqlx::query_as::<_, TerminalRow>(&format!(
            "{} WHERE owner_kind = 'project' AND owner_id = ? ORDER BY created_at",
            TERMINAL_SELECT
        ))
        .bind(project_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?
        .into_iter()
        .map(terminal_projection)
        .collect::<Result<Vec<_>, RuntimeError>>()?;
        for projection in &mut projections {
            self.attach_scrollback_cursors(projection).await;
        }
        Ok(projections)
    }

    async fn attach_scrollback_cursors(&self, terminal: &mut TerminalProjection) {
        if let Ok(stream) = self.logs.projection(terminal.scrollback_stream_id).await {
            terminal.first_cursor = stream.first_cursor;
            terminal.next_cursor = stream.next_cursor;
        }
    }

    /// Issue a single-use Terminal access ticket. The raw token is returned once
    /// to the caller; only a hash is persisted. Tickets expire after
    /// [`Self::TICKET_TTL`] and are bound to the requesting actor and Origin.
    pub async fn issue_terminal_ticket(
        &self,
        request: TerminalTicketRequest,
    ) -> Result<TerminalTicket, RuntimeError> {
        let row = self.terminal_row(request.terminal_id).await?;
        if !matches!(
            parse_terminal_status(&row.status)?,
            TerminalStatus::Running | TerminalStatus::Starting
        ) {
            return Err(RuntimeError::TerminalNotWritable(request.terminal_id));
        }
        let id = TerminalId::new().to_string();
        let token = random_token(32);
        let token_hash = purpose_hash("terminal-ticket", &token);
        let now = now_utc();
        let expires_at = format_utc(now + TICKET_TTL);
        sqlx::query(
            "INSERT INTO runtime_access_tickets \
             (id, terminal_id, token_hash, actor_id, origin, expires_at, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(row.id)
        .bind(&token_hash)
        .bind(&request.actor_id)
        .bind(&request.origin)
        .bind(&expires_at)
        .bind(format_utc(now))
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(TerminalTicket {
            terminal_id: request.terminal_id,
            token,
            expires_at,
        })
    }

    /// Consume a Terminal access ticket atomically. Validates origin, expiry,
    /// and that the ticket has not already been consumed or revoked, then marks
    /// it consumed. Returns the Terminal id the ticket grants access to.
    pub async fn consume_terminal_ticket(
        &self,
        token: &str,
        actor_id: &str,
        origin: &str,
    ) -> Result<TerminalId, RuntimeError> {
        let token_hash = purpose_hash("terminal-ticket", token);
        let row = sqlx::query_as::<_, TerminalTicketRow>(
            "SELECT terminal_id, actor_id, origin, expires_at, consumed_at, revoked_at \
             FROM runtime_access_tickets WHERE token_hash = ?",
        )
        .bind(&token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?
        .ok_or(RuntimeError::TerminalTicketInvalid)?;
        let now = now_utc();
        if row.consumed_at.is_some() || row.revoked_at.is_some() {
            return Err(RuntimeError::TerminalTicketInvalid);
        }
        if row.actor_id != actor_id || row.origin != origin {
            return Err(RuntimeError::TerminalTicketInvalid);
        }
        if parse_iso(&row.expires_at)? <= now {
            return Err(RuntimeError::TerminalTicketInvalid);
        }
        let changed = sqlx::query(
            "UPDATE runtime_access_tickets SET consumed_at = ? \
             WHERE token_hash = ? AND consumed_at IS NULL AND revoked_at IS NULL",
        )
        .bind(format_utc(now))
        .bind(&token_hash)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?
        .rows_affected();
        if changed == 0 {
            return Err(RuntimeError::TerminalTicketInvalid);
        }
        row.terminal_id
            .parse()
            .map_err(|_| RuntimeError::TerminalTicketInvalid)
    }

    pub async fn write_terminal_input(
        &self,
        id: TerminalId,
        input: Vec<u8>,
    ) -> Result<(), RuntimeError> {
        let nonce = self.terminal_nonce(id).await?;
        self.executor.write_terminal_input(id, &nonce, input).await
    }

    pub async fn resize_terminal(
        &self,
        id: TerminalId,
        size: TerminalSize,
    ) -> Result<TerminalProjection, RuntimeError> {
        let nonce = self.terminal_nonce(id).await?;
        self.executor.resize_terminal(id, &nonce, size).await?;
        let mut work = self.unit_of_work.begin().await.map_err(storage_error)?;
        sqlx::query(
            "UPDATE terminals SET cols = ?, rows = ?, version = ?, updated_at = ? \
             WHERE id = ? AND status IN ('starting', 'running')",
        )
        .bind(i64::from(size.cols))
        .bind(i64::from(size.rows))
        .bind(new_version())
        .bind(now_utc_str())
        .bind(id.to_string())
        .execute(work.connection())
        .await
        .map_err(storage_error)?;
        let terminal = self.append_terminal_changed_in_tx(&mut work, id).await?;
        work.commit().await.map_err(storage_error)?;
        Ok(terminal)
    }

    pub async fn signal_terminal(
        &self,
        id: TerminalId,
        signal: TerminalSignal,
    ) -> Result<(), RuntimeError> {
        let nonce = self.terminal_nonce(id).await?;
        self.executor.signal_terminal(id, &nonce, signal).await
    }

    pub async fn close_terminal(&self, id: TerminalId) -> Result<TerminalProjection, RuntimeError> {
        let nonce = self.terminal_nonce(id).await?;
        sqlx::query(
            "UPDATE terminals SET status = 'closing', version = ?, updated_at = ? \
             WHERE id = ? AND status IN ('starting', 'running')",
        )
        .bind(new_version())
        .bind(now_utc_str())
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        let completion = self.executor.close_terminal(id, &nonce).await?;
        self.finalize_terminal(id, completion).await?;
        self.terminal(id).await
    }

    pub async fn terminal_scrollback(
        &self,
        id: TerminalId,
        after: LogCursor,
        limit_bytes: usize,
    ) -> Result<LogRange, RuntimeError> {
        let row = self.terminal_row(id).await?;
        let stream_id: LogStreamId = row.scrollback_stream_id.parse().map_err(storage_error)?;
        self.logs.read(stream_id, after, limit_bytes).await
    }

    async fn finalize_terminal(
        &self,
        id: TerminalId,
        completion: ProcessCompletion,
    ) -> Result<(), RuntimeError> {
        // Closing -> Exited is the only durable transition here; the recorded
        // completion decides the final projection.
        let status = TerminalStatus::Exited;
        let mut work = self.unit_of_work.begin().await.map_err(storage_error)?;
        let changed = sqlx::query(
            "UPDATE terminals SET status = ?, exit_json = ?, version = ?, updated_at = ?, \
             ended_at = ? WHERE id = ? AND status IN ('starting', 'running', 'closing')",
        )
        .bind(terminal_status_str(status))
        .bind(serde_json::to_string(&completion.exit).map_err(storage_error)?)
        .bind(new_version())
        .bind(now_utc_str())
        .bind(now_utc_str())
        .bind(id.to_string())
        .execute(work.connection())
        .await
        .map_err(storage_error)?
        .rows_affected();
        if changed != 0 {
            self.append_terminal_changed_in_tx(&mut work, id).await?;
            work.commit().await.map_err(storage_error)?;
        } else {
            work.rollback().await.map_err(storage_error)?;
        }
        Ok(())
    }

    async fn mark_terminal_failed(&self, id: TerminalId) -> Result<(), RuntimeError> {
        sqlx::query(
            "UPDATE terminals SET status = 'failed', exit_json = ?, version = ?, updated_at = ?, \
             ended_at = ? WHERE id = ? AND status = 'starting'",
        )
        .bind(json!({"exit_code": null, "signal": "start_failed"}).to_string())
        .bind(new_version())
        .bind(now_utc_str())
        .bind(now_utc_str())
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        let _ = self
            .logs
            .close(
                self.terminal_row(id)
                    .await?
                    .scrollback_stream_id
                    .parse()
                    .map_err(storage_error)?,
            )
            .await;
        Ok(())
    }

    async fn append_terminal_changed_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        id: TerminalId,
    ) -> Result<TerminalProjection, RuntimeError> {
        let row = sqlx::query_as::<_, TerminalRow>(&format!("{} WHERE id = ?", TERMINAL_SELECT))
            .bind(id.to_string())
            .fetch_one(work.connection())
            .await
            .map_err(storage_error)?;
        let mut terminal = terminal_projection(row)?;
        if let Some((first_cursor, next_cursor)) = sqlx::query_as::<_, (i64, i64)>(
            "SELECT first_cursor, next_cursor FROM log_streams WHERE id = ?",
        )
        .bind(terminal.scrollback_stream_id.to_string())
        .fetch_optional(work.connection())
        .await
        .map_err(storage_error)?
        {
            terminal.first_cursor =
                LogCursor::new(u64::try_from(first_cursor).map_err(storage_error)?);
            terminal.next_cursor =
                LogCursor::new(u64::try_from(next_cursor).map_err(storage_error)?);
        }
        work.append_event(NewEvent {
            event_type: EventType::TerminalChanged,
            actor: json!({"kind": "runtime_system"}),
            resource: Some(json!({"kind": "terminal", "id": terminal.id})),
            correlation_id: format!("runtime-terminal-{}", terminal.id),
            causation_id: None,
            payload: json!({
                "id": terminal.id,
                "project_id": terminal.project_id,
                "status": terminal.status,
                "next_cursor": terminal.next_cursor,
                "version": terminal.version,
            }),
        })
        .await
        .map_err(storage_error)?;
        Ok(terminal)
    }

    pub async fn recover_uncertain(&self) -> Result<(), RuntimeError> {
        let now = now_utc_str();
        let mut work = self.unit_of_work.begin().await.map_err(storage_error)?;
        self.recover_uncertain_in_tx(&mut work, &now).await?;
        work.commit().await.map_err(storage_error)
    }

    pub async fn recover_uncertain_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        now: &str,
    ) -> Result<(), RuntimeError> {
        let runtime_ids = sqlx::query_scalar::<_, String>(
            "SELECT id FROM runtimes \
             WHERE status IN ('starting', 'ready', 'stopping') ORDER BY created_at, id",
        )
        .fetch_all(work.connection())
        .await
        .map_err(storage_error)?
        .into_iter()
        .map(|id| id.parse::<RuntimeId>().map_err(storage_error))
        .collect::<Result<Vec<_>, _>>()?;
        let async_task_ids = sqlx::query_scalar::<_, String>(
            "SELECT id FROM async_tasks WHERE status IN ('queued', 'running') ORDER BY created_at, id",
        )
        .fetch_all(work.connection())
        .await
        .map_err(storage_error)?
        .into_iter()
        .map(|id| id.parse::<AsyncTaskId>().map_err(storage_error))
        .collect::<Result<Vec<_>, _>>()?;
        let terminal_ids = sqlx::query_scalar::<_, String>(
            "SELECT id FROM terminals \
             WHERE status IN ('starting', 'running', 'closing') \
             ORDER BY created_at, id",
        )
        .fetch_all(work.connection())
        .await
        .map_err(storage_error)?
        .into_iter()
        .map(|id| id.parse::<TerminalId>().map_err(storage_error))
        .collect::<Result<Vec<_>, _>>()?;

        sqlx::query(
            "UPDATE runtimes SET status = 'lost', stop_reason = 'control_plane_restart', \
             version = ?, updated_at = ?, stopped_at = ? \
             WHERE status IN ('starting', 'ready', 'stopping')",
        )
        .bind(new_version())
        .bind(now)
        .bind(now)
        .execute(work.connection())
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "UPDATE async_tasks SET status = 'lost', exit_json = ?, version = ?, ended_at = ? \
             WHERE status IN ('queued', 'running')",
        )
        .bind(json!({"exit_code": null, "signal": "control_plane_restart"}).to_string())
        .bind(new_version())
        .bind(now)
        .execute(work.connection())
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "UPDATE terminals SET status = 'lost', version = ?, updated_at = ?, ended_at = ? \
             WHERE status IN ('starting', 'running', 'closing')",
        )
        .bind(new_version())
        .bind(now)
        .bind(now)
        .execute(work.connection())
        .await
        .map_err(storage_error)?;
        for id in &runtime_ids {
            self.append_runtime_changed_in_tx(work, *id)
                .await
                .map_err(|error| {
                    RuntimeError::unavailable(format!(
                        "append runtime recovery event for {id}: {error}"
                    ))
                })?;
        }
        for id in &async_task_ids {
            self.append_async_task_changed_in_tx(work, *id).await?;
        }
        for id in &terminal_ids {
            self.append_terminal_changed_in_tx(work, *id).await?;
        }
        sqlx::query(
            "UPDATE runtime_access_tickets SET revoked_at = ? \
             WHERE consumed_at IS NULL AND revoked_at IS NULL",
        )
        .bind(now)
        .execute(work.connection())
        .await
        .map_err(storage_error)?;
        Ok(())
    }

    async fn append_runtime_changed_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        id: RuntimeId,
    ) -> Result<RuntimeProjection, RuntimeError> {
        let row = sqlx::query_as::<_, RuntimeRow>(RUNTIME_SELECT)
            .bind(id.to_string())
            .fetch_one(work.connection())
            .await
            .map_err(storage_error)?;
        let runtime = runtime_projection(row)?;
        work.append_event(NewEvent {
            event_type: EventType::RuntimeChanged,
            actor: json!({"kind": "runtime_system"}),
            resource: Some(json!({"kind": "runtime", "id": runtime.id})),
            correlation_id: format!("runtime-runtime-{}", runtime.id),
            causation_id: None,
            payload: json!({
                "id": runtime.id,
                "scope": runtime.scope,
                "status": runtime.status,
                "version": runtime.version,
                "stopped_at": runtime.stopped_at,
            }),
        })
        .await
        .map_err(|error| RuntimeError::unavailable(format!("persist runtime event: {error}")))?;
        Ok(runtime)
    }

    async fn finalize_async_task(
        &self,
        id: AsyncTaskId,
        completion: ProcessCompletion,
        forced: Option<AsyncTaskStatus>,
    ) -> Result<(), RuntimeError> {
        let mut work = self.unit_of_work.begin().await.map_err(storage_error)?;
        let cancel_requested: bool = sqlx::query_scalar(
            "SELECT cancellation_requested_at IS NOT NULL FROM async_tasks WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_one(work.connection())
        .await
        .map_err(storage_error)?;
        let status = forced.unwrap_or_else(|| {
            if cancel_requested {
                AsyncTaskStatus::Canceled
            } else if completion.exit.exit_code == Some(0) {
                AsyncTaskStatus::Succeeded
            } else {
                AsyncTaskStatus::Failed
            }
        });
        let changed = sqlx::query(
            "UPDATE async_tasks SET status = ?, exit_json = ?, usage_json = ?, version = ?, ended_at = ? \
             WHERE id = ? AND status IN ('queued', 'running')",
        )
        .bind(async_task_status_str(status))
        .bind(serde_json::to_string(&completion.exit).map_err(storage_error)?)
        .bind(serde_json::to_string(&completion.usage).map_err(storage_error)?)
        .bind(new_version())
        .bind(now_utc_str())
        .bind(id.to_string())
        .execute(work.connection())
        .await
        .map_err(storage_error)?
        .rows_affected();
        if changed != 0 {
            self.append_async_task_changed_in_tx(&mut work, id).await?;
            work.commit().await.map_err(storage_error)?;
            // Notify application wake-up; lagging/disconnected receivers are fine.
            let _ = self.async_task_settled_tx.send(id);
        } else {
            work.rollback().await.map_err(storage_error)?;
        }
        Ok(())
    }

    async fn append_async_task_changed_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        id: AsyncTaskId,
    ) -> Result<AsyncTaskProjection, RuntimeError> {
        let row = sqlx::query_as::<_, AsyncTaskRow>(&format!("{} WHERE id = ?", ASYNC_TASK_SELECT))
            .bind(id.to_string())
            .fetch_one(work.connection())
            .await
            .map_err(storage_error)?;
        let async_task = async_task_projection(row)?;
        work
            .append_event(NewEvent {
                event_type: EventType::AsyncTaskChanged,
                actor: json!({"kind": "runtime_system"}),
                resource: Some(json!({"kind": "async_task", "id": async_task.id})),
                correlation_id: format!("runtime-async_task-{}", async_task.id),
                causation_id: None,
                payload: json!({"id": async_task.id, "session_id": async_task.session_id, "status": async_task.status, "version": async_task.version}),
            })
            .await
            .map_err(storage_error)?;
        Ok(async_task)
    }

    async fn runtime_row(&self, id: RuntimeId) -> Result<RuntimeRow, RuntimeError> {
        sqlx::query_as::<_, RuntimeRow>(RUNTIME_SELECT)
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?
            .ok_or(RuntimeError::RuntimeUnavailable)
    }

    async fn runtime_nonce(&self, id: RuntimeId) -> Result<String, RuntimeError> {
        let row = self.runtime_row(id).await?;
        if row.status != "ready" {
            return Err(RuntimeError::RuntimeUnavailable);
        }
        Ok(row.executor_nonce)
    }

    async fn async_task_row(&self, id: AsyncTaskId) -> Result<AsyncTaskRow, RuntimeError> {
        sqlx::query_as::<_, AsyncTaskRow>(&format!("{} WHERE id = ?", ASYNC_TASK_SELECT))
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?
            .ok_or(RuntimeError::AsyncTaskLost(id))
    }

    async fn async_task_nonce(&self, id: AsyncTaskId) -> Result<String, RuntimeError> {
        Ok(self.async_task_row(id).await?.executor_nonce)
    }

    async fn terminal_row(&self, id: TerminalId) -> Result<TerminalRow, RuntimeError> {
        sqlx::query_as::<_, TerminalRow>(&format!("{} WHERE id = ?", TERMINAL_SELECT))
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?
            .ok_or(RuntimeError::TerminalNotWritable(id))
    }

    async fn terminal_nonce(&self, id: TerminalId) -> Result<String, RuntimeError> {
        let row = self.terminal_row(id).await?;
        if !matches!(
            parse_terminal_status(&row.status)?,
            TerminalStatus::Running | TerminalStatus::Starting
        ) {
            return Err(RuntimeError::TerminalNotWritable(id));
        }
        Ok(row.executor_nonce)
    }

    async fn async_task_cancellation_requested(
        &self,
        id: AsyncTaskId,
    ) -> Result<bool, RuntimeError> {
        sqlx::query_scalar(
            "SELECT cancellation_requested_at IS NOT NULL FROM async_tasks WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_one(&self.pool)
        .await
        .map_err(storage_error)
    }

    async fn cancel_started_async_task(
        &self,
        id: AsyncTaskId,
        nonce: &str,
    ) -> Result<AsyncTaskProjection, RuntimeError> {
        match tokio::time::timeout(
            ASYNC_TASK_CANCEL_TIMEOUT,
            self.executor.cancel_async_task(id, nonce),
        )
        .await
        {
            Ok(Ok(completion)) => {
                self.finalize_async_task(id, completion, Some(AsyncTaskStatus::Canceled))
                    .await?;
            }
            Ok(Err(_)) | Err(_) => {
                self.mark_async_task_lost(id, "cancel_unconfirmed").await?;
            }
        }
        self.async_task(id).await
    }

    async fn mark_async_task_lost(
        &self,
        id: AsyncTaskId,
        reason: &str,
    ) -> Result<(), RuntimeError> {
        let log_stream_id = self
            .async_task_row(id)
            .await?
            .log_stream_id
            .parse::<LogStreamId>()
            .map_err(storage_error)?;
        let mut work = self.unit_of_work.begin().await.map_err(storage_error)?;
        let changed = sqlx::query(
            "UPDATE async_tasks SET status = 'lost', exit_json = ?, version = ?, ended_at = ? \
             WHERE id = ? AND status IN ('queued', 'running')",
        )
        .bind(json!({"exit_code": null, "signal": reason}).to_string())
        .bind(new_version())
        .bind(now_utc_str())
        .bind(id.to_string())
        .execute(work.connection())
        .await
        .map_err(storage_error)?
        .rows_affected();
        if changed == 0 {
            work.rollback().await.map_err(storage_error)?;
            return Ok(());
        }
        self.append_async_task_changed_in_tx(&mut work, id).await?;
        work.commit().await.map_err(storage_error)?;
        let _ = self.logs.close(log_stream_id).await;
        let _ = self.async_task_settled_tx.send(id);
        Ok(())
    }

    async fn mark_terminal_lost(&self, id: TerminalId, reason: &str) -> Result<(), RuntimeError> {
        let scrollback_stream_id = self
            .terminal_row(id)
            .await?
            .scrollback_stream_id
            .parse::<LogStreamId>()
            .map_err(storage_error)?;
        let mut work = self.unit_of_work.begin().await.map_err(storage_error)?;
        let changed = sqlx::query(
            "UPDATE terminals SET status = 'lost', exit_json = ?, version = ?, \
             updated_at = ?, ended_at = ? WHERE id = ? \
             AND status IN ('starting', 'running', 'closing')",
        )
        .bind(json!({"exit_code": null, "signal": reason}).to_string())
        .bind(new_version())
        .bind(now_utc_str())
        .bind(now_utc_str())
        .bind(id.to_string())
        .execute(work.connection())
        .await
        .map_err(storage_error)?
        .rows_affected();
        if changed != 0 {
            self.append_terminal_changed_in_tx(&mut work, id).await?;
            work.commit().await.map_err(storage_error)?;
        } else {
            work.rollback().await.map_err(storage_error)?;
        }
        let _ = self.logs.close(scrollback_stream_id).await;
        Ok(())
    }
}

const RUNTIME_SELECT: &str = "SELECT id, scope_kind, scope_id, executor_nonce, limits_json, \
    status, version, created_at, updated_at, stopped_at FROM runtimes WHERE id = ?";
const ASYNC_TASK_SELECT: &str = "SELECT id, runtime_id, session_id, initiated_by_tool_call_id, \
    controlling_turn_id, command_summary, executor_nonce, log_stream_id, status, exit_json, \
    usage_json, version, created_at, started_at, ended_at FROM async_tasks";
const TERMINAL_SELECT: &str = "SELECT id, runtime_id, owner_kind, owner_id, \
    executor_nonce, cols, rows, scrollback_stream_id, status, exit_json, version, created_at, \
    updated_at, ended_at FROM terminals";
const SESSION_LOG_STREAMS: &str = "SELECT id FROM log_streams WHERE \
    owner_kind = 'async_task' AND owner_id IN (SELECT id FROM async_tasks WHERE session_id = ?)";
const PROJECT_LOG_STREAMS: &str = "SELECT id FROM log_streams WHERE \
    (owner_kind = 'terminal' AND owner_id IN (SELECT id FROM terminals WHERE runtime_id IN \
        (SELECT id FROM runtimes WHERE scope_kind = 'project' AND scope_id = ?))) OR \
    (owner_kind = 'sync' AND owner_id IN (SELECT id FROM runtimes \
        WHERE scope_kind = 'project' AND scope_id = ?))";

const TICKET_TTL: chrono::TimeDelta = chrono::Duration::seconds(30);
const ASYNC_TASK_CANCEL_TIMEOUT: Duration = Duration::from_secs(5);
const ASYNC_TASK_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(20);
const COMMAND_SUMMARY_MAX_CHARS: usize = 120;

fn terminal_projection(row: TerminalRow) -> Result<TerminalProjection, RuntimeError> {
    if row.owner_kind != "project" {
        return Err(RuntimeError::RuntimeUnavailable);
    }
    let exit = row
        .exit_json
        .map(|value| serde_json::from_str::<ExitSummary>(&value).map_err(storage_error))
        .transpose()?;
    Ok(TerminalProjection {
        id: row.id.parse().map_err(storage_error)?,
        runtime_id: row.runtime_id.parse().map_err(storage_error)?,
        project_id: row.owner_id.parse().map_err(storage_error)?,
        status: parse_terminal_status(&row.status)?,
        size: TerminalSize::new(
            u16::try_from(row.cols).unwrap_or(1).max(1),
            u16::try_from(row.rows).unwrap_or(1).max(1),
        )
        .expect("terminal dimensions are clamped into range"),
        scrollback_stream_id: row.scrollback_stream_id.parse().map_err(storage_error)?,
        first_cursor: LogCursor::ZERO,
        next_cursor: LogCursor::ZERO,
        writable: matches!(
            parse_terminal_status(&row.status)?,
            TerminalStatus::Starting | TerminalStatus::Running
        ),
        exit,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
        ended_at: row.ended_at,
    })
}

fn parse_terminal_status(value: &str) -> Result<TerminalStatus, RuntimeError> {
    match value {
        "starting" => Ok(TerminalStatus::Starting),
        "running" => Ok(TerminalStatus::Running),
        "closing" => Ok(TerminalStatus::Closing),
        "exited" => Ok(TerminalStatus::Exited),
        "failed" => Ok(TerminalStatus::Failed),
        "lost" => Ok(TerminalStatus::Lost),
        _ => Err(RuntimeError::RuntimeUnavailable),
    }
}

const fn terminal_status_str(value: TerminalStatus) -> &'static str {
    match value {
        TerminalStatus::Starting => "starting",
        TerminalStatus::Running => "running",
        TerminalStatus::Closing => "closing",
        TerminalStatus::Exited => "exited",
        TerminalStatus::Failed => "failed",
        TerminalStatus::Lost => "lost",
    }
}

/// Parse an ISO-8601 UTC timestamp previously produced by `format_utc`. Returns
/// the millisecond-precision `DateTime` used for expiry comparisons.
fn parse_iso(value: &str) -> Result<chrono::DateTime<chrono::Utc>, RuntimeError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|_| RuntimeError::TerminalTicketInvalid)
}

fn runtime_projection(row: RuntimeRow) -> Result<RuntimeProjection, RuntimeError> {
    let id = row
        .id
        .parse()
        .map_err(|_| RuntimeError::unavailable(format!("invalid runtime id={}", row.id)))?;
    let scope = runtime_scope(&row.scope_kind, &row.scope_id).map_err(|_| {
        RuntimeError::unavailable(format!(
            "unknown runtime scope kind={} id={}",
            row.scope_kind, row.scope_id
        ))
    })?;
    let status = parse_runtime_status(&row.status)
        .map_err(|_| RuntimeError::unavailable(format!("unknown runtime status={}", row.status)))?;
    let limits = serde_json::from_str::<ResourceLimits>(&row.limits_json)
        .map_err(|_| RuntimeError::unavailable(format!("invalid runtime limits for id={id}")))?;
    Ok(RuntimeProjection {
        id,
        scope,
        status,
        limits,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
        stopped_at: row.stopped_at,
    })
}

fn runtime_scope(kind: &str, id: &str) -> Result<RuntimeScope, RuntimeError> {
    match kind {
        "project" => Ok(RuntimeScope::project(id.parse().map_err(storage_error)?)),
        _ => Err(RuntimeError::RuntimeUnavailable),
    }
}

fn async_task_projection(row: AsyncTaskRow) -> Result<AsyncTaskProjection, RuntimeError> {
    Ok(AsyncTaskProjection {
        id: row.id.parse().map_err(storage_error)?,
        runtime_id: row.runtime_id.parse().map_err(storage_error)?,
        session_id: row.session_id.parse().map_err(storage_error)?,
        controlling_turn_id: row.controlling_turn_id.parse().map_err(storage_error)?,
        initiated_by_tool_call_id: row
            .initiated_by_tool_call_id
            .parse()
            .map_err(storage_error)?,
        status: parse_async_task_status(&row.status)?,
        command_summary: row.command_summary,
        log_stream_id: row.log_stream_id.parse().map_err(storage_error)?,
        exit: row
            .exit_json
            .map(|value| serde_json::from_str::<ExitSummary>(&value).map_err(storage_error))
            .transpose()?,
        usage: serde_json::from_str::<ResourceUsage>(&row.usage_json).map_err(storage_error)?,
        version: row.version,
        created_at: row.created_at,
        started_at: row.started_at,
        ended_at: row.ended_at,
    })
}

fn parse_runtime_status(value: &str) -> Result<RuntimeStatus, RuntimeError> {
    match value {
        "starting" => Ok(RuntimeStatus::Starting),
        "ready" => Ok(RuntimeStatus::Ready),
        "stopping" => Ok(RuntimeStatus::Stopping),
        "stopped" => Ok(RuntimeStatus::Stopped),
        "failed" => Ok(RuntimeStatus::Failed),
        "lost" => Ok(RuntimeStatus::Lost),
        _ => Err(RuntimeError::RuntimeUnavailable),
    }
}

fn parse_async_task_status(value: &str) -> Result<AsyncTaskStatus, RuntimeError> {
    match value {
        "queued" => Ok(AsyncTaskStatus::Queued),
        "running" => Ok(AsyncTaskStatus::Running),
        "succeeded" => Ok(AsyncTaskStatus::Succeeded),
        "failed" => Ok(AsyncTaskStatus::Failed),
        "canceled" => Ok(AsyncTaskStatus::Canceled),
        "lost" => Ok(AsyncTaskStatus::Lost),
        _ => Err(RuntimeError::RuntimeUnavailable),
    }
}

const fn async_task_status_str(value: AsyncTaskStatus) -> &'static str {
    match value {
        AsyncTaskStatus::Queued => "queued",
        AsyncTaskStatus::Running => "running",
        AsyncTaskStatus::Succeeded => "succeeded",
        AsyncTaskStatus::Failed => "failed",
        AsyncTaskStatus::Canceled => "canceled",
        AsyncTaskStatus::Lost => "lost",
    }
}

/// One-line, length-bounded rendering of an async task's own command.
///
/// This string is the only thing the task list and the model's task inventory
/// have to tell two concurrent background tasks apart, so it carries the real
/// command. Any secret the caller injected as an environment variable is
/// replaced first: the summary is durable and is read back into prompts.
fn command_summary(spec: &AsyncTaskSpec) -> String {
    let secrets = spec
        .execution
        .environment()
        .secrets()
        .iter()
        .map(|secret| secret.value().expose())
        .collect::<Vec<_>>();
    redacted_one_line(spec.execution.command().input(), &secrets)
}

fn redacted_one_line(command: &str, secrets: &[&str]) -> String {
    let mut command = command.to_owned();
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        command = command.replace(*secret, "[secret redacted]");
    }
    let one_line = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut summary: String = one_line.chars().take(COMMAND_SUMMARY_MAX_CHARS).collect();
    if one_line.chars().count() > COMMAND_SUMMARY_MAX_CHARS {
        summary.push('…');
    }
    summary
}

fn new_version() -> String {
    format!("v_{}", RuntimeId::new())
}

fn storage_error(_error: impl Into<anyhow::Error>) -> RuntimeError {
    RuntimeError::RuntimeUnavailable
}

#[cfg(test)]
mod tests {
    use super::{COMMAND_SUMMARY_MAX_CHARS, redacted_one_line};

    #[test]
    fn a_task_summary_carries_its_own_command_on_one_line() {
        assert_eq!(
            redacted_one_line("cargo test\n  --workspace", &[]),
            "cargo test --workspace"
        );
    }

    #[test]
    fn an_injected_secret_never_reaches_the_durable_summary() {
        let summary = redacted_one_line("gh auth login --with-token ghp_example", &["ghp_example"]);
        assert_eq!(summary, "gh auth login --with-token [secret redacted]");
        assert!(!summary.contains("ghp_example"));
    }

    #[test]
    fn a_long_command_is_marked_as_shortened() {
        let command = "echo ".repeat(200);
        let summary = redacted_one_line(&command, &[]);
        assert!(summary.ends_with('…'));
        assert_eq!(summary.chars().count(), COMMAND_SUMMARY_MAX_CHARS + 1);
    }
}
