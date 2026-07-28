use std::{path::Path, sync::Arc};

use serde_json::json;
use sqlx::{FromRow, SqliteConnection, SqlitePool};

use super::{
    interface::{
        CapabilityScope, DeploymentCapabilityProbe, EffectiveCapabilityConfig, ExecutionResult,
        ExecutionSpec, ExecutorKind, ExitSummary, JobProjection, JobSpec, JobStatus, LogChannel,
        LogCursor, LogRange, LogStreamProjection, ProcessCompletion, ResourceLimits, ResourceUsage,
        RuntimeCapabilityEvaluator, RuntimeError, RuntimeExecutor, RuntimeProjection, RuntimeSpec,
        RuntimeStatus, ServiceHealth, ServiceImpact, ServiceProjection, ServiceSpec, ServiceStatus,
        TerminalOwner, TerminalProjection, TerminalSignal, TerminalSize, TerminalSpec,
        TerminalStatus, TerminalTicket, TerminalTicketRequest,
    },
    log_store::{LogRetention, LogStore},
};
use crate::platform::{
    clock::{Clock, SystemClock, format_utc},
    events::{EventStore, NewEvent},
    id::{JobId, LogStreamId, RuntimeId, ServiceId, TerminalId, TurnId},
    secret::{purpose_hash, random_token},
};

#[derive(Clone)]
pub struct RuntimeInterface {
    pool: SqlitePool,
    events: EventStore,
    logs: LogStore,
    executor: Arc<dyn RuntimeExecutor>,
    /// Broadcast of Job ids that just reached a durable terminal status.
    /// `application::runtime_events` subscribes and resumes waiting Turns.
    job_settled_tx: tokio::sync::broadcast::Sender<JobId>,
}

#[derive(FromRow)]
struct RuntimeRow {
    id: String,
    session_id: String,
    executor_kind: String,
    executor_nonce: String,
    limits_json: String,
    capability_snapshot_json: String,
    status: String,
    version: String,
    created_at: String,
    updated_at: String,
    stopped_at: Option<String>,
}

#[derive(FromRow)]
struct JobRow {
    id: String,
    runtime_id: String,
    session_id: String,
    initiated_by_tool_call_id: String,
    controlling_turn_id: String,
    cli_session_id: Option<String>,
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
struct ServiceRow {
    id: String,
    runtime_id: String,
    session_id: String,
    initiated_by_tool_call_id: String,
    impact: String,
    command_summary: String,
    executor_nonce: String,
    health_json: Option<String>,
    log_stream_id: String,
    status: String,
    exit_json: Option<String>,
    version: String,
    created_at: String,
    started_at: Option<String>,
    ended_at: Option<String>,
}

#[derive(FromRow)]
#[allow(dead_code)]
struct TerminalRow {
    id: String,
    runtime_id: String,
    owner_kind: String,
    owner_id: String,
    executor_pty_identity: Option<String>,
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
#[allow(dead_code)]
struct TerminalTicketRow {
    terminal_id: String,
    token_hash: String,
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
        let (job_settled_tx, _) = tokio::sync::broadcast::channel(64);
        Self {
            logs: LogStore::new(pool.clone(), data_root),
            pool,
            events,
            executor,
            job_settled_tx,
        }
    }

    /// Subscribe to durable Job terminal-state notifications.
    pub fn subscribe_job_settled(&self) -> tokio::sync::broadcast::Receiver<JobId> {
        self.job_settled_tx.subscribe()
    }

    pub async fn has_unfinished_jobs_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: TurnId,
    ) -> Result<bool, RuntimeError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(1) FROM jobs \
             WHERE controlling_turn_id = ? AND status IN ('queued', 'running')",
        )
        .bind(turn_id.to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_error)?;
        Ok(count > 0)
    }

    pub async fn ensure_runtime(
        &self,
        spec: &RuntimeSpec,
    ) -> Result<RuntimeProjection, RuntimeError> {
        if let Some(existing) = self.current_runtime(spec.session_id()).await?
            && existing.id == spec.id()
            && existing.status == RuntimeStatus::Ready
        {
            return Ok(existing);
        }
        let now = format_utc(SystemClock.now());
        let capabilities = RuntimeCapabilityEvaluator::effective(
            &DeploymentCapabilityProbe::detect(),
            EffectiveCapabilityConfig {
                executor: spec.executor(),
                production: false,
                allow_insecure_local_executor: true,
                bash_egress_configured: spec.network_policy()
                    == super::interface::NetworkPolicy::ProjectRules,
                live_preview_configured: false,
                scope: CapabilityScope::Session,
            },
        );
        let placeholder_nonce = format!("pending-{}", spec.id());
        sqlx::query(
            "INSERT INTO runtimes \
             (id, session_id, executor_kind, executor_nonce, limits_json, capability_snapshot_json, \
              status, version, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, 'starting', ?, ?, ?)",
        )
        .bind(spec.id().to_string())
        .bind(spec.session_id().to_string())
        .bind(executor_kind_str(spec.executor()))
        .bind(placeholder_nonce)
        .bind(serde_json::to_string(spec.limits()).map_err(storage_error)?)
        .bind(serde_json::to_string(&capabilities).map_err(storage_error)?)
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
                .bind(format_utc(SystemClock.now()))
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
                .bind(format_utc(SystemClock.now()))
                .bind(format_utc(SystemClock.now()))
                .bind(spec.id().to_string())
                .execute(&self.pool)
                .await;
                Err(error)
            }
        }
    }

    pub async fn stop_runtime(&self, id: RuntimeId) -> Result<RuntimeProjection, RuntimeError> {
        let nonce = self.runtime_nonce(id).await?;
        let now = format_utc(SystemClock.now());
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

    pub async fn start_job(&self, spec: JobSpec) -> Result<JobProjection, RuntimeError> {
        let job_id = spec.id;
        let runtime_nonce = self.runtime_nonce(spec.execution.runtime_id()).await?;
        let log = self
            .logs
            .create(super::interface::LogOwnerKind::Job, &spec.id.to_string())
            .await?;
        let now = format_utc(SystemClock.now());
        let cli = match spec.execution.command().kind() {
            super::interface::CommandKind::DelegatedCli { cli, session_id } => (
                Some(delegated_cli_str(*cli)),
                session_id.map(|value| value.to_string()),
            ),
            super::interface::CommandKind::Shell => (None, None),
        };
        sqlx::query(
            "INSERT INTO jobs \
             (id, runtime_id, session_id, initiated_by_tool_call_id, controlling_turn_id, cli_kind, \
              cli_session_id, command_summary, executor_nonce, log_stream_id, status, usage_json, \
              version, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'queued', ?, ?, ?)",
        )
        .bind(job_id.to_string())
        .bind(spec.execution.runtime_id().to_string())
        .bind(spec.session_id.to_string())
        .bind(spec.initiated_by_tool_call_id.to_string())
        .bind(spec.controlling_turn_id.to_string())
        .bind(cli.0)
        .bind(cli.1)
        .bind(command_summary(spec.execution.command().kind()))
        .bind(&runtime_nonce)
        .bind(log.id.to_string())
        .bind(serde_json::to_string(&ResourceUsage::default()).map_err(storage_error)?)
        .bind(new_version())
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        match self.executor.start_job(spec, log.id).await {
            Ok(handle) if handle.executor_nonce == runtime_nonce => {
                sqlx::query(
                    "UPDATE jobs SET executor_process_identity = ?, status = 'running', \
                     version = ?, started_at = ? WHERE id = ? AND status = 'queued'",
                )
                .bind(handle.process_identity)
                .bind(new_version())
                .bind(&now)
                .bind(job_id.to_string())
                .execute(&self.pool)
                .await
                .map_err(storage_error)?;
                let job = self.job_by_log(log.id).await?;
                self.emit_job(&job).await;
                let this = self.clone();
                let job_id = job.id;
                tokio::spawn(async move {
                    if let Ok(completion) = this.executor.wait_job(job_id, &runtime_nonce).await {
                        let _ = this.finalize_job(job_id, completion, None).await;
                    }
                });
                Ok(job)
            }
            Ok(_) => {
                self.mark_job_lost_by_log(log.id).await?;
                Err(RuntimeError::RuntimeUnavailable)
            }
            Err(error) => {
                self.mark_job_lost_by_log(log.id).await?;
                Err(error)
            }
        }
    }

    pub async fn write_job_stdin(&self, id: JobId, input: Vec<u8>) -> Result<(), RuntimeError> {
        let nonce = self.job_nonce(id).await?;
        self.executor.write_job_stdin(id, &nonce, input).await
    }

    pub async fn cancel_job(&self, id: JobId) -> Result<JobProjection, RuntimeError> {
        let nonce = self.job_nonce(id).await?;
        sqlx::query(
            "UPDATE jobs SET exit_json = ?, version = ? WHERE id = ? AND status = 'running'",
        )
        .bind(
            json!({"exit_code": null, "signal": "cancel_requested", "cancel_requested": true})
                .to_string(),
        )
        .bind(new_version())
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        let completion = self.executor.cancel_job(id, &nonce).await?;
        self.finalize_job(id, completion, Some(JobStatus::Canceled))
            .await?;
        self.job(id).await
    }

    pub async fn start_service(
        &self,
        spec: ServiceSpec,
    ) -> Result<ServiceProjection, RuntimeError> {
        let runtime_nonce = self.runtime_nonce(spec.execution.runtime_id()).await?;
        let log = self
            .logs
            .create(
                super::interface::LogOwnerKind::Service,
                &spec.id.to_string(),
            )
            .await?;
        let now = format_utc(SystemClock.now());
        sqlx::query(
            "INSERT INTO services \
             (id, runtime_id, session_id, initiated_by_tool_call_id, impact, command_summary, \
              executor_nonce, log_stream_id, status, version, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'starting', ?, ?)",
        )
        .bind(spec.id.to_string())
        .bind(spec.execution.runtime_id().to_string())
        .bind(spec.session_id.to_string())
        .bind(spec.initiated_by_tool_call_id.to_string())
        .bind(service_impact_str(spec.impact))
        .bind(command_summary(spec.execution.command().kind()))
        .bind(&runtime_nonce)
        .bind(log.id.to_string())
        .bind(new_version())
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        match self.executor.start_service(spec, log.id).await {
            Ok(handle) if handle.executor_nonce == runtime_nonce => {
                sqlx::query(
                    "UPDATE services SET executor_process_identity = ?, status = 'running', \
                     version = ?, started_at = ? WHERE log_stream_id = ? AND status = 'starting'",
                )
                .bind(handle.process_identity)
                .bind(new_version())
                .bind(&now)
                .bind(log.id.to_string())
                .execute(&self.pool)
                .await
                .map_err(storage_error)?;
                let service = self.service_by_log(log.id).await?;
                self.emit_service(&service).await;
                let this = self.clone();
                let service_id = service.id;
                tokio::spawn(async move {
                    if let Ok(completion) =
                        this.executor.wait_service(service_id, &runtime_nonce).await
                    {
                        let _ = this.finalize_service(service_id, completion, None).await;
                    }
                });
                Ok(service)
            }
            Ok(_) => {
                self.mark_service_failed_by_log(log.id).await?;
                Err(RuntimeError::RuntimeUnavailable)
            }
            Err(error) => {
                self.mark_service_failed_by_log(log.id).await?;
                Err(error)
            }
        }
    }

    pub async fn stop_service(&self, id: ServiceId) -> Result<ServiceProjection, RuntimeError> {
        let nonce = self.service_nonce(id).await?;
        sqlx::query(
            "UPDATE services SET status = 'stopping', version = ? WHERE id = ? AND status IN \
             ('starting', 'running', 'unhealthy')",
        )
        .bind(new_version())
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        let completion = self.executor.stop_service(id, &nonce).await?;
        self.finalize_service(id, completion, Some(ServiceStatus::Stopped))
            .await?;
        self.service(id).await
    }

    pub async fn runtime(&self, id: RuntimeId) -> Result<RuntimeProjection, RuntimeError> {
        runtime_projection(self.runtime_row(id).await?)
    }

    pub async fn current_runtime(
        &self,
        session_id: crate::platform::id::SessionId,
    ) -> Result<Option<RuntimeProjection>, RuntimeError> {
        let row = sqlx::query_as::<_, RuntimeRow>(
            "SELECT id, session_id, executor_kind, executor_nonce, limits_json, \
             capability_snapshot_json, status, version, created_at, updated_at, stopped_at \
             FROM runtimes WHERE session_id = ? AND status IN ('starting', 'ready', 'stopping')",
        )
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        row.map(runtime_projection).transpose()
    }

    pub async fn job(&self, id: JobId) -> Result<JobProjection, RuntimeError> {
        job_projection(self.job_row(id).await?)
    }

    pub async fn jobs(
        &self,
        session_id: crate::platform::id::SessionId,
    ) -> Result<Vec<JobProjection>, RuntimeError> {
        sqlx::query_as::<_, JobRow>(&format!(
            "{} WHERE session_id = ? ORDER BY created_at",
            JOB_SELECT
        ))
        .bind(session_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?
        .into_iter()
        .map(job_projection)
        .collect()
    }

    pub async fn service(&self, id: ServiceId) -> Result<ServiceProjection, RuntimeError> {
        service_projection(self.service_row(id).await?)
    }

    pub async fn services(
        &self,
        session_id: crate::platform::id::SessionId,
    ) -> Result<Vec<ServiceProjection>, RuntimeError> {
        sqlx::query_as::<_, ServiceRow>(&format!(
            "{} WHERE session_id = ? ORDER BY created_at",
            SERVICE_SELECT
        ))
        .bind(session_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?
        .into_iter()
        .map(service_projection)
        .collect()
    }

    pub async fn log_range(
        &self,
        id: LogStreamId,
        after: LogCursor,
        limit_bytes: usize,
    ) -> Result<LogRange, RuntimeError> {
        self.logs.read(id, after, limit_bytes).await
    }

    pub async fn log_projection(
        &self,
        id: LogStreamId,
    ) -> Result<LogStreamProjection, RuntimeError> {
        self.logs.projection(id).await
    }

    pub async fn create_terminal_log_stream(
        &self,
        owner_id: &str,
    ) -> Result<LogStreamProjection, RuntimeError> {
        let stream = self
            .logs
            .create(super::interface::LogOwnerKind::Terminal, owner_id)
            .await?;
        Ok(stream)
    }

    pub async fn append_terminal_output(
        &self,
        id: LogStreamId,
        input: &[u8],
        secret_values: &[&str],
    ) -> Result<LogStreamProjection, RuntimeError> {
        self.logs
            .append(
                id,
                LogChannel::Stdout,
                input,
                secret_values,
                LogRetention::TERMINAL,
            )
            .await
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
        let now = format_utc(SystemClock.now());
        let (owner_kind, owner_id) = terminal_owner_parts(spec.owner);
        sqlx::query(
            "INSERT INTO terminals \
             (id, runtime_id, owner_kind, owner_id, executor_nonce, cols, rows, \
              scrollback_stream_id, status, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'starting', ?, ?, ?)",
        )
        .bind(spec.id.to_string())
        .bind(spec.runtime_id.to_string())
        .bind(owner_kind)
        .bind(&owner_id)
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
                let changed = sqlx::query(
                    "UPDATE terminals SET executor_pty_identity = ?, status = 'running', \
                     version = ?, updated_at = ? WHERE id = ? AND status = 'starting'",
                )
                .bind(&handle.process_identity)
                .bind(new_version())
                .bind(format_utc(SystemClock.now()))
                .bind(terminal_id.to_string())
                .execute(&self.pool)
                .await
                .map_err(storage_error)?
                .rows_affected();
                let terminal = self.terminal(terminal_id).await?;
                if changed != 0 {
                    self.emit_terminal(&terminal).await;
                }
                let this = self.clone();
                let terminal_id = terminal.id;
                let nonce = runtime_nonce;
                tokio::spawn(async move {
                    if let Ok(completion) =
                        this.executor.await_terminal_exit(terminal_id, &nonce).await
                    {
                        let _ = this.finalize_terminal(terminal_id, completion).await;
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
        owner: TerminalOwner,
    ) -> Result<Vec<TerminalProjection>, RuntimeError> {
        let (owner_kind, owner_id) = terminal_owner_parts(owner);
        let mut projections = sqlx::query_as::<_, TerminalRow>(&format!(
            "{} WHERE owner_kind = ? AND owner_id = ? ORDER BY created_at",
            TERMINAL_SELECT
        ))
        .bind(owner_kind)
        .bind(&owner_id)
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
        let now = SystemClock.now();
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
            "SELECT terminal_id, token_hash, actor_id, origin, expires_at, consumed_at, revoked_at \
             FROM runtime_access_tickets WHERE token_hash = ?",
        )
        .bind(&token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?
        .ok_or(RuntimeError::TerminalTicketInvalid)?;
        let now = SystemClock.now();
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
        sqlx::query(
            "UPDATE terminals SET cols = ?, rows = ?, version = ?, updated_at = ? \
             WHERE id = ? AND status IN ('starting', 'running')",
        )
        .bind(i64::from(size.cols))
        .bind(i64::from(size.rows))
        .bind(new_version())
        .bind(format_utc(SystemClock.now()))
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        let terminal = self.terminal(id).await?;
        self.emit_terminal(&terminal).await;
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
        .bind(format_utc(SystemClock.now()))
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

    pub async fn terminal_scrollback_projection(
        &self,
        id: TerminalId,
    ) -> Result<LogStreamProjection, RuntimeError> {
        let row = self.terminal_row(id).await?;
        let stream_id: LogStreamId = row.scrollback_stream_id.parse().map_err(storage_error)?;
        self.logs.projection(stream_id).await
    }

    async fn finalize_terminal(
        &self,
        id: TerminalId,
        completion: ProcessCompletion,
    ) -> Result<(), RuntimeError> {
        // Closing→Exited is the only durable transition here; the prior branch
        // collapsed to the same status, so the distinction is dropped on the
        // floor and the recorded completion decides the projection.
        let _ = self.terminal_row(id).await.ok();
        let status = TerminalStatus::Exited;
        let changed = sqlx::query(
            "UPDATE terminals SET status = ?, exit_json = ?, version = ?, updated_at = ?, \
             ended_at = ? WHERE id = ? AND status IN ('starting', 'running', 'closing')",
        )
        .bind(terminal_status_str(status))
        .bind(serde_json::to_string(&completion.exit).map_err(storage_error)?)
        .bind(new_version())
        .bind(format_utc(SystemClock.now()))
        .bind(format_utc(SystemClock.now()))
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?
        .rows_affected();
        if changed != 0 {
            self.emit_terminal(&self.terminal(id).await?).await;
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
        .bind(format_utc(SystemClock.now()))
        .bind(format_utc(SystemClock.now()))
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

    async fn emit_terminal(&self, terminal: &TerminalProjection) {
        let _ = self
            .events
            .append(NewEvent {
                event_type: "terminal.changed".into(),
                actor: json!({"kind": "runtime_system"}),
                resource: Some(json!({"kind": "terminal", "id": terminal.id})),
                correlation_id: format!("runtime-terminal-{}", terminal.id),
                causation_id: None,
                payload: json!({
                    "id": terminal.id,
                    "owner": terminal.owner,
                    "status": terminal.status,
                    "next_cursor": terminal.next_cursor,
                    "version": terminal.version,
                }),
            })
            .await;
    }

    pub async fn recover_uncertain(&self) -> Result<(), RuntimeError> {
        let now = format_utc(SystemClock.now());
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        sqlx::query(
            "UPDATE runtimes SET status = 'lost', stop_reason = 'control_plane_restart', \
             version = ?, updated_at = ?, stopped_at = ? \
             WHERE status IN ('starting', 'ready', 'stopping')",
        )
        .bind(new_version())
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "UPDATE jobs SET status = 'lost', exit_json = ?, version = ?, ended_at = ? \
             WHERE status IN ('queued', 'running')",
        )
        .bind(json!({"exit_code": null, "signal": "control_plane_restart"}).to_string())
        .bind(new_version())
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "UPDATE services SET status = 'stopped_after_restart', exit_json = ?, version = ?, \
             ended_at = ? WHERE status IN ('starting', 'running', 'unhealthy', 'stopping')",
        )
        .bind(json!({"exit_code": null, "signal": "control_plane_restart"}).to_string())
        .bind(new_version())
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "UPDATE terminals SET status = 'lost', version = ?, updated_at = ?, ended_at = ? \
             WHERE status IN ('starting', 'running', 'closing')",
        )
        .bind(new_version())
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "UPDATE runtime_access_tickets SET revoked_at = ? \
             WHERE consumed_at IS NULL AND revoked_at IS NULL",
        )
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        tx.commit().await.map_err(storage_error)
    }

    async fn finalize_job(
        &self,
        id: JobId,
        completion: ProcessCompletion,
        forced: Option<JobStatus>,
    ) -> Result<(), RuntimeError> {
        let cancel_requested: i64 = sqlx::query_scalar(
            "SELECT coalesce(json_extract(exit_json, '$.cancel_requested'), 0) FROM jobs WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_one(&self.pool)
        .await
        .map_err(storage_error)?;
        let status = forced.unwrap_or_else(|| {
            if cancel_requested != 0 {
                JobStatus::Canceled
            } else if completion.exit.exit_code == Some(0) {
                JobStatus::Succeeded
            } else {
                JobStatus::Failed
            }
        });
        let changed = sqlx::query(
            "UPDATE jobs SET status = ?, exit_json = ?, usage_json = ?, version = ?, ended_at = ? \
             WHERE id = ? AND status IN ('queued', 'running')",
        )
        .bind(job_status_str(status))
        .bind(serde_json::to_string(&completion.exit).map_err(storage_error)?)
        .bind(serde_json::to_string(&completion.usage).map_err(storage_error)?)
        .bind(new_version())
        .bind(format_utc(SystemClock.now()))
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?
        .rows_affected();
        if changed != 0 {
            self.emit_job(&self.job(id).await?).await;
            // Notify application wake-up; lagging/disconnected receivers are fine.
            let _ = self.job_settled_tx.send(id);
        }
        Ok(())
    }

    async fn finalize_service(
        &self,
        id: ServiceId,
        completion: ProcessCompletion,
        forced: Option<ServiceStatus>,
    ) -> Result<(), RuntimeError> {
        let stop_requested: bool =
            sqlx::query_scalar("SELECT status = 'stopping' FROM services WHERE id = ?")
                .bind(id.to_string())
                .fetch_one(&self.pool)
                .await
                .map_err(storage_error)?;
        let status = forced.unwrap_or_else(|| {
            if stop_requested || completion.exit.exit_code == Some(0) {
                ServiceStatus::Stopped
            } else {
                ServiceStatus::Failed
            }
        });
        let changed = sqlx::query(
            "UPDATE services SET status = ?, exit_json = ?, version = ?, ended_at = ? \
             WHERE id = ? AND status IN ('starting', 'running', 'unhealthy', 'stopping')",
        )
        .bind(service_status_str(status))
        .bind(serde_json::to_string(&completion.exit).map_err(storage_error)?)
        .bind(new_version())
        .bind(format_utc(SystemClock.now()))
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?
        .rows_affected();
        if changed != 0 {
            self.emit_service(&self.service(id).await?).await;
        }
        Ok(())
    }

    async fn emit_job(&self, job: &JobProjection) {
        let _ = self
            .events
            .append(NewEvent {
                event_type: "job.changed".into(),
                actor: json!({"kind": "runtime_system"}),
                resource: Some(json!({"kind": "job", "id": job.id})),
                correlation_id: format!("runtime-job-{}", job.id),
                causation_id: None,
                payload: json!({"id": job.id, "session_id": job.session_id, "status": job.status, "version": job.version}),
            })
            .await;
    }

    async fn emit_service(&self, service: &ServiceProjection) {
        let _ = self
            .events
            .append(NewEvent {
                event_type: "service.changed".into(),
                actor: json!({"kind": "runtime_system"}),
                resource: Some(json!({"kind": "service", "id": service.id})),
                correlation_id: format!("runtime-service-{}", service.id),
                causation_id: None,
                payload: json!({"id": service.id, "session_id": service.session_id, "status": service.status, "version": service.version}),
            })
            .await;
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

    async fn job_row(&self, id: JobId) -> Result<JobRow, RuntimeError> {
        sqlx::query_as::<_, JobRow>(&format!("{} WHERE id = ?", JOB_SELECT))
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?
            .ok_or(RuntimeError::JobLost(id))
    }

    async fn job_by_log(&self, id: LogStreamId) -> Result<JobProjection, RuntimeError> {
        let row = sqlx::query_as::<_, JobRow>(&format!("{} WHERE log_stream_id = ?", JOB_SELECT))
            .bind(id.to_string())
            .fetch_one(&self.pool)
            .await
            .map_err(storage_error)?;
        job_projection(row)
    }

    async fn job_nonce(&self, id: JobId) -> Result<String, RuntimeError> {
        Ok(self.job_row(id).await?.executor_nonce)
    }

    async fn service_row(&self, id: ServiceId) -> Result<ServiceRow, RuntimeError> {
        sqlx::query_as::<_, ServiceRow>(&format!("{} WHERE id = ?", SERVICE_SELECT))
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?
            .ok_or(RuntimeError::ServiceLost(id))
    }

    async fn service_by_log(&self, id: LogStreamId) -> Result<ServiceProjection, RuntimeError> {
        let row =
            sqlx::query_as::<_, ServiceRow>(&format!("{} WHERE log_stream_id = ?", SERVICE_SELECT))
                .bind(id.to_string())
                .fetch_one(&self.pool)
                .await
                .map_err(storage_error)?;
        service_projection(row)
    }

    async fn service_nonce(&self, id: ServiceId) -> Result<String, RuntimeError> {
        Ok(self.service_row(id).await?.executor_nonce)
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

    async fn mark_job_lost_by_log(&self, id: LogStreamId) -> Result<(), RuntimeError> {
        sqlx::query(
            "UPDATE jobs SET status = 'lost', exit_json = ?, version = ?, ended_at = ? \
             WHERE log_stream_id = ? AND status IN ('queued', 'running')",
        )
        .bind(json!({"exit_code": null, "signal": "start_failed"}).to_string())
        .bind(new_version())
        .bind(format_utc(SystemClock.now()))
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        let _ = self.logs.close(id).await;
        Ok(())
    }

    async fn mark_service_failed_by_log(&self, id: LogStreamId) -> Result<(), RuntimeError> {
        sqlx::query(
            "UPDATE services SET status = 'failed', exit_json = ?, version = ?, ended_at = ? \
             WHERE log_stream_id = ? AND status IN ('starting', 'running')",
        )
        .bind(json!({"exit_code": null, "signal": "start_failed"}).to_string())
        .bind(new_version())
        .bind(format_utc(SystemClock.now()))
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        let _ = self.logs.close(id).await;
        Ok(())
    }
}

const RUNTIME_SELECT: &str = "SELECT id, session_id, executor_kind, executor_nonce, limits_json, \
    capability_snapshot_json, status, version, created_at, updated_at, stopped_at FROM runtimes WHERE id = ?";
const JOB_SELECT: &str = "SELECT id, runtime_id, session_id, initiated_by_tool_call_id, \
    controlling_turn_id, cli_session_id, command_summary, executor_nonce, log_stream_id, status, exit_json, \
    usage_json, version, created_at, started_at, ended_at FROM jobs";
const SERVICE_SELECT: &str = "SELECT id, runtime_id, session_id, initiated_by_tool_call_id, impact, \
    command_summary, executor_nonce, health_json, log_stream_id, status, exit_json, version, created_at, started_at, \
    ended_at FROM services";
const TERMINAL_SELECT: &str = "SELECT id, runtime_id, owner_kind, owner_id, executor_pty_identity, \
    executor_nonce, cols, rows, scrollback_stream_id, status, exit_json, version, created_at, \
    updated_at, ended_at FROM terminals";

const TICKET_TTL: chrono::TimeDelta = chrono::Duration::seconds(30);

fn terminal_projection(row: TerminalRow) -> Result<TerminalProjection, RuntimeError> {
    let owner = parse_terminal_owner(&row.owner_kind, &row.owner_id)?;
    let exit = row
        .exit_json
        .map(|value| serde_json::from_str::<ExitSummary>(&value).map_err(storage_error))
        .transpose()?;
    Ok(TerminalProjection {
        id: row.id.parse().map_err(storage_error)?,
        runtime_id: row.runtime_id.parse().map_err(storage_error)?,
        owner,
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

fn terminal_owner_parts(owner: TerminalOwner) -> (&'static str, String) {
    match owner {
        TerminalOwner::Project(id) => ("project", id.to_string()),
        TerminalOwner::Session(id) => ("session", id.to_string()),
    }
}

fn parse_terminal_owner(kind: &str, id: &str) -> Result<TerminalOwner, RuntimeError> {
    match kind {
        "project" => Ok(TerminalOwner::Project(id.parse().map_err(storage_error)?)),
        "session" => Ok(TerminalOwner::Session(id.parse().map_err(storage_error)?)),
        _ => Err(RuntimeError::RuntimeUnavailable),
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
    Ok(RuntimeProjection {
        id: row.id.parse().map_err(storage_error)?,
        session_id: row.session_id.parse().map_err(storage_error)?,
        executor: parse_executor_kind(&row.executor_kind)?,
        status: parse_runtime_status(&row.status)?,
        capabilities: serde_json::from_str(&row.capability_snapshot_json).map_err(storage_error)?,
        limits: serde_json::from_str::<ResourceLimits>(&row.limits_json).map_err(storage_error)?,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
        stopped_at: row.stopped_at,
    })
}

fn job_projection(row: JobRow) -> Result<JobProjection, RuntimeError> {
    Ok(JobProjection {
        id: row.id.parse().map_err(storage_error)?,
        runtime_id: row.runtime_id.parse().map_err(storage_error)?,
        session_id: row.session_id.parse().map_err(storage_error)?,
        controlling_turn_id: row.controlling_turn_id.parse().map_err(storage_error)?,
        initiated_by_tool_call_id: row
            .initiated_by_tool_call_id
            .parse()
            .map_err(storage_error)?,
        cli_session_id: row
            .cli_session_id
            .map(|value| value.parse().map_err(storage_error))
            .transpose()?,
        status: parse_job_status(&row.status)?,
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

fn service_projection(row: ServiceRow) -> Result<ServiceProjection, RuntimeError> {
    Ok(ServiceProjection {
        id: row.id.parse().map_err(storage_error)?,
        runtime_id: row.runtime_id.parse().map_err(storage_error)?,
        session_id: row.session_id.parse().map_err(storage_error)?,
        initiated_by_tool_call_id: row
            .initiated_by_tool_call_id
            .parse()
            .map_err(storage_error)?,
        status: parse_service_status(&row.status)?,
        impact: parse_service_impact(&row.impact)?,
        command_summary: row.command_summary,
        health: row
            .health_json
            .map(|value| serde_json::from_str::<ServiceHealth>(&value).map_err(storage_error))
            .transpose()?
            .unwrap_or(ServiceHealth::Unknown),
        log_stream_id: row.log_stream_id.parse().map_err(storage_error)?,
        ports: Vec::new(),
        exit: row
            .exit_json
            .map(|value| serde_json::from_str::<ExitSummary>(&value).map_err(storage_error))
            .transpose()?,
        version: row.version,
        created_at: row.created_at,
        started_at: row.started_at,
        ended_at: row.ended_at,
    })
}

fn parse_executor_kind(value: &str) -> Result<ExecutorKind, RuntimeError> {
    match value {
        "local" => Ok(ExecutorKind::Local),
        "container" => Ok(ExecutorKind::Container),
        _ => Err(RuntimeError::RuntimeUnavailable),
    }
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

fn parse_job_status(value: &str) -> Result<JobStatus, RuntimeError> {
    match value {
        "queued" => Ok(JobStatus::Queued),
        "running" => Ok(JobStatus::Running),
        "succeeded" => Ok(JobStatus::Succeeded),
        "failed" => Ok(JobStatus::Failed),
        "canceled" => Ok(JobStatus::Canceled),
        "lost" => Ok(JobStatus::Lost),
        _ => Err(RuntimeError::RuntimeUnavailable),
    }
}

fn parse_service_status(value: &str) -> Result<ServiceStatus, RuntimeError> {
    match value {
        "starting" => Ok(ServiceStatus::Starting),
        "running" => Ok(ServiceStatus::Running),
        "unhealthy" => Ok(ServiceStatus::Unhealthy),
        "stopping" => Ok(ServiceStatus::Stopping),
        "stopped" => Ok(ServiceStatus::Stopped),
        "stopped_after_restart" => Ok(ServiceStatus::StoppedAfterRestart),
        "failed" => Ok(ServiceStatus::Failed),
        _ => Err(RuntimeError::RuntimeUnavailable),
    }
}

fn parse_service_impact(value: &str) -> Result<ServiceImpact, RuntimeError> {
    match value {
        "read_only" => Ok(ServiceImpact::ReadOnly),
        "ignored_output" => Ok(ServiceImpact::IgnoredOutput),
        "source_writing" => Ok(ServiceImpact::SourceWriting),
        _ => Err(RuntimeError::RuntimeUnavailable),
    }
}

const fn executor_kind_str(value: ExecutorKind) -> &'static str {
    match value {
        ExecutorKind::Local => "local",
        ExecutorKind::Container => "container",
    }
}

const fn job_status_str(value: JobStatus) -> &'static str {
    match value {
        JobStatus::Queued => "queued",
        JobStatus::Running => "running",
        JobStatus::Succeeded => "succeeded",
        JobStatus::Failed => "failed",
        JobStatus::Canceled => "canceled",
        JobStatus::Lost => "lost",
    }
}

const fn service_status_str(value: ServiceStatus) -> &'static str {
    match value {
        ServiceStatus::Starting => "starting",
        ServiceStatus::Running => "running",
        ServiceStatus::Unhealthy => "unhealthy",
        ServiceStatus::Stopping => "stopping",
        ServiceStatus::Stopped => "stopped",
        ServiceStatus::StoppedAfterRestart => "stopped_after_restart",
        ServiceStatus::Failed => "failed",
    }
}

const fn service_impact_str(value: ServiceImpact) -> &'static str {
    match value {
        ServiceImpact::ReadOnly => "read_only",
        ServiceImpact::IgnoredOutput => "ignored_output",
        ServiceImpact::SourceWriting => "source_writing",
    }
}

const fn delegated_cli_str(value: super::interface::DelegatedCliKind) -> &'static str {
    match value {
        super::interface::DelegatedCliKind::ClaudeCode => "claude_code",
        super::interface::DelegatedCliKind::Codex => "codex",
    }
}

fn command_summary(kind: &super::interface::CommandKind) -> &'static str {
    match kind {
        super::interface::CommandKind::Shell => "Shell command",
        super::interface::CommandKind::DelegatedCli {
            cli: super::interface::DelegatedCliKind::ClaudeCode,
            ..
        } => "Claude Code delegated task",
        super::interface::CommandKind::DelegatedCli {
            cli: super::interface::DelegatedCliKind::Codex,
            ..
        } => "Codex delegated task",
    }
}

fn new_version() -> String {
    format!("v_{}", RuntimeId::new())
}

fn storage_error(_error: impl Into<anyhow::Error>) -> RuntimeError {
    RuntimeError::RuntimeUnavailable
}
