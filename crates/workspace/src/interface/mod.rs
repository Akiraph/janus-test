//! Public Workspace capability boundary.
//!
//! Owns copy lifecycle, content revision identity, manifests, diffs, controlled
//! file mutations, and the filesystem part of Apply/Sync. Session lifecycle
//! checks and public event composition remain workflow responsibilities.
//!
//! The `impl WorkspaceInterface` surface is split by domain into the private
//! submodules `diff` (propagation and conflicts), `manifest` (revisions and
//! manifests), `session_copy` (copy lifecycle), and `working_tree` (file ops
//! and the mutation pipeline). The shared helpers below belong to the facade.

mod diff;
mod manifest;
mod session_copy;
mod working_tree;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt::Display,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};

use anyhow::anyhow;
use janus_infrastructure::clock::now_utc_str;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqliteConnection, SqlitePool};
use tracing::warn;
use utoipa::ToSchema;
use uuid::Uuid;

use janus_infrastructure::events::EventType;
use janus_infrastructure::managed_storage::BlobStore;

pub use super::diff::{
    DiffLineKind, DiffSummary, PropagationConflict, PropagationConflictPath, PropagationDirection,
    PropagationResult, line_hunks,
};
use super::manifest::{
    ManifestNode, ManifestRoot, NodeKind, collect_manifest as walk_manifest, hash_file_node,
    is_text_bytes, is_workspace_internal_path,
};
pub use super::path::{PathError, validate_workspace_path};
use super::session_copy::{
    create_session_worktree, main_repo_abs, main_worktree_is_clean, remove_session_tree,
    session_managed_dir, session_repo_abs,
};
use super::working_tree::{
    diff_working_trees, git_head, hash_working_tree, propagate_paths, read_manifest_node,
    rehash_working_tree_paths, seed_session_from_main, working_tree_fingerprint,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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

type StoredFileMutationIntentRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
);

struct CopyRoots {
    session_handle: WorkspaceHandle,
    main_handle: WorkspaceHandle,
    project_id: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PropagationIntent {
    direction: PropagationDirection,
    actor: serde_json::Value,
    baseline: PropagationBaseline,
    main_head: String,
    session_head: String,
    paths: Vec<String>,
    #[serde(default)]
    source_preimage: BTreeMap<String, Option<ManifestNode>>,
    #[serde(default)]
    target_preimage: BTreeMap<String, Option<ManifestNode>>,
}

struct PropagationFinalizeRequest<'a> {
    session_id: &'a str,
    roots: &'a CopyRoots,
    direction: PropagationDirection,
    next_baseline: &'a PropagationBaseline,
    session_after: &'a ManifestRoot,
    main_after: &'a ManifestRoot,
    actor: &'a serde_json::Value,
    transfer_paths: &'a [String],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMutationEventContext {
    pub event_type: EventType,
    pub actor: serde_json::Value,
    pub resource: serde_json::Value,
    pub correlation_id: String,
    pub causation_id: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredFileMutationIntent {
    id: String,
    handle: WorkspaceHandle,
    project_id: String,
    mutation: FileMutation,
    expected_revision: Option<RevisionRef>,
    cause: String,
    actor: serde_json::Value,
    pre_manifest: ManifestRoot,
    event: Option<FileMutationEventContext>,
}

#[derive(Debug, Clone)]
pub struct PreparedFileMutation {
    intent: StoredFileMutationIntent,
}

#[derive(Debug, Clone)]
pub struct AppliedFileMutation {
    pub intent_id: String,
    pub manifest_root_hash: String,
}

#[derive(Debug, Clone)]
pub struct RecoveredFileMutation {
    pub intent_id: String,
    pub revision: RevisionRef,
    pub event: FileMutationEventContext,
}

impl PreparedFileMutation {
    pub fn intent_id(&self) -> &str {
        &self.intent.id
    }
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
static WORKSPACE_LOCKS: OnceLock<Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

/// Exclusive in-process ownership of one project's Main and Session copies.
///
/// The durable revision precondition remains authoritative across processes;
/// this guard closes the local race between filesystem transfer and its
/// revision transaction.
pub struct WorkspaceMutationGuard {
    project_id: String,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

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

fn workspace_lock(data_root: &Path, project_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    let key = format!("{}:{project_id}", data_root.to_string_lossy());
    let locks = WORKSPACE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

/// Agent / tool file mutation against a Session (or, later, any) copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

pub struct FileMutationRequest<'a> {
    pub handle: &'a WorkspaceHandle,
    pub mutation: FileMutation,
    pub expected: Option<&'a RevisionRef>,
    pub cause: &'a str,
    pub actor: serde_json::Value,
    pub event: Option<FileMutationEventContext>,
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

    /// Acquire the exclusive project lock used by filesystem mutations and
    /// propagation. Callers that also write a related projection can hold it
    /// across their Unit of Work transaction.
    pub async fn acquire_mutation_lock(
        &self,
        handle: &WorkspaceHandle,
    ) -> Result<WorkspaceMutationGuard, WorkspaceError> {
        let project_id: Option<String> =
            sqlx::query_scalar("SELECT project_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;
        let project_id = project_id.ok_or(WorkspaceError::NotFound)?;
        Ok(self.lock_project(&project_id).await)
    }

    pub async fn acquire_project_mutation_lock(
        &self,
        project_id: impl Display,
    ) -> Result<WorkspaceMutationGuard, WorkspaceError> {
        Ok(self.lock_project(&project_id.to_string()).await)
    }

    async fn lock_project(&self, project_id: &str) -> WorkspaceMutationGuard {
        let lock = workspace_lock(&self.data_root, project_id);
        WorkspaceMutationGuard {
            project_id: project_id.to_owned(),
            _guard: lock.lock_owned().await,
        }
    }

}

async fn validate_file_mutation(
    root: &Path,
    mutation: &FileMutation,
) -> Result<(), WorkspaceError> {
    match mutation {
        FileMutation::Write { path, .. } => {
            let rel = validate_workspace_path(path)?;
            reject_git_path(&rel)?;
            if tokio::fs::try_exists(root.join(&rel))
                .await
                .map_err(|error| WorkspaceError::Internal(error.into()))?
                && tokio::fs::metadata(root.join(&rel))
                    .await
                    .map_err(|error| WorkspaceError::Internal(error.into()))?
                    .is_dir()
            {
                return Err(WorkspaceError::NotEditable(path.clone()));
            }
        }
        FileMutation::Patch { path, .. } => {
            let rel = validate_workspace_path(path)?;
            reject_git_path(&rel)?;
            if !tokio::fs::metadata(root.join(&rel))
                .await
                .map_err(|_| WorkspaceError::PathNotFound(path.clone()))?
                .is_file()
            {
                return Err(WorkspaceError::PathNotFound(path.clone()));
            }
        }
        FileMutation::Delete { path } => {
            let rel = validate_workspace_path(path)?;
            reject_git_path(&rel)?;
            if !tokio::fs::try_exists(root.join(&rel))
                .await
                .map_err(|error| WorkspaceError::Internal(error.into()))?
            {
                return Err(WorkspaceError::PathNotFound(path.clone()));
            }
        }
        FileMutation::DeleteTree { path } => {
            let rel = validate_workspace_path(path)?;
            reject_git_path(&rel)?;
            if !tokio::fs::try_exists(root.join(&rel))
                .await
                .map_err(|error| WorkspaceError::Internal(error.into()))?
            {
                return Err(WorkspaceError::PathNotFound(path.clone()));
            }
        }
        FileMutation::Move { from, to } => {
            let from_rel = validate_workspace_path(from)?;
            let to_rel = validate_workspace_path(to)?;
            reject_git_path(&from_rel)?;
            reject_git_path(&to_rel)?;
            if !tokio::fs::try_exists(root.join(&from_rel))
                .await
                .map_err(|error| WorkspaceError::Internal(error.into()))?
            {
                return Err(WorkspaceError::PathNotFound(from.clone()));
            }
        }
    }
    Ok(())
}

fn reject_git_path(path: &Path) -> Result<(), WorkspaceError> {
    if is_git_path(path) {
        return Err(WorkspaceError::InvalidPath(PathError::Invalid));
    }
    Ok(())
}

async fn apply_file_mutation_fs(
    root: &Path,
    mutation: &FileMutation,
) -> Result<(), WorkspaceError> {
    match mutation {
        FileMutation::Write { path, content } | FileMutation::Patch { path, content } => {
            let rel = validate_workspace_path(path)?;
            reject_git_path(&rel)?;
            let abs = root.join(&rel);
            if matches!(mutation, FileMutation::Patch { .. }) && !abs.is_file() {
                return Err(WorkspaceError::PathNotFound(path.clone()));
            }
            if let Some(parent) = abs.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|error| WorkspaceError::Internal(anyhow!("mkdir: {error}")))?;
            }
            atomic_write(&abs, content).await?;
        }
        FileMutation::Delete { path } => {
            let rel = validate_workspace_path(path)?;
            reject_git_path(&rel)?;
            let abs = root.join(&rel);
            if abs.is_file() {
                tokio::fs::remove_file(&abs)
                    .await
                    .map_err(|error| WorkspaceError::Internal(anyhow!("remove file: {error}")))?;
            } else if abs.is_dir() {
                tokio::fs::remove_dir(&abs)
                    .await
                    .map_err(|error| WorkspaceError::Internal(anyhow!("remove dir: {error}")))?;
            } else {
                return Err(WorkspaceError::PathNotFound(path.clone()));
            }
        }
        FileMutation::DeleteTree { path } => {
            let rel = validate_workspace_path(path)?;
            reject_git_path(&rel)?;
            let abs = root.join(&rel);
            if abs.is_dir() {
                tokio::fs::remove_dir_all(&abs)
                    .await
                    .map_err(|error| WorkspaceError::Internal(anyhow!("remove tree: {error}")))?;
            } else if abs.is_file() {
                tokio::fs::remove_file(&abs)
                    .await
                    .map_err(|error| WorkspaceError::Internal(anyhow!("remove file: {error}")))?;
            } else {
                return Err(WorkspaceError::PathNotFound(path.clone()));
            }
        }
        FileMutation::Move { from, to } => {
            let from_rel = validate_workspace_path(from)?;
            let to_rel = validate_workspace_path(to)?;
            reject_git_path(&from_rel)?;
            reject_git_path(&to_rel)?;
            let source = root.join(&from_rel);
            let target = root.join(&to_rel);
            if !source.exists() {
                return Err(WorkspaceError::PathNotFound(from.clone()));
            }
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|error| WorkspaceError::Internal(anyhow!("mkdir: {error}")))?;
            }
            tokio::fs::rename(&source, &target)
                .await
                .map_err(|error| WorkspaceError::Internal(anyhow!("move: {error}")))?;
        }
    }
    Ok(())
}

fn normalize_mutation_path(path: &str) -> Result<String, WorkspaceError> {
    Ok(validate_workspace_path(path)?
        .to_string_lossy()
        .replace('\\', "/"))
}

fn path_matches(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn mutation_scope(pre: &ManifestRoot, mutation: &FileMutation) -> BTreeSet<String> {
    let mut scope = BTreeSet::new();
    let mut add = |prefix: &str| {
        scope.insert(prefix.to_owned());
        scope.extend(
            pre.nodes
                .keys()
                .filter(|path| path_matches(path, prefix))
                .cloned(),
        );
    };
    match mutation {
        FileMutation::Write { path, .. }
        | FileMutation::Patch { path, .. }
        | FileMutation::Delete { path }
        | FileMutation::DeleteTree { path } => {
            if let Ok(path) = normalize_mutation_path(path) {
                add(&path);
            }
        }
        FileMutation::Move { from, to } => {
            if let Ok(from) = normalize_mutation_path(from) {
                add(&from);
            }
            if let Ok(to) = normalize_mutation_path(to) {
                add(&to);
            }
        }
    }
    scope
}

fn expected_post_manifest(
    pre: &ManifestRoot,
    mutation: &FileMutation,
) -> Result<ManifestRoot, WorkspaceError> {
    let mut nodes = pre.nodes.clone();
    match mutation {
        FileMutation::Write { path, content } | FileMutation::Patch { path, content } => {
            let path = normalize_mutation_path(path)?;
            nodes.retain(|candidate, _| !path_matches(candidate, &path));
            let mode = pre
                .nodes
                .get(&path)
                .map(|node| node.mode)
                .unwrap_or(0o100644);
            let is_text = is_text_bytes(content);
            nodes.insert(
                path,
                ManifestNode {
                    kind: NodeKind::File,
                    mode,
                    byte_len: content.len() as u64,
                    blob_sha: None,
                    node_hash: hash_file_node(mode, content, is_text),
                    is_text,
                },
            );
        }
        FileMutation::Delete { path } | FileMutation::DeleteTree { path } => {
            let path = normalize_mutation_path(path)?;
            nodes.retain(|candidate, _| !path_matches(candidate, &path));
        }
        FileMutation::Move { from, to } => {
            let from = normalize_mutation_path(from)?;
            let to = normalize_mutation_path(to)?;
            let moved: Vec<(String, ManifestNode)> = pre
                .nodes
                .iter()
                .filter(|(path, _)| path_matches(path, &from))
                .map(|(path, node)| {
                    let suffix = path.strip_prefix(&from).unwrap_or_default();
                    let suffix = suffix.strip_prefix('/').unwrap_or(suffix);
                    let target = if suffix.is_empty() {
                        to.clone()
                    } else {
                        format!("{to}/{suffix}")
                    };
                    (target, node.clone())
                })
                .collect();
            nodes.retain(|candidate, _| {
                !path_matches(candidate, &from) && !path_matches(candidate, &to)
            });
            nodes.extend(moved);
        }
    }
    Ok(ManifestRoot {
        root_hash: String::new(),
        nodes,
    })
}

fn manifests_match_scope(
    current: &ManifestRoot,
    expected: &ManifestRoot,
    scope: &BTreeSet<String>,
) -> bool {
    let project = |manifest: &ManifestRoot| {
        manifest
            .nodes
            .iter()
            .filter(|(path, _)| scope.iter().any(|prefix| path_matches(path, prefix)))
            .map(|(path, node)| {
                (
                    path.clone(),
                    (
                        node.kind.clone(),
                        node.mode,
                        node.byte_len,
                        node.node_hash.clone(),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    project(current) == project(expected)
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
