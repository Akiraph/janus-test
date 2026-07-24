//! Durable Operation journal and work queue.
//!
//! Long-running commands (clone, git fetch/update/push, project delete) must
//! survive HTTP disconnects and process restarts. Per `DAT-OP-01/02`, an
//! Operation is created before any non-rollbackable external side effect; each
//! step uses a stable key so handlers re-enter after a crash and skip already
//! succeeded steps. The queue is SQLite `work_items` (no Redis/scheduler), and
//! workers lease items with a random nonce so a canceled task or restarted
//! process cannot silently commit a stale attempt.

use serde::Serialize;
use serde_json::Value;
use sqlx::SqlitePool;
use utoipa::ToSchema;

use super::clock::{Clock, SystemClock, format_utc};
use crate::platform::id::{CorrelationId, OperationId, WorkItemId};
use rand::RngCore;

/// Stable operation kinds. Each maps to a handler in the work queue.
pub const KIND_CLONE: &str = "project.clone";
pub const KIND_DELETE_PROJECT: &str = "project.delete";
pub const KIND_GIT_FETCH: &str = "git.fetch";
pub const KIND_GIT_UPDATE: &str = "git.update";
pub const KIND_GIT_PUSH: &str = "git.push";
pub const KIND_GIT_CHECKOUT: &str = "git.checkout";

/// Operation status. `needs_attention` means execution stopped at a decidable
/// state requiring an explicit follow-up command (e.g. a Git Update Conflict),
/// not "still running" or "unknown failure".
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
}

impl OperationInterface {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Create an operation, honoring idempotency. Same key + same digest returns
    /// the stored result; same key + different digest is a caller bug (409).
    /// `digest` should cover the normalized request body so retries are safe.
    pub async fn create(
        &self,
        request: CreateOperation<'_>,
    ) -> Result<CreatedOperation, OperationError> {
        if let Some(idem) = &request.idempotency
            && let Some(view) = self.lookup_idempotency(idem).await?
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
        let now = format_utc(SystemClock.now());
        let version = format!("v_{}", crate::platform::id::OperationId::new());
        let operation_id = id.to_string();
        let mut tx = self.pool.begin().await?;
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
            .execute(&mut *tx)
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
                .execute(&mut *tx)
                .await?;
        }
        self.emit_operation_changed(&mut tx, &operation_id, kind, "queued", &version, &request.correlation_id)
            .await?;
        tx.commit().await?;
        Ok(CreatedOperation {
            operation: self.get(&operation_id).await?.ok_or(OperationError::NotFound)?,
            outcome: IdempotencyOutcome::New,
        })
    }

    /// Enqueue a work item for a background handler to lease.
    pub async fn enqueue_work(
        &self,
        handler_kind: &str,
        payload: Value,
    ) -> Result<WorkItemId, OperationError> {
        let id = WorkItemId::new();
        let now = format_utc(SystemClock.now());
        sqlx::query("INSERT INTO work_items (id, handler_kind, payload_json, not_before, lease_nonce, lease_expires_at, attempts, dead, created_at) VALUES (?, ?, ?, ?, NULL, NULL, 0, 0, ?)")
            .bind(id.to_string())
            .bind(handler_kind)
            .bind(serde_json::to_string(&payload)?)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await?;
        Ok(id)
    }

    /// Claim the oldest available work item for a handler. Returns the item id,
    /// payload and operation correlation if claimed. A random nonce leases it so
    /// only the holder may complete; a canceled task or restarted process leaves
    /// the lease to expire and be reclaimed (`DAT-OP-02`).
    pub async fn claim_work(
        &self,
        handler_kind: &str,
        lease_ttl_seconds: i64,
    ) -> Result<Option<ClaimedWork>, OperationError> {
        let now_dt = SystemClock.now();
        let now = format_utc(now_dt);
        let lease_expires = format_utc(
            now_dt
                .checked_add_signed(chrono::Duration::seconds(lease_ttl_seconds))
                .unwrap_or(now_dt),
        );
        let nonce = random_nonce();

        // SQLite cannot reliably UPDATE a row selected from the same table in a
        // nested subquery. Select first, then CAS the lease with the nonce.
        let candidate: Option<(String, String)> = sqlx::query_as(
            "SELECT id, payload_json FROM work_items
             WHERE handler_kind = ? AND dead = 0
               AND not_before <= ?
               AND (lease_expires_at IS NULL OR lease_expires_at < ?)
             ORDER BY created_at ASC
             LIMIT 1",
        )
        .bind(handler_kind)
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
    pub async fn complete_work(
        &self,
        work_id: &str,
        nonce: &str,
    ) -> Result<bool, OperationError> {
        let changed = sqlx::query(
            "DELETE FROM work_items WHERE id = ? AND lease_nonce = ?",
        )
        .bind(work_id)
        .bind(nonce)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(changed > 0)
    }

    /// Mark a claimed item failed; increment attempts. The item stays claimable
    /// (a recovery pass can re-lease it) unless the handler marks it dead.
    pub async fn fail_work(
        &self,
        work_id: &str,
        nonce: &str,
        dead: bool,
    ) -> Result<bool, OperationError> {
        let changed = sqlx::query(
            "UPDATE work_items SET attempts = attempts + 1, dead = ?, lease_nonce = NULL, lease_expires_at = NULL WHERE id = ? AND lease_nonce = ?",
        )
        .bind(dead)
        .bind(work_id)
        .bind(nonce)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(changed > 0)
    }

    /// Transition an operation to `running` and record that a step started.
    /// Idempotent per step key: a step already `succeeded` is returned as-is so
    /// handlers can safely re-enter after a crash (`DAT-OP-01`).
    pub async fn begin_step(
        &self,
        operation_id: &str,
        step_key: &str,
        input_summary: Value,
    ) -> Result<StepState, OperationError> {
        let now = format_utc(SystemClock.now());
        let mut tx = self.pool.begin().await?;
        let existing: Option<(String,)> =
            sqlx::query_as("SELECT status FROM operation_steps WHERE operation_id = ? AND step_key = ?")
                .bind(operation_id)
                .bind(step_key)
                .fetch_optional(&mut *tx)
                .await?;
        let state = match existing {
            Some((status,)) if status == "succeeded" => StepState::AlreadySucceeded,
            Some(_) => StepState::Running,
            None => {
                sqlx::query("INSERT INTO operation_steps (operation_id, step_key, attempts, status, input_summary, external_ref, compensation_json, created_at, updated_at) VALUES (?, ?, 1, 'running', ?, NULL, NULL, ?, ?)")
                    .bind(operation_id)
                    .bind(step_key)
                    .bind(serde_json::to_string(&input_summary)?)
                    .bind(&now)
                    .bind(&now)
                    .execute(&mut *tx)
                    .await?;
                StepState::Running
            }
        };
        // Advance operation to running + current_step on first/any step.
        let version = format!("v_{}", crate::platform::id::OperationId::new());
        sqlx::query("UPDATE operations SET status = 'running', current_step = ?, version = ?, updated_at = ? WHERE id = ? AND status = 'queued'")
            .bind(step_key)
            .bind(&version)
            .bind(&now)
            .bind(operation_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(state)
    }

    /// Mark a step succeeded. The handler passes its external_ref (e.g. a git
    /// commit sha) so a crashed retry can query the real side effect instead of
    /// assuming "timed out = did not happen" (`DAT-OP-01`).
    pub async fn complete_step(
        &self,
        operation_id: &str,
        step_key: &str,
        external_ref: Option<&str>,
    ) -> Result<(), OperationError> {
        let now = format_utc(SystemClock.now());
        sqlx::query("UPDATE operation_steps SET status = 'succeeded', external_ref = COALESCE(?, external_ref), updated_at = ? WHERE operation_id = ? AND step_key = ?")
            .bind(external_ref)
            .bind(&now)
            .bind(operation_id)
            .bind(step_key)
            .execute(&self.pool)
            .await?;
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
        let now = format_utc(SystemClock.now());
        let version = format!("v_{}", crate::platform::id::OperationId::new());
        let mut tx = self.pool.begin().await?;
        let changed = sqlx::query("UPDATE operations SET status = ?, result_json = ?, problem_json = ?, current_step = NULL, version = ?, updated_at = ? WHERE id = ?")
            .bind(status.as_str())
            .bind(result.as_ref().map(serde_json::to_string).transpose()?)
            .bind(problem.as_ref().map(serde_json::to_string).transpose()?)
            .bind(&version)
            .bind(&now)
            .bind(operation_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if changed == 0 {
            return Err(OperationError::NotFound);
        }
        // Keep idempotency record in step with the terminal outcome.
        sqlx::query("UPDATE idempotency_records SET status = ? WHERE operation_id = ?")
            .bind(status.as_str())
            .bind(operation_id)
            .execute(&mut *tx)
            .await?;
        self.emit_operation_changed(
            &mut tx,
            operation_id,
            "",
            status.as_str(),
            &version,
            &correlation_id,
        )
        .await?;
        tx.commit().await?;
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

    /// Operations that are `running` with an expired lease: a restart must
    /// resume or mark them. Used by startup recovery (`DAT-RECOVER-01`).
    pub async fn stale_running(&self) -> Result<Vec<String>, OperationError> {
        let now = format_utc(SystemClock.now());
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM operations WHERE status = 'running' AND (lease_expires_at IS NULL OR lease_expires_at < ?)",
        )
        .bind(&now)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn lookup_idempotency(
        &self,
        idem: &IdempotencyRequest,
    ) -> Result<Option<OperationView>, OperationError> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT request_digest, operation_id FROM idempotency_records WHERE key = ?",
        )
        .bind(&idem.key)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            None => Ok(None),
            Some((stored_digest, operation_id)) => {
                if stored_digest != idem.digest {
                    return Err(OperationError::Internal(anyhow::anyhow!(
                        "IDEMPOTENCY_KEY_REUSED"
                    )));
                }
                self.get(&operation_id).await
            }
        }
    }

    async fn emit_operation_changed(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        operation_id: &str,
        kind: &str,
        status: &str,
        version: &str,
        correlation_id: &CorrelationId,
    ) -> Result<(), OperationError> {
        // Append to public_events in the same tx so the event and the status
        // change commit atomically (`EVT-002`). The EventStore.append opens its
        // own connection, so instead we insert directly here.
        let event_id = crate::platform::id::EventId::new().to_string();
        let now = format_utc(SystemClock.now());
        let payload = serde_json::json!({
            "operation_id": operation_id,
            "kind": kind,
            "status": status,
        });
        let actor = serde_json::json!({"kind": "system", "id": null, "display_name": "Janus"});
        let resource = serde_json::json!({"kind": "operation", "id": operation_id, "version": version});
        sqlx::query("INSERT INTO public_events (event_id, event_type, schema_version, actor_json, resource_json, correlation_id, causation_id, payload_json, occurred_at) VALUES (?, 'operation.changed', 1, ?, ?, ?, NULL, ?, ?)")
            .bind(&event_id)
            .bind(serde_json::to_string(&actor)?)
            .bind(serde_json::to_string(&resource)?)
            .bind(correlation_id.to_string())
            .bind(serde_json::to_string(&payload)?)
            .bind(&now)
            .execute(&mut **tx)
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
}

pub struct CreatedOperation {
    pub operation: OperationView,
    pub outcome: IdempotencyOutcome,
}

/// Parameters for creating an operation. Grouped to keep `create` under the
/// argument-count lint while staying explicit at call sites.
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
    Ok(OperationView {
        id: row.id,
        kind: row.kind,
        status: row.status,
        target_kind: row.target_kind,
        target_id: row.target_id,
        current_step: row.current_step,
        progress: row
            .progress_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?,
        result: row.result_json.as_deref().map(serde_json::from_str).transpose()?,
        problem: row
            .problem_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?,
        correlation_id: row.correlation_id,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// A work item leased to a handler. The nonce must be presented to complete/fail.
#[derive(Debug, Clone)]
pub struct ClaimedWork {
    pub id: String,
    pub nonce: String,
    pub payload: Value,
}

fn random_nonce() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
