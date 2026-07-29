//! Public workspace propagation boundary.
//!
//! M2 delivered Main copies + revision identity. M3 Stage 1 adds Session copies,
//! full Merkle manifest collection (`manifest_root_hash`), path-level Diff summary,
//! and Agent file mutations that advance Session Content Revisions.
//!
//! Apply/Sync execution, three-way merge, and external file watchers remain M4/M5.

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use serde::Serialize;
use sqlx::{SqliteConnection, SqlitePool};
use utoipa::ToSchema;

use crate::platform::{
    clock::{Clock, SystemClock, format_utc},
    id::{ProjectId, RevisionId, SessionId, SnapshotId},
    managed_storage::BlobStore,
    path::{PathError, validate_workspace_path},
};

use super::diff::DiffSummary;
use super::manifest::{ManifestRoot, collect_manifest as walk_manifest};
use super::session_copy::{
    create_session_worktree, main_repo_abs, remove_session_tree, session_managed_dir,
    session_repo_abs,
};
use super::working_tree::{diff_working_trees, hash_working_tree, seed_session_from_main};

/// Opaque handle for a workspace copy, stored in `workspace_copies.handle`.
/// Main: `main:<project-id>`; Session: `session:<session-id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(transparent)]
pub struct WorkspaceHandle(pub String);

impl WorkspaceHandle {
    pub fn main(project_id: ProjectId) -> Self {
        Self(format!("main:{project_id}"))
    }

    pub fn session(session_id: SessionId) -> Self {
        Self(format!("session:{session_id}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Content Revision identity exposed as opaque `rev_<uuid>` string.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(transparent)]
pub struct RevisionRef(pub String);

impl RevisionRef {
    pub fn new(id: RevisionId) -> Self {
        Self(format!("rev_{id}"))
    }
}

/// Result of ensuring a Session workspace copy exists.
#[derive(Debug, Clone, Serialize)]
pub struct SessionCopyResult {
    pub handle: WorkspaceHandle,
    pub revision: RevisionRef,
    pub source_main_revision: RevisionRef,
    pub manifest_root_hash: String,
    pub managed_dir: String,
}

type ExistingSessionCopy = (Option<String>, Option<String>, String, Option<String>);

/// Agent / tool file mutation against a Session (or, later, any) copy.
#[derive(Debug, Clone)]
pub enum FileMutation {
    /// Create or overwrite a file with the given bytes.
    Write { path: String, content: Vec<u8> },
    /// Replace file content after a patch has been applied by the tool layer.
    /// Requires the target path to already exist.
    Patch { path: String, content: Vec<u8> },
    /// Delete a file (or empty directory).
    Delete { path: String },
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceSyncError {
    #[error("workspace copy not found")]
    NotFound,
    #[error("revision mismatch: expected {expected}, current {current}")]
    RevisionMismatch { expected: String, current: String },
    #[error("invalid workspace path: {0}")]
    InvalidPath(#[from] PathError),
    #[error("path not found: {0}")]
    PathNotFound(String),
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
    data_root: PathBuf,
    blobs: BlobStore,
}

impl WorkspaceSyncInterface {
    pub fn new(pool: SqlitePool, data_root: &Path, blobs: BlobStore) -> Self {
        Self {
            pool,
            data_root: data_root.to_path_buf(),
            blobs,
        }
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub fn session_repo_path(&self, session_id: SessionId) -> PathBuf {
        session_repo_abs(&self.data_root, session_id)
    }

    /// Create the Main workspace copy for a project and its first Content
    /// Revision. Idempotent: if the copy already exists, the existing revision
    /// is returned. M2 leaves `manifest_root_hash` NULL for Main; Session copies
    /// always populate it (see [`Self::ensure_session_copy`]).
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

        let copy_version = format!("v_{}", RevisionId::new());
        let revision_id = RevisionId::new();
        let revision_ref = RevisionRef::new(revision_id);

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT OR IGNORE INTO workspace_copies \
             (handle, project_id, session_id, kind, managed_dir, current_revision_id, \
              observation_generation, dirty, version, created_at, updated_at) \
             VALUES (?, ?, NULL, 'main', ?, ?, 0, 0, ?, ?, ?)",
        )
        .bind(handle.as_str())
        .bind(project_id.to_string())
        .bind(managed_dir)
        .bind(revision_ref.0.clone())
        .bind(&copy_version)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO content_revisions \
             (revision_id, workspace_handle, sequence, manifest_root_hash, cause, \
              actor_json, prev_revision_id, stable, occurred_at) \
             VALUES (?, ?, 1, NULL, ?, ?, NULL, 1, ?)",
        )
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

    /// Read the current revision identity for any workspace copy.
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
                "copy has no current revision"
            ))),
            None => Err(WorkspaceSyncError::NotFound),
        }
    }

    /// Advance a copy to a new revision without collecting a Merkle root
    /// (Main editor path from M2). Prefer [`Self::apply_file_mutation`] for
    /// Session tool writes so `manifest_root_hash` is populated.
    pub async fn bump_revision(
        &self,
        handle: &WorkspaceHandle,
        expected: Option<&RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
    ) -> Result<RevisionRef, WorkspaceSyncError> {
        self.advance_revision(handle, expected, cause, actor, None, None)
            .await
    }

    pub async fn bump_revision_in_tx(
        &self,
        tx: &mut SqliteConnection,
        handle: &WorkspaceHandle,
        expected: Option<&RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
    ) -> Result<RevisionRef, WorkspaceSyncError> {
        self.advance_revision_in_tx(tx, handle, expected, cause, actor, None)
            .await
    }

    /// Create a Session workspace copy from Project Main.
    ///
    /// Idempotent: if the Session handle already exists, returns the existing
    /// revision without re-copying. Copies Main managed files (skip `.git`),
    /// optionally `git init`s a baseline, collects a full Merkle manifest,
    /// writes revision sequence=1 with `manifest_root_hash`, and initializes
    /// `propagation_links` cursors to that create pair.
    pub async fn ensure_session_copy(
        &self,
        project_id: ProjectId,
        session_id: SessionId,
        source_main_revision: Option<&RevisionRef>,
        actor: serde_json::Value,
    ) -> Result<SessionCopyResult, WorkspaceSyncError> {
        let handle = WorkspaceHandle::session(session_id);
        let existing: Option<ExistingSessionCopy> = sqlx::query_as(
            "SELECT current_revision_id, \
                    (SELECT manifest_root_hash FROM content_revisions \
                     WHERE revision_id = workspace_copies.current_revision_id), \
                    managed_dir, \
                    (SELECT initial_main_revision_id FROM propagation_links \
                     WHERE session_id = workspace_copies.session_id) \
             FROM workspace_copies WHERE handle = ?",
        )
        .bind(handle.as_str())
        .fetch_optional(&self.pool)
        .await?;

        if let Some((Some(revision_id), root, managed_dir, Some(source_main_revision))) = existing {
            return Ok(SessionCopyResult {
                handle,
                revision: RevisionRef(revision_id),
                source_main_revision: RevisionRef(source_main_revision),
                manifest_root_hash: root.unwrap_or_default(),
                managed_dir,
            });
        }

        let main_handle = WorkspaceHandle::main(project_id);
        let main_row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT managed_dir, current_revision_id FROM workspace_copies WHERE handle = ?",
        )
        .bind(main_handle.as_str())
        .fetch_optional(&self.pool)
        .await?;
        let (main_managed_dir, main_revision_id) = main_row.ok_or(WorkspaceSyncError::NotFound)?;
        let main_revision_id = main_revision_id.ok_or_else(|| {
            WorkspaceSyncError::Internal(anyhow!("main copy has no current revision"))
        })?;
        if let Some(expected) = source_main_revision
            && expected.0 != main_revision_id
        {
            return Err(WorkspaceSyncError::RevisionMismatch {
                expected: expected.0.clone(),
                current: main_revision_id,
            });
        }

        let managed_dir = session_managed_dir(session_id);
        let session_abs = session_repo_abs(&self.data_root, session_id);
        let main_abs = main_repo_abs(&self.data_root, &main_managed_dir);

        // Session copy is a git worktree of the Main clone — shared .git object
        // store, detached-HEAD checkout at Main's current tree. No file copy,
        // no re-init; the Session inherits Main's history.
        create_session_worktree(&main_abs, &session_abs).map_err(WorkspaceSyncError::Internal)?;
        seed_session_from_main(&main_abs, &session_abs).map_err(WorkspaceSyncError::Internal)?;

        let manifest = hash_working_tree(&session_abs)
            .await
            .map_err(WorkspaceSyncError::Internal)?;
        let root_hash = manifest.root_hash.clone();
        let now = format_utc(SystemClock.now());
        let copy_version = format!("v_{}", RevisionId::new());
        let revision_ref = RevisionRef::new(RevisionId::new());
        let snapshot_id = SnapshotId::new();
        let link_version = format!("v_{}", RevisionId::new());

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO workspace_copies \
             (handle, project_id, session_id, kind, managed_dir, current_revision_id, \
              observation_generation, dirty, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'session', ?, ?, 0, 0, ?, ?, ?)",
        )
        .bind(handle.as_str())
        .bind(project_id.to_string())
        .bind(session_id.to_string())
        .bind(&managed_dir)
        .bind(revision_ref.0.clone())
        .bind(&copy_version)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO content_revisions \
             (revision_id, workspace_handle, sequence, manifest_root_hash, cause, \
              actor_json, prev_revision_id, stable, occurred_at) \
             VALUES (?, ?, 1, ?, 'session.create', ?, NULL, 1, ?)",
        )
        .bind(revision_ref.0.clone())
        .bind(handle.as_str())
        .bind(&root_hash)
        .bind(serde_json::to_string(&actor)?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO workspace_snapshots \
             (snapshot_id, revision_id, manifest_root_hash, purpose, integrity_state, created_at) \
             VALUES (?, ?, ?, 'session_create', 'complete', ?)",
        )
        .bind(snapshot_id.to_string())
        .bind(revision_ref.0.clone())
        .bind(&root_hash)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO propagation_links \
             (project_id, session_id, source_branch, initial_main_revision_id, \
              main_to_session_cursor_revision_id, session_to_main_cursor_revision_id, \
              version, created_at, updated_at) \
             VALUES (?, ?, 'main', ?, ?, ?, ?, ?, ?)",
        )
        .bind(project_id.to_string())
        .bind(session_id.to_string())
        .bind(&main_revision_id)
        .bind(&main_revision_id)
        .bind(revision_ref.0.clone())
        .bind(&link_version)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(SessionCopyResult {
            handle,
            revision: revision_ref,
            source_main_revision: RevisionRef(main_revision_id),
            manifest_root_hash: root_hash,
            managed_dir,
        })
    }

    /// Apply a file mutation to a Session copy, then full-rescan the Merkle
    /// tree and bump the Content Revision.
    ///
    /// ABA: even if content returns to a previous root hash, a new
    /// `revision_id` is always allocated (monotonic identity).
    ///
    /// M3 MVP uses full rescan after every mutation (documented tradeoff;
    /// incremental ancestor rehash is a later optimization).
    pub async fn apply_file_mutation(
        &self,
        handle: &WorkspaceHandle,
        mutation: FileMutation,
        expected: Option<&RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
    ) -> Result<RevisionRef, WorkspaceSyncError> {
        let managed_dir = self.managed_dir_for(handle).await?;
        let root = self.data_root.join(&managed_dir);

        match &mutation {
            FileMutation::Write { path, content } => {
                let rel = validate_workspace_path(path)?;
                if is_git_path(&rel) {
                    return Err(WorkspaceSyncError::InvalidPath(PathError::Invalid));
                }
                let abs = root.join(&rel);
                if let Some(parent) = abs.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|e| WorkspaceSyncError::Internal(anyhow!("mkdir: {e}")))?;
                }
                atomic_write(&abs, content).await?;
            }
            FileMutation::Patch { path, content } => {
                let rel = validate_workspace_path(path)?;
                if is_git_path(&rel) {
                    return Err(WorkspaceSyncError::InvalidPath(PathError::Invalid));
                }
                let abs = root.join(&rel);
                if !abs.is_file() {
                    return Err(WorkspaceSyncError::PathNotFound(path.clone()));
                }
                atomic_write(&abs, content).await?;
            }
            FileMutation::Delete { path } => {
                let rel = validate_workspace_path(path)?;
                if is_git_path(&rel) {
                    return Err(WorkspaceSyncError::InvalidPath(PathError::Invalid));
                }
                let abs = root.join(&rel);
                if abs.is_file() {
                    tokio::fs::remove_file(&abs)
                        .await
                        .map_err(|e| WorkspaceSyncError::Internal(anyhow!("remove file: {e}")))?;
                } else if abs.is_dir() {
                    // Only empty dirs (non-recursive by default).
                    tokio::fs::remove_dir(&abs)
                        .await
                        .map_err(|e| WorkspaceSyncError::Internal(anyhow!("remove dir: {e}")))?;
                } else {
                    return Err(WorkspaceSyncError::PathNotFound(path.clone()));
                }
            }
        }

        let manifest = hash_working_tree(&root)
            .await
            .map_err(WorkspaceSyncError::Internal)?;
        self.advance_revision(
            handle,
            expected,
            cause,
            actor,
            Some(manifest.root_hash.as_str()),
            Some("tool_write"),
        )
        .await
    }

    /// Full Merkle scan of a workspace copy. Used by Diff and tests.
    pub async fn collect_manifest(
        &self,
        handle: &WorkspaceHandle,
    ) -> Result<ManifestRoot, WorkspaceSyncError> {
        let managed_dir = self.managed_dir_for(handle).await?;
        let root = self.data_root.join(&managed_dir);
        walk_manifest(&root, &self.blobs, handle.as_str())
            .await
            .map_err(WorkspaceSyncError::Internal)
    }

    /// Path-level Diff summary of Session current tree vs Main current tree.
    pub async fn diff_summary(
        &self,
        session_id: SessionId,
    ) -> Result<DiffSummary, WorkspaceSyncError> {
        let session_handle = WorkspaceHandle::session(session_id);
        let dirs: Option<(String, String)> = sqlx::query_as(
            "SELECT session.managed_dir, main.managed_dir \
             FROM workspace_copies AS session \
             JOIN workspace_copies AS main \
               ON main.project_id = session.project_id AND main.kind = 'main' \
             WHERE session.handle = ?",
        )
        .bind(session_handle.as_str())
        .fetch_optional(&self.pool)
        .await?;
        let (session_dir, main_dir) = dirs.ok_or(WorkspaceSyncError::NotFound)?;
        diff_working_trees(
            &self.data_root.join(session_dir),
            &self.data_root.join(main_dir),
        )
        .await
        .map_err(WorkspaceSyncError::Internal)
    }

    /// Cascade-delete a Session copy: directory tree + DB rows for that handle
    /// (workspace_copies cascades content_revisions/snapshots; links by session_id).
    /// Does **not** touch Main or Runtime.
    pub async fn delete_session_copy(
        &self,
        session_id: SessionId,
    ) -> Result<(), WorkspaceSyncError> {
        let handle = WorkspaceHandle::session(session_id);
        let exists: Option<String> =
            sqlx::query_scalar("SELECT handle FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;
        if exists.is_none() {
            // Idempotent: already gone is success.
            let _ = remove_session_tree(&self.data_root, session_id);
            return Ok(());
        }

        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM propagation_links WHERE session_id = ?")
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?;
        // content_revisions / workspace_snapshots cascade from workspace_copies.
        sqlx::query("DELETE FROM workspace_copies WHERE handle = ?")
            .bind(handle.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        remove_session_tree(&self.data_root, session_id).map_err(WorkspaceSyncError::Internal)?;
        Ok(())
    }

    async fn managed_dir_for(
        &self,
        handle: &WorkspaceHandle,
    ) -> Result<String, WorkspaceSyncError> {
        let row: Option<String> =
            sqlx::query_scalar("SELECT managed_dir FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;
        row.ok_or(WorkspaceSyncError::NotFound)
    }

    async fn advance_revision(
        &self,
        handle: &WorkspaceHandle,
        expected: Option<&RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
        manifest_root_hash: Option<&str>,
        snapshot_purpose: Option<&str>,
    ) -> Result<RevisionRef, WorkspaceSyncError> {
        let mut tx = self.pool.begin().await?;
        let revision = self
            .advance_revision_in_tx(
                &mut tx,
                handle,
                expected,
                cause,
                actor,
                manifest_root_hash.zip(snapshot_purpose),
            )
            .await?;
        tx.commit().await?;
        Ok(revision)
    }

    async fn advance_revision_in_tx(
        &self,
        tx: &mut SqliteConnection,
        handle: &WorkspaceHandle,
        expected: Option<&RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
        snapshot: Option<(&str, &str)>,
    ) -> Result<RevisionRef, WorkspaceSyncError> {
        let current: Option<Option<String>> =
            sqlx::query_scalar("SELECT current_revision_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&mut *tx)
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
        let next_sequence: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM content_revisions \
             WHERE workspace_handle = ?",
        )
        .bind(handle.as_str())
        .fetch_one(&mut *tx)
        .await?;
        let revision_ref = RevisionRef::new(RevisionId::new());
        let copy_version = format!("v_{}", RevisionId::new());

        sqlx::query(
            "INSERT INTO content_revisions \
             (revision_id, workspace_handle, sequence, manifest_root_hash, cause, \
              actor_json, prev_revision_id, stable, occurred_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?)",
        )
        .bind(revision_ref.0.clone())
        .bind(handle.as_str())
        .bind(next_sequence)
        .bind(snapshot.map(|(root, _)| root))
        .bind(cause)
        .bind(serde_json::to_string(&actor)?)
        .bind(&current)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        if let Some((root, purpose)) = snapshot {
            let snapshot_id = SnapshotId::new();
            sqlx::query(
                "INSERT INTO workspace_snapshots \
                 (snapshot_id, revision_id, manifest_root_hash, purpose, integrity_state, created_at) \
                 VALUES (?, ?, ?, ?, 'complete', ?)",
            )
            .bind(snapshot_id.to_string())
            .bind(revision_ref.0.clone())
            .bind(root)
            .bind(purpose)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE workspace_copies SET current_revision_id = ?, version = ?, updated_at = ? \
             WHERE handle = ?",
        )
        .bind(revision_ref.0.clone())
        .bind(&copy_version)
        .bind(&now)
        .bind(handle.as_str())
        .execute(&mut *tx)
        .await?;
        Ok(revision_ref)
    }
}

fn is_git_path(rel: &Path) -> bool {
    rel.components().any(|c| c.as_os_str() == ".git")
}

async fn atomic_write(abs: &Path, content: &[u8]) -> Result<(), WorkspaceSyncError> {
    let tmp = abs.with_extension("janus-tmp");
    tokio::fs::write(&tmp, content)
        .await
        .map_err(|e| WorkspaceSyncError::Internal(anyhow!("write temp: {e}")))?;
    tokio::fs::rename(&tmp, abs)
        .await
        .map_err(|e| WorkspaceSyncError::Internal(anyhow!("rename: {e}")))?;
    Ok(())
}
