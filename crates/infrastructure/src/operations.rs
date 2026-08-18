//! Durable operation journal and work queue.
//!
//! Callers record intent before external side effects; stable step keys and leases
//! make retries and process restarts idempotent. Work kinds and handlers live in
//! server/application - not here.

use serde::Serialize;
use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};
use utoipa::ToSchema;

use super::{
    events::{EventStore, EventType, NewEvent},
    unit_of_work::{UnitOfWork, UnitOfWorkTransaction},
};
use crate::{
    clock::{format_utc, now_utc_str},
    id::{CorrelationId, OperationId, WorkItemId},
    random_hex_token,
};

pub const MAX_WORK_ATTEMPTS: i64 = 5;
const MAX_WORK_RETRY_DELAY_SECONDS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkFailureDisposition {
    Retry,
    DeadLetter,
}

/// Operation status. `needs_attention` means execution stopped at a decidable
/// state that requires an explicit follow-up command, not an unknown failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Canceled,
    NeedsAttention,
}

impl OperationStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::NeedsAttention => "needs_attention",
        }
    }
}

pub struct OperationCompletion {
    pub status: OperationStatus,
    pub result: Option<Value>,
    pub problem: Option<Value>,
    pub correlation_id: CorrelationId,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OperationView {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub target_kind: String,
    pub target_id: Option<String>,
    pub current_step: Option<String>,
    pub progress: Option<Value>,
    pub result: Option<Value>,
    pub problem: Option<Value>,
    pub correlation_id: String,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    #[error("operation not found")]
    NotFound,
    #[error("work claim is stale")]
    StaleWorkClaim,
    #[error("storage error: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

/// What an idempotent request resolved to. `Stored` means a prior identical
/// request's result is returned; `Reused` means an in-flight operation's id is
/// returned without waiting.
#[derive(Debug, Clone, Copy)]
pub enum IdempotencyOutcome {
    New,
    Stored,
    Reused,
}

#[derive(Clone)]
pub struct OperationInterface {
    pool: SqlitePool,
    unit_of_work: UnitOfWork,
}

impl OperationInterface {
    pub fn new(pool: SqlitePool, events: EventStore) -> Self {
        Self {
            pool: pool.clone(),
            unit_of_work: UnitOfWork::new(pool, events),
        }
    }

    /// Create an operation, honoring idempotency. Same key + same digest returns
    /// the stored result; same key + different digest is a caller error.
    /// `digest` should cover the normalized request body so retries are safe.
    pub async fn create(
        &self,
        request: CreateOperation<'_>,
        work: Option<CreateWork<'_>>,
    ) -> Result<CreatedOperation, OperationError> {
        let mut transaction = self.unit_of_work.begin().await?;
        let created = self.create_in_tx(&mut transaction, request, work).await?;
        transaction.commit().await?;
        Ok(created)
    }

    pub async fn create_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        request: CreateOperation<'_>,
        work_item: Option<CreateWork<'_>>,
    ) -> Result<CreatedOperation, OperationError> {
        if let Some(idem) = &request.idempotency
            && let Some(view) = self
                .lookup_idempotency_in_tx(work.connection(), idem)
                .await?
        {
            let terminal = matches!(
                view.status.as_str(),
                "succeeded" | "failed" | "canceled" | "needs_attention"
            );
            return Ok(CreatedOperation {
                operation: view,
                outcome: if terminal {
                    IdempotencyOutcome::Stored
                } else {
                    IdempotencyOutcome::Reused
                },
            });
        }

        let kind = request.kind;
        let id = OperationId::new();
        let now = now_utc_str();
        let version = format!("v_{}", OperationId::new());
        let operation_id = id.to_string();
        sqlx::query("INSERT INTO operations (id, kind, actor_json, target_kind, target_id, status, current_step, conditions_json, result_json, problem_json, correlation_id, lease_nonce, lease_expires_at, progress_json, version, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 'queued', NULL, ?, NULL, NULL, ?, NULL, NULL, NULL, ?, ?, ?)")
            .bind(&operation_id)
            .bind(kind)
            .bind(serde_json::to_string(&request.actor)?)
            .bind(request.target_kind)
            .bind(request.target_id)
            .bind(serde_json::to_string(&request.conditions)?)
            .bind(request.correlation_id.to_string())
            .bind(&version)
            .bind(&now)
            .bind(&now)
            .execute(work.connection())
            .await?;
        if let Some(idem) = &request.idempotency {
            sqlx::query("INSERT INTO idempotency_records (key, owner_id, method, normalized_route, request_digest, status, response_ref, operation_id, expires_at) VALUES (?, ?, ?, ?, ?, 'running', NULL, ?, ?)")
                .bind(&idem.key)
                .bind(&idem.owner_id)
                .bind(&idem.method)
                .bind(&idem.normalized_route)
                .bind(&idem.digest)
                .bind(&operation_id)
                .bind(&idem.expires_at)
                .execute(work.connection())
                .await?;
        }
        if let Some(work_item) = work_item {
            let work_id = WorkItemId::new();
            let mut payload = work_item.payload;
            let fields = payload.as_object_mut().ok_or_else(|| {
                OperationError::Internal(anyhow::anyhow!("work payload must be a JSON object"))
            })?;
            fields.insert(
                "operation_id".into(),
                serde_json::Value::String(operation_id.clone()),
            );
            sqlx::query("INSERT INTO work_items (id, handler_kind, payload_json, not_before, lease_nonce, lease_expires_at, attempts, dead, created_at) VALUES (?, ?, ?, ?, NULL, NULL, 0, 0, ?)")
                .bind(work_id.to_string())
                .bind(work_item.handler_kind)
                .bind(serde_json::to_string(&payload)?)
                .bind(&now)
                .bind(&now)
                .execute(work.connection())
                .await?;
        }
        self.emit_operation_changed(
            work,
            &operation_id,
            kind,
            "queued",
            &version,
            &request.correlation_id,
        )
        .await?;
        Ok(CreatedOperation {
            operation: self
                .get_in_tx(work.connection(), &operation_id)
                .await?
                .ok_or(OperationError::NotFound)?,
            outcome: IdempotencyOutcome::New,
        })
    }

    /// Enqueue a generic durable work item in the caller's transaction.
    ///
    /// The infrastructure layer knows only the queue shape. Handler kinds and
    /// their payload semantics remain owned by the application control plane.
    pub async fn enqueue_work_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        handler_kind: &str,
        payload: Value,
    ) -> Result<WorkItemId, OperationError> {
        let work_id = WorkItemId::new();
        let now = now_utc_str();
        sqlx::query(
            "INSERT INTO work_items \
             (id, handler_kind, payload_json, not_before, lease_nonce, lease_expires_at, \
              attempts, dead, created_at) \
             VALUES (?, ?, ?, ?, NULL, NULL, 0, 0, ?)",
        )
        .bind(work_id.to_string())
        .bind(handler_kind)
        .bind(serde_json::to_string(&payload)?)
        .bind(&now)
        .bind(&now)
        .execute(work.connection())
        .await?;
        Ok(work_id)
    }

    /// Claim the oldest available work item for a handler.
    ///
    /// A random nonce leases the row so only the holder may complete it; expired
    /// leases are reclaimable after crash or cancellation.
    pub async fn claim_work(
        &self,
        handler_kind: &str,
        lease_ttl_seconds: i64,
    ) -> Result<Option<ClaimedWork>, OperationError> {
        let now_dt = crate::clock::now_utc();
        let now = now_utc_str();
        let lease_expires = format_utc(now_dt + chrono::Duration::seconds(lease_ttl_seconds));
        let nonce = random_hex_token();

        // A worker can die without calling fail_work. Reclaiming an expired
        // lease must still obey the same attempt bound as an explicit failure.
        sqlx::query(
            "UPDATE work_items SET dead = 1 \
             WHERE dead = 0 AND attempts >= ? \
               AND lease_expires_at IS NOT NULL AND lease_expires_at < ?",
        )
        .bind(MAX_WORK_ATTEMPTS)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        // The conditional update is the ownership check. Keep it even if the
        // candidate query changes: another worker may claim the row in between.
        let candidate: Option<(String, String)> = sqlx::query_as(
            "SELECT id, payload_json FROM work_items
             WHERE handler_kind = ? AND dead = 0
               AND attempts < ?
               AND not_before <= ?
               AND (lease_expires_at IS NULL OR lease_expires_at < ?)
             ORDER BY created_at ASC
             LIMIT 1",
        )
        .bind(handler_kind)
        .bind(MAX_WORK_ATTEMPTS)
        .bind(&now)
        .bind(&now)
        .fetch_optional(&self.pool)
        .await?;
        let Some((id, payload_json)) = candidate else {
            return Ok(None);
        };
        let changed = sqlx::query(
            "UPDATE work_items
             SET lease_nonce = ?, lease_expires_at = ?, attempts = attempts + 1
             WHERE id = ?
               AND dead = 0
               AND (lease_expires_at IS NULL OR lease_expires_at < ?)",
        )
        .bind(&nonce)
        .bind(&lease_expires)
        .bind(&id)
        .bind(&now)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed == 0 {
            return Ok(None);
        }
        Ok(Some(ClaimedWork {
            id,
            nonce,
            payload: serde_json::from_str(&payload_json)?,
        }))
    }

    /// Mark a claimed item done (handler succeeded); removes it from the queue.
    /// Only the current lease holder (matching nonce) may complete.
    pub async fn complete_work(&self, work_id: &str, nonce: &str) -> Result<bool, OperationError> {
        let now = now_utc_str();
        let changed = sqlx::query(
            "DELETE FROM work_items \
             WHERE id = ? AND lease_nonce = ? AND lease_expires_at >= ?",
        )
        .bind(work_id)
        .bind(nonce)
        .bind(&now)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(changed > 0)
    }

    /// Extend a live work lease without changing its attempt. Long-running
    /// external commands must renew before the original TTL expires so a
    /// second worker cannot start the same non-idempotent effect concurrently.
    pub async fn renew_work(
        &self,
        work_id: &str,
        nonce: &str,
        lease_ttl_seconds: i64,
    ) -> Result<bool, OperationError> {
        let now_dt = crate::clock::now_utc();
        let now = now_utc_str();
        let lease_expires =
            format_utc(now_dt + chrono::Duration::seconds(lease_ttl_seconds.max(1)));
        let changed = sqlx::query(
            "UPDATE work_items SET lease_expires_at = ? \
             WHERE id = ? AND lease_nonce = ? AND dead = 0 \
               AND lease_expires_at >= ?",
        )
        .bind(&lease_expires)
        .bind(work_id)
        .bind(nonce)
        .bind(&now)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(changed > 0)
    }

    /// Mark a claimed item failed. Attempts are counted when a lease is claimed,
    /// so a failed callback must not increment them a second time. Retryable
    /// failures become eligible after bounded exponential backoff with jitter;
    /// an explicit dead-letter result or the attempt limit moves the item to
    /// dead-letter state.
    pub async fn fail_work(
        &self,
        work_id: &str,
        nonce: &str,
        disposition: WorkFailureDisposition,
    ) -> Result<bool, OperationError> {
        let Some((attempts,)) = sqlx::query_as::<_, (i64,)>(
            "SELECT attempts FROM work_items WHERE id = ? AND lease_nonce = ?",
        )
        .bind(work_id)
        .bind(nonce)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(false);
        };
        let now = now_utc_str();
        let exhausted = matches!(disposition, WorkFailureDisposition::DeadLetter)
            || attempts >= MAX_WORK_ATTEMPTS;
        let base_delay_seconds = if exhausted {
            0
        } else {
            (1_i64 << attempts.saturating_sub(1).min(6)).min(MAX_WORK_RETRY_DELAY_SECONDS)
        };
        let jitter_seed = random_hex_token();
        let jitter_source = jitter_seed
            .get(..8)
            .and_then(|value| u64::from_str_radix(value, 16).ok())
            .unwrap_or(0);
        let jitter_seconds = if base_delay_seconds == 0 {
            0
        } else {
            (jitter_source % (base_delay_seconds as u64 / 4 + 1)) as i64
        };
        let delay_seconds = (base_delay_seconds + jitter_seconds).min(MAX_WORK_RETRY_DELAY_SECONDS);
        let not_before =
            format_utc(crate::clock::now_utc() + chrono::Duration::seconds(delay_seconds));
        let changed = sqlx::query(
            "UPDATE work_items SET not_before = ?, dead = ?, lease_nonce = NULL, \
             lease_expires_at = NULL WHERE id = ? AND lease_nonce = ? \
             AND lease_expires_at >= ?",
        )
        .bind(&not_before)
        .bind(exhausted)
        .bind(work_id)
        .bind(nonce)
        .bind(&now)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(changed > 0)
    }

    pub async fn work_will_dead_letter(
        &self,
        work_id: &str,
        nonce: &str,
        disposition: WorkFailureDisposition,
    ) -> Result<bool, OperationError> {
        let Some((attempts,)) = sqlx::query_as::<_, (i64,)>(
            "SELECT attempts FROM work_items \
             WHERE id = ? AND lease_nonce = ? AND dead = 0 \
               AND lease_expires_at >= ?",
        )
        .bind(work_id)
        .bind(nonce)
        .bind(now_utc_str())
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(false);
        };
        Ok(matches!(disposition, WorkFailureDisposition::DeadLetter)
            || attempts >= MAX_WORK_ATTEMPTS)
    }

    /// Transition an operation to `running` and record that a step started.
    ///
    /// Idempotent per step key: a step already `succeeded` is returned as-is so
    /// handlers can safely re-enter after a crash without redoing side effects.
    pub async fn begin_step_claimed(
        &self,
        claim: WorkClaim<'_>,
        operation_id: &str,
        step_key: &str,
        input_summary: Value,
    ) -> Result<StepState, OperationError> {
        let now = now_utc_str();
        let mut tx = self.unit_of_work.begin().await?;
        let owned: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM work_items \
             WHERE id = ? AND lease_nonce = ? AND dead = 0 \
               AND lease_expires_at >= ?",
        )
        .bind(claim.id)
        .bind(claim.nonce)
        .bind(&now)
        .fetch_optional(tx.connection())
        .await?;
        if owned.is_none() {
            tx.rollback().await?;
            return Err(OperationError::StaleWorkClaim);
        }
        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT status FROM operation_steps WHERE operation_id = ? AND step_key = ?",
        )
        .bind(operation_id)
        .bind(step_key)
        .fetch_optional(tx.connection())
        .await?;
        let state = match existing {
            Some((status,)) if status == "succeeded" => StepState::AlreadySucceeded,
            // A process can die after the external effect but before this
            // row is completed. Re-running a non-idempotent effect here would
            // be the most dangerous possible recovery policy, so leave the
            // step for an explicit reconciliation decision.
            Some((status,)) if status == "running" => StepState::NeedsReconciliation,
            Some(_) => StepState::Running,
            None => {
                sqlx::query("INSERT INTO operation_steps (operation_id, step_key, attempts, status, input_summary, external_ref, compensation_json, created_at, updated_at) VALUES (?, ?, 1, 'running', ?, NULL, NULL, ?, ?)")
                    .bind(operation_id)
                    .bind(step_key)
                    .bind(serde_json::to_string(&input_summary)?)
                    .bind(&now)
                    .bind(&now)
                    .execute(tx.connection())
                    .await?;
                StepState::Running
            }
        };
        let version = format!("v_{}", OperationId::new());
        sqlx::query("UPDATE operations SET status = 'running', current_step = ?, version = ?, updated_at = ? WHERE id = ? AND status = 'queued'")
            .bind(step_key)
            .bind(&version)
            .bind(&now)
            .bind(operation_id)
            .execute(tx.connection())
            .await?;
        tx.commit().await?;
        Ok(state)
    }

    /// Mark a step succeeded only while its work claim is still current.
    /// `external_ref` records an external result so a retried handler can
    /// distinguish a completed side effect from an expired lease.
    pub async fn complete_step_claimed(
        &self,
        claim: WorkClaim<'_>,
        operation_id: &str,
        step_key: &str,
        external_ref: Option<&str>,
    ) -> Result<(), OperationError> {
        let now = now_utc_str();
        let changed = sqlx::query(
            "UPDATE operation_steps SET status = 'succeeded', \
                    external_ref = COALESCE(?, external_ref), updated_at = ? \
             WHERE operation_id = ? AND step_key = ? \
               AND EXISTS(SELECT 1 FROM work_items \
                          WHERE id = ? AND lease_nonce = ? AND dead = 0 \
                            AND lease_expires_at >= ?)",
        )
        .bind(external_ref)
        .bind(&now)
        .bind(operation_id)
        .bind(step_key)
        .bind(claim.id)
        .bind(claim.nonce)
        .bind(&now)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed == 0 {
            return Err(OperationError::StaleWorkClaim);
        }
        Ok(())
    }

    /// Transition an operation to a terminal status with an optional result or
    /// problem, emitting `operation.changed` in the same transaction.
    pub async fn finish(
        &self,
        operation_id: &str,
        status: OperationStatus,
        result: Option<Value>,
        problem: Option<Value>,
        correlation_id: CorrelationId,
    ) -> Result<(), OperationError> {
        let now = now_utc_str();
        let version = format!("v_{}", OperationId::new());
        let mut work = self.unit_of_work.begin().await?;
        let kind: Option<String> = sqlx::query_scalar("SELECT kind FROM operations WHERE id = ?")
            .bind(operation_id)
            .fetch_optional(work.connection())
            .await?;
        let Some(kind) = kind else {
            work.rollback().await?;
            return Err(OperationError::NotFound);
        };
        let changed = sqlx::query("UPDATE operations SET status = ?, result_json = ?, problem_json = ?, current_step = NULL, version = ?, updated_at = ? WHERE id = ?")
            .bind(status.as_str())
            .bind(result.as_ref().map(serde_json::to_string).transpose()?)
            .bind(problem.as_ref().map(serde_json::to_string).transpose()?)
            .bind(&version)
            .bind(&now)
            .bind(operation_id)
            .execute(work.connection())
            .await?
            .rows_affected();
        if changed == 0 {
            return Err(OperationError::NotFound);
        }
        if status == OperationStatus::NeedsAttention {
            sqlx::query(
                "UPDATE operation_steps SET status = 'failed', compensation_json = ?, updated_at = ? \
                 WHERE operation_id = ? AND status = 'running'",
            )
            .bind(reconciliation_problem())
            .bind(&now)
            .bind(operation_id)
            .execute(work.connection())
            .await?;
        }
        // Keep idempotency record in step with the terminal outcome.
        sqlx::query("UPDATE idempotency_records SET status = ? WHERE operation_id = ?")
            .bind(status.as_str())
            .bind(operation_id)
            .execute(work.connection())
            .await?;
        self.emit_operation_changed(
            &mut work,
            operation_id,
            &kind,
            status.as_str(),
            &version,
            &correlation_id,
        )
        .await?;
        work.commit().await?;
        Ok(())
    }

    /// Finish an Operation only while the supplied durable work lease is still
    /// current. A stale worker gets `Ok(false)` and cannot overwrite a newer
    /// attempt or a startup recovery decision.
    pub async fn finish_claimed(
        &self,
        operation_id: &str,
        work_id: &str,
        work_nonce: &str,
        completion: OperationCompletion,
    ) -> Result<bool, OperationError> {
        let now = now_utc_str();
        let version = format!("v_{}", OperationId::new());
        let mut work = self.unit_of_work.begin().await?;
        let kind: Option<(String,)> = sqlx::query_as(
            "SELECT kind FROM operations \
             WHERE id = ? AND status IN ('queued', 'running') \
               AND EXISTS(SELECT 1 FROM work_items \
                          WHERE id = ? AND lease_nonce = ? AND dead = 0 \
                            AND lease_expires_at >= ?)",
        )
        .bind(operation_id)
        .bind(work_id)
        .bind(work_nonce)
        .bind(&now)
        .fetch_optional(work.connection())
        .await?;
        let Some((kind,)) = kind else {
            work.rollback().await?;
            return Ok(false);
        };
        let changed = sqlx::query(
            "UPDATE operations SET status = ?, result_json = ?, problem_json = ?, \
             current_step = NULL, version = ?, updated_at = ? \
             WHERE id = ? AND status IN ('queued', 'running') \
               AND EXISTS(SELECT 1 FROM work_items \
                          WHERE id = ? AND lease_nonce = ? AND dead = 0 \
                            AND lease_expires_at >= ?)",
        )
        .bind(completion.status.as_str())
        .bind(
            completion
                .result
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(
            completion
                .problem
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(&version)
        .bind(&now)
        .bind(operation_id)
        .bind(work_id)
        .bind(work_nonce)
        .bind(&now)
        .execute(work.connection())
        .await?
        .rows_affected();
        if changed == 0 {
            work.rollback().await?;
            return Ok(false);
        }
        if completion.status == OperationStatus::NeedsAttention {
            sqlx::query(
                "UPDATE operation_steps SET status = 'failed', compensation_json = ?, updated_at = ? \
                 WHERE operation_id = ? AND status = 'running'",
            )
            .bind(reconciliation_problem())
            .bind(&now)
            .bind(operation_id)
            .execute(work.connection())
            .await?;
        }
        sqlx::query("UPDATE idempotency_records SET status = ? WHERE operation_id = ?")
            .bind(completion.status.as_str())
            .bind(operation_id)
            .execute(work.connection())
            .await?;
        self.emit_operation_changed(
            &mut work,
            operation_id,
            &kind,
            completion.status.as_str(),
            &version,
            &completion.correlation_id,
        )
        .await?;
        work.commit().await?;
        Ok(true)
    }

    /// Verify that a worker still owns its durable lease immediately before an
    /// external effect. The completion update remains the final fence after the
    /// effect, so a lease expiring during the effect becomes reconciliation work
    /// instead of an invisible overwrite.
    pub async fn assert_claimed(&self, claim: WorkClaim<'_>) -> Result<(), OperationError> {
        let owned: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM work_items \
             WHERE id = ? AND lease_nonce = ? AND dead = 0 \
               AND lease_expires_at >= ?",
        )
        .bind(claim.id)
        .bind(claim.nonce)
        .bind(now_utc_str())
        .fetch_optional(&self.pool)
        .await?;
        if owned.is_none() {
            return Err(OperationError::StaleWorkClaim);
        }
        Ok(())
    }

    pub async fn get(&self, operation_id: &str) -> Result<Option<OperationView>, OperationError> {
        let row = sqlx::query_as::<_, OperationRow>(
            "SELECT id, kind, status, target_kind, target_id, current_step, progress_json, result_json, problem_json, correlation_id, version, created_at, updated_at FROM operations WHERE id = ?",
        )
        .bind(operation_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(view_from_row).transpose()
    }

    /// List operations owned by an authenticated caller. The owner is carried
    /// by the idempotency record, which is already required for public
    /// operation-producing commands and keeps this read path scoped without
    /// exposing the operation journal globally.
    pub async fn list_by_kind_owner(
        &self,
        kind: &str,
        owner_id: &str,
        limit: i64,
    ) -> Result<Vec<OperationView>, OperationError> {
        let limit = limit.clamp(1, 200);
        let rows = sqlx::query_as::<_, OperationRow>(
            "SELECT o.id, o.kind, o.status, o.target_kind, o.target_id, o.current_step, \
                    o.progress_json, o.result_json, o.problem_json, o.correlation_id, \
                    o.version, o.created_at, o.updated_at \
             FROM operations o \
             INNER JOIN idempotency_records i ON i.operation_id = o.id \
             WHERE o.kind = ? AND i.owner_id = ? \
             ORDER BY o.created_at DESC, o.id DESC LIMIT ?",
        )
        .bind(kind)
        .bind(owner_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(view_from_row).collect()
    }

    /// Publish durable workflow progress while fencing the update to the
    /// current worker lease. This lets clients discover child resources before
    /// the parent operation reaches a terminal state.
    pub async fn update_progress_claimed(
        &self,
        claim: WorkClaim<'_>,
        operation_id: &str,
        current_step: &str,
        progress: Value,
        correlation_id: CorrelationId,
    ) -> Result<bool, OperationError> {
        let now = now_utc_str();
        let version = format!("v_{}", OperationId::new());
        let mut work = self.unit_of_work.begin().await?;
        let kind: Option<(String,)> = sqlx::query_as(
            "SELECT kind FROM operations \
             WHERE id = ? AND status IN ('queued', 'running') \
               AND EXISTS(SELECT 1 FROM work_items \
                          WHERE id = ? AND lease_nonce = ? AND dead = 0 \
                            AND lease_expires_at >= ?)",
        )
        .bind(operation_id)
        .bind(claim.id)
        .bind(claim.nonce)
        .bind(&now)
        .fetch_optional(work.connection())
        .await?;
        let Some((kind,)) = kind else {
            work.rollback().await?;
            return Ok(false);
        };
        let changed = sqlx::query(
            "UPDATE operations SET status = 'running', current_step = ?, \
                    progress_json = ?, version = ?, updated_at = ? \
             WHERE id = ? AND status IN ('queued', 'running') \
               AND EXISTS(SELECT 1 FROM work_items \
                          WHERE id = ? AND lease_nonce = ? AND dead = 0 \
                            AND lease_expires_at >= ?)",
        )
        .bind(current_step)
        .bind(serde_json::to_string(&progress)?)
        .bind(&version)
        .bind(&now)
        .bind(operation_id)
        .bind(claim.id)
        .bind(claim.nonce)
        .bind(&now)
        .execute(work.connection())
        .await?
        .rows_affected();
        if changed == 0 {
            work.rollback().await?;
            return Ok(false);
        }
        self.emit_operation_changed(
            &mut work,
            operation_id,
            &kind,
            "running",
            &version,
            &correlation_id,
        )
        .await?;
        work.commit().await?;
        Ok(true)
    }

    async fn get_in_tx(
        &self,
        tx: &mut SqliteConnection,
        operation_id: &str,
    ) -> Result<Option<OperationView>, OperationError> {
        let row = sqlx::query_as::<_, OperationRow>(
            "SELECT id, kind, status, target_kind, target_id, current_step, progress_json, result_json, problem_json, correlation_id, version, created_at, updated_at FROM operations WHERE id = ?",
        )
        .bind(operation_id)
        .fetch_optional(tx)
        .await?;
        row.map(view_from_row).transpose()
    }

    pub async fn in_flight_for_target(
        &self,
        kind: &str,
        target_kind: &str,
        target_id: &str,
    ) -> Result<Option<String>, OperationError> {
        sqlx::query_scalar(
            "SELECT id FROM operations \
             WHERE kind = ? AND target_kind = ? AND target_id = ? \
               AND status IN ('queued', 'running') \
             ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(kind)
        .bind(target_kind)
        .bind(target_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(OperationError::from)
    }

    /// Operations stuck in `running` with an expired lease - startup recovery must
    /// resume or terminalize them.
    pub async fn stale_running(&self) -> Result<Vec<String>, OperationError> {
        let now = now_utc_str();
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM operations WHERE status = 'running' AND (lease_expires_at IS NULL OR lease_expires_at < ?)",
        )
        .bind(&now)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn lookup_idempotency_in_tx(
        &self,
        tx: &mut SqliteConnection,
        idem: &IdempotencyRequest,
    ) -> Result<Option<OperationView>, OperationError> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT request_digest, operation_id FROM idempotency_records WHERE key = ?",
        )
        .bind(&idem.key)
        .fetch_optional(&mut *tx)
        .await?;
        match row {
            None => Ok(None),
            Some((stored_digest, operation_id)) => {
                if stored_digest != idem.digest {
                    return Err(OperationError::Internal(anyhow::anyhow!(
                        "IDEMPOTENCY_KEY_REUSED"
                    )));
                }
                self.get_in_tx(tx, &operation_id).await
            }
        }
    }

    async fn emit_operation_changed(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        operation_id: &str,
        kind: &str,
        status: &str,
        version: &str,
        correlation_id: &CorrelationId,
    ) -> Result<(), OperationError> {
        work.append_event(NewEvent {
            event_type: EventType::OperationChanged,
            actor: serde_json::json!({"kind": "system", "id": null, "display_name": "Janus"}),
            resource: Some(serde_json::json!({
                "kind": "operation",
                "id": operation_id,
                "version": version,
            })),
            correlation_id: correlation_id.to_string(),
            causation_id: None,
            payload: serde_json::json!({
                "operation_id": operation_id,
                "kind": kind,
                "status": status,
            }),
        })
        .await?;
        Ok(())
    }
}

/// Result of `begin_step`: tells a handler whether to run the step or skip it.
#[derive(Debug, Clone, Copy)]
pub enum StepState {
    /// Step already succeeded in a prior attempt; skip and read `external_ref`.
    AlreadySucceeded,
    /// Step is now running; execute it.
    Running,
    /// The previous worker may have completed an external effect before its
    /// lease expired. Do not execute the effect again without reconciliation.
    NeedsReconciliation,
}

pub struct CreatedOperation {
    pub operation: OperationView,
    pub outcome: IdempotencyOutcome,
}

#[derive(Debug, Clone)]
pub struct CreateOperation<'a> {
    pub kind: &'a str,
    pub actor: Value,
    pub target_kind: &'a str,
    pub target_id: Option<&'a str>,
    pub conditions: Value,
    pub correlation_id: CorrelationId,
    pub idempotency: Option<IdempotencyRequest>,
}

#[derive(Debug, Clone)]
pub struct CreateWork<'a> {
    pub handler_kind: &'a str,
    pub payload: Value,
}

#[derive(Debug, Clone)]
pub struct IdempotencyRequest {
    pub key: String,
    pub owner_id: String,
    pub method: String,
    pub normalized_route: String,
    pub digest: String,
    pub expires_at: String,
}

#[derive(sqlx::FromRow)]
struct OperationRow {
    id: String,
    kind: String,
    status: String,
    target_kind: String,
    target_id: Option<String>,
    current_step: Option<String>,
    progress_json: Option<String>,
    result_json: Option<String>,
    problem_json: Option<String>,
    correlation_id: String,
    version: String,
    created_at: String,
    updated_at: String,
}

fn view_from_row(row: OperationRow) -> Result<OperationView, OperationError> {
    let OperationRow {
        id,
        kind,
        status,
        target_kind,
        target_id,
        current_step,
        progress_json,
        result_json,
        problem_json,
        correlation_id,
        version,
        created_at,
        updated_at,
    } = row;
    Ok(OperationView {
        id,
        kind,
        status,
        target_kind,
        target_id,
        current_step,
        progress: parse_json_opt(progress_json)?,
        result: parse_json_opt(result_json)?,
        problem: parse_json_opt(problem_json)?,
        correlation_id,
        version,
        created_at,
        updated_at,
    })
}

fn parse_json_opt(stored: Option<String>) -> Result<Option<Value>, serde_json::Error> {
    match stored {
        Some(s) => serde_json::from_str(&s).map(Some),
        None => Ok(None),
    }
}

fn reconciliation_problem() -> String {
    serde_json::json!({
        "code": "OPERATION_STEP_REQUIRES_RECONCILIATION",
        "detail": "the external effect may have completed before the worker lease expired",
    })
    .to_string()
}

/// A work item leased to a handler. The nonce is the holder's ownership proof.
#[derive(Debug, Clone)]
pub struct ClaimedWork {
    pub id: String,
    pub nonce: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy)]
pub struct WorkClaim<'a> {
    pub id: &'a str,
    pub nonce: &'a str,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        clock::{format_utc, now_utc, now_utc_str},
        id::OwnerId,
    };
    use chrono::Duration;
    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// In-memory SQLite with the real server migration, an owner row (for the
    /// idempotency FK), and a bare OperationInterface.
    async fn test_harness() -> (SqlitePool, OperationInterface, String) {
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
        let owner_id = OwnerId::new();
        sqlx::query("INSERT INTO owners (id, display_name, created_at) VALUES (?, 'test', ?)")
            .bind(owner_id.to_string())
            .bind(now_utc_str())
            .execute(&pool)
            .await
            .unwrap();
        let ops = OperationInterface::new(pool.clone(), EventStore::new(pool.clone()));
        (pool, ops, owner_id.to_string())
    }

    fn create_request<'a>(idem: Option<IdempotencyRequest>) -> CreateOperation<'a> {
        CreateOperation {
            kind: "project.create",
            actor: json!({"kind": "system", "id": null, "display_name": "Janus"}),
            target_kind: "project",
            target_id: Some("p_1"),
            conditions: json!({}),
            correlation_id: CorrelationId::new(),
            idempotency: idem,
        }
    }

    fn create_work<'a>() -> CreateWork<'a> {
        CreateWork {
            handler_kind: "git.clone",
            payload: json!({"url": "https://example.com/repo.git"}),
        }
    }

    #[tokio::test]
    async fn lease_renewal_and_expiry() {
        let (pool, ops, _owner_id) = test_harness().await;
        ops.create(create_request(None), Some(create_work()))
            .await
            .unwrap();
        let claimed = ops
            .claim_work("git.clone", 60)
            .await
            .unwrap()
            .expect("claimable");
        // Renew with the matching nonce while the lease is live.
        assert!(
            ops.renew_work(&claimed.id, &claimed.nonce, 60)
                .await
                .unwrap()
        );
        // A stale nonce cannot renew.
        assert!(
            !ops.renew_work(&claimed.id, "wrong-nonce", 60)
                .await
                .unwrap()
        );
        // Force the lease into the past.
        sqlx::query("UPDATE work_items SET lease_expires_at = ? WHERE id = ?")
            .bind(format_utc(now_utc() - Duration::seconds(10)))
            .bind(&claimed.id)
            .execute(&pool)
            .await
            .unwrap();
        // Renewing an expired lease fails.
        assert!(
            !ops.renew_work(&claimed.id, &claimed.nonce, 60)
                .await
                .unwrap()
        );
        // The expired item is reclaimable with a new nonce.
        let reclaimed = ops
            .claim_work("git.clone", 60)
            .await
            .unwrap()
            .expect("reclaimable");
        assert_ne!(reclaimed.nonce, claimed.nonce);
        // The old nonce can no longer complete the work.
        assert!(
            !ops.complete_work(&reclaimed.id, &claimed.nonce)
                .await
                .unwrap()
        );
        // The new nonce can.
        assert!(
            ops.complete_work(&reclaimed.id, &reclaimed.nonce)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn idempotency_key_dedup() {
        let (_pool, ops, owner_id) = test_harness().await;
        let key = "clone-project-p_1".to_owned();
        let digest = "sha256:same".to_owned();
        let make_idem = |digest: &str| IdempotencyRequest {
            key: key.clone(),
            owner_id: owner_id.clone(),
            method: "POST".to_owned(),
            normalized_route: "/api/projects".to_owned(),
            digest: digest.to_owned(),
            expires_at: format_utc(now_utc() + Duration::hours(1)),
        };
        let first = ops
            .create(create_request(Some(make_idem(&digest))), None)
            .await
            .unwrap();
        assert!(matches!(first.outcome, IdempotencyOutcome::New));
        let op_id = first.operation.id.clone();
        // Same key + same digest while in-flight -> Reused with the same operation.
        let second = ops
            .create(create_request(Some(make_idem(&digest))), None)
            .await
            .unwrap();
        assert!(matches!(second.outcome, IdempotencyOutcome::Reused));
        assert_eq!(second.operation.id, op_id);
        // Terminal finish, then same key + same digest -> Stored.
        ops.finish(
            &op_id,
            OperationStatus::Succeeded,
            Some(json!({"ok": true})),
            None,
            CorrelationId::new(),
        )
        .await
        .unwrap();
        let third = ops
            .create(create_request(Some(make_idem(&digest))), None)
            .await
            .unwrap();
        assert!(matches!(third.outcome, IdempotencyOutcome::Stored));
        assert_eq!(third.operation.id, op_id);
        // Same key + different digest is a caller error.
        let err = ops
            .create(create_request(Some(make_idem("sha256:different"))), None)
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("IDEMPOTENCY_KEY_REUSED"));
    }

    #[tokio::test]
    async fn work_dead_letters_after_attempt_cap() {
        let (pool, ops, _owner_id) = test_harness().await;
        ops.create(create_request(None), Some(create_work()))
            .await
            .unwrap();
        for attempt in 1..=MAX_WORK_ATTEMPTS {
            let claimed = ops
                .claim_work("git.clone", 60)
                .await
                .unwrap()
                .expect("claimable");
            let will_dl = ops
                .work_will_dead_letter(&claimed.id, &claimed.nonce, WorkFailureDisposition::Retry)
                .await
                .unwrap();
            assert_eq!(will_dl, attempt >= MAX_WORK_ATTEMPTS);
            assert!(
                ops.fail_work(&claimed.id, &claimed.nonce, WorkFailureDisposition::Retry)
                    .await
                    .unwrap()
            );
            // Make the item immediately eligible for the next claim.
            sqlx::query("UPDATE work_items SET not_before = ? WHERE id = ?")
                .bind(format_utc(now_utc() - Duration::seconds(30)))
                .bind(&claimed.id)
                .execute(&pool)
                .await
                .unwrap();
        }
        // After the cap the item is dead and cannot be claimed again.
        assert!(ops.claim_work("git.clone", 60).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn stale_running_reports_expired_leases() {
        let (pool, ops, _owner_id) = test_harness().await;
        let created = ops
            .create(create_request(None), Some(create_work()))
            .await
            .unwrap();
        let op_id = created.operation.id.clone();
        // Running with a live lease is not stale.
        sqlx::query(
            "UPDATE operations SET status = 'running', lease_nonce = 'x', lease_expires_at = ? \
             WHERE id = ?",
        )
        .bind(format_utc(now_utc() + Duration::seconds(60)))
        .bind(&op_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(ops.stale_running().await.unwrap().is_empty());
        // Expiring the lease makes it stale.
        sqlx::query("UPDATE operations SET lease_expires_at = ? WHERE id = ?")
            .bind(format_utc(now_utc() - Duration::seconds(10)))
            .bind(&op_id)
            .execute(&pool)
            .await
            .unwrap();
        let stale = ops.stale_running().await.unwrap();
        assert!(stale.contains(&op_id));
    }

    #[tokio::test]
    async fn step_replay_is_idempotent() {
        let (_pool, ops, _owner_id) = test_harness().await;
        let created = ops
            .create(create_request(None), Some(create_work()))
            .await
            .unwrap();
        let op_id = created.operation.id.clone();
        let claimed = ops
            .claim_work("git.clone", 60)
            .await
            .unwrap()
            .expect("claimable");
        let claim = WorkClaim {
            id: &claimed.id,
            nonce: &claimed.nonce,
        };
        // Fresh step starts running.
        assert!(matches!(
            ops.begin_step_claimed(claim, &op_id, "clone", json!({"repo": "x"}))
                .await
                .unwrap(),
            StepState::Running
        ));
        // Complete it; replaying the same step returns AlreadySucceeded.
        ops.complete_step_claimed(claim, &op_id, "clone", Some("refs/heads/main"))
            .await
            .unwrap();
        assert!(matches!(
            ops.begin_step_claimed(claim, &op_id, "clone", json!({"repo": "x"}))
                .await
                .unwrap(),
            StepState::AlreadySucceeded
        ));
        // A step left running mid-effect must not be silently re-executed.
        assert!(matches!(
            ops.begin_step_claimed(claim, &op_id, "update", json!({}))
                .await
                .unwrap(),
            StepState::Running
        ));
        assert!(matches!(
            ops.begin_step_claimed(claim, &op_id, "update", json!({}))
                .await
                .unwrap(),
            StepState::NeedsReconciliation
        ));
        // A stale claim is rejected outright.
        let stale = WorkClaim {
            id: &claimed.id,
            nonce: "stale-nonce",
        };
        assert!(matches!(
            ops.begin_step_claimed(stale, &op_id, "delete", json!({}))
                .await
                .unwrap_err(),
            OperationError::StaleWorkClaim
        ));
    }
}
