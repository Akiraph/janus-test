//! Public Workspace capability boundary.
//!
//! Owns copy lifecycle, content revision identity, manifests, diffs, controlled
//! file mutations, and the filesystem part of Apply/Sync. Session lifecycle
//! checks and public event composition remain workflow responsibilities.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt::Display,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Duration,
};

use anyhow::anyhow;
use janus_infrastructure::clock::now_utc_str;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqliteConnection, SqlitePool};
use tracing::warn;
use utoipa::ToSchema;
use uuid::Uuid;

use janus_infrastructure::managed_storage::BlobStore;

pub use super::diff::{
    DiffLineKind, DiffSummary, PropagationConflict, PropagationConflictPath, PropagationDirection,
    PropagationResult, line_hunks,
};
use super::manifest::{
    ManifestNode, ManifestRoot, NodeKind, collect_manifest as walk_manifest,
    is_workspace_internal_path,
};
pub use super::path::{PathError, validate_workspace_path};
use super::session_copy::{
    create_session_worktree, main_repo_abs, main_worktree_is_clean, propagation_base_abs,
    remove_session_tree, session_managed_dir, session_repo_abs,
};
use super::working_tree::{
    diff_working_trees, git_head, hash_working_tree, mirror_managed_tree, propagate_paths,
    read_manifest_node, rehash_working_tree_paths, seed_session_from_main,
    working_tree_fingerprint,
};

#[derive(Debug, thiserror::Error)]
pub enum PropagationError {
    #[error("workspace propagation conflict")]
    Conflict(PropagationConflict),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
}

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

type ExistingSessionCopy = (
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
);

struct CopyRoots {
    session_handle: WorkspaceHandle,
    main_handle: WorkspaceHandle,
    session_dir: PathBuf,
    main_dir: PathBuf,
}

struct PropagationStatus {
    sync_enabled: bool,
    apply_enabled: bool,
    pending_conflict: Option<PropagationConflict>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PropagationBaseline {
    root_hash: String,
    nodes: BTreeMap<String, ManifestNode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    main_head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_head: Option<String>,
}

impl PropagationBaseline {
    fn from_manifest(
        manifest: ManifestRoot,
        main_head: Option<String>,
        session_head: Option<String>,
    ) -> Self {
        Self {
            root_hash: manifest.root_hash,
            nodes: manifest.nodes,
            main_head,
            session_head,
        }
    }

    fn manifest(&self) -> ManifestRoot {
        ManifestRoot {
            root_hash: self.root_hash.clone(),
            nodes: self.nodes.clone(),
        }
    }
}

static HEAD_MANIFESTS: OnceLock<Mutex<HashMap<String, ManifestRoot>>> = OnceLock::new();

const SESSION_TREE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

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

    /// Read current revisions for several workspace copies in one query.
    /// Missing or revision-less copies are omitted, matching the optional
    /// behavior used by session summaries while a copy is being created.
    pub async fn current_revisions(
        &self,
        handles: &[WorkspaceHandle],
    ) -> Result<HashMap<String, RevisionRef>, WorkspaceError> {
        if handles.is_empty() {
            return Ok(HashMap::new());
        }

        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT handle, current_revision_id FROM workspace_copies WHERE handle IN (",
        );
        let mut separated = query.separated(", ");
        for handle in handles {
            separated.push_bind(handle.as_str());
        }
        separated.push_unseparated(")");

        let rows: Vec<(String, Option<String>)> =
            query.build_query_as().fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .filter_map(|(handle, revision)| {
                revision.map(|revision| (handle, RevisionRef(revision)))
            })
            .collect())
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
    /// revision without touching its worktree. Creates a Git worktree from
    /// Main, seeds only dirty Main paths when necessary, records the current
    /// Merkle manifest as the persisted propagation baseline, writes revision
    /// sequence=1, and initializes `propagation_links` cursors to that pair.
    pub async fn ensure_session_copy(
        &self,
        project_id: impl Display,
        session_id: impl Display,
        source_main_revision: Option<&RevisionRef>,
        actor: serde_json::Value,
    ) -> Result<SessionCopyResult, WorkspaceError> {
        let session_id = session_id.to_string();
        let handle = WorkspaceHandle::session(&session_id);
        let existing: Option<ExistingSessionCopy> = sqlx::query_as(
            "SELECT current_revision_id, \
                    (SELECT manifest_root_hash FROM content_revisions \
                     WHERE revision_id = workspace_copies.current_revision_id), \
                    managed_dir, \
                    (SELECT initial_main_revision_id FROM propagation_links \
                     WHERE session_id = workspace_copies.session_id), \
                    (SELECT baseline_manifest_json FROM propagation_links \
                     WHERE session_id = workspace_copies.session_id) \
             FROM workspace_copies WHERE handle = ?",
        )
        .bind(handle.as_str())
        .fetch_optional(&self.pool)
        .await?;

        if let Some((
            Some(revision_id),
            root,
            managed_dir,
            Some(source_main_revision),
            baseline_manifest,
        )) = existing
        {
            let session_abs = self.data_root.join(&managed_dir);
            if baseline_manifest.is_none() {
                self.ensure_propagation_base(&session_id, &session_abs)
                    .await?;
            }
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
        let root_hash = manifest.root_hash.clone();
        let baseline =
            PropagationBaseline::from_manifest(manifest, Some(head.clone()), Some(head.clone()));
        let baseline_json = serde_json::to_string(&baseline)?;
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
              version, created_at, updated_at, baseline_manifest_json) \
             VALUES (?, ?, 'main', ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(project_id.to_string())
        .bind(session_id.to_string())
        .bind(&main_revision_id)
        .bind(&main_revision_id)
        .bind(revision_ref.0.clone())
        .bind(&link_version)
        .bind(&now)
        .bind(&now)
        .bind(baseline_json)
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
        // Reuse the persisted propagation baseline and rehash only Git-changed
        // paths for Session writes. If the expected revision loses a race,
        // bytes stay on disk but the identity does not advance.
        let manifest = if let Some(session_id) = handle.as_str().strip_prefix("session:") {
            let baseline = self.ensure_propagation_base(session_id, &root).await?;
            let previous = baseline.manifest();
            refresh_manifest(&root, &previous, baseline.session_head.as_deref())
                .await?
                .0
        } else {
            hash_working_tree(&root)
                .await
                .map_err(WorkspaceError::Internal)?
        };
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
        let session_id = session_id.to_string();
        let roots = self.copy_roots(&session_id).await?;
        let baseline = self
            .ensure_propagation_base(&session_id, &roots.session_dir)
            .await?;
        let mut summary = diff_working_trees(&roots.session_dir, &roots.main_dir)
            .await
            .map_err(WorkspaceError::Internal)?;
        let diff_paths = summary
            .paths
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>();
        let status = self
            .propagation_status(&session_id, &roots, &baseline.manifest(), &diff_paths)
            .await?;
        summary.sync_enabled = status.sync_enabled;
        summary.apply_enabled = status.apply_enabled;
        summary.pending_conflict = status.pending_conflict;
        Ok(summary)
    }

    /// Propagate one workspace side into the other without creating a Git
    /// commit. A three-way preflight uses the last synchronized filesystem
    /// snapshot so unrelated changes are copied while same-path edits surface
    /// as one structured conflict.
    pub async fn propagate(
        &self,
        session_id: impl Display,
        direction: PropagationDirection,
        actor: serde_json::Value,
    ) -> Result<PropagationResult, PropagationError> {
        let session_id = session_id.to_string();
        let roots = self.copy_roots(&session_id).await?;
        let baseline = self
            .ensure_propagation_base(&session_id, &roots.session_dir)
            .await?;
        let previous_manifest = baseline.manifest();
        let (main_result, session_result) = tokio::join!(
            refresh_manifest(
                &roots.main_dir,
                &previous_manifest,
                baseline.main_head.as_deref(),
            ),
            refresh_manifest(
                &roots.session_dir,
                &previous_manifest,
                baseline.session_head.as_deref(),
            ),
        );
        let (main, main_head) = main_result?;
        let (session, session_head) = session_result?;
        let pending = self.pending_conflict(&session_id).await?;

        let mut paths: BTreeSet<String> = previous_manifest
            .nodes
            .keys()
            .chain(main.nodes.keys())
            .chain(session.nodes.keys())
            .filter(|path| {
                !is_workspace_internal_path(path)
                    && baseline
                        .nodes
                        .get(*path)
                        .or_else(|| main.nodes.get(*path))
                        .or_else(|| session.nodes.get(*path))
                        .is_some_and(|node| node.kind == NodeKind::File)
            })
            .cloned()
            .collect();
        if let Some(conflict) = &pending {
            paths.extend(
                conflict
                    .paths
                    .iter()
                    .filter(|path| !is_workspace_internal_path(&path.path))
                    .map(|path| path.path.clone()),
            );
        }

        let pending_paths: BTreeMap<String, &PropagationConflictPath> = pending
            .as_ref()
            .map(|conflict| {
                conflict
                    .paths
                    .iter()
                    .filter(|path| !is_workspace_internal_path(&path.path))
                    .map(|path| (path.path.clone(), path))
                    .collect()
            })
            .unwrap_or_default();
        let mut transfer_paths = BTreeSet::new();
        let mut conflict_paths = Vec::new();

        for path in paths {
            validate_workspace_path(&path).map_err(WorkspaceError::InvalidPath)?;
            let base = previous_manifest.nodes.get(&path);
            let main_node = main.nodes.get(&path);
            let session_node = session.nodes.get(&path);
            let main_changed = !same_node(main_node, base);
            let session_changed = !same_node(session_node, base);
            let sides_match = same_node(main_node, session_node);

            if direction == PropagationDirection::Apply
                && pending_paths.contains_key(&path)
                && pending_path_resolved(pending_paths[&path], main_node, session_node)
            {
                transfer_paths.insert(path.clone());
                continue;
            }

            match direction {
                PropagationDirection::Sync if main_changed => {
                    if session_changed && !sides_match {
                        conflict_paths.push(conflict_path(&path, base, main_node, session_node));
                    } else {
                        if !sides_match {
                            transfer_paths.insert(path);
                        }
                    }
                }
                PropagationDirection::Apply if session_changed => {
                    if main_changed && !sides_match {
                        conflict_paths.push(conflict_path(&path, base, main_node, session_node));
                    } else {
                        if !sides_match {
                            transfer_paths.insert(path);
                        }
                    }
                }
                _ => {}
            }
        }

        if !conflict_paths.is_empty() {
            let conflict = PropagationConflict {
                direction,
                paths: conflict_paths,
            };
            self.store_pending_conflict(&session_id, &roots, &conflict)
                .await?;
            return Err(PropagationError::Conflict(conflict));
        }

        if !transfer_paths.is_empty() {
            let source = match direction {
                PropagationDirection::Sync => roots.main_dir.clone(),
                PropagationDirection::Apply => roots.session_dir.clone(),
            };
            let target = match direction {
                PropagationDirection::Sync => roots.session_dir.clone(),
                PropagationDirection::Apply => roots.main_dir.clone(),
            };
            let transfer_path_list = transfer_paths.iter().cloned().collect::<Vec<_>>();
            tokio::task::spawn_blocking(move || {
                propagate_paths(&source, &target, &transfer_path_list)
            })
            .await
            .map_err(|error| WorkspaceError::Internal(anyhow!(error.to_string())))?
            .map_err(WorkspaceError::Internal)?;
        }

        let (session_after, main_after) = if transfer_paths.is_empty() {
            (session, main)
        } else {
            let transfer_path_list = transfer_paths.iter().cloned().collect::<Vec<_>>();
            match direction {
                PropagationDirection::Sync => (
                    rehash_working_tree_paths(&roots.session_dir, &session, &transfer_path_list)
                        .await
                        .map_err(WorkspaceError::Internal)?,
                    main,
                ),
                PropagationDirection::Apply => (
                    session,
                    rehash_working_tree_paths(&roots.main_dir, &main, &transfer_path_list)
                        .await
                        .map_err(WorkspaceError::Internal)?,
                ),
            }
        };
        let next_manifest =
            merge_propagation_baseline(&previous_manifest, &main_after, &session_after);
        let next_baseline =
            PropagationBaseline::from_manifest(next_manifest, Some(main_head), Some(session_head));
        self.store_propagation_baseline(&session_id, &next_baseline)
            .await?;

        let (session_revision, main_revision) = if transfer_paths.is_empty() {
            (
                self.current_revision(&roots.session_handle).await?,
                self.current_revision(&roots.main_handle).await?,
            )
        } else {
            let session_revision = self
                .record_manifest_revision(
                    &roots.session_handle,
                    &session_after.root_hash,
                    "workspace.propagation",
                    actor.clone(),
                )
                .await?;
            let main_revision = self
                .record_manifest_revision(
                    &roots.main_handle,
                    &main_after.root_hash,
                    "workspace.propagation",
                    actor,
                )
                .await?;
            (session_revision, main_revision)
        };

        self.update_propagation_cursor(&session_id, direction, &session_revision, &main_revision)
            .await?;
        self.clear_pending_conflict(&session_id).await?;

        Ok(PropagationResult {
            direction,
            changed_paths: transfer_paths.into_iter().collect(),
            session_revision: session_revision.0,
            main_revision: main_revision.0,
        })
    }

    async fn copy_roots(&self, session_id: &str) -> Result<CopyRoots, WorkspaceError> {
        let session_handle = WorkspaceHandle::session(session_id);
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT session.managed_dir, main.managed_dir, session.project_id \
             FROM workspace_copies AS session \
             JOIN workspace_copies AS main \
               ON main.project_id = session.project_id AND main.kind = 'main' \
             WHERE session.handle = ?",
        )
        .bind(session_handle.as_str())
        .fetch_optional(&self.pool)
        .await?;
        let (session_dir, main_dir, project_id) = row.ok_or(WorkspaceError::NotFound)?;
        Ok(CopyRoots {
            session_handle,
            main_handle: WorkspaceHandle::main(project_id),
            session_dir: self.data_root.join(session_dir),
            main_dir: self.data_root.join(main_dir),
        })
    }

    async fn ensure_propagation_base(
        &self,
        session_id: &str,
        session_dir: &Path,
    ) -> Result<PropagationBaseline, WorkspaceError> {
        let stored: Option<Option<String>> = sqlx::query_scalar(
            "SELECT baseline_manifest_json FROM propagation_links WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(stored) = stored else {
            return Err(WorkspaceError::NotFound);
        };
        if let Some(json) = stored {
            return Ok(serde_json::from_str(&json)?);
        }

        let baseline_dir = propagation_base_abs(&self.data_root, session_id);
        if !tokio::fs::try_exists(&baseline_dir)
            .await
            .map_err(|error| WorkspaceError::Internal(error.into()))?
        {
            let source = session_dir.to_path_buf();
            tokio::task::spawn_blocking(move || mirror_managed_tree(&source, &baseline_dir))
                .await
                .map_err(|error| WorkspaceError::Internal(anyhow!(error.to_string())))?
                .map_err(WorkspaceError::Internal)?;
        }
        let manifest = walk_manifest(
            &propagation_base_abs(&self.data_root, session_id),
            &self.blobs,
            &format!("workspace:propagation-base:{session_id}"),
        )
        .await
        .map_err(WorkspaceError::Internal)?;
        let baseline = PropagationBaseline::from_manifest(manifest, None, None);
        self.store_propagation_baseline(session_id, &baseline)
            .await?;
        Ok(baseline)
    }

    async fn store_propagation_baseline(
        &self,
        session_id: &str,
        baseline: &PropagationBaseline,
    ) -> Result<(), WorkspaceError> {
        let now = now_utc_str();
        let version = format!("v_{}", Uuid::now_v7());
        let json = serde_json::to_string(baseline)?;
        let result = sqlx::query(
            "UPDATE propagation_links SET baseline_manifest_json = ?, version = ?, updated_at = ? WHERE session_id = ?",
        )
        .bind(json)
        .bind(version)
        .bind(now)
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(WorkspaceError::NotFound);
        }
        Ok(())
    }

    async fn propagation_status(
        &self,
        session_id: &str,
        roots: &CopyRoots,
        baseline: &ManifestRoot,
        diff_paths: &[&str],
    ) -> Result<PropagationStatus, WorkspaceError> {
        let mut sync_enabled = false;
        let mut apply_enabled = false;
        for path in diff_paths {
            let base = baseline.nodes.get(*path);
            let main_node = read_manifest_node(&roots.main_dir, path)
                .await
                .map_err(WorkspaceError::Internal)?;
            let session_node = read_manifest_node(&roots.session_dir, path)
                .await
                .map_err(WorkspaceError::Internal)?;
            if same_node(main_node.as_ref(), session_node.as_ref()) {
                continue;
            }
            if !same_node(main_node.as_ref(), base) {
                sync_enabled = true;
            }
            if !same_node(session_node.as_ref(), base) {
                apply_enabled = true;
            }
        }
        Ok(PropagationStatus {
            sync_enabled,
            apply_enabled,
            pending_conflict: self.pending_conflict(session_id).await?,
        })
    }

    async fn pending_conflict(
        &self,
        session_id: &str,
    ) -> Result<Option<PropagationConflict>, WorkspaceError> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT direction, paths_json FROM workspace_propagation_conflicts WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((direction, paths_json)) = row else {
            return Ok(None);
        };
        let direction = match direction.as_str() {
            "sync" => PropagationDirection::Sync,
            "apply" => PropagationDirection::Apply,
            other => {
                return Err(WorkspaceError::Internal(anyhow!(
                    "unknown propagation direction {other}"
                )));
            }
        };
        let paths = serde_json::from_str(&paths_json)?;
        Ok(Some(PropagationConflict { direction, paths }))
    }

    async fn store_pending_conflict(
        &self,
        session_id: &str,
        roots: &CopyRoots,
        conflict: &PropagationConflict,
    ) -> Result<(), WorkspaceError> {
        let session_revision = self.current_revision(&roots.session_handle).await?;
        let main_revision = self.current_revision(&roots.main_handle).await?;
        let now = now_utc_str();
        sqlx::query(
            "INSERT INTO workspace_propagation_conflicts \
             (session_id, direction, session_revision_id, main_revision_id, paths_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(session_id) DO UPDATE SET direction = excluded.direction, \
             session_revision_id = excluded.session_revision_id, main_revision_id = excluded.main_revision_id, \
             paths_json = excluded.paths_json, updated_at = excluded.updated_at",
        )
        .bind(session_id)
        .bind(conflict.direction.as_str())
        .bind(session_revision.0)
        .bind(main_revision.0)
        .bind(serde_json::to_string(&conflict.paths)?)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn clear_pending_conflict(&self, session_id: &str) -> Result<(), WorkspaceError> {
        sqlx::query("DELETE FROM workspace_propagation_conflicts WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn update_propagation_cursor(
        &self,
        session_id: &str,
        direction: PropagationDirection,
        session_revision: &RevisionRef,
        main_revision: &RevisionRef,
    ) -> Result<(), WorkspaceError> {
        let now = now_utc_str();
        let version = format!("v_{}", Uuid::now_v7());
        let result = match direction {
            PropagationDirection::Sync => {
                sqlx::query(
                    "UPDATE propagation_links SET main_to_session_cursor_revision_id = ?, version = ?, updated_at = ? WHERE session_id = ?",
                )
                .bind(&main_revision.0)
                .bind(&version)
                .bind(&now)
                .bind(session_id)
                .execute(&self.pool)
                .await?
            }
            PropagationDirection::Apply => {
                sqlx::query(
                    "UPDATE propagation_links SET session_to_main_cursor_revision_id = ?, version = ?, updated_at = ? WHERE session_id = ?",
                )
                .bind(&session_revision.0)
                .bind(&version)
                .bind(&now)
                .bind(session_id)
                .execute(&self.pool)
                .await?
            }
        };
        if result.rows_affected() == 0 {
            return Err(WorkspaceError::NotFound);
        }
        Ok(())
    }

    async fn record_manifest_revision(
        &self,
        handle: &WorkspaceHandle,
        manifest_root_hash: &str,
        cause: &str,
        actor: serde_json::Value,
    ) -> Result<RevisionRef, WorkspaceError> {
        self.advance_revision(
            handle,
            None,
            cause,
            actor,
            Some(manifest_root_hash),
            Some("external_scan"),
        )
        .await
    }

    /// Cascade-delete a Session copy: directory tree + DB rows for that handle
    /// (workspace_copies cascades content_revisions/snapshots; links by session_id).
    /// Does **not** touch Main or Runtime.
    pub async fn delete_session_copy(
        &self,
        session_id: impl Display,
    ) -> Result<(), WorkspaceError> {
        let session_id = session_id.to_string();
        let handle = WorkspaceHandle::session(&session_id);
        let exists: Option<String> =
            sqlx::query_scalar("SELECT handle FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;
        if exists.is_none() {
            // Idempotent: already gone is success.
            self.cleanup_session_tree(&session_id).await;
            return Ok(());
        }

        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM workspace_propagation_conflicts WHERE session_id = ?")
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?;
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

        // The durable deletion is complete once the metadata transaction is
        // committed. Files may contain tool-created trees or open handles, so
        // keep cleanup bounded and prevent one bad worktree from stalling the
        // session lifecycle queue.
        self.cleanup_session_tree(&session_id).await;
        Ok(())
    }

    async fn cleanup_session_tree(&self, session_id: &str) {
        let data_root = self.data_root.clone();
        let session_id = session_id.to_owned();
        let cleanup_session_id = session_id.clone();
        let cleanup = tokio::task::spawn_blocking(move || {
            remove_session_tree(&data_root, &cleanup_session_id)
        });
        match tokio::time::timeout(SESSION_TREE_CLEANUP_TIMEOUT, cleanup).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => {
                warn!(%error, %session_id, "session worktree cleanup failed after metadata deletion");
            }
            Ok(Err(error)) => {
                warn!(%error, %session_id, "session worktree cleanup task failed");
            }
            Err(_) => {
                warn!(%session_id, "session worktree cleanup exceeded its timeout; leaving it detached");
            }
        }
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

async fn refresh_manifest(
    root: &Path,
    previous: &ManifestRoot,
    previous_head: Option<&str>,
) -> Result<(ManifestRoot, String), WorkspaceError> {
    let previous_files = previous
        .nodes
        .iter()
        .filter(|(_, node)| node.kind == NodeKind::File)
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let root_for_scan = root.to_path_buf();
    let fingerprint = tokio::task::spawn_blocking(move || {
        working_tree_fingerprint(&root_for_scan, &previous_files)
    })
    .await
    .map_err(|error| WorkspaceError::Internal(anyhow!(error.to_string())))?
    .map_err(WorkspaceError::Internal)?;

    let manifest = if previous_head == Some(fingerprint.head.as_str()) {
        if fingerprint.changed_paths.is_empty() {
            previous.clone()
        } else {
            rehash_working_tree_paths(root, previous, &fingerprint.changed_paths)
                .await
                .map_err(WorkspaceError::Internal)?
        }
    } else {
        hash_working_tree(root)
            .await
            .map_err(WorkspaceError::Internal)?
    };
    Ok((manifest, fingerprint.head))
}

fn same_node(left: Option<&ManifestNode>, right: Option<&ManifestNode>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.kind == right.kind && left.node_hash == right.node_hash,
        (None, None) => true,
        _ => false,
    }
}

/// Advance only the parts of the baseline where both copies now agree. A
/// propagation can legitimately leave unrelated edits on the opposite side;
/// those paths must continue comparing against the old baseline so the next
/// Apply or Sync still detects them.
fn merge_propagation_baseline(
    previous: &ManifestRoot,
    main: &ManifestRoot,
    session: &ManifestRoot,
) -> ManifestRoot {
    let mut paths = BTreeSet::new();
    paths.extend(previous.nodes.keys().cloned());
    paths.extend(main.nodes.keys().cloned());
    paths.extend(session.nodes.keys().cloned());

    let mut nodes = previous.nodes.clone();
    for path in paths {
        let main_node = main.nodes.get(&path);
        let session_node = session.nodes.get(&path);
        if !same_node(main_node, session_node) {
            continue;
        }
        match main_node {
            Some(node) => {
                nodes.insert(path, node.clone());
            }
            None => {
                nodes.remove(&path);
            }
        }
    }

    ManifestRoot {
        root_hash: if main.root_hash == session.root_hash {
            main.root_hash.clone()
        } else {
            previous.root_hash.clone()
        },
        nodes,
    }
}

fn node_hash(node: Option<&ManifestNode>) -> Option<String> {
    node.map(|node| node.node_hash.clone())
}

fn conflict_path(
    path: &str,
    base: Option<&ManifestNode>,
    main: Option<&ManifestNode>,
    session: Option<&ManifestNode>,
) -> PropagationConflictPath {
    let kind = match (base, main, session) {
        (Some(_), None, Some(_)) => "deleted_in_main",
        (Some(_), Some(_), None) => "deleted_in_session",
        (None, Some(_), Some(_)) => "added_both",
        _ => "modified",
    };
    PropagationConflictPath {
        path: path.to_owned(),
        kind: kind.to_owned(),
        base_hash: node_hash(base),
        main_hash: node_hash(main),
        session_hash: node_hash(session),
    }
}

fn pending_path_resolved(
    pending: &PropagationConflictPath,
    main: Option<&ManifestNode>,
    session: Option<&ManifestNode>,
) -> bool {
    node_hash(main).as_deref() == pending.main_hash.as_deref()
        && node_hash(session).as_deref() != pending.session_hash.as_deref()
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
