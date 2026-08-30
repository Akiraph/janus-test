//! Durable operation journal and work queue.
//!
//! Callers record intent before external side effects; stable step keys and leases
//! make retries and process restarts idempotent. Work kinds and handlers live in
//! server/application - not here.

use std::collections::HashSet;

use futures_util::TryStreamExt;
use mongodb::{
    ClientSession,
    bson::{Bson, Document, doc},
    options::ReturnDocument,
};
use serde::Serialize;
use serde_json::Value;
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
    Storage(#[from] mongodb::error::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
    #[error("document value access error: {0}")]
    ValueAccess(#[from] mongodb::bson::document::ValueAccessError),
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
    pool: mongodb::Database,
    unit_of_work: UnitOfWork,
}

impl OperationInterface {
    pub fn new(pool: mongodb::Database, events: EventStore) -> Self {
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
        work: &mut UnitOfWorkTransaction,
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
        let actor_json = serde_json::to_string(&request.actor)?;
        let conditions_json = serde_json::to_string(&request.conditions)?;
        let mut operation = doc! {
            "_id": &operation_id,
            "kind": kind,
            "actor_json": &actor_json,
            "target_kind": request.target_kind,
            "status": "queued",
            "conditions_json": &conditions_json,
            "correlation_id": request.correlation_id.to_string(),
            "version": &version,
            "created_at": &now,
            "updated_at": &now,
        };
        if let Some(target_id) = request.target_id {
            operation.insert("target_id", target_id);
        }
        self.pool
            .collection::<Document>("operations")
            .insert_one(operation)
            .session(&mut *work.connection())
            .await?;
        if let Some(idem) = &request.idempotency {
            // A prior record for this key can linger after its window lapses
            // (the lookup above only honors live ones). Replace it rather than
            // collide on the unique `_id`, so a reused key starts a fresh
            // operation instead of failing with a duplicate-key error.
            self.pool
                .collection::<Document>("idempotency_records")
                .replace_one(
                    doc! {"_id": &idem.key},
                    doc! {
                        "owner_id": &idem.owner_id,
                        "method": &idem.method,
                        "normalized_route": &idem.normalized_route,
                        "request_digest": &idem.digest,
                        "status": "running",
                        "operation_id": &operation_id,
                        "expires_at": &idem.expires_at,
                    },
                )
                .session(&mut *work.connection())
                .upsert(true)
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
            let payload_json = serde_json::to_string(&payload)?;
            self.pool
                .collection::<Document>("work_items")
                .insert_one(doc! {
                    "_id": work_id.to_string(),
                    "handler_kind": work_item.handler_kind,
                    "payload_json": &payload_json,
                    "not_before": &now,
                    "attempts": 0i64,
                    "dead": false,
                    "created_at": &now,
                })
                .session(&mut *work.connection())
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
        work: &mut UnitOfWorkTransaction,
        handler_kind: &str,
        payload: Value,
    ) -> Result<WorkItemId, OperationError> {
        let work_id = WorkItemId::new();
        let now = now_utc_str();
        let payload_json = serde_json::to_string(&payload)?;
        self.pool
            .collection::<Document>("work_items")
            .insert_one(doc! {
                "_id": work_id.to_string(),
                "handler_kind": handler_kind,
                "payload_json": &payload_json,
                "not_before": &now,
                "attempts": 0i64,
                "dead": false,
                "created_at": &now,
            })
            .session(&mut *work.connection())
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
        self.pool
            .collection::<Document>("work_items")
            .update_many(
                doc! {
                    "dead": false,
                    "attempts": {"$gte": MAX_WORK_ATTEMPTS},
                    // BSON sorts nulls before strings, so `$lt` would match a
                    // missing lease; an unleased item cannot be dead-lettered.
                    "lease_expires_at": {"$ne": null, "$lt": &now},
                },
                doc! {"$set": {"dead": true}},
            )
            .await?;

        // The find-and-update is the ownership check, replacing the SQL
        // SELECT-then-UPDATE two-step with a single atomic claim.
        let claimed = self
            .pool
            .collection::<Document>("work_items")
            .find_one_and_update(
                doc! {
                    "handler_kind": handler_kind,
                    "dead": false,
                    "attempts": {"$lt": MAX_WORK_ATTEMPTS},
                    "not_before": {"$lte": &now},
                    "$or": [
                        {"lease_expires_at": null},
                        {"lease_expires_at": {"$lt": &now}},
                    ],
                },
                doc! {
                    "$set": {"lease_nonce": &nonce, "lease_expires_at": &lease_expires},
                    "$inc": {"attempts": 1i64},
                },
            )
            .sort(doc! {"created_at": 1})
            .return_document(ReturnDocument::After)
            .await?;
        let Some(claimed) = claimed else {
            return Ok(None);
        };
        let id = claimed.get_str("_id")?.to_owned();
        let payload_json = claimed.get_str("payload_json")?;
        Ok(Some(ClaimedWork {
            id,
            nonce,
            payload: serde_json::from_str(payload_json)?,
        }))
    }

    /// Mark a claimed item done (handler succeeded); removes it from the queue.
    /// Only the current lease holder (matching nonce) may complete.
    pub async fn complete_work(&self, work_id: &str, nonce: &str) -> Result<bool, OperationError> {
        let now = now_utc_str();
        let deleted = self
            .pool
            .collection::<Document>("work_items")
            .delete_one(doc! {
                "_id": work_id,
                "lease_nonce": nonce,
                "lease_expires_at": {"$gte": &now},
            })
            .await?
            .deleted_count;
        Ok(deleted > 0)
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
        let changed = self
            .pool
            .collection::<Document>("work_items")
            .update_one(
                doc! {
                    "_id": work_id,
                    "lease_nonce": nonce,
                    "dead": false,
                    "lease_expires_at": {"$gte": &now},
                },
                doc! {"$set": {"lease_expires_at": &lease_expires}},
            )
            .await?
            .matched_count;
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
        let Some(document) = self
            .pool
            .collection::<Document>("work_items")
            .find_one(doc! {"_id": work_id, "lease_nonce": nonce})
            .await?
        else {
            return Ok(false);
        };
        let attempts = document.get_i64("attempts")?;
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
        let changed = self
            .pool
            .collection::<Document>("work_items")
            .update_one(
                doc! {
                    "_id": work_id,
                    "lease_nonce": nonce,
                    "lease_expires_at": {"$gte": &now},
                },
                doc! {
                    "$set": {
                        "not_before": &not_before,
                        "dead": exhausted,
                        "lease_nonce": null,
                        "lease_expires_at": null,
                    }
                },
            )
            .await?
            .matched_count;
        Ok(changed > 0)
    }

    pub async fn work_will_dead_letter(
        &self,
        work_id: &str,
        nonce: &str,
        disposition: WorkFailureDisposition,
    ) -> Result<bool, OperationError> {
        let Some(document) = self
            .pool
            .collection::<Document>("work_items")
            .find_one(doc! {
                "_id": work_id,
                "lease_nonce": nonce,
                "dead": false,
                "lease_expires_at": {"$gte": now_utc_str()},
            })
            .await?
        else {
            return Ok(false);
        };
        let attempts = document.get_i64("attempts")?;
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
        let owned = self
            .pool
            .collection::<Document>("work_items")
            .find_one(doc! {
                "_id": claim.id,
                "lease_nonce": claim.nonce,
                "dead": false,
                "lease_expires_at": {"$gte": &now},
            })
            .session(&mut *tx.connection())
            .await?;
        if owned.is_none() {
            tx.rollback().await?;
            return Err(OperationError::StaleWorkClaim);
        }
        let existing = self
            .pool
            .collection::<Document>("operation_steps")
            .find_one(doc! {"operation_id": operation_id, "step_key": step_key})
            .session(&mut *tx.connection())
            .await?;
        let state = match existing {
            Some(document) if document.get_str("status")? == "succeeded" => {
                StepState::AlreadySucceeded
            }
            // A process can die after the external effect but before this
            // row is completed. Re-running a non-idempotent effect here would
            // be the most dangerous possible recovery policy, so leave the
            // step for an explicit reconciliation decision.
            Some(document) if document.get_str("status")? == "running" => {
                StepState::NeedsReconciliation
            }
            Some(_) => StepState::Running,
            None => {
                let input_summary_json = serde_json::to_string(&input_summary)?;
                self.pool
                    .collection::<Document>("operation_steps")
                    .insert_one(doc! {
                        "operation_id": operation_id,
                        "step_key": step_key,
                        "attempts": 1i64,
                        "status": "running",
                        "input_summary": &input_summary_json,
                        "created_at": &now,
                        "updated_at": &now,
                    })
                    .session(&mut *tx.connection())
                    .await?;
                StepState::Running
            }
        };
        let version = format!("v_{}", OperationId::new());
        // A later step re-enters while the operation is already `running`, so
        // the transition filter must accept both. A terminal operation (startup
        // recovery, a competing finish) must not keep executing steps under a
        // surviving work lease.
        let changed = self
            .pool
            .collection::<Document>("operations")
            .update_one(
                doc! {"_id": operation_id, "status": {"$in": ["queued", "running"]}},
                doc! {
                    "$set": {
                        "status": "running",
                        "current_step": step_key,
                        "version": &version,
                        "updated_at": &now,
                    }
                },
            )
            .session(&mut *tx.connection())
            .await?
            .matched_count;
        if changed == 0 {
            tx.rollback().await?;
            return Err(OperationError::StaleWorkClaim);
        }
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
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        let owned = self
            .pool
            .collection::<Document>("work_items")
            .find_one(doc! {
                "_id": claim.id,
                "lease_nonce": claim.nonce,
                "dead": false,
                "lease_expires_at": {"$gte": &now},
            })
            .session(&mut session)
            .await?;
        if owned.is_none() {
            session.abort_transaction().await?;
            return Err(OperationError::StaleWorkClaim);
        }
        // `external_ref` semantics mirror SQL `COALESCE(?, external_ref)`:
        // only a caller-supplied ref overwrites a recorded one.
        let mut set = doc! {"status": "succeeded", "updated_at": &now};
        if let Some(reference) = external_ref {
            set.insert("external_ref", reference);
        }
        let changed = self
            .pool
            .collection::<Document>("operation_steps")
            .update_one(
                doc! {"operation_id": operation_id, "step_key": step_key},
                doc! {"$set": set},
            )
            .session(&mut session)
            .await?
            .matched_count;
        if changed == 0 {
            session.abort_transaction().await?;
            return Err(OperationError::StaleWorkClaim);
        }
        session.commit_transaction().await?;
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
        let kind_document = self
            .pool
            .collection::<Document>("operations")
            .find_one(doc! {"_id": operation_id})
            .session(&mut *work.connection())
            .await?;
        let Some(kind_document) = kind_document else {
            work.rollback().await?;
            return Err(OperationError::NotFound);
        };
        let kind = kind_document.get_str("kind")?.to_owned();
        let mut set = doc! {
            "status": status.as_str(),
            "current_step": null,
            "version": &version,
            "updated_at": &now,
        };
        if let Some(result) = result.as_ref() {
            set.insert("result_json", serde_json::to_string(result)?);
        }
        if let Some(problem) = problem.as_ref() {
            set.insert("problem_json", serde_json::to_string(problem)?);
        }
        let changed = self
            .pool
            .collection::<Document>("operations")
            .update_one(doc! {"_id": operation_id}, doc! {"$set": set})
            .session(&mut *work.connection())
            .await?
            .matched_count;
        if changed == 0 {
            return Err(OperationError::NotFound);
        }
        if status == OperationStatus::NeedsAttention {
            self.pool
                .collection::<Document>("operation_steps")
                .update_many(
                    doc! {"operation_id": operation_id, "status": "running"},
                    doc! {
                        "$set": {
                            "status": "failed",
                            "compensation_json": reconciliation_problem(),
                            "updated_at": &now,
                        }
                    },
                )
                .session(&mut *work.connection())
                .await?;
        }
        // Keep idempotency record in step with the terminal outcome.
        self.pool
            .collection::<Document>("idempotency_records")
            .update_many(
                doc! {"operation_id": operation_id},
                doc! {"$set": {"status": status.as_str()}},
            )
            .session(&mut *work.connection())
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
        let kind_document = self
            .pool
            .collection::<Document>("operations")
            .find_one(doc! {"_id": operation_id, "status": {"$in": ["queued", "running"]}})
            .session(&mut *work.connection())
            .await?;
        let Some(kind_document) = kind_document else {
            work.rollback().await?;
            return Ok(false);
        };
        let kind = kind_document.get_str("kind")?.to_owned();
        let owned = self
            .pool
            .collection::<Document>("work_items")
            .find_one(doc! {
                "_id": work_id,
                "lease_nonce": work_nonce,
                "dead": false,
                "lease_expires_at": {"$gte": &now},
            })
            .session(&mut *work.connection())
            .await?;
        if owned.is_none() {
            work.rollback().await?;
            return Ok(false);
        }
        let mut set = doc! {
            "status": completion.status.as_str(),
            "current_step": null,
            "version": &version,
            "updated_at": &now,
        };
        if let Some(result) = completion.result.as_ref() {
            set.insert("result_json", serde_json::to_string(result)?);
        }
        if let Some(problem) = completion.problem.as_ref() {
            set.insert("problem_json", serde_json::to_string(problem)?);
        }
        let changed = self
            .pool
            .collection::<Document>("operations")
            .update_one(
                doc! {"_id": operation_id, "status": {"$in": ["queued", "running"]}},
                doc! {"$set": set},
            )
            .session(&mut *work.connection())
            .await?
            .matched_count;
        if changed == 0 {
            work.rollback().await?;
            return Ok(false);
        }
        if completion.status == OperationStatus::NeedsAttention {
            self.pool
                .collection::<Document>("operation_steps")
                .update_many(
                    doc! {"operation_id": operation_id, "status": "running"},
                    doc! {
                        "$set": {
                            "status": "failed",
                            "compensation_json": reconciliation_problem(),
                            "updated_at": &now,
                        }
                    },
                )
                .session(&mut *work.connection())
                .await?;
        }
        self.pool
            .collection::<Document>("idempotency_records")
            .update_many(
                doc! {"operation_id": operation_id},
                doc! {"$set": {"status": completion.status.as_str()}},
            )
            .session(&mut *work.connection())
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
        let owned = self
            .pool
            .collection::<Document>("work_items")
            .find_one(doc! {
                "_id": claim.id,
                "lease_nonce": claim.nonce,
                "dead": false,
                "lease_expires_at": {"$gte": now_utc_str()},
            })
            .await?;
        if owned.is_none() {
            return Err(OperationError::StaleWorkClaim);
        }
        Ok(())
    }

    pub async fn get(&self, operation_id: &str) -> Result<Option<OperationView>, OperationError> {
        let document = self
            .pool
            .collection::<Document>("operations")
            .find_one(doc! {"_id": operation_id})
            .await?;
        document.map(view_from_document).transpose()
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
        // The SQL `INNER JOIN` becomes two sequential queries: the owner's
        // operation ids from the idempotency records, then the operations.
        let mut ids = self
            .pool
            .collection::<Document>("idempotency_records")
            .find(doc! {"owner_id": owner_id})
            .await?;
        let mut operation_ids = Vec::new();
        while let Some(document) = ids.try_next().await? {
            if let Ok(operation_id) = document.get_str("operation_id") {
                operation_ids.push(operation_id.to_owned());
            }
        }
        if operation_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut operations = self
            .pool
            .collection::<Document>("operations")
            .find(doc! {"kind": kind, "_id": {"$in": operation_ids}})
            .sort(doc! {"created_at": -1, "_id": -1})
            .limit(limit)
            .await?;
        let mut views = Vec::new();
        while let Some(document) = operations.try_next().await? {
            views.push(view_from_document(document)?);
        }
        Ok(views)
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
        let kind_document = self
            .pool
            .collection::<Document>("operations")
            .find_one(doc! {"_id": operation_id, "status": {"$in": ["queued", "running"]}})
            .session(&mut *work.connection())
            .await?;
        let Some(kind_document) = kind_document else {
            work.rollback().await?;
            return Ok(false);
        };
        let kind = kind_document.get_str("kind")?.to_owned();
        let owned = self
            .pool
            .collection::<Document>("work_items")
            .find_one(doc! {
                "_id": claim.id,
                "lease_nonce": claim.nonce,
                "dead": false,
                "lease_expires_at": {"$gte": &now},
            })
            .session(&mut *work.connection())
            .await?;
        if owned.is_none() {
            work.rollback().await?;
            return Ok(false);
        }
        let progress_json = serde_json::to_string(&progress)?;
        let changed = self
            .pool
            .collection::<Document>("operations")
            .update_one(
                doc! {"_id": operation_id, "status": {"$in": ["queued", "running"]}},
                doc! {
                    "$set": {
                        "status": "running",
                        "current_step": current_step,
                        "progress_json": &progress_json,
                        "version": &version,
                        "updated_at": &now,
                    }
                },
            )
            .session(&mut *work.connection())
            .await?
            .matched_count;
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
        session: &mut ClientSession,
        operation_id: &str,
    ) -> Result<Option<OperationView>, OperationError> {
        let document = self
            .pool
            .collection::<Document>("operations")
            .find_one(doc! {"_id": operation_id})
            .session(&mut *session)
            .await?;
        document.map(view_from_document).transpose()
    }

    pub async fn in_flight_for_target(
        &self,
        kind: &str,
        target_kind: &str,
        target_id: &str,
    ) -> Result<Option<String>, OperationError> {
        let document = self
            .pool
            .collection::<Document>("operations")
            .find_one(doc! {
                "kind": kind,
                "target_kind": target_kind,
                "target_id": target_id,
                "status": {"$in": ["queued", "running"]},
            })
            .sort(doc! {"updated_at": -1})
            .await?;
        Ok(document.and_then(|document| document.get_str("_id").ok().map(str::to_owned)))
    }

    /// Operations stuck in `running` with no live work lease - startup recovery
    /// must resume or terminalize them. The operations document carries no
    /// lease of its own; ownership lives on the linked `work_items` row, so a
    /// running operation is stale only when its work item's lease is missing or
    /// expired (or when no work item exists to resume it). Matching only on the
    /// operation status would report every running operation after a restart
    /// even when another worker still holds a live lease.
    pub async fn stale_running(&self) -> Result<Vec<String>, OperationError> {
        let now = now_utc_str();
        let mut running_cursor = self
            .pool
            .collection::<Document>("operations")
            .find(doc! {"status": "running"})
            .projection(doc! {"_id": 1})
            .await?;
        let mut running_ids = Vec::new();
        while let Some(document) = running_cursor.try_next().await? {
            running_ids.push(document.get_str("_id")?.to_owned());
        }
        if running_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut live_cursor = self
            .pool
            .collection::<Document>("work_items")
            .find(doc! {"dead": false, "lease_expires_at": {"$gte": &now}})
            .projection(doc! {"payload_json": 1})
            .await?;
        let mut live_operation_ids = HashSet::new();
        while let Some(document) = live_cursor.try_next().await? {
            let payload: Value = serde_json::from_str(document.get_str("payload_json")?)?;
            if let Some(operation_id) = payload.get("operation_id").and_then(Value::as_str) {
                live_operation_ids.insert(operation_id.to_owned());
            }
        }
        Ok(running_ids
            .into_iter()
            .filter(|id| !live_operation_ids.contains(id))
            .collect())
    }

    /// Delete idempotency records whose window has elapsed. Expired records
    /// must not satisfy lookups, but without pruning the two journals grow
    /// without bound; startup recovery is a cheap place to sweep them.
    pub async fn prune_expired_idempotency(&self) -> Result<u64, OperationError> {
        let now = now_utc_str();
        let deleted = self
            .pool
            .collection::<Document>("idempotency_records")
            .delete_many(doc! {"expires_at": {"$lt": &now}})
            .await?
            .deleted_count;
        let deleted_command = self
            .pool
            .collection::<Document>("command_idempotency_records")
            .delete_many(doc! {"expires_at": {"$lt": &now}})
            .await?
            .deleted_count;
        Ok(deleted + deleted_command)
    }

    async fn lookup_idempotency_in_tx(
        &self,
        session: &mut ClientSession,
        idem: &IdempotencyRequest,
    ) -> Result<Option<OperationView>, OperationError> {
        let now = now_utc_str();
        let document = self
            .pool
            .collection::<Document>("idempotency_records")
            .find_one(doc! {"_id": &idem.key, "expires_at": {"$gte": &now}})
            .session(&mut *session)
            .await?;
        let Some(document) = document else {
            return Ok(None);
        };
        let stored_digest = document.get_str("request_digest")?;
        let operation_id = document.get_str("operation_id")?;
        if stored_digest != idem.digest {
            return Err(OperationError::Internal(anyhow::anyhow!(
                "IDEMPOTENCY_KEY_REUSED"
            )));
        }
        self.get_in_tx(session, operation_id).await
    }

    async fn emit_operation_changed(
        &self,
        work: &mut UnitOfWorkTransaction,
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

fn view_from_document(document: Document) -> Result<OperationView, OperationError> {
    Ok(OperationView {
        id: document.get_str("_id")?.to_owned(),
        kind: document.get_str("kind")?.to_owned(),
        status: document.get_str("status")?.to_owned(),
        target_kind: document.get_str("target_kind")?.to_owned(),
        target_id: document
            .get("target_id")
            .and_then(Bson::as_str)
            .map(str::to_owned),
        current_step: document
            .get("current_step")
            .and_then(Bson::as_str)
            .map(str::to_owned),
        progress: parse_json_opt(document.get("progress_json").and_then(Bson::as_str))?,
        result: parse_json_opt(document.get("result_json").and_then(Bson::as_str))?,
        problem: parse_json_opt(document.get("problem_json").and_then(Bson::as_str))?,
        correlation_id: document.get_str("correlation_id")?.to_owned(),
        version: document.get_str("version")?.to_owned(),
        created_at: document.get_str("created_at")?.to_owned(),
        updated_at: document.get_str("updated_at")?.to_owned(),
    })
}

fn parse_json_opt(stored: Option<&str>) -> Result<Option<Value>, serde_json::Error> {
    match stored {
        Some(s) => serde_json::from_str(s).map(Some),
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

#[cfg(all(test, feature = "testing"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        clock::{format_utc, now_utc, now_utc_str},
        id::OwnerId,
        testing::TestDb,
    };
    use chrono::Duration;
    use mongodb::bson::{Document, doc};
    use serde_json::json;

    /// A configured throwaway database with an owner row and a bare
    /// OperationInterface.
    async fn test_harness() -> (Arc<TestDb>, OperationInterface, String) {
        let db = TestDb::open().await.unwrap();
        let owner_id = OwnerId::new();
        db.database()
            .collection::<Document>("owners")
            .insert_one(doc! {
                "_id": owner_id.to_string(),
                "display_name": "test",
                "created_at": now_utc_str(),
            })
            .await
            .unwrap();
        let ops = OperationInterface::new(db.database().clone(), db.events());
        (db, ops, owner_id.to_string())
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
        let (db, ops, _owner_id) = test_harness().await;
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
        db.database()
            .collection::<Document>("work_items")
            .update_one(
                doc! {"_id": &claimed.id},
                doc! {"$set": {"lease_expires_at": format_utc(now_utc() - Duration::seconds(10))}},
            )
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
        let (_db, ops, owner_id) = test_harness().await;
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
        let (db, ops, _owner_id) = test_harness().await;
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
            db.database()
                .collection::<Document>("work_items")
                .update_one(
                    doc! {"_id": &claimed.id},
                    doc! {"$set": {"not_before": format_utc(now_utc() - Duration::seconds(30))}},
                )
                .await
                .unwrap();
        }
        // After the cap the item is dead and cannot be claimed again.
        assert!(ops.claim_work("git.clone", 60).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn stale_running_reports_expired_or_missing_leases() {
        let (db, ops, _owner_id) = test_harness().await;
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
        ops.begin_step_claimed(claim, &op_id, "clone", json!({"repo": "x"}))
            .await
            .unwrap();
        // A running operation whose linked work lease is still live is not stale.
        assert!(ops.stale_running().await.unwrap().is_empty());
        // A running operation with no work item cannot be resumed, so it is stale.
        let bare = ops.create(create_request(None), None).await.unwrap();
        let bare_id = bare.operation.id.clone();
        db.database()
            .collection::<Document>("operations")
            .update_one(doc! {"_id": &bare_id}, doc! {"$set": {"status": "running"}})
            .await
            .unwrap();
        assert!(ops.stale_running().await.unwrap().contains(&bare_id));
        // Expiring the work lease makes the linked running operation stale.
        db.database()
            .collection::<Document>("work_items")
            .update_one(
                doc! {"_id": &claimed.id},
                doc! {"$set": {"lease_expires_at": format_utc(now_utc() - Duration::seconds(10))}},
            )
            .await
            .unwrap();
        let stale = ops.stale_running().await.unwrap();
        assert!(stale.contains(&op_id));
        assert!(stale.contains(&bare_id));
    }

    #[tokio::test]
    async fn step_replay_is_idempotent() {
        let (_db, ops, _owner_id) = test_harness().await;
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

    #[tokio::test]
    async fn begin_step_rejects_terminal_operation() {
        let (_db, ops, _owner_id) = test_harness().await;
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
        // Startup recovery can terminalize an operation while its work lease is
        // still live. Beginning a step on it must fail instead of running the
        // external effect on a finished operation.
        ops.finish(
            &op_id,
            OperationStatus::Succeeded,
            Some(json!({"ok": true})),
            None,
            CorrelationId::new(),
        )
        .await
        .unwrap();
        assert!(matches!(
            ops.begin_step_claimed(claim, &op_id, "clone", json!({"repo": "x"}))
                .await
                .unwrap_err(),
            OperationError::StaleWorkClaim
        ));
    }
}
