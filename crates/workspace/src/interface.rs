//! Public Workspace capability boundary.
//!
//! Owns copy lifecycle, content revision identity, manifests, diffs, and
//! controlled file mutations. Apply/Sync orchestration and external watchers
//! remain workflow responsibilities.

use std::{
    collections::HashMap,
    fmt::Display,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use anyhow::anyhow;
use janus_infrastructure::clock::now_utc_str;
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};
use utoipa::ToSchema;
use uuid::Uuid;

use janus_infrastructure::managed_storage::BlobStore;

use super::diff::DiffSummary;
pub use super::diff::{DiffLineKind, line_hunks};
use super::manifest::{ManifestRoot, collect_manifest as walk_manifest};
pub use super::path::{PathError, validate_workspace_path};
use super::session_copy::{
    create_session_worktree, main_repo_abs, main_worktree_is_clean, remove_session_tree,
    session_managed_dir, session_repo_abs,
};
use super::working_tree::{
    diff_working_trees, git_head, hash_working_tree, rehash_working_tree_paths,
    seed_session_from_main,
};

/// Opaque handle for a workspace copy, stored in `workspace_copies.handle`.
/// Main: `main:<project-id>`; Session: `session:<session-id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(transparent)]
pub struct WorkspaceHandle(pub String);

impl WorkspaceHandle {
    pub fn main(project_id: impl Display) -> Self {
        Self(format!("main:{project_id}"))
    }

    pub fn session(session_id: impl Display) -> Self {
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
    pub fn new(id: impl Display) -> Self {
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

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SaveTextInput {
    pub path: String,
    pub content: String,
    pub expected_main_revision: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MoveFileInput {
    pub from: String,
    pub to: String,
    pub expected_main_revision: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DeleteFileInput {
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
    pub expected_main_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FileTreeView {
    pub path: String,
    pub kind: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FileMetaView {
    pub path: String,
    pub size: u64,
    pub editable: bool,
    pub mime: Option<String>,
    pub main_revision: Option<String>,
}

type ExistingSessionCopy = (Option<String>, Option<String>, String, Option<String>);

static HEAD_MANIFESTS: OnceLock<Mutex<HashMap<String, ManifestRoot>>> = OnceLock::new();

fn cached_head_manifest(head: &str) -> Option<ManifestRoot> {
    HEAD_MANIFESTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()?
        .get(head)
        .cloned()
}

fn cache_head_manifest(head: &str, manifest: &ManifestRoot) {
    if let Ok(mut manifests) = HEAD_MANIFESTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        // This is an optimization only: a bounded cache prevents repeated
        // clean-head scans without making revision correctness depend on it.
        if manifests.len() >= 16 && !manifests.contains_key(head) {
            manifests.clear();
        }
        manifests.insert(head.to_owned(), manifest.clone());
    }
}

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
    /// Delete a file or directory tree when the caller explicitly permits it.
    DeleteTree { path: String },
    /// Rename a file or directory inside one workspace copy.
    Move { from: String, to: String },
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("workspace copy not found")]
    NotFound,
    #[error("revision mismatch: expected {expected}, current {current}")]
    RevisionMismatch { expected: String, current: String },
    #[error("invalid workspace path: {0}")]
    InvalidPath(#[from] PathError),
    #[error("path not found: {0}")]
    PathNotFound(String),
    #[error("file is not editable: {0}")]
    NotEditable(String),
    #[error("storage error: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

#[derive(Clone)]
pub struct WorkspaceInterface {
    pool: SqlitePool,
    data_root: PathBuf,
    blobs: BlobStore,
}

impl WorkspaceInterface {
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

    pub fn session_repo_path(&self, session_id: impl Display) -> PathBuf {
        session_repo_abs(&self.data_root, session_id)
    }

    pub async fn workspace_root(
        &self,
        handle: &WorkspaceHandle,
    ) -> Result<PathBuf, WorkspaceError> {
        let managed_dir = self.managed_dir_for(handle).await?;
        tokio::fs::canonicalize(self.data_root.join(managed_dir))
            .await
            .map_err(|error| WorkspaceError::Internal(error.into()))
    }

    pub async fn file_meta(
        &self,
        handle: &WorkspaceHandle,
        raw_path: &str,
    ) -> Result<FileMetaView, WorkspaceError> {
        let rel = validate_workspace_path(raw_path)?;
        let abs = self.workspace_root(handle).await?.join(&rel);
        let meta = tokio::fs::metadata(&abs)
            .await
            .map_err(|_| WorkspaceError::PathNotFound(raw_path.to_owned()))?;
        let revision = self
            .current_revision(handle)
            .await
            .ok()
            .map(|revision| revision.0);
        Ok(FileMetaView {
            path: raw_path.to_owned(),
            size: meta.len(),
            editable: meta.len() <= 10 * 1024 * 1024 && is_utf8_text_file(&abs).await,
            mime: guess_mime(&abs),
            main_revision: revision,
        })
    }

    pub async fn file_content(
        &self,
        handle: &WorkspaceHandle,
        raw_path: &str,
    ) -> Result<Vec<u8>, WorkspaceError> {
        let rel = validate_workspace_path(raw_path)?;
        let abs = self.workspace_root(handle).await?.join(rel);
        tokio::fs::read(&abs)
            .await
            .map_err(|_| WorkspaceError::PathNotFound(raw_path.to_owned()))
    }

    pub async fn file_tree(
        &self,
        handle: &WorkspaceHandle,
        raw_path: &str,
    ) -> Result<Vec<FileTreeView>, WorkspaceError> {
        let rel = if raw_path.is_empty() {
            PathBuf::new()
        } else {
            validate_workspace_path(raw_path)?
        };
        let abs = self.workspace_root(handle).await?.join(&rel);
        let mut entries = tokio::fs::read_dir(&abs)
            .await
            .map_err(|_| WorkspaceError::PathNotFound(raw_path.to_owned()))?;
        let mut out = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| WorkspaceError::Internal(error.into()))?
        {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".git" {
                continue;
            }
            let meta = entry
                .metadata()
                .await
                .map_err(|error| WorkspaceError::Internal(error.into()))?;
            let child_path = if rel.as_os_str().is_empty() {
                name.clone()
            } else {
                format!("{}/{name}", rel.to_string_lossy())
            };
            out.push(FileTreeView {
                path: child_path,
                kind: if meta.is_dir() { "dir" } else { "file" }.into(),
                size: meta.len(),
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// Create the Main workspace copy for a project and its first Content
    /// Revision. Idempotent: if the copy already exists, the existing revision
    /// is returned. Main revisions leave `manifest_root_hash` NULL; Session
    /// revisions always populate it (see [`Self::ensure_session_copy`]).
    pub async fn ensure_main_copy(
        &self,
        project_id: impl Display,
        managed_dir: &str,
        cause: &str,
        actor: serde_json::Value,
    ) -> Result<RevisionRef, WorkspaceError> {
        let handle = WorkspaceHandle::main(&project_id);
        let now = now_utc_str();

        let existing: Option<(Option<String>,)> =
            sqlx::query_as("SELECT current_revision_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;

        if let Some((Some(revision_id),)) = existing {
            return Ok(RevisionRef(revision_id));
        }

        let copy_version = format!("v_{}", Uuid::now_v7());
        let revision_id = Uuid::now_v7();
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
    ) -> Result<RevisionRef, WorkspaceError> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT current_revision_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some((Some(revision_id),)) => Ok(RevisionRef(revision_id)),
            Some((None,)) => Err(WorkspaceError::Internal(anyhow!(
                "copy has no current revision"
            ))),
            None => Err(WorkspaceError::NotFound),
        }
    }

    /// Advance a copy to a new revision without collecting a Merkle root
    /// (Main editor path). Prefer [`Self::apply_file_mutation`] for
    /// Session tool writes so `manifest_root_hash` is populated.
    pub async fn bump_revision(
        &self,
        handle: &WorkspaceHandle,
        expected: Option<&RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
    ) -> Result<RevisionRef, WorkspaceError> {
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
    ) -> Result<RevisionRef, WorkspaceError> {
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
        project_id: impl Display,
        session_id: impl Display,
        source_main_revision: Option<&RevisionRef>,
        actor: serde_json::Value,
    ) -> Result<SessionCopyResult, WorkspaceError> {
        let handle = WorkspaceHandle::session(&session_id);
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

        let main_handle = WorkspaceHandle::main(&project_id);
        let main_row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT managed_dir, current_revision_id FROM workspace_copies WHERE handle = ?",
        )
        .bind(main_handle.as_str())
        .fetch_optional(&self.pool)
        .await?;
        let (main_managed_dir, main_revision_id) = main_row.ok_or(WorkspaceError::NotFound)?;
        let main_revision_id = main_revision_id.ok_or_else(|| {
            WorkspaceError::Internal(anyhow!("main copy has no current revision"))
        })?;
        if let Some(expected) = source_main_revision
            && expected.0 != main_revision_id
        {
            return Err(WorkspaceError::RevisionMismatch {
                expected: expected.0.clone(),
                current: main_revision_id,
            });
        }

        let managed_dir = session_managed_dir(&session_id);
        let session_abs = session_repo_abs(&self.data_root, &session_id);
        let main_abs = main_repo_abs(&self.data_root, &main_managed_dir);

        // Session copy is a git worktree of the Main clone - shared .git object
        // store, detached-HEAD checkout at Main's current tree. No file copy,
        // no re-init; the Session inherits Main's history.
        let main_for_copy = main_abs.clone();
        let session_for_copy = session_abs.clone();
        let (head, main_was_clean) = tokio::task::spawn_blocking(move || {
            let head = git_head(&main_for_copy)?;
            let clean = main_worktree_is_clean(&main_for_copy)?;
            create_session_worktree(&main_for_copy, &session_for_copy)?;
            Ok::<(String, bool), anyhow::Error>((head, clean))
        })
        .await
        .map_err(|error| WorkspaceError::Internal(anyhow!("workspace copy task failed: {error}")))?
        .map_err(WorkspaceError::Internal)?;

        let base_manifest = match cached_head_manifest(&head) {
            Some(manifest) => manifest,
            None => {
                let manifest = hash_working_tree(&session_abs)
                    .await
                    .map_err(WorkspaceError::Internal)?;
                cache_head_manifest(&head, &manifest);
                manifest
            }
        };
        let manifest = if main_was_clean {
            base_manifest
        } else {
            let main_for_seed = main_abs.clone();
            let session_for_seed = session_abs.clone();
            let changed_paths = tokio::task::spawn_blocking(move || {
                seed_session_from_main(&main_for_seed, &session_for_seed)
            })
            .await
            .map_err(|error| {
                WorkspaceError::Internal(anyhow!("workspace seed task failed: {error}"))
            })?
            .map_err(WorkspaceError::Internal)?;
            rehash_working_tree_paths(&session_abs, &base_manifest, &changed_paths)
                .await
                .map_err(WorkspaceError::Internal)?
        };
        let root_hash = manifest.root_hash;
        let now = now_utc_str();
        let copy_version = format!("v_{}", Uuid::now_v7());
        let revision_ref = RevisionRef::new(Uuid::now_v7());
        let snapshot_id = Uuid::now_v7();
        let link_version = format!("v_{}", Uuid::now_v7());

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
    /// The current implementation uses a full rescan after every mutation;
    /// incremental ancestor rehashing is an internal optimization left for later.
    pub async fn apply_file_mutation(
        &self,
        handle: &WorkspaceHandle,
        mutation: FileMutation,
        expected: Option<&RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
    ) -> Result<RevisionRef, WorkspaceError> {
        let managed_dir = self.managed_dir_for(handle).await?;
        let root = self.data_root.join(&managed_dir);

        match &mutation {
            FileMutation::Write { path, content } => {
                let rel = validate_workspace_path(path)?;
                if is_git_path(&rel) {
                    return Err(WorkspaceError::InvalidPath(PathError::Invalid));
                }
                let abs = root.join(&rel);
                if let Some(parent) = abs.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|e| WorkspaceError::Internal(anyhow!("mkdir: {e}")))?;
                }
                atomic_write(&abs, content).await?;
            }
            FileMutation::Patch { path, content } => {
                let rel = validate_workspace_path(path)?;
                if is_git_path(&rel) {
                    return Err(WorkspaceError::InvalidPath(PathError::Invalid));
                }
                let abs = root.join(&rel);
                if !abs.is_file() {
                    return Err(WorkspaceError::PathNotFound(path.clone()));
                }
                atomic_write(&abs, content).await?;
            }
            FileMutation::Delete { path } => {
                let rel = validate_workspace_path(path)?;
                if is_git_path(&rel) {
                    return Err(WorkspaceError::InvalidPath(PathError::Invalid));
                }
                let abs = root.join(&rel);
                if abs.is_file() {
                    tokio::fs::remove_file(&abs)
                        .await
                        .map_err(|e| WorkspaceError::Internal(anyhow!("remove file: {e}")))?;
                } else if abs.is_dir() {
                    // Only empty dirs (non-recursive by default).
                    tokio::fs::remove_dir(&abs)
                        .await
                        .map_err(|e| WorkspaceError::Internal(anyhow!("remove dir: {e}")))?;
                } else {
                    return Err(WorkspaceError::PathNotFound(path.clone()));
                }
            }
            FileMutation::DeleteTree { path } => {
                let rel = validate_workspace_path(path)?;
                if is_git_path(&rel) {
                    return Err(WorkspaceError::InvalidPath(PathError::Invalid));
                }
                let abs = root.join(&rel);
                if abs.is_dir() {
                    tokio::fs::remove_dir_all(&abs)
                        .await
                        .map_err(|e| WorkspaceError::Internal(anyhow!("remove tree: {e}")))?;
                } else if abs.is_file() {
                    tokio::fs::remove_file(&abs)
                        .await
                        .map_err(|e| WorkspaceError::Internal(anyhow!("remove file: {e}")))?;
                } else {
                    return Err(WorkspaceError::PathNotFound(path.clone()));
                }
            }
            FileMutation::Move { from, to } => {
                let from_rel = validate_workspace_path(from)?;
                let to_rel = validate_workspace_path(to)?;
                if is_git_path(&from_rel) || is_git_path(&to_rel) {
                    return Err(WorkspaceError::InvalidPath(PathError::Invalid));
                }
                let source = root.join(&from_rel);
                let target = root.join(&to_rel);
                if !source.exists() {
                    return Err(WorkspaceError::PathNotFound(from.clone()));
                }
                if let Some(parent) = target.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|e| WorkspaceError::Internal(anyhow!("mkdir: {e}")))?;
                }
                tokio::fs::rename(&source, &target)
                    .await
                    .map_err(|e| WorkspaceError::Internal(anyhow!("move: {e}")))?;
            }
        }

        // The filesystem mutation happens before the short revision transaction.
        // If the expected revision loses a race, bytes stay on disk but the
        // identity does not advance; callers must re-read before retrying.
        let manifest = hash_working_tree(&root)
            .await
            .map_err(WorkspaceError::Internal)?;
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
    ) -> Result<ManifestRoot, WorkspaceError> {
        let managed_dir = self.managed_dir_for(handle).await?;
        let root = self.data_root.join(&managed_dir);
        walk_manifest(&root, &self.blobs, handle.as_str())
            .await
            .map_err(WorkspaceError::Internal)
    }

    /// Path-level Diff summary of Session current tree vs Main current tree.
    pub async fn diff_summary(
        &self,
        session_id: impl Display,
    ) -> Result<DiffSummary, WorkspaceError> {
        let session_handle = WorkspaceHandle::session(&session_id);
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
        let (session_dir, main_dir) = dirs.ok_or(WorkspaceError::NotFound)?;
        diff_working_trees(
            &self.data_root.join(session_dir),
            &self.data_root.join(main_dir),
        )
        .await
        .map_err(WorkspaceError::Internal)
    }

    /// Cascade-delete a Session copy: directory tree + DB rows for that handle
    /// (workspace_copies cascades content_revisions/snapshots; links by session_id).
    /// Does **not** touch Main or Runtime.
    pub async fn delete_session_copy(
        &self,
        session_id: impl Display,
    ) -> Result<(), WorkspaceError> {
        let handle = WorkspaceHandle::session(&session_id);
        let exists: Option<String> =
            sqlx::query_scalar("SELECT handle FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;
        if exists.is_none() {
            // Idempotent: already gone is success.
            let _ = remove_session_tree(&self.data_root, &session_id);
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

        remove_session_tree(&self.data_root, &session_id).map_err(WorkspaceError::Internal)?;
        Ok(())
    }

    async fn managed_dir_for(&self, handle: &WorkspaceHandle) -> Result<String, WorkspaceError> {
        let row: Option<String> =
            sqlx::query_scalar("SELECT managed_dir FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;
        row.ok_or(WorkspaceError::NotFound)
    }

    async fn advance_revision(
        &self,
        handle: &WorkspaceHandle,
        expected: Option<&RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
        manifest_root_hash: Option<&str>,
        snapshot_purpose: Option<&str>,
    ) -> Result<RevisionRef, WorkspaceError> {
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
    ) -> Result<RevisionRef, WorkspaceError> {
        let current: Option<Option<String>> =
            sqlx::query_scalar("SELECT current_revision_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        let current = current
            .ok_or(WorkspaceError::NotFound)?
            .ok_or_else(|| WorkspaceError::Internal(anyhow!("copy has no revision")))?;
        if let Some(expected_ref) = expected
            && expected_ref.0 != current
        {
            return Err(WorkspaceError::RevisionMismatch {
                expected: expected_ref.0.clone(),
                current,
            });
        }

        let now = now_utc_str();
        let next_sequence: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM content_revisions \
             WHERE workspace_handle = ?",
        )
        .bind(handle.as_str())
        .fetch_one(&mut *tx)
        .await?;
        let revision_ref = RevisionRef::new(Uuid::now_v7());
        let copy_version = format!("v_{}", Uuid::now_v7());

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
            let snapshot_id = Uuid::now_v7();
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

async fn atomic_write(abs: &Path, content: &[u8]) -> Result<(), WorkspaceError> {
    let tmp = abs.with_extension("janus-tmp");
    tokio::fs::write(&tmp, content)
        .await
        .map_err(|e| WorkspaceError::Internal(anyhow!("write temp: {e}")))?;
    tokio::fs::rename(&tmp, abs)
        .await
        .map_err(|e| WorkspaceError::Internal(anyhow!("rename: {e}")))?;
    Ok(())
}

fn guess_mime(path: &Path) -> Option<String> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("rs") => Some("text/rust".into()),
        Some("md") => Some("text/markdown".into()),
        Some("toml") => Some("text/toml".into()),
        Some("json") => Some("application/json".into()),
        Some("png") => Some("image/png".into()),
        Some("jpg") | Some("jpeg") => Some("image/jpeg".into()),
        Some("webp") => Some("image/webp".into()),
        _ => None,
    }
}

async fn is_utf8_text_file(path: &Path) -> bool {
    use tokio::io::AsyncReadExt;

    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(_) => return false,
    };
    let mut buffer = [0_u8; 8192];
    let length = match file.read(&mut buffer).await {
        Ok(length) => length,
        Err(_) => return false,
    };
    !buffer[..length].contains(&0) && std::str::from_utf8(&buffer[..length]).is_ok()
}
