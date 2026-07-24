//! Public workspace propagation boundary.
//!
//! M2 scope (see `design.md`): only Main copies and the revision identity
//! record. `WorkspaceSyncInterface` creates a Main workspace copy, records an
//! initial Content Revision, exposes the current revision, and advances the
//! revision on a managed write. Merkle manifest collection, three-way Apply/Sync,
//! propagation cursors and Checkpoints arrive in M3/M5/M6.
//!
//! Revision identity (WS-003): a monotone `sequence` plus a unique `revision_id`
//! (UUIDv7). M2 leaves `manifest_root_hash` NULL; the revision_id is still a
//! stable, monotone content identity sufficient to detect ABA on `If-Match`.

use anyhow::anyhow;
use serde::Serialize;
use sqlx::SqlitePool;
use utoipa::ToSchema;

use crate::platform::{
    clock::{Clock, SystemClock, format_utc},
    id::{ProjectId, RevisionId},
};

/// Opaque handle for a workspace copy, stored verbatim in `workspace_copies.handle`.
/// Main copies use `main:<project-id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(transparent)]
pub struct WorkspaceHandle(pub String);

impl WorkspaceHandle {
    pub fn main(project_id: ProjectId) -> Self {
        Self(format!("main:{project_id}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A Content Revision identity. Exposed to clients as an opaque `v_01J...`
/// string via `main_revision` on the Project projection; used as an `If-Match`
/// condition so concurrent edits return `RESOURCE_VERSION_MISMATCH` instead of
/// half-writing.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(transparent)]
pub struct RevisionRef(pub String);

impl RevisionRef {
    pub fn new(id: RevisionId) -> Self {
        Self(format!("rev_{id}"))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceSyncError {
    #[error("workspace copy not found")]
    NotFound,
    #[error("revision mismatch: expected {expected}, current {current}")]
    RevisionMismatch { expected: String, current: String },
    #[error("storage error: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

#[derive(Clone)]
pub struct WorkspaceSyncInterface {
    pool: SqlitePool,
}

impl WorkspaceSyncInterface {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Create the Main workspace copy for a project and its first Content
    /// Revision. Idempotent: if the copy already exists, the existing revision
    /// is returned instead of erroring. Called by the clone operation once the
    /// on-disk repository exists.
    pub async fn ensure_main_copy(
        &self,
        project_id: ProjectId,
        managed_dir: &str,
        cause: &str,
        actor: serde_json::Value,
    ) -> Result<RevisionRef, WorkspaceSyncError> {
        let handle = WorkspaceHandle::main(project_id);
        let now = format_utc(SystemClock.now());

        let existing: Option<(Option<String>,)> =
            sqlx::query_as("SELECT current_revision_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;

        if let Some((Some(revision_id),)) = existing {
            return Ok(RevisionRef(revision_id));
        }

        let copy_version = format!("v_{}", crate::platform::id::RevisionId::new());
        let revision_id = RevisionId::new();
        let revision_ref = RevisionRef::new(revision_id);

        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT OR IGNORE INTO workspace_copies (handle, project_id, session_id, kind, managed_dir, current_revision_id, observation_generation, dirty, version, created_at, updated_at) VALUES (?, ?, NULL, 'main', ?, ?, 0, 0, ?, ?, ?)")
            .bind(handle.as_str())
            .bind(project_id.to_string())
            .bind(managed_dir)
            .bind(revision_ref.0.clone())
            .bind(&copy_version)
            .bind(&now)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO content_revisions (revision_id, workspace_handle, sequence, manifest_root_hash, cause, actor_json, prev_revision_id, stable, occurred_at) VALUES (?, ?, 1, NULL, ?, ?, NULL, 1, ?)")
            .bind(revision_ref.0.clone())
            .bind(handle.as_str())
            .bind(cause)
            .bind(serde_json::to_string(&actor)?)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(revision_ref)
    }

    /// Read the current revision identity for a Main copy.
    pub async fn current_revision(
        &self,
        handle: &WorkspaceHandle,
    ) -> Result<RevisionRef, WorkspaceSyncError> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT current_revision_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some((Some(revision_id),)) => Ok(RevisionRef(revision_id)),
            Some((None,)) => Err(WorkspaceSyncError::Internal(anyhow!(
                "main copy has no current revision"
            ))),
            None => Err(WorkspaceSyncError::NotFound),
        }
    }

    /// Advance the Main copy to a new revision. If `expected` is provided, the
    /// current revision must match it or the caller sees `RevisionMismatch`
    /// (clients get `RESOURCE_VERSION_MISMATCH` from the handler). Returns the
    /// new revision identity.
    pub async fn bump_revision(
        &self,
        handle: &WorkspaceHandle,
        expected: Option<&RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
    ) -> Result<RevisionRef, WorkspaceSyncError> {
        let current: Option<Option<String>> =
            sqlx::query_scalar("SELECT current_revision_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;
        let current = current
            .ok_or(WorkspaceSyncError::NotFound)?
            .ok_or_else(|| WorkspaceSyncError::Internal(anyhow!("copy has no revision")))?;
        if let Some(expected_ref) = expected
            && expected_ref.0 != current
        {
            return Err(WorkspaceSyncError::RevisionMismatch {
                expected: expected_ref.0.clone(),
                current,
            });
        }

        let now = format_utc(SystemClock.now());
        let next_sequence = self.next_sequence(handle).await?;
        let revision_id = RevisionId::new();
        let revision_ref = RevisionRef::new(revision_id);
        let copy_version = format!("v_{}", RevisionId::new());

        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO content_revisions (revision_id, workspace_handle, sequence, manifest_root_hash, cause, actor_json, prev_revision_id, stable, occurred_at) VALUES (?, ?, ?, NULL, ?, ?, ?, 1, ?)")
            .bind(revision_ref.0.clone())
            .bind(handle.as_str())
            .bind(next_sequence)
            .bind(cause)
            .bind(serde_json::to_string(&actor)?)
            .bind(&current)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE workspace_copies SET current_revision_id = ?, version = ?, updated_at = ? WHERE handle = ?")
            .bind(revision_ref.0.clone())
            .bind(&copy_version)
            .bind(&now)
            .bind(handle.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(revision_ref)
    }

    async fn next_sequence(&self, handle: &WorkspaceHandle) -> Result<i64, WorkspaceSyncError> {
        let max: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(sequence) FROM content_revisions WHERE workspace_handle = ?",
        )
        .bind(handle.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(max.unwrap_or(0) + 1)
    }
}
