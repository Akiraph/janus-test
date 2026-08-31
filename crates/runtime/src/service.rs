use std::{
    collections::HashSet,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::TryStreamExt;
use janus_infrastructure::clock::{format_utc, now_utc, now_utc_str};
use mongodb::{
    ClientSession,
    bson::{Bson, Document, doc},
};
use serde_json::json;

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
    pool: mongodb::Database,
    unit_of_work: UnitOfWork,
    logs: LogStore,
    executor: Arc<dyn RuntimeExecutor>,
    /// Broadcast of AsyncTask ids that just reached a durable terminal status.
    /// The application delivery worker subscribes and opens a new Turn.
    async_task_settled_tx: tokio::sync::broadcast::Sender<AsyncTaskId>,
    /// Single-flight for `ensure_runtime` per runtime id: the check-then-insert
    /// of the starting row and the executor start must run exactly once, or two
    /// callers can create duplicate rows / overwrite each other's nonce.
    ensure_locks: Arc<EnsureLocks>,
}

/// Leak-free keyed mutex: only ids with an ensure in flight are held, and an
/// entry is removed when the permit drops, so the set stays bounded by the
/// number of concurrent ensure calls rather than every runtime ever seen.
struct EnsureLocks {
    busy: Mutex<HashSet<String>>,
}

impl EnsureLocks {
    async fn acquire(&self, key: &str) -> EnsurePermit<'_> {
        loop {
            {
                let mut busy = self.busy.lock().expect("ensure locks poisoned");
                if busy.insert(key.to_owned()) {
                    return EnsurePermit {
                        locks: self,
                        key: key.to_owned(),
                    };
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

struct EnsurePermit<'a> {
    locks: &'a EnsureLocks,
    key: String,
}

impl Drop for EnsurePermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut busy) = self.locks.busy.lock() {
            busy.remove(&self.key);
        }
    }
}

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

impl RuntimeRow {
    fn from_document(document: &Document) -> Result<Self, RuntimeError> {
        Ok(Self {
            id: document.get_str("_id").map_err(storage_error)?.to_owned(),
            scope_kind: document
                .get_str("scope_kind")
                .map_err(storage_error)?
                .to_owned(),
            scope_id: document
                .get_str("scope_id")
                .map_err(storage_error)?
                .to_owned(),
            executor_nonce: document
                .get_str("executor_nonce")
                .map_err(storage_error)?
                .to_owned(),
            limits_json: document
                .get_str("limits_json")
                .map_err(storage_error)?
                .to_owned(),
            status: document
                .get_str("status")
                .map_err(storage_error)?
                .to_owned(),
            version: document
                .get_str("version")
                .map_err(storage_error)?
                .to_owned(),
            created_at: document
                .get_str("created_at")
                .map_err(storage_error)?
                .to_owned(),
            updated_at: document
                .get_str("updated_at")
                .map_err(storage_error)?
                .to_owned(),
            stopped_at: opt_str(document, "stopped_at"),
        })
    }
}

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

impl AsyncTaskRow {
    fn from_document(document: &Document) -> Result<Self, RuntimeError> {
        Ok(Self {
            id: document.get_str("_id").map_err(storage_error)?.to_owned(),
            runtime_id: document
                .get_str("runtime_id")
                .map_err(storage_error)?
                .to_owned(),
            session_id: document
                .get_str("session_id")
                .map_err(storage_error)?
                .to_owned(),
            initiated_by_tool_call_id: document
                .get_str("initiated_by_tool_call_id")
                .map_err(storage_error)?
                .to_owned(),
            controlling_turn_id: document
                .get_str("controlling_turn_id")
                .map_err(storage_error)?
                .to_owned(),
            command_summary: document
                .get_str("command_summary")
                .map_err(storage_error)?
                .to_owned(),
            executor_nonce: document
                .get_str("executor_nonce")
                .map_err(storage_error)?
                .to_owned(),
            log_stream_id: document
                .get_str("log_stream_id")
                .map_err(storage_error)?
                .to_owned(),
            status: document
                .get_str("status")
                .map_err(storage_error)?
                .to_owned(),
            exit_json: opt_str(document, "exit_json"),
            usage_json: document
                .get_str("usage_json")
                .map_err(storage_error)?
                .to_owned(),
            version: document
                .get_str("version")
                .map_err(storage_error)?
                .to_owned(),
            created_at: document
                .get_str("created_at")
                .map_err(storage_error)?
                .to_owned(),
            started_at: opt_str(document, "started_at"),
            ended_at: opt_str(document, "ended_at"),
        })
    }
}

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

impl TerminalRow {
    fn from_document(document: &Document) -> Result<Self, RuntimeError> {
        Ok(Self {
            id: document.get_str("_id").map_err(storage_error)?.to_owned(),
            runtime_id: document
                .get_str("runtime_id")
                .map_err(storage_error)?
                .to_owned(),
            owner_kind: document
                .get_str("owner_kind")
                .map_err(storage_error)?
                .to_owned(),
            owner_id: document
                .get_str("owner_id")
                .map_err(storage_error)?
                .to_owned(),
            executor_nonce: document
                .get_str("executor_nonce")
                .map_err(storage_error)?
                .to_owned(),
            cols: document.get_i64("cols").map_err(storage_error)?,
            rows: document.get_i64("rows").map_err(storage_error)?,
            scrollback_stream_id: document
                .get_str("scrollback_stream_id")
                .map_err(storage_error)?
                .to_owned(),
            status: document
                .get_str("status")
                .map_err(storage_error)?
                .to_owned(),
            exit_json: opt_str(document, "exit_json"),
            version: document
                .get_str("version")
                .map_err(storage_error)?
                .to_owned(),
            created_at: document
                .get_str("created_at")
                .map_err(storage_error)?
                .to_owned(),
            updated_at: document
                .get_str("updated_at")
                .map_err(storage_error)?
                .to_owned(),
            ended_at: opt_str(document, "ended_at"),
        })
    }
}

struct TerminalTicketRow {
    terminal_id: String,
    actor_id: String,
    origin: String,
    expires_at: String,
    consumed_at: Option<String>,
    revoked_at: Option<String>,
}

impl TerminalTicketRow {
    fn from_document(document: &Document) -> Result<Self, RuntimeError> {
        Ok(Self {
            terminal_id: document
                .get_str("terminal_id")
                .map_err(storage_error)?
                .to_owned(),
            actor_id: document
                .get_str("actor_id")
                .map_err(storage_error)?
                .to_owned(),
            origin: document
                .get_str("origin")
                .map_err(storage_error)?
                .to_owned(),
            expires_at: document
                .get_str("expires_at")
                .map_err(storage_error)?
                .to_owned(),
            consumed_at: opt_str(document, "consumed_at"),
            revoked_at: opt_str(document, "revoked_at"),
        })
    }
}

fn opt_str(document: &Document, key: &str) -> Option<String> {
    document.get(key).and_then(Bson::as_str).map(str::to_owned)
}

impl RuntimeInterface {
    pub fn new(
        pool: mongodb::Database,
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
            ensure_locks: Arc::new(EnsureLocks {
                busy: Mutex::new(HashSet::new()),
            }),
        }
    }

    /// Subscribe to durable AsyncTask terminal-state notifications.
    pub fn subscribe_async_task_settled(&self) -> tokio::sync::broadcast::Receiver<AsyncTaskId> {
        self.async_task_settled_tx.subscribe()
    }

    pub async fn has_unfinished_async_tasks_in_tx(
        &self,
        tx: &mut ClientSession,
        turn_id: TurnId,
    ) -> Result<bool, RuntimeError> {
        Ok(self.unfinished_async_task_count_in_tx(tx, turn_id).await? > 0)
    }

    pub async fn unfinished_async_task_count(&self, turn_id: TurnId) -> Result<i64, RuntimeError> {
        let count = self
            .pool
            .collection::<Document>("async_tasks")
            .count_documents(doc! {
                "controlling_turn_id": turn_id.to_string(),
                "status": {"$in": ["queued", "running"]},
            })
            .await
            .map_err(storage_error)?;
        i64::try_from(count).map_err(storage_error)
    }

    /// Finite AsyncTasks still controlled by `turn_id` that have not reached a terminal
    /// status. Used by application Cancel to bound Runtime cancellation before
    /// settling the Turn.
    pub async fn unfinished_async_tasks_for_turn(
        &self,
        turn_id: TurnId,
    ) -> Result<Vec<AsyncTaskProjection>, RuntimeError> {
        let mut rows = self
            .pool
            .collection::<Document>("async_tasks")
            .find(doc! {
                "controlling_turn_id": turn_id.to_string(),
                "status": {"$in": ["queued", "running"]},
            })
            .sort(doc! {"created_at": 1, "_id": 1})
            .await
            .map_err(storage_error)?;
        let mut projections = Vec::new();
        while let Some(document) = rows.try_next().await.map_err(storage_error)? {
            projections.push(async_task_projection(AsyncTaskRow::from_document(
                &document,
            )?)?);
        }
        Ok(projections)
    }

    pub async fn unfinished_async_task_count_in_tx(
        &self,
        tx: &mut ClientSession,
        turn_id: TurnId,
    ) -> Result<i64, RuntimeError> {
        let mut rows = self
            .pool
            .collection::<Document>("async_tasks")
            .find(doc! {
                "controlling_turn_id": turn_id.to_string(),
                "status": {"$in": ["queued", "running"]},
            })
            .session(&mut *tx)
            .await
            .map_err(storage_error)?;
        let mut count: i64 = 0;
        while rows
            .next(&mut *tx)
            .await
            .transpose()
            .map_err(storage_error)?
            .is_some()
        {
            count += 1;
        }
        Ok(count)
    }

    pub async fn transfer_unfinished_async_tasks_in_tx(
        &self,
        tx: &mut ClientSession,
        from_turn_id: TurnId,
        to_turn_id: TurnId,
    ) -> Result<u64, RuntimeError> {
        let changed = self
            .pool
            .collection::<Document>("async_tasks")
            .update_many(
                doc! {
                    "controlling_turn_id": from_turn_id.to_string(),
                    "status": {"$in": ["queued", "running"]},
                },
                doc! {
                    "$set": {
                        "controlling_turn_id": to_turn_id.to_string(),
                        "version": new_version(),
                    }
                },
            )
            .session(&mut *tx)
            .await
            .map_err(storage_error)?;
        Ok(changed.matched_count)
    }

    pub async fn terminal_async_tasks_for_turn_in_tx(
        &self,
        tx: &mut ClientSession,
        turn_id: TurnId,
    ) -> Result<Vec<AsyncTaskProjection>, RuntimeError> {
        let mut rows = self
            .pool
            .collection::<Document>("async_tasks")
            .find(doc! {
                "controlling_turn_id": turn_id.to_string(),
                "status": {"$in": ["succeeded", "failed", "canceled", "lost"]},
            })
            .sort(doc! {"created_at": 1, "_id": 1})
            .session(&mut *tx)
            .await
            .map_err(storage_error)?;
        let mut projections = Vec::new();
        while let Some(document) = rows
            .next(&mut *tx)
            .await
            .transpose()
            .map_err(storage_error)?
        {
            projections.push(async_task_projection(AsyncTaskRow::from_document(
                &document,
            )?)?);
        }
        Ok(projections)
    }

    pub async fn ensure_runtime(
        &self,
        spec: &RuntimeSpec,
    ) -> Result<RuntimeProjection, RuntimeError> {
        let _ensure_permit = self.ensure_locks.acquire(&spec.id().to_string()).await;
        if let Some(existing) = self.current_runtime(spec.scope()).await? {
            if existing.id != spec.id() {
                return Err(RuntimeError::unavailable(format!(
                    "a different runtime {} already exists for this scope",
                    existing.id
                )));
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
                let now = now_utc_str();
                let version = new_version();
                self.pool
                    .collection::<Document>("runtimes")
                    .update_one(
                        doc! {"_id": existing.id.to_string(), "status": {"$in": ["starting", "ready"]}},
                        doc! {
                            "$set": {
                                "executor_identity": handle.executor_identity,
                                "executor_nonce": handle.executor_nonce,
                                "status": "ready",
                                "version": version,
                                "updated_at": now,
                            }
                        },
                    )
                    .await
                    .map_err(storage_error)?;
                return self.runtime(existing.id).await;
            }
            return Err(RuntimeError::unavailable(format!(
                "runtime {} is not ready (status {:?})",
                existing.id, existing.status
            )));
        }
        let now = now_utc_str();
        let placeholder_nonce = format!("pending-{}", spec.id());
        let limits_json = serde_json::to_string(spec.limits()).map_err(storage_error)?;
        let version = new_version();
        self.pool
            .collection::<Document>("runtimes")
            .insert_one(doc! {
                "_id": spec.id().to_string(),
                "scope_kind": spec.scope().kind(),
                "scope_id": spec.scope().id(),
                "executor_nonce": placeholder_nonce,
                "limits_json": limits_json,
                "status": "starting",
                "version": version,
                "created_at": &now,
                "updated_at": &now,
            })
            .await
            .map_err(storage_error)?;

        match self.executor.ensure_runtime(spec).await {
            Ok(handle) => {
                let now = now_utc_str();
                let version = new_version();
                self.pool
                    .collection::<Document>("runtimes")
                    .update_one(
                        doc! {"_id": spec.id().to_string(), "status": "starting"},
                        doc! {
                            "$set": {
                                "executor_identity": handle.executor_identity,
                                "executor_nonce": handle.executor_nonce,
                                "status": "ready",
                                "version": version,
                                "updated_at": now,
                            }
                        },
                    )
                    .await
                    .map_err(storage_error)?;
                self.runtime(spec.id()).await
            }
            Err(error) => {
                let now = now_utc_str();
                let version = new_version();
                let _ = self
                    .pool
                    .collection::<Document>("runtimes")
                    .update_one(
                        doc! {"_id": spec.id().to_string()},
                        doc! {
                            "$set": {
                                "status": "failed",
                                "stop_reason": error.code().as_str(),
                                "version": version,
                                "updated_at": &now,
                                "stopped_at": &now,
                            }
                        },
                    )
                    .await;
                Err(error)
            }
        }
    }

    pub async fn stop_runtime(&self, id: RuntimeId) -> Result<RuntimeProjection, RuntimeError> {
        let nonce = self.runtime_nonce(id).await?;
        let now = now_utc_str();
        let version = new_version();
        self.pool
            .collection::<Document>("runtimes")
            .update_one(
                doc! {"_id": id.to_string(), "status": {"$in": ["starting", "ready"]}},
                doc! {"$set": {"status": "stopping", "version": version, "updated_at": &now}},
            )
            .await
            .map_err(storage_error)?;
        self.executor.stop_runtime(id, &nonce).await?;
        let now = now_utc_str();
        let version = new_version();
        self.pool
            .collection::<Document>("runtimes")
            .update_one(
                doc! {"_id": id.to_string()},
                doc! {
                    "$set": {
                        "status": "stopped",
                        "stop_reason": "requested",
                        "version": version,
                        "updated_at": &now,
                        "stopped_at": &now,
                    }
                },
            )
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
        let usage_json = serde_json::to_string(&ResourceUsage::default()).map_err(storage_error)?;
        let version = new_version();
        self.pool
            .collection::<Document>("async_tasks")
            .insert_one(doc! {
                "_id": async_task_id.to_string(),
                "runtime_id": spec.execution.runtime_id().to_string(),
                "session_id": spec.session_id.to_string(),
                "initiated_by_tool_call_id": spec.initiated_by_tool_call_id.to_string(),
                "controlling_turn_id": spec.controlling_turn_id.to_string(),
                "command_summary": command_summary(&spec),
                "executor_nonce": &runtime_nonce,
                "log_stream_id": log.id.to_string(),
                "status": "queued",
                "usage_json": usage_json,
                "version": version,
                "created_at": &now,
            })
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
                let version = new_version();
                let changed = self
                    .pool
                    .collection::<Document>("async_tasks")
                    .update_one(
                        doc! {
                            "_id": async_task_id.to_string(),
                            "status": "queued",
                            "cancellation_requested_at": null,
                        },
                        doc! {
                            "$set": {
                                "executor_process_identity": handle.process_identity,
                                "status": "running",
                                "version": version,
                                "started_at": started_at,
                            }
                        },
                    )
                    .session(work.connection())
                    .await
                    .map_err(storage_error)?
                    .matched_count;
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
                Err(RuntimeError::unavailable(
                    "async task executor nonce mismatch",
                ))
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
        let now = now_utc_str();
        let version = new_version();
        self.pool
            .collection::<Document>("async_tasks")
            .update_one(
                doc! {
                    "_id": id.to_string(),
                    "status": {"$in": ["queued", "running"]},
                    "cancellation_requested_at": null,
                },
                doc! {"$set": {"cancellation_requested_at": now, "version": version}},
            )
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
        let row = self
            .pool
            .collection::<Document>("runtimes")
            .find_one(doc! {
                "scope_kind": scope.kind(),
                "scope_id": scope.id(),
                "status": {"$in": ["starting", "ready", "stopping"]},
            })
            .await
            .map_err(storage_error)?;
        row.as_ref()
            .map(RuntimeRow::from_document)
            .transpose()?
            .map(runtime_projection)
            .transpose()
    }

    pub async fn live_runtimes(&self) -> Result<Vec<RuntimeProjection>, RuntimeError> {
        let mut rows = self
            .pool
            .collection::<Document>("runtimes")
            .find(doc! {"status": {"$in": ["starting", "ready", "stopping"]}})
            .sort(doc! {"created_at": 1, "_id": 1})
            .await
            .map_err(storage_error)?;
        let mut projections = Vec::new();
        while let Some(document) = rows.try_next().await.map_err(storage_error)? {
            projections.push(runtime_projection(RuntimeRow::from_document(&document)?)?);
        }
        Ok(projections)
    }

    pub async fn delete_session_log_files(
        &self,
        session_id: SessionId,
    ) -> Result<(), RuntimeError> {
        let ids = self.session_log_stream_ids(&session_id.to_string()).await?;
        let ids = ids
            .into_iter()
            .map(|id| id.parse().map_err(storage_error))
            .collect::<Result<Vec<LogStreamId>, _>>()?;
        self.logs.delete_files(&ids).await
    }

    pub async fn delete_project_log_files(
        &self,
        project_id: ProjectId,
    ) -> Result<(), RuntimeError> {
        let ids = self.project_log_stream_ids(&project_id.to_string()).await?;
        let ids = ids
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
        let log_ids = self.project_log_stream_ids(&project_id).await?;
        let mut work = self.unit_of_work.begin().await.map_err(storage_error)?;
        let mut runtime_ids = Vec::new();
        let mut runtimes = self
            .pool
            .collection::<Document>("runtimes")
            .find(doc! {"scope_kind": "project", "scope_id": &project_id})
            .session(&mut *work.connection())
            .await
            .map_err(storage_error)?;
        while let Some(document) = runtimes
            .next(&mut *work.connection())
            .await
            .transpose()
            .map_err(storage_error)?
        {
            runtime_ids.push(document.get_str("_id").map_err(storage_error)?.to_owned());
        }
        if !runtime_ids.is_empty() {
            self.pool
                .collection::<Document>("terminals")
                .delete_many(doc! {"runtime_id": {"$in": &runtime_ids}})
                .session(&mut *work.connection())
                .await
                .map_err(storage_error)?;
            self.pool
                .collection::<Document>("runtimes")
                .delete_many(doc! {"_id": {"$in": &runtime_ids}})
                .session(&mut *work.connection())
                .await
                .map_err(storage_error)?;
        }
        if !log_ids.is_empty() {
            self.pool
                .collection::<Document>("log_streams")
                .delete_many(doc! {"_id": {"$in": &log_ids}})
                .session(&mut *work.connection())
                .await
                .map_err(storage_error)?;
        }
        work.commit().await.map_err(storage_error)
    }

    pub async fn delete_session_resources_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
    ) -> Result<(), RuntimeError> {
        let session_id = session_id.to_string();
        let log_ids = self.session_log_stream_ids_in_tx(tx, &session_id).await?;
        self.pool
            .collection::<Document>("async_tasks")
            .delete_many(doc! {"session_id": &session_id})
            .session(&mut *tx)
            .await
            .map_err(storage_error)?;
        if !log_ids.is_empty() {
            self.pool
                .collection::<Document>("log_streams")
                .delete_many(doc! {"_id": {"$in": &log_ids}})
                .session(&mut *tx)
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
        let limit = i64::try_from(limit.clamp(1, 1000)).unwrap_or(1000);
        let mut rows = self
            .pool
            .collection::<Document>("async_tasks")
            .find(doc! {})
            .sort(doc! {"created_at": -1, "_id": -1})
            .limit(limit)
            .await
            .map_err(storage_error)?;
        let mut projections = Vec::new();
        while let Some(document) = rows.try_next().await.map_err(storage_error)? {
            projections.push(async_task_projection(AsyncTaskRow::from_document(
                &document,
            )?)?);
        }
        Ok(projections)
    }

    pub async fn undelivered_terminal_task_ids(
        &self,
        limit: usize,
    ) -> Result<Vec<AsyncTaskId>, RuntimeError> {
        let limit = i64::try_from(limit.clamp(1, 1000)).unwrap_or(1000);
        let mut rows = self
            .pool
            .collection::<Document>("async_tasks")
            .find(doc! {
                "status": {"$in": ["succeeded", "failed", "canceled", "lost"]},
                "delivery_completed_at": null,
            })
            .sort(doc! {"ended_at": 1, "_id": 1})
            .limit(limit)
            .await
            .map_err(storage_error)?;
        let mut ids = Vec::new();
        while let Some(document) = rows.try_next().await.map_err(storage_error)? {
            let id = document.get_str("_id").map_err(storage_error)?.to_owned();
            ids.push(id.parse().map_err(storage_error)?);
        }
        Ok(ids)
    }

    pub async fn claim_task_delivery_in_tx(
        &self,
        tx: &mut ClientSession,
        id: AsyncTaskId,
    ) -> Result<bool, RuntimeError> {
        let changed = self
            .pool
            .collection::<Document>("async_tasks")
            .update_one(
                doc! {
                    "_id": id.to_string(),
                    "status": {"$in": ["succeeded", "failed", "canceled", "lost"]},
                    "delivery_completed_at": null,
                    "delivery_claimed_at": null,
                },
                doc! {"$set": {"delivery_claimed_at": now_utc_str()}},
            )
            .session(&mut *tx)
            .await
            .map_err(storage_error)?;
        Ok(changed.matched_count == 1)
    }

    pub async fn complete_task_delivery_in_tx(
        &self,
        tx: &mut ClientSession,
        id: AsyncTaskId,
    ) -> Result<(), RuntimeError> {
        self.pool
            .collection::<Document>("async_tasks")
            .update_one(
                doc! {"_id": id.to_string(), "delivery_completed_at": null},
                doc! {"$set": {"delivery_completed_at": now_utc_str()}},
            )
            .session(&mut *tx)
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
        let version = new_version();
        self.pool
            .collection::<Document>("terminals")
            .insert_one(doc! {
                "_id": spec.id.to_string(),
                "runtime_id": spec.runtime_id.to_string(),
                "owner_kind": "project",
                "owner_id": spec.project_id.to_string(),
                "executor_nonce": &runtime_nonce,
                "cols": i64::from(spec.size.cols),
                "rows": i64::from(spec.size.rows),
                "scrollback_stream_id": scrollback.id.to_string(),
                "status": "starting",
                "version": version,
                "created_at": &now,
                "updated_at": &now,
            })
            .await
            .map_err(storage_error)?;
        let terminal_id = spec.id;
        match self.executor.start_terminal(spec, scrollback.id).await {
            Ok(handle) if handle.executor_nonce == runtime_nonce => {
                let mut work = self.unit_of_work.begin().await.map_err(storage_error)?;
                let version = new_version();
                let changed = self
                    .pool
                    .collection::<Document>("terminals")
                    .update_one(
                        doc! {"_id": terminal_id.to_string(), "status": "starting"},
                        doc! {
                            "$set": {
                                "executor_pty_identity": handle.process_identity,
                                "status": "running",
                                "version": version,
                                "updated_at": now_utc_str(),
                            }
                        },
                    )
                    .session(work.connection())
                    .await
                    .map_err(storage_error)?
                    .matched_count;
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
                Err(RuntimeError::unavailable(
                    "terminal executor nonce mismatch",
                ))
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
        let mut rows = self
            .pool
            .collection::<Document>("terminals")
            .find(doc! {"owner_kind": "project", "owner_id": project_id.to_string()})
            .sort(doc! {"created_at": 1, "_id": 1})
            .await
            .map_err(storage_error)?;
        let mut projections = Vec::new();
        while let Some(document) = rows.try_next().await.map_err(storage_error)? {
            projections.push(terminal_projection(TerminalRow::from_document(&document)?)?);
        }
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
        self.pool
            .collection::<Document>("runtime_access_tickets")
            .insert_one(doc! {
                "_id": &id,
                "terminal_id": &row.id,
                "token_hash": &token_hash,
                "actor_id": &request.actor_id,
                "origin": &request.origin,
                "expires_at": &expires_at,
                "created_at": format_utc(now),
            })
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
        let row = self
            .pool
            .collection::<Document>("runtime_access_tickets")
            .find_one(doc! {"token_hash": &token_hash})
            .await
            .map_err(storage_error)?
            .map(|document| TerminalTicketRow::from_document(&document))
            .transpose()?
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
        let changed = self
            .pool
            .collection::<Document>("runtime_access_tickets")
            .update_one(
                doc! {
                    "token_hash": &token_hash,
                    "consumed_at": null,
                    "revoked_at": null,
                },
                doc! {"$set": {"consumed_at": format_utc(now)}},
            )
            .await
            .map_err(storage_error)?;
        if changed.matched_count == 0 {
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
        let version = new_version();
        let changed = self
            .pool
            .collection::<Document>("terminals")
            .update_one(
                doc! {"_id": id.to_string(), "status": {"$in": ["starting", "running"]}},
                doc! {
                    "$set": {
                        "cols": i64::from(size.cols),
                        "rows": i64::from(size.rows),
                        "version": version,
                        "updated_at": now_utc_str(),
                    }
                },
            )
            .session(work.connection())
            .await
            .map_err(storage_error)?
            .matched_count;
        if changed == 0 {
            work.rollback().await.map_err(storage_error)?;
            return Err(RuntimeError::TerminalNotWritable(id));
        }
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
        let version = new_version();
        self.pool
            .collection::<Document>("terminals")
            .update_one(
                doc! {"_id": id.to_string(), "status": {"$in": ["starting", "running"]}},
                doc! {"$set": {"status": "closing", "version": version, "updated_at": now_utc_str()}},
            )
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
        let exit_json = serde_json::to_string(&completion.exit).map_err(storage_error)?;
        let version = new_version();
        let now = now_utc_str();
        let changed = self
            .pool
            .collection::<Document>("terminals")
            .update_one(
                doc! {"_id": id.to_string(), "status": {"$in": ["starting", "running", "closing"]}},
                doc! {
                    "$set": {
                        "status": terminal_status_str(status),
                        "exit_json": exit_json,
                        "version": version,
                        "updated_at": &now,
                        "ended_at": &now,
                    }
                },
            )
            .session(work.connection())
            .await
            .map_err(storage_error)?
            .matched_count;
        if changed != 0 {
            self.append_terminal_changed_in_tx(&mut work, id).await?;
            work.commit().await.map_err(storage_error)?;
        } else {
            work.rollback().await.map_err(storage_error)?;
        }
        Ok(())
    }

    async fn mark_terminal_failed(&self, id: TerminalId) -> Result<(), RuntimeError> {
        let scrollback_stream_id = self
            .terminal_row(id)
            .await?
            .scrollback_stream_id
            .parse::<LogStreamId>()
            .map_err(storage_error)?;
        let mut work = self.unit_of_work.begin().await.map_err(storage_error)?;
        let version = new_version();
        let now = now_utc_str();
        let changed = self
            .pool
            .collection::<Document>("terminals")
            .update_one(
                doc! {"_id": id.to_string(), "status": "starting"},
                doc! {
                    "$set": {
                        "status": "failed",
                        "exit_json": json!({"exit_code": null, "signal": "start_failed"}).to_string(),
                        "version": version,
                        "updated_at": &now,
                        "ended_at": &now,
                    }
                },
            )
            .session(work.connection())
            .await
            .map_err(storage_error)?
            .matched_count;
        if changed != 0 {
            self.append_terminal_changed_in_tx(&mut work, id).await?;
            work.commit().await.map_err(storage_error)?;
        } else {
            work.rollback().await.map_err(storage_error)?;
        }
        let _ = self.logs.close(scrollback_stream_id).await;
        Ok(())
    }

    async fn append_terminal_changed_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction,
        id: TerminalId,
    ) -> Result<TerminalProjection, RuntimeError> {
        let document = self
            .pool
            .collection::<Document>("terminals")
            .find_one(doc! {"_id": id.to_string()})
            .session(&mut *work.connection())
            .await
            .map_err(storage_error)?
            .ok_or(RuntimeError::TerminalNotWritable(id))?;
        let mut terminal = terminal_projection(TerminalRow::from_document(&document)?)?;
        let stream = self
            .pool
            .collection::<Document>("log_streams")
            .find_one(doc! {"_id": terminal.scrollback_stream_id.to_string()})
            .session(&mut *work.connection())
            .await
            .map_err(storage_error)?;
        if let Some(stream) = stream {
            terminal.first_cursor = LogCursor::new(
                u64::try_from(stream.get_i64("first_cursor").map_err(storage_error)?)
                    .map_err(storage_error)?,
            );
            terminal.next_cursor = LogCursor::new(
                u64::try_from(stream.get_i64("next_cursor").map_err(storage_error)?)
                    .map_err(storage_error)?,
            );
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
        work: &mut UnitOfWorkTransaction,
        now: &str,
    ) -> Result<(), RuntimeError> {
        let mut runtime_ids = Vec::new();
        let mut runtimes = self
            .pool
            .collection::<Document>("runtimes")
            .find(doc! {"status": {"$in": ["starting", "ready", "stopping"]}})
            .sort(doc! {"created_at": 1, "_id": 1})
            .session(&mut *work.connection())
            .await
            .map_err(storage_error)?;
        while let Some(document) = runtimes
            .next(&mut *work.connection())
            .await
            .transpose()
            .map_err(storage_error)?
        {
            let id = document.get_str("_id").map_err(storage_error)?.to_owned();
            runtime_ids.push(id.parse::<RuntimeId>().map_err(storage_error)?);
        }
        let mut async_task_ids = Vec::new();
        let mut tasks = self
            .pool
            .collection::<Document>("async_tasks")
            .find(doc! {"status": {"$in": ["queued", "running"]}})
            .sort(doc! {"created_at": 1, "_id": 1})
            .session(&mut *work.connection())
            .await
            .map_err(storage_error)?;
        while let Some(document) = tasks
            .next(&mut *work.connection())
            .await
            .transpose()
            .map_err(storage_error)?
        {
            let id = document.get_str("_id").map_err(storage_error)?.to_owned();
            async_task_ids.push(id.parse::<AsyncTaskId>().map_err(storage_error)?);
        }
        let mut terminal_ids = Vec::new();
        let mut terminals = self
            .pool
            .collection::<Document>("terminals")
            .find(doc! {"status": {"$in": ["starting", "running", "closing"]}})
            .sort(doc! {"created_at": 1, "_id": 1})
            .session(&mut *work.connection())
            .await
            .map_err(storage_error)?;
        while let Some(document) = terminals
            .next(&mut *work.connection())
            .await
            .transpose()
            .map_err(storage_error)?
        {
            let id = document.get_str("_id").map_err(storage_error)?.to_owned();
            terminal_ids.push(id.parse::<TerminalId>().map_err(storage_error)?);
        }

        let version = new_version();
        self.pool
            .collection::<Document>("runtimes")
            .update_many(
                doc! {"status": {"$in": ["starting", "ready", "stopping"]}},
                doc! {
                    "$set": {
                        "status": "lost",
                        "stop_reason": "control_plane_restart",
                        "version": version,
                        "updated_at": now,
                        "stopped_at": now,
                    }
                },
            )
            .session(&mut *work.connection())
            .await
            .map_err(storage_error)?;
        let version = new_version();
        self.pool
            .collection::<Document>("async_tasks")
            .update_many(
                doc! {"status": {"$in": ["queued", "running"]}},
                doc! {
                    "$set": {
                        "status": "lost",
                        "exit_json": json!({"exit_code": null, "signal": "control_plane_restart"})
                            .to_string(),
                        "version": version,
                        "ended_at": now,
                    }
                },
            )
            .session(&mut *work.connection())
            .await
            .map_err(storage_error)?;
        let version = new_version();
        self.pool
            .collection::<Document>("terminals")
            .update_many(
                doc! {"status": {"$in": ["starting", "running", "closing"]}},
                doc! {
                    "$set": {
                        "status": "lost",
                        "version": version,
                        "updated_at": now,
                        "ended_at": now,
                    }
                },
            )
            .session(&mut *work.connection())
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
        self.pool
            .collection::<Document>("runtime_access_tickets")
            .update_many(
                doc! {"consumed_at": null, "revoked_at": null},
                doc! {"$set": {"revoked_at": now}},
            )
            .session(&mut *work.connection())
            .await
            .map_err(storage_error)?;
        Ok(())
    }

    async fn append_runtime_changed_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction,
        id: RuntimeId,
    ) -> Result<RuntimeProjection, RuntimeError> {
        let document = self
            .pool
            .collection::<Document>("runtimes")
            .find_one(doc! {"_id": id.to_string()})
            .session(&mut *work.connection())
            .await
            .map_err(storage_error)?
            .ok_or_else(|| RuntimeError::unavailable(format!("runtime {id} was not found")))?;
        let runtime = runtime_projection(RuntimeRow::from_document(&document)?)?;
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
        let cancel_requested = self
            .pool
            .collection::<Document>("async_tasks")
            .find_one(doc! {"_id": id.to_string()})
            .session(&mut *work.connection())
            .await
            .map_err(storage_error)?
            .map(|document| {
                document
                    .get("cancellation_requested_at")
                    .and_then(Bson::as_str)
                    .is_some()
            })
            .unwrap_or(false);
        let status = forced.unwrap_or_else(|| {
            if cancel_requested {
                AsyncTaskStatus::Canceled
            } else if completion.exit.exit_code == Some(0) {
                AsyncTaskStatus::Succeeded
            } else {
                AsyncTaskStatus::Failed
            }
        });
        let version = new_version();
        let now = now_utc_str();
        let exit_json = serde_json::to_string(&completion.exit).map_err(storage_error)?;
        let usage_json = serde_json::to_string(&completion.usage).map_err(storage_error)?;
        let changed = self
            .pool
            .collection::<Document>("async_tasks")
            .update_one(
                doc! {"_id": id.to_string(), "status": {"$in": ["queued", "running"]}},
                doc! {
                    "$set": {
                        "status": async_task_status_str(status),
                        "exit_json": exit_json,
                        "usage_json": usage_json,
                        "version": version,
                        "ended_at": now,
                    }
                },
            )
            .session(work.connection())
            .await
            .map_err(storage_error)?
            .matched_count;
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
        work: &mut UnitOfWorkTransaction,
        id: AsyncTaskId,
    ) -> Result<AsyncTaskProjection, RuntimeError> {
        let document = self
            .pool
            .collection::<Document>("async_tasks")
            .find_one(doc! {"_id": id.to_string()})
            .session(&mut *work.connection())
            .await
            .map_err(storage_error)?
            .ok_or(RuntimeError::AsyncTaskLost(id))?;
        let async_task = async_task_projection(AsyncTaskRow::from_document(&document)?)?;
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
        self.pool
            .collection::<Document>("runtimes")
            .find_one(doc! {"_id": id.to_string()})
            .await
            .map_err(storage_error)?
            .map(|document| RuntimeRow::from_document(&document))
            .transpose()?
            .ok_or_else(|| RuntimeError::unavailable(format!("runtime {id} was not found")))
    }

    async fn runtime_nonce(&self, id: RuntimeId) -> Result<String, RuntimeError> {
        let row = self.runtime_row(id).await?;
        if row.status != "ready" {
            return Err(RuntimeError::unavailable(format!(
                "runtime {id} is not ready"
            )));
        }
        Ok(row.executor_nonce)
    }

    async fn async_task_row(&self, id: AsyncTaskId) -> Result<AsyncTaskRow, RuntimeError> {
        self.pool
            .collection::<Document>("async_tasks")
            .find_one(doc! {"_id": id.to_string()})
            .await
            .map_err(storage_error)?
            .map(|document| AsyncTaskRow::from_document(&document))
            .transpose()?
            .ok_or(RuntimeError::AsyncTaskLost(id))
    }

    async fn async_task_nonce(&self, id: AsyncTaskId) -> Result<String, RuntimeError> {
        Ok(self.async_task_row(id).await?.executor_nonce)
    }

    async fn terminal_row(&self, id: TerminalId) -> Result<TerminalRow, RuntimeError> {
        self.pool
            .collection::<Document>("terminals")
            .find_one(doc! {"_id": id.to_string()})
            .await
            .map_err(storage_error)?
            .map(|document| TerminalRow::from_document(&document))
            .transpose()?
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
        let document = self
            .pool
            .collection::<Document>("async_tasks")
            .find_one(doc! {"_id": id.to_string()})
            .await
            .map_err(storage_error)?
            .ok_or(RuntimeError::AsyncTaskLost(id))?;
        Ok(document
            .get("cancellation_requested_at")
            .and_then(Bson::as_str)
            .is_some())
    }

    async fn cancel_started_async_task(
        &self,
        id: AsyncTaskId,
        nonce: &str,
    ) -> Result<AsyncTaskProjection, RuntimeError> {
        // Kill the process; the spawned wait_async_task watcher settles the
        // durable projection once the process exits. Finalizing here as well
        // would open a second transaction writing the same async_tasks
        // document concurrently (MongoDB WriteConflict), so poll for the
        // terminal state instead and fall back to marking the task lost if the
        // watcher never settles.
        let _ = tokio::time::timeout(
            ASYNC_TASK_CANCEL_TIMEOUT,
            self.executor.cancel_async_task(id, nonce),
        )
        .await;
        let deadline = tokio::time::Instant::now() + ASYNC_TASK_SETTLE_GRACE;
        loop {
            let async_task = self.async_task(id).await?;
            if async_task.status.is_terminal() {
                return Ok(async_task);
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(ASYNC_TASK_CANCEL_POLL_INTERVAL).await;
        }
        self.mark_async_task_lost(id, "cancel_unconfirmed").await?;
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
        let version = new_version();
        let now = now_utc_str();
        let changed = self
            .pool
            .collection::<Document>("async_tasks")
            .update_one(
                doc! {"_id": id.to_string(), "status": {"$in": ["queued", "running"]}},
                doc! {
                    "$set": {
                        "status": "lost",
                        "exit_json": json!({"exit_code": null, "signal": reason}).to_string(),
                        "version": version,
                        "ended_at": now,
                    }
                },
            )
            .session(work.connection())
            .await
            .map_err(storage_error)?
            .matched_count;
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
        let version = new_version();
        let now = now_utc_str();
        let changed = self
            .pool
            .collection::<Document>("terminals")
            .update_one(
                doc! {"_id": id.to_string(), "status": {"$in": ["starting", "running", "closing"]}},
                doc! {
                    "$set": {
                        "status": "lost",
                        "exit_json": json!({"exit_code": null, "signal": reason}).to_string(),
                        "version": version,
                        "updated_at": &now,
                        "ended_at": &now,
                    }
                },
            )
            .session(work.connection())
            .await
            .map_err(storage_error)?
            .matched_count;
        if changed != 0 {
            self.append_terminal_changed_in_tx(&mut work, id).await?;
            work.commit().await.map_err(storage_error)?;
        } else {
            work.rollback().await.map_err(storage_error)?;
        }
        let _ = self.logs.close(scrollback_stream_id).await;
        Ok(())
    }

    async fn session_log_stream_ids(&self, session_id: &str) -> Result<Vec<String>, RuntimeError> {
        let mut async_task_ids = Vec::new();
        let mut tasks = self
            .pool
            .collection::<Document>("async_tasks")
            .find(doc! {"session_id": session_id})
            .await
            .map_err(storage_error)?;
        while let Some(document) = tasks.try_next().await.map_err(storage_error)? {
            async_task_ids.push(document.get_str("_id").map_err(storage_error)?.to_owned());
        }
        if async_task_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut log_ids = Vec::new();
        let mut streams = self
            .pool
            .collection::<Document>("log_streams")
            .find(doc! {"owner_kind": "async_task", "owner_id": {"$in": &async_task_ids}})
            .await
            .map_err(storage_error)?;
        while let Some(document) = streams.try_next().await.map_err(storage_error)? {
            log_ids.push(document.get_str("_id").map_err(storage_error)?.to_owned());
        }
        Ok(log_ids)
    }

    async fn session_log_stream_ids_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: &str,
    ) -> Result<Vec<String>, RuntimeError> {
        let mut async_task_ids = Vec::new();
        let mut tasks = self
            .pool
            .collection::<Document>("async_tasks")
            .find(doc! {"session_id": session_id})
            .session(&mut *tx)
            .await
            .map_err(storage_error)?;
        while let Some(document) = tasks
            .next(&mut *tx)
            .await
            .transpose()
            .map_err(storage_error)?
        {
            async_task_ids.push(document.get_str("_id").map_err(storage_error)?.to_owned());
        }
        if async_task_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut log_ids = Vec::new();
        let mut streams = self
            .pool
            .collection::<Document>("log_streams")
            .find(doc! {"owner_kind": "async_task", "owner_id": {"$in": &async_task_ids}})
            .session(&mut *tx)
            .await
            .map_err(storage_error)?;
        while let Some(document) = streams
            .next(&mut *tx)
            .await
            .transpose()
            .map_err(storage_error)?
        {
            log_ids.push(document.get_str("_id").map_err(storage_error)?.to_owned());
        }
        Ok(log_ids)
    }

    async fn project_log_stream_ids(&self, project_id: &str) -> Result<Vec<String>, RuntimeError> {
        let mut runtime_ids = Vec::new();
        let mut runtimes = self
            .pool
            .collection::<Document>("runtimes")
            .find(doc! {"scope_kind": "project", "scope_id": project_id})
            .await
            .map_err(storage_error)?;
        while let Some(document) = runtimes.try_next().await.map_err(storage_error)? {
            runtime_ids.push(document.get_str("_id").map_err(storage_error)?.to_owned());
        }
        if runtime_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut terminal_ids = Vec::new();
        let mut terminals = self
            .pool
            .collection::<Document>("terminals")
            .find(doc! {"runtime_id": {"$in": &runtime_ids}})
            .await
            .map_err(storage_error)?;
        while let Some(document) = terminals.try_next().await.map_err(storage_error)? {
            terminal_ids.push(document.get_str("_id").map_err(storage_error)?.to_owned());
        }
        let mut log_ids = Vec::new();
        let filter = if terminal_ids.is_empty() {
            doc! {"owner_kind": "sync", "runtime_id": {"$in": &runtime_ids}}
        } else {
            doc! {
                "$or": [
                    doc! {"owner_kind": "terminal", "owner_id": {"$in": &terminal_ids}},
                    doc! {"owner_kind": "sync", "runtime_id": {"$in": &runtime_ids}},
                ]
            }
        };
        let mut streams = self
            .pool
            .collection::<Document>("log_streams")
            .find(filter)
            .await
            .map_err(storage_error)?;
        while let Some(document) = streams.try_next().await.map_err(storage_error)? {
            log_ids.push(document.get_str("_id").map_err(storage_error)?.to_owned());
        }
        Ok(log_ids)
    }
}

const TICKET_TTL: chrono::TimeDelta = chrono::Duration::seconds(30);
const ASYNC_TASK_CANCEL_TIMEOUT: Duration = Duration::from_secs(5);
const ASYNC_TASK_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// How long a cancel waits for the wait_async_task watcher to settle the
/// durable projection after the process has been killed.
const ASYNC_TASK_SETTLE_GRACE: Duration = Duration::from_secs(2);
const COMMAND_SUMMARY_MAX_CHARS: usize = 120;

fn terminal_projection(row: TerminalRow) -> Result<TerminalProjection, RuntimeError> {
    if row.owner_kind != "project" {
        return Err(RuntimeError::unavailable(format!(
            "terminal owner kind is {:?}, not \"project\"",
            row.owner_kind
        )));
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
        _ => Err(RuntimeError::unavailable(format!(
            "unknown terminal status {value:?}"
        ))),
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
        _ => Err(RuntimeError::unavailable(format!(
            "unknown runtime scope kind {kind:?}"
        ))),
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
        _ => Err(RuntimeError::unavailable(format!(
            "unknown runtime status {value:?}"
        ))),
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
        _ => Err(RuntimeError::unavailable(format!(
            "unknown async task status {value:?}"
        ))),
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

fn storage_error(error: impl Into<anyhow::Error>) -> RuntimeError {
    RuntimeError::unavailable(error.into().to_string())
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
