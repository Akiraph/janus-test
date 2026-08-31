//! Public Source Control capability.
//!
//! Owns the Git port (`GitRunner`) and the capability that drives it: status /
//! diff / log / branches / remotes, fetch / stage / unstage / commit / push,
//! the three-way update with persistent conflicts, and the
//! `project_git_state` / `git_update_conflicts` / `git_update_conflict_paths`
//! tables. Repo config and GitHub credentials live in `projects`; this
//! capability reads them read-only to resolve a short-lived credential and
//! never writes project metadata or states.

use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use futures_util::TryStreamExt;
use janus_infrastructure::{
    clock::now_utc_str,
    events::{EventStore, EventType, NewEvent},
    id::{CorrelationId, GitUpdateConflictId, ProjectId},
    operations::{
        CreateOperation, IdempotencyRequest, OperationInterface, OperationStatus, OperationView,
    },
    secrets::SecretCipher,
    unit_of_work::UnitOfWork,
};
use janus_workspace::interface::{WorkspaceError, WorkspaceHandle, WorkspaceInterface};
use mongodb::bson::{Bson, Document, doc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

/// Boxed future used by the object-safe Git port.
pub type GitFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Normalized Git failure mapped by the transport to stable `GIT_*` codes.
#[derive(Debug, Clone, Error, Serialize, Deserialize)]
pub enum GitError {
    #[error("git authentication failed")]
    AuthFailed,
    #[error("git remote is unreachable")]
    RemoteUnavailable,
    #[error("git non-fast-forward")]
    NonFastForward,
    #[error("git index is not empty")]
    IndexNotEmpty,
    #[error("git histories diverged")]
    Diverged,
    #[error("git checkout would overwrite local changes")]
    CheckoutConflict,
    #[error("git update produced a three-way content conflict: {paths:?}")]
    UpdateConflict { paths: Vec<String> },
    /// The named remote is absent from the repository configuration.
    #[error("the git remote is not configured for this project")]
    RemoteNotFound,
    /// The remote answered, but the repository itself does not exist there.
    #[error("the remote repository does not exist or is not visible to this credential")]
    RepositoryNotFound,
    /// The requested branch / ref does not exist locally or on the remote.
    #[error("the requested git branch does not exist")]
    RefNotFound,
    #[error("there is nothing staged to commit")]
    NothingToCommit,
    #[error("git has no author identity configured (user.name and user.email)")]
    IdentityUnset,
    #[error("another git process is holding the repository lock; retry in a moment")]
    RepositoryLocked,
    #[error("git process failed: {0}")]
    CommandFailed(String),
    #[error("git output was not valid UTF-8 or unexpected: {0}")]
    BadOutput(String),
}

impl GitError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::AuthFailed => "GIT_AUTH_FAILED",
            Self::RemoteUnavailable => "GIT_REMOTE_UNAVAILABLE",
            Self::NonFastForward => "GIT_NON_FAST_FORWARD",
            Self::IndexNotEmpty => "GIT_INDEX_NOT_EMPTY",
            Self::Diverged => "GIT_DIVERGED",
            Self::CheckoutConflict => "GIT_CHECKOUT_CONFLICT",
            Self::UpdateConflict { .. } => "GIT_UPDATE_CONFLICT",
            Self::RemoteNotFound => "GIT_REMOTE_NOT_FOUND",
            Self::RepositoryNotFound => "GIT_REPOSITORY_NOT_FOUND",
            Self::RefNotFound => "GIT_REF_NOT_FOUND",
            Self::NothingToCommit => "GIT_NOTHING_TO_COMMIT",
            Self::IdentityUnset => "GIT_IDENTITY_UNSET",
            Self::RepositoryLocked => "GIT_REPOSITORY_LOCKED",
            Self::CommandFailed(_) | Self::BadOutput(_) => "INTERNAL_ERROR",
        }
    }
}

/// A credential passed to clone/fetch/push. Implementations must keep the
/// password out of Git configuration and process arguments.
#[derive(Debug, Clone)]
pub enum GitCredential {
    None,
    /// `(username, password)` for HTTPS basic auth. The password is the PAT.
    HttpsBasic {
        username: String,
        password: String,
    },
}

/// Three-layer status projection (`git/status`).
#[derive(Debug, Clone, Serialize, Default)]
pub struct GitStatus {
    pub head_sha: Option<String>,
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub working: Vec<String>,
    pub index: Vec<String>,
    pub untracked: Vec<String>,
}

/// One diff view among the three supported by `GET /git/diff`.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffView {
    WorkingVsIndex,
    IndexVsHead,
    WorkingVsHead,
}

impl DiffView {
    /// Return the process arguments for the system adapter.
    pub fn args(self) -> &'static [&'static str] {
        match self {
            Self::WorkingVsIndex => &["diff"],
            Self::IndexVsHead => &["diff", "--cached"],
            Self::WorkingVsHead => &["diff", "HEAD"],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GitLogEntry {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub committed_at: String,
    pub message: String,
    pub changed_files: u64,
    pub insertions: u64,
    pub deletions: u64,
}

/// A path that collides between local edits and an incoming remote update.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateConflictPath {
    pub path: String,
    pub kind: String,
    pub base_hash: Option<String>,
    pub remote_hash: Option<String>,
    pub main_hash: Option<String>,
}

/// Result of a three-way update. Conflict persistence belongs to the caller.
#[derive(Debug, Clone)]
pub enum UpdateOutcome {
    /// HEAD advanced to the remote tip; working-tree-only edits were preserved.
    FastForward {
        new_head: String,
        base_tree: String,
        remote_tree: String,
    },
    Failed(GitError),
    /// Main was left unchanged. The caller persists the conflict rows.
    Conflict {
        paths: Vec<UpdateConflictPath>,
        base_tree: String,
        remote_tree: String,
        main_tree: String,
        head_sha: String,
        remote_sha: String,
    },
}

/// Object-safe Git port implemented by the system adapter or a test double.
pub trait GitRunner: Send + Sync {
    fn clone<'a>(
        &'a self,
        url: &'a str,
        branch: Option<&'a str>,
        into: &'a Path,
        credential: &'a GitCredential,
    ) -> GitFuture<'a, Result<(), GitError>>;

    fn status<'a>(&'a self, repo: &'a Path) -> GitFuture<'a, Result<GitStatus, GitError>>;

    fn diff<'a>(
        &'a self,
        repo: &'a Path,
        view: DiffView,
    ) -> GitFuture<'a, Result<String, GitError>>;

    fn log<'a>(
        &'a self,
        repo: &'a Path,
        limit: u32,
    ) -> GitFuture<'a, Result<Vec<GitLogEntry>, GitError>>;

    fn branches<'a>(&'a self, repo: &'a Path) -> GitFuture<'a, Result<Vec<String>, GitError>>;

    fn remotes<'a>(&'a self, repo: &'a Path) -> GitFuture<'a, Result<Vec<String>, GitError>>;

    fn fetch<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        credential: &'a GitCredential,
    ) -> GitFuture<'a, Result<(), GitError>>;

    fn stage<'a>(
        &'a self,
        repo: &'a Path,
        paths: &'a [String],
    ) -> GitFuture<'a, Result<(), GitError>>;

    fn unstage<'a>(
        &'a self,
        repo: &'a Path,
        paths: &'a [String],
    ) -> GitFuture<'a, Result<(), GitError>>;

    fn commit<'a>(
        &'a self,
        repo: &'a Path,
        message: &'a str,
    ) -> GitFuture<'a, Result<String, GitError>>;

    fn push<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        branch: &'a str,
        credential: &'a GitCredential,
    ) -> GitFuture<'a, Result<(), GitError>>;

    fn update<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        branch: &'a str,
        credential: &'a GitCredential,
    ) -> GitFuture<'a, Result<UpdateOutcome, GitError>>;

    fn checkout<'a>(
        &'a self,
        repo: &'a Path,
        branch: &'a str,
    ) -> GitFuture<'a, Result<(), GitError>>;

    /// Apply a resolved conflict choice to the Main working tree.
    fn apply_conflict_choice<'a>(
        &'a self,
        repo: &'a Path,
        path: &'a str,
        choice: &'a str,
        remote_hash: Option<&'a str>,
        main_hash: Option<&'a str>,
        edited_bytes: Option<&'a [u8]>,
    ) -> GitFuture<'a, Result<(), GitError>>;

    /// Fast-forward HEAD/index after the working tree contains the resolution.
    fn complete_fast_forward<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        branch: &'a str,
    ) -> GitFuture<'a, Result<String, GitError>>;
}

// ----- Git capability surface ---------------------------------------------

/// Durable operation kinds this capability creates on `operations`.
pub const KIND_GIT_FETCH: &str = "git.fetch";
pub const KIND_GIT_UPDATE: &str = "git.update";
pub const KIND_GIT_PUSH: &str = "git.push";

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct GitUpdateInput {
    pub remote: String,
    pub branch: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GitUpdateConflictView {
    pub id: String,
    pub project_id: String,
    pub state: String,
    pub base_tree: String,
    pub remote_tree: String,
    pub main_tree: String,
    pub operation_id: String,
    pub version: String,
    pub paths: Vec<GitUpdateConflictPathView>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GitUpdateConflictPathView {
    pub path: String,
    pub kind: String,
    pub base_hash: Option<String>,
    pub remote_hash: Option<String>,
    pub main_hash: Option<String>,
    pub choice: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ResolveGitUpdateConflictInput {
    pub paths: Vec<ResolveGitUpdateConflictPath>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ResolveGitUpdateConflictPath {
    pub path: String,
    /// One of: main | remote | delete | edited_text
    pub choice: String,
    #[serde(default)]
    pub edited_text: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceControlError {
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("project not found")]
    NotFound,
    #[error("github credential not found")]
    CredentialNotFound,
    #[error("git update conflict not found")]
    ConflictNotFound,
    #[error("revision mismatch: expected {expected}, current {current}")]
    RevisionMismatch { expected: String, current: String },
    #[error("git operation failed: {0}")]
    Git(#[from] GitError),
    #[error("workspace sync error: {0}")]
    Workspace(#[from] WorkspaceError),
    #[error("operation error: {0}")]
    Operation(#[from] janus_infrastructure::operations::OperationError),
    #[error("storage error: {0}")]
    Storage(#[from] mongodb::error::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
    #[error("document value access error: {0}")]
    ValueAccess(#[from] mongodb::bson::document::ValueAccessError),
}

/// Stable error codes for the `GIT_*` family and friends; transport maps these
/// to RFC 9457 Problems.
impl SourceControlError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "VALIDATION_FAILED",
            Self::NotFound | Self::CredentialNotFound | Self::ConflictNotFound => {
                "RESOURCE_NOT_FOUND"
            }
            Self::RevisionMismatch { .. } => "RESOURCE_VERSION_MISMATCH",
            Self::Git(git) => git.code(),
            Self::Workspace(WorkspaceError::RevisionMismatch { .. }) => "RESOURCE_VERSION_MISMATCH",
            Self::Workspace(_) => "INTERNAL_ERROR",
            Self::Operation(_)
            | Self::Storage(_)
            | Self::Serde(_)
            | Self::Io(_)
            | Self::Internal(_)
            | Self::ValueAccess(_) => "INTERNAL_ERROR",
        }
    }
}

/// Repo config the git capability reads (read-only) from the `projects` table.
struct RepoConfigRow {
    state: String,
    repo_url: String,
    repo_branch: Option<String>,
    repo_access: String,
    github_credential_id: Option<String>,
}

impl RepoConfigRow {
    fn from_doc(document: &Document) -> Result<Self, SourceControlError> {
        Ok(Self {
            state: document.get_str("state")?.to_owned(),
            repo_url: document.get_str("repo_url")?.to_owned(),
            repo_branch: document
                .get("repo_branch")
                .and_then(Bson::as_str)
                .map(str::to_owned),
            repo_access: document.get_str("repo_access")?.to_owned(),
            github_credential_id: document
                .get("github_credential_id")
                .and_then(Bson::as_str)
                .map(str::to_owned),
        })
    }
}

struct CredentialRow {
    pat_ciphertext: Option<Vec<u8>>,
}

impl CredentialRow {
    fn from_doc(document: &Document) -> Result<Self, SourceControlError> {
        Ok(Self {
            pat_ciphertext: document
                .get("pat_ciphertext")
                .and_then(|value| match value {
                    Bson::Binary(binary) => Some(binary.bytes.clone()),
                    _ => None,
                }),
        })
    }
}

struct ConflictRow {
    id: String,
    project_id: String,
    base_tree: String,
    remote_tree: String,
    main_tree: String,
    state: String,
    operation_id: String,
    version: String,
    created_at: String,
    updated_at: String,
}

impl ConflictRow {
    fn from_doc(document: &Document) -> Result<Self, SourceControlError> {
        Ok(Self {
            id: document.get_str("_id")?.to_owned(),
            project_id: document.get_str("project_id")?.to_owned(),
            base_tree: document.get_str("base_tree")?.to_owned(),
            remote_tree: document.get_str("remote_tree")?.to_owned(),
            main_tree: document.get_str("main_tree")?.to_owned(),
            state: document.get_str("state")?.to_owned(),
            operation_id: document.get_str("operation_id")?.to_owned(),
            version: document.get_str("version")?.to_owned(),
            created_at: document.get_str("created_at")?.to_owned(),
            updated_at: document.get_str("updated_at")?.to_owned(),
        })
    }
}

struct ConflictPathRow {
    path: String,
    kind: String,
    base_hash: Option<String>,
    remote_hash: Option<String>,
    main_hash: Option<String>,
    choice: Option<String>,
}

impl ConflictPathRow {
    fn from_doc(document: &Document) -> Result<Self, SourceControlError> {
        Ok(Self {
            path: document.get_str("path")?.to_owned(),
            kind: document.get_str("kind")?.to_owned(),
            base_hash: document
                .get("base_hash")
                .and_then(Bson::as_str)
                .map(str::to_owned),
            remote_hash: document
                .get("remote_hash")
                .and_then(Bson::as_str)
                .map(str::to_owned),
            main_hash: document
                .get("main_hash")
                .and_then(Bson::as_str)
                .map(str::to_owned),
            choice: document
                .get("choice")
                .and_then(Bson::as_str)
                .map(str::to_owned),
        })
    }
}

fn pat_aad(owner_id: &str, id: &str) -> String {
    format!("v1/{owner_id}/github_credentials/{id}/pat")
}

fn sha2_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// The problem payload recorded on a durable git Operation. A classified
/// failure carries its user-facing message so the SCM panel can say what went
/// wrong; an unclassified one stays opaque, matching how the transport scrubs
/// the detail of an `INTERNAL_ERROR`.
fn operation_problem(error: &GitError) -> serde_json::Value {
    let detail = if error.code() == "INTERNAL_ERROR" {
        "The git operation could not be completed.".to_owned()
    } else {
        error.to_string()
    };
    serde_json::json!({"code": error.code(), "detail": detail})
}

/// The git capability: drives the `GitRunner` over a Project's Main repo and
/// owns the git state/conflict tables. Reads the Project's repo config and
/// GitHub credential from `projects` (read-only) so orchestration stays here;
/// it never writes project metadata or project states.
#[derive(Clone)]
pub struct SourceControlInterface {
    pool: mongodb::Database,
    unit_of_work: UnitOfWork,
    cipher: SecretCipher,
    operations: OperationInterface,
    workspace: WorkspaceInterface,
    workspaces_root: PathBuf,
    git: Arc<dyn GitRunner>,
}

impl SourceControlInterface {
    pub fn new(
        pool: mongodb::Database,
        cipher: SecretCipher,
        operations: OperationInterface,
        workspace: WorkspaceInterface,
        events: EventStore,
        data_root: &Path,
        git: Arc<dyn GitRunner>,
    ) -> Self {
        let unit_of_work = UnitOfWork::new(pool.clone(), events);
        Self {
            workspaces_root: data_root.join("workspaces"),
            pool,
            unit_of_work,
            cipher,
            operations,
            workspace,
            git,
        }
    }

    // ----- Main repo path helpers -----

    fn main_repo_dir(&self, project_id: &str) -> PathBuf {
        self.workspaces_root
            .join("main")
            .join(project_id)
            .join("repo")
    }

    fn main_handle_for(&self, project_id: &str) -> Result<WorkspaceHandle, SourceControlError> {
        let project_id_typed: ProjectId = project_id
            .parse()
            .map_err(|_| SourceControlError::Internal(anyhow::anyhow!("bad project id")))?;
        Ok(WorkspaceHandle::main(project_id_typed))
    }

    async fn remove_main_workspace(&self, project_id: &str) -> Result<(), SourceControlError> {
        let repo = self.main_repo_dir(project_id);
        match tokio::fs::remove_dir_all(repo.parent().unwrap_or(&repo)).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(SourceControlError::Io(error)),
        }
    }

    // ----- Read-only project repo config / credentials -----

    async fn fetch_repo_config(
        &self,
        owner_id: &str,
        id: &str,
    ) -> Result<RepoConfigRow, SourceControlError> {
        let document = self
            .pool
            .collection::<Document>("projects")
            .find_one(doc! {"_id": id, "owner_id": owner_id})
            .await?;
        let Some(document) = document else {
            return Err(SourceControlError::NotFound);
        };
        RepoConfigRow::from_doc(&document)
    }

    async fn require_ready(
        &self,
        owner_id: &str,
        id: &str,
    ) -> Result<RepoConfigRow, SourceControlError> {
        let row = self.fetch_repo_config(owner_id, id).await?;
        if row.state != "ready" {
            return Err(SourceControlError::Validation(format!(
                "project is not ready (state: {})",
                row.state
            )));
        }
        Ok(row)
    }

    async fn pat_for(
        &self,
        owner_id: &str,
        credential_id: &str,
    ) -> Result<Option<String>, SourceControlError> {
        let document = self
            .pool
            .collection::<Document>("github_credentials")
            .find_one(doc! {"_id": credential_id, "owner_id": owner_id})
            .await?;
        let Some(document) = document else {
            return Err(SourceControlError::CredentialNotFound);
        };
        let row = CredentialRow::from_doc(&document)?;
        match row.pat_ciphertext {
            Some(stored) => {
                let secret = self
                    .cipher
                    .decrypt(&stored, &pat_aad(owner_id, credential_id))?;
                Ok(Some(secret.expose().to_owned()))
            }
            None => Ok(None),
        }
    }

    async fn credential_for_row(
        &self,
        owner_id: &str,
        row: &RepoConfigRow,
    ) -> Result<GitCredential, SourceControlError> {
        match row.repo_access.as_str() {
            "public_https" => Ok(GitCredential::None),
            "github_private" => {
                let cred_id = row
                    .github_credential_id
                    .as_ref()
                    .ok_or_else(|| SourceControlError::Validation("missing credential".into()))?;
                let pat = self.pat_for(owner_id, cred_id).await?;
                Ok(GitCredential::HttpsBasic {
                    username: "x-access-token".into(),
                    password: pat.unwrap_or_default(),
                })
            }
            _ => Ok(GitCredential::None),
        }
    }

    async fn credential_for_project(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<GitCredential, SourceControlError> {
        let row = self.fetch_repo_config(owner_id, project_id).await?;
        self.credential_for_row(owner_id, &row).await
    }

    /// Clone a `creating` Project's repo into its Main workspace dir, reusing
    /// an existing clone when one is present. Project state transitions (error
    /// on failure, ready on success) are the caller's — this only returns the
    /// git result.
    pub async fn clone_project(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<(), SourceControlError> {
        let row = self.fetch_repo_config(owner_id, project_id).await?;
        if row.state != "creating" {
            return Err(SourceControlError::Validation(format!(
                "project is not creating (state: {})",
                row.state
            )));
        }
        let credential = self.credential_for_row(owner_id, &row).await?;

        let clone_lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let dest = self.main_repo_dir(project_id);
        let existing_clone = if dest.is_dir() {
            self.git
                .status(&dest)
                .await
                .ok()
                .and_then(|status| status.head_sha)
                .is_some()
        } else {
            false
        };
        if !existing_clone {
            if dest.exists()
                && let Err(error) = self.remove_main_workspace(project_id).await
            {
                drop(clone_lock);
                return Err(error);
            }
            let clone_result = GitRunner::clone(
                self.git.as_ref(),
                &row.repo_url,
                row.repo_branch.as_deref(),
                &dest,
                &credential,
            )
            .await;
            if let Err(error) = clone_result {
                drop(clone_lock);
                return Err(error.into());
            }
        }
        drop(clone_lock);
        Ok(())
    }

    // ----- Git queries -----

    pub async fn git_status(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<GitStatus, SourceControlError> {
        self.require_ready(owner_id, project_id).await?;
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let dir = self.main_repo_dir(project_id);
        Ok(self.git.status(&dir).await?)
    }

    pub async fn git_diff(
        &self,
        owner_id: &str,
        project_id: &str,
        view: DiffView,
    ) -> Result<String, SourceControlError> {
        self.require_ready(owner_id, project_id).await?;
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let dir = self.main_repo_dir(project_id);
        Ok(self.git.diff(&dir, view).await?)
    }

    pub async fn git_log(
        &self,
        owner_id: &str,
        project_id: &str,
        limit: u32,
    ) -> Result<Vec<GitLogEntry>, SourceControlError> {
        self.require_ready(owner_id, project_id).await?;
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let dir = self.main_repo_dir(project_id);
        Ok(self.git.log(&dir, limit).await?)
    }

    pub async fn git_branches(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<Vec<String>, SourceControlError> {
        self.require_ready(owner_id, project_id).await?;
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let dir = self.main_repo_dir(project_id);
        Ok(self.git.branches(&dir).await?)
    }

    pub async fn git_remotes(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<Vec<String>, SourceControlError> {
        self.require_ready(owner_id, project_id).await?;
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let dir = self.main_repo_dir(project_id);
        Ok(self.git.remotes(&dir).await?)
    }

    // ----- Git commands -----

    /// Git takes `remote`/`branch` as positional arguments; a value beginning
    /// with `-` is parsed as an option instead (e.g. `--delete` on push), so
    /// refuse option-shaped inputs at the capability edge. Refs also cannot
    /// contain spaces or control bytes per git-check-ref-format.
    fn validate_git_arg(name: &str, value: &str) -> Result<(), SourceControlError> {
        if value.is_empty() {
            return Err(SourceControlError::Validation(format!(
                "{name} must not be empty"
            )));
        }
        if value.starts_with('-')
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte == b' ')
        {
            return Err(SourceControlError::Validation(format!(
                "{name} must not start with '-' or contain whitespace or control characters"
            )));
        }
        Ok(())
    }

    pub async fn git_fetch(
        &self,
        owner_id: &str,
        project_id: &str,
        remote: &str,
        correlation_id: CorrelationId,
        idempotency: Option<IdempotencyRequest>,
    ) -> Result<OperationView, SourceControlError> {
        Self::validate_git_arg("remote", remote)?;
        self.require_ready(owner_id, project_id).await?;
        let credential = self.credential_for_project(owner_id, project_id).await?;
        let created = self
            .operations
            .create(
                CreateOperation {
                    kind: KIND_GIT_FETCH,
                    actor: serde_json::json!({"kind": "owner", "id": owner_id}),
                    target_kind: "project",
                    target_id: Some(project_id),
                    conditions: serde_json::json!({"project_id": project_id, "remote": remote}),
                    correlation_id,
                    idempotency,
                },
                None,
            )
            .await?;
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let dir = self.main_repo_dir(project_id);
        if let Err(error) = self.git.fetch(&dir, remote, &credential).await {
            self.operations
                .finish(
                    &created.operation.id,
                    OperationStatus::Failed,
                    None,
                    Some(operation_problem(&error)),
                    correlation_id,
                )
                .await?;
            return Err(error.into());
        }
        self.operations
            .finish(
                &created.operation.id,
                OperationStatus::Succeeded,
                None,
                None,
                correlation_id,
            )
            .await?;
        self.refresh_git_state(owner_id, project_id, "fetch", correlation_id)
            .await?;
        Ok(created.operation)
    }

    pub async fn git_stage(
        &self,
        owner_id: &str,
        project_id: &str,
        paths: &[String],
        correlation_id: CorrelationId,
    ) -> Result<(), SourceControlError> {
        self.require_ready(owner_id, project_id).await?;
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let dir = self.main_repo_dir(project_id);
        self.git.stage(&dir, paths).await?;
        self.refresh_git_state(owner_id, project_id, "stage", correlation_id)
            .await
    }

    pub async fn git_unstage(
        &self,
        owner_id: &str,
        project_id: &str,
        paths: &[String],
        correlation_id: CorrelationId,
    ) -> Result<(), SourceControlError> {
        self.require_ready(owner_id, project_id).await?;
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let dir = self.main_repo_dir(project_id);
        self.git.unstage(&dir, paths).await?;
        self.refresh_git_state(owner_id, project_id, "unstage", correlation_id)
            .await
    }

    pub async fn git_commit(
        &self,
        owner_id: &str,
        project_id: &str,
        message: &str,
        correlation_id: CorrelationId,
    ) -> Result<String, SourceControlError> {
        self.require_ready(owner_id, project_id).await?;
        if message.trim().is_empty() {
            return Err(SourceControlError::Validation(
                "a commit message is required".into(),
            ));
        }
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let dir = self.main_repo_dir(project_id);
        let sha = self.git.commit(&dir, message).await?;
        self.refresh_git_state(owner_id, project_id, "commit", correlation_id)
            .await?;
        Ok(sha)
    }

    pub async fn git_push(
        &self,
        owner_id: &str,
        project_id: &str,
        remote: &str,
        branch: &str,
        correlation_id: CorrelationId,
        idempotency: Option<IdempotencyRequest>,
    ) -> Result<OperationView, SourceControlError> {
        Self::validate_git_arg("remote", remote)?;
        Self::validate_git_arg("branch", branch)?;
        self.require_ready(owner_id, project_id).await?;
        let credential = self.credential_for_project(owner_id, project_id).await?;
        let created = self
            .operations
            .create(
                CreateOperation {
                    kind: KIND_GIT_PUSH,
                    actor: serde_json::json!({"kind": "owner", "id": owner_id}),
                    target_kind: "project",
                    target_id: Some(project_id),
                    conditions: serde_json::json!({
                        "project_id": project_id,
                        "remote": remote,
                        "branch": branch,
                    }),
                    correlation_id,
                    idempotency,
                },
                None,
            )
            .await?;
        // Push executes inline (short) when possible; the Operation records it.
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let dir = self.main_repo_dir(project_id);
        if let Err(error) = self.git.push(&dir, remote, branch, &credential).await {
            self.operations
                .finish(
                    &created.operation.id,
                    OperationStatus::Failed,
                    None,
                    Some(operation_problem(&error)),
                    correlation_id,
                )
                .await?;
            return Err(error.into());
        }
        self.operations
            .finish(
                &created.operation.id,
                OperationStatus::Succeeded,
                None,
                None,
                correlation_id,
            )
            .await?;
        self.refresh_git_state(owner_id, project_id, "push", correlation_id)
            .await?;
        Ok(created.operation)
    }

    /// Run a Git Update (fetch + three-way content merge). On conflict, Main is
    /// left unchanged and a persistent Git Update Conflict is created; the
    /// Operation enters `needs_attention`.
    pub async fn git_update(
        &self,
        owner_id: &str,
        project_id: &str,
        input: GitUpdateInput,
        correlation_id: CorrelationId,
        idempotency: Option<IdempotencyRequest>,
    ) -> Result<OperationView, SourceControlError> {
        Self::validate_git_arg("remote", &input.remote)?;
        Self::validate_git_arg("branch", &input.branch)?;
        self.require_ready(owner_id, project_id).await?;
        let credential = self.credential_for_project(owner_id, project_id).await?;
        let created = self
            .operations
            .create(
                CreateOperation {
                    kind: KIND_GIT_UPDATE,
                    actor: serde_json::json!({"kind": "owner", "id": owner_id}),
                    target_kind: "project",
                    target_id: Some(project_id),
                    conditions: serde_json::json!({
                        "project_id": project_id,
                        "remote": input.remote,
                        "branch": input.branch,
                    }),
                    correlation_id,
                    idempotency,
                },
                None,
            )
            .await?;
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let dir = self.main_repo_dir(project_id);
        let outcome = self
            .git
            .update(&dir, &input.remote, &input.branch, &credential)
            .await?;
        match outcome {
            UpdateOutcome::FastForward { new_head, .. } => {
                self.refresh_git_state(owner_id, project_id, "update", correlation_id)
                    .await?;
                // Bump content revision so Diff consumers refresh.
                if let Ok(handle) = self.main_handle_for(project_id) {
                    let _ = self
                        .workspace
                        .bump_revision(
                            &handle,
                            None,
                            "git.update",
                            serde_json::json!({"kind": "owner", "id": owner_id}),
                        )
                        .await;
                }
                self.operations
                    .finish(
                        &created.operation.id,
                        OperationStatus::Succeeded,
                        Some(serde_json::json!({"new_head": new_head})),
                        None,
                        correlation_id,
                    )
                    .await?;
            }
            UpdateOutcome::Failed(error) => {
                self.operations
                    .finish(
                        &created.operation.id,
                        OperationStatus::Failed,
                        None,
                        Some(operation_problem(&error)),
                        correlation_id,
                    )
                    .await?;
                return Err(error.into());
            }
            UpdateOutcome::Conflict {
                paths,
                base_tree,
                remote_tree,
                main_tree,
                ..
            } => {
                self.persist_update_conflict(
                    project_id,
                    &created.operation.id,
                    &base_tree,
                    &remote_tree,
                    &main_tree,
                    &paths,
                )
                .await?;
                self.operations
                    .finish(
                        &created.operation.id,
                        OperationStatus::NeedsAttention,
                        Some(serde_json::json!({
                            "conflict_paths": paths.len(),
                            "base_tree": base_tree,
                            "remote_tree": remote_tree,
                            "main_tree": main_tree,
                        })),
                        Some(serde_json::json!({"code": "GIT_UPDATE_CONFLICT"})),
                        correlation_id,
                    )
                    .await?;
            }
        }
        self.operations
            .get(&created.operation.id)
            .await?
            .ok_or(SourceControlError::NotFound)
    }

    pub async fn list_update_conflicts(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<Vec<GitUpdateConflictView>, SourceControlError> {
        self.require_ready(owner_id, project_id).await?;
        let mut cursor = self
            .pool
            .collection::<Document>("git_update_conflicts")
            .find(doc! {"project_id": project_id, "state": {"$in": ["open", "applying"]}})
            .sort(doc! {"created_at": -1})
            .await?;
        let mut rows = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            rows.push(ConflictRow::from_doc(&document)?);
        }
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(self.conflict_view(row).await?);
        }
        Ok(out)
    }

    pub async fn get_update_conflict(
        &self,
        owner_id: &str,
        project_id: &str,
        conflict_id: &str,
    ) -> Result<GitUpdateConflictView, SourceControlError> {
        self.require_ready(owner_id, project_id).await?;
        let document = self
            .pool
            .collection::<Document>("git_update_conflicts")
            .find_one(doc! {"_id": conflict_id, "project_id": project_id})
            .await?;
        let Some(document) = document else {
            return Err(SourceControlError::ConflictNotFound);
        };
        let row = ConflictRow::from_doc(&document)?;
        self.conflict_view(row).await
    }

    /// Persist path choices; when every path has a choice, apply them and
    /// complete the fast-forward. Version changes supersede the conflict.
    pub async fn resolve_update_conflict(
        &self,
        owner_id: &str,
        project_id: &str,
        conflict_id: &str,
        expected_version: &str,
        input: ResolveGitUpdateConflictInput,
        correlation_id: CorrelationId,
    ) -> Result<GitUpdateConflictView, SourceControlError> {
        self.require_ready(owner_id, project_id).await?;
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let document = self
            .pool
            .collection::<Document>("git_update_conflicts")
            .find_one(doc! {"_id": conflict_id, "project_id": project_id})
            .await?;
        let Some(document) = document else {
            return Err(SourceControlError::ConflictNotFound);
        };
        let row = ConflictRow::from_doc(&document)?;
        if row.version != expected_version {
            return Err(SourceControlError::RevisionMismatch {
                expected: expected_version.into(),
                current: row.version,
            });
        }
        if row.state != "open" && row.state != "applying" {
            return Err(SourceControlError::Validation(format!(
                "conflict is not open (state: {})",
                row.state
            )));
        }

        // Save each path choice. A path that is not part of this conflict has to
        // be rejected: accepting it silently leaves the conflict open with no
        // reason the user can see.
        let now = now_utc_str();
        for path in &input.paths {
            if !matches!(
                path.choice.as_str(),
                "main" | "remote" | "delete" | "edited_text"
            ) {
                return Err(SourceControlError::Validation(format!(
                    "invalid choice {}",
                    path.choice
                )));
            }
            let edited_blob = if path.choice == "edited_text" {
                let text = path.edited_text.as_deref().ok_or_else(|| {
                    SourceControlError::Validation("edited_text requires edited_text body".into())
                })?;
                Some(sha2_hash(text.as_bytes()))
            } else {
                None
            };
            let changed = self
                .pool
                .collection::<Document>("git_update_conflict_paths")
                .update_one(
                    doc! {"conflict_id": conflict_id, "path": &path.path},
                    doc! {"$set": {
                        "choice": &path.choice,
                        "edited_blob_sha": edited_blob.as_deref(),
                        "version": format!("v_{}", GitUpdateConflictId::new()),
                    }},
                )
                .await?
                .matched_count;
            if changed == 0 {
                return Err(SourceControlError::Validation(format!(
                    "path is not part of this conflict: {}",
                    path.path
                )));
            }
            if path.choice == "edited_text"
                && let Some(text) = &path.edited_text
            {
                self.stage_conflict_edit(project_id, &path.path, text.as_bytes())
                    .await?;
            }
        }

        // Check whether every path now has a choice. Paths with no choice are
        // stored without a `choice` field, so `{choice: null}` matches them.
        let unresolved = self
            .pool
            .collection::<Document>("git_update_conflict_paths")
            .count_documents(doc! {"conflict_id": conflict_id, "choice": null})
            .await?;
        if unresolved > 0 {
            // Partial save — stay open.
            let new_version = format!("v_{}", GitUpdateConflictId::new());
            self.pool
                .collection::<Document>("git_update_conflicts")
                .update_one(
                    doc! {"_id": conflict_id},
                    doc! {"$set": {"version": &new_version, "updated_at": &now}},
                )
                .await?;
            return self
                .get_update_conflict(owner_id, project_id, conflict_id)
                .await;
        }

        // All paths chosen: apply and complete. The version filter stops a
        // concurrent resolver from entering the apply phase too.
        let applying_version = format!("v_{}", GitUpdateConflictId::new());
        let applied = self
            .pool
            .collection::<Document>("git_update_conflicts")
            .update_one(
                doc! {"_id": conflict_id, "version": expected_version},
                doc! {"$set": {
                    "state": "applying",
                    "version": &applying_version,
                    "updated_at": &now,
                }},
            )
            .await?
            .matched_count;
        if applied == 0 {
            return Err(SourceControlError::Validation(format!(
                "conflict version changed while resolving; reload and retry"
            )));
        }

        let mut cursor = self
            .pool
            .collection::<Document>("git_update_conflict_paths")
            .find(doc! {"conflict_id": conflict_id})
            .await?;
        let mut path_rows = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            path_rows.push(ConflictPathRow::from_doc(&document)?);
        }
        let dir = self.main_repo_dir(project_id);
        for path_row in &path_rows {
            let choice = path_row.choice.as_deref().unwrap_or("main");
            let edited_bytes = if choice == "edited_text" {
                let staged = self.conflict_edit_path(project_id, &path_row.path);
                // Writing an empty file here would destroy the resolution the
                // user typed, so a missing staged body has to fail the apply.
                Some(tokio::fs::read(&staged).await.map_err(|_| {
                    SourceControlError::Validation(format!(
                        "the edited text for {} is no longer staged; resubmit the resolution",
                        path_row.path
                    ))
                })?)
            } else {
                None
            };
            self.git
                .apply_conflict_choice(
                    &dir,
                    &path_row.path,
                    choice,
                    path_row.remote_hash.as_deref(),
                    path_row.main_hash.as_deref(),
                    edited_bytes.as_deref(),
                )
                .await?;
        }

        // Determine remote/branch from the operation conditions when possible;
        // fall back to project branch + origin.
        let project = self.fetch_repo_config(owner_id, project_id).await?;
        let remote = "origin";
        let branch = project.repo_branch.as_deref().unwrap_or("main");
        self.git.complete_fast_forward(&dir, remote, branch).await?;
        self.refresh_git_state(
            owner_id,
            project_id,
            "update_conflict_resolved",
            correlation_id,
        )
        .await?;
        if let Ok(handle) = self.main_handle_for(project_id) {
            let _ = self
                .workspace
                .bump_revision(
                    &handle,
                    None,
                    "git.update.resolve",
                    serde_json::json!({"kind": "owner", "id": owner_id}),
                )
                .await;
        }
        let resolved = self
            .pool
            .collection::<Document>("git_update_conflicts")
            .update_one(
                doc! {"_id": conflict_id, "version": &applying_version},
                doc! {"$set": {
                    "state": "resolved",
                    "version": format!("v_{}", GitUpdateConflictId::new()),
                    "updated_at": &now,
                }},
            )
            .await?
            .matched_count;
        if resolved == 0 {
            return Err(SourceControlError::Validation(format!(
                "conflict changed while applying resolution; reload and retry"
            )));
        }
        // Best-effort: mark the original operation succeeded if still open.
        let _ = self
            .operations
            .finish(
                &row.operation_id,
                OperationStatus::Succeeded,
                Some(serde_json::json!({"resolved_conflict": conflict_id})),
                None,
                correlation_id,
            )
            .await;
        self.get_update_conflict(owner_id, project_id, conflict_id)
            .await
    }

    /// Where a resolved `edited_text` body waits between the partial save that
    /// carried it and the apply step. The file name is a hash of the path so a
    /// client-supplied path can neither collide with another path nor escape the
    /// staging directory.
    fn conflict_edit_path(&self, project_id: &str, path: &str) -> PathBuf {
        self.workspaces_root
            .join("main")
            .join(project_id)
            .join(".janus-conflict-edits")
            .join(sha2_hash(path.as_bytes()))
    }

    async fn stage_conflict_edit(
        &self,
        project_id: &str,
        path: &str,
        bytes: &[u8],
    ) -> Result<(), SourceControlError> {
        let staged = self.conflict_edit_path(project_id, path);
        if let Some(parent) = staged.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&staged, bytes).await?;
        Ok(())
    }

    async fn refresh_git_state(
        &self,
        owner_id: &str,
        project_id: &str,
        cause: &str,
        correlation_id: CorrelationId,
    ) -> Result<(), SourceControlError> {
        let dir = self.main_repo_dir(project_id);
        let status = self.git.status(&dir).await?;
        let now = now_utc_str();
        let version = format!("v_{}", ProjectId::new());
        let mut work = self.unit_of_work.begin().await?;
        self.pool
            .collection::<Document>("project_git_state")
            .update_one(
                doc! {"_id": project_id},
                doc! {"$set": {
                    "git_state_version": &version,
                    "head_sha": status.head_sha.as_deref(),
                    "branch": status.branch.as_deref(),
                    "ahead": i64::from(status.ahead),
                    "behind": i64::from(status.behind),
                    "last_scan_at": &now,
                    "version": &version,
                    "updated_at": &now,
                }},
            )
            .upsert(true)
            .session(&mut *work.connection())
            .await?;
        work.append_event(NewEvent {
            event_type: EventType::GitStateChanged,
            actor: serde_json::json!({"kind": "owner", "id": owner_id}),
            resource: Some(serde_json::json!({"kind": "project", "id": project_id})),
            correlation_id: correlation_id.to_string(),
            causation_id: None,
            payload: serde_json::json!({"cause": cause}),
        })
        .await?;
        work.commit().await?;
        Ok(())
    }

    async fn persist_update_conflict(
        &self,
        project_id: &str,
        operation_id: &str,
        base_tree: &str,
        remote_tree: &str,
        main_tree: &str,
        paths: &[UpdateConflictPath],
    ) -> Result<String, SourceControlError> {
        let conflict_id = GitUpdateConflictId::new().to_string();
        let now = now_utc_str();
        let version = format!("v_{}", GitUpdateConflictId::new());
        // Supersede + insert + paths are one logical write: a partial failure
        // must not leave a conflict row without its paths.
        let mut work = self.unit_of_work.begin().await?;
        self.pool
            .collection::<Document>("git_update_conflicts")
            .update_many(
                doc! {"project_id": project_id, "state": {"$in": ["open", "applying"]}},
                doc! {"$set": {"state": "superseded", "updated_at": &now}},
            )
            .session(work.connection())
            .await?;
        self.pool
            .collection::<Document>("git_update_conflicts")
            .insert_one(doc! {
                "_id": &conflict_id,
                "project_id": project_id,
                "base_tree": base_tree,
                "remote_tree": remote_tree,
                "main_tree": main_tree,
                "state": "open",
                "operation_id": operation_id,
                "version": &version,
                "created_at": &now,
                "updated_at": &now,
            })
            .session(work.connection())
            .await?;
        for path in paths {
            self.pool
                .collection::<Document>("git_update_conflict_paths")
                .insert_one(doc! {
                    "conflict_id": &conflict_id,
                    "path": &path.path,
                    "kind": &path.kind,
                    "base_hash": path.base_hash.as_deref(),
                    "remote_hash": path.remote_hash.as_deref(),
                    "main_hash": path.main_hash.as_deref(),
                    "version": format!("v_{}", GitUpdateConflictId::new()),
                })
                .session(work.connection())
                .await?;
        }
        work.commit().await?;
        Ok(conflict_id)
    }

    /// Remove every git metadata row for a deleted Project. Mongo has no ON
    /// DELETE CASCADE and the collection-ownership rules forbid `projects`
    /// from writing these collections, so the application layer must call this
    /// when a Project is deleted.
    pub async fn delete_project_state(&self, project_id: &str) -> Result<(), SourceControlError> {
        let conflict_ids = self
            .pool
            .collection::<Document>("git_update_conflicts")
            .distinct("_id", doc! {"project_id": project_id})
            .await?;
        if !conflict_ids.is_empty() {
            self.pool
                .collection::<Document>("git_update_conflict_paths")
                .delete_many(doc! {"conflict_id": {"$in": conflict_ids}})
                .await?;
        }
        self.pool
            .collection::<Document>("git_update_conflicts")
            .delete_many(doc! {"project_id": project_id})
            .await?;
        self.pool
            .collection::<Document>("project_git_state")
            .delete_one(doc! {"_id": project_id})
            .await?;
        Ok(())
    }

    async fn conflict_view(
        &self,
        row: ConflictRow,
    ) -> Result<GitUpdateConflictView, SourceControlError> {
        let mut cursor = self
            .pool
            .collection::<Document>("git_update_conflict_paths")
            .find(doc! {"conflict_id": &row.id})
            .sort(doc! {"path": 1})
            .await?;
        let mut paths = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            paths.push(ConflictPathRow::from_doc(&document)?);
        }
        Ok(GitUpdateConflictView {
            id: row.id,
            project_id: row.project_id,
            state: row.state,
            base_tree: row.base_tree,
            remote_tree: row.remote_tree,
            main_tree: row.main_tree,
            operation_id: row.operation_id,
            version: row.version,
            paths: paths
                .into_iter()
                .map(|p| GitUpdateConflictPathView {
                    path: p.path,
                    kind: p.kind,
                    base_hash: p.base_hash,
                    remote_hash: p.remote_hash,
                    main_hash: p.main_hash,
                    choice: p.choice,
                })
                .collect(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_stable() {
        let cases = [
            (GitError::AuthFailed, "GIT_AUTH_FAILED"),
            (GitError::RemoteUnavailable, "GIT_REMOTE_UNAVAILABLE"),
            (GitError::NonFastForward, "GIT_NON_FAST_FORWARD"),
            (GitError::IndexNotEmpty, "GIT_INDEX_NOT_EMPTY"),
            (GitError::Diverged, "GIT_DIVERGED"),
            (GitError::CheckoutConflict, "GIT_CHECKOUT_CONFLICT"),
            (
                GitError::UpdateConflict { paths: Vec::new() },
                "GIT_UPDATE_CONFLICT",
            ),
            (GitError::RemoteNotFound, "GIT_REMOTE_NOT_FOUND"),
            (GitError::RepositoryNotFound, "GIT_REPOSITORY_NOT_FOUND"),
            (GitError::RefNotFound, "GIT_REF_NOT_FOUND"),
            (GitError::NothingToCommit, "GIT_NOTHING_TO_COMMIT"),
            (GitError::IdentityUnset, "GIT_IDENTITY_UNSET"),
            (GitError::RepositoryLocked, "GIT_REPOSITORY_LOCKED"),
            (GitError::CommandFailed("exit 1".into()), "INTERNAL_ERROR"),
            (GitError::BadOutput("invalid".into()), "INTERNAL_ERROR"),
        ];

        for (error, code) in cases {
            assert_eq!(error.code(), code);
        }
    }

    /// Every user-actionable git failure must carry a code of its own: the
    /// transport replaces the detail of an `INTERNAL_ERROR` with a fixed
    /// sentence, so anything mapped there reaches the user with no reason at all.
    #[test]
    fn user_actionable_git_failures_are_not_internal_errors() {
        let actionable = [
            GitError::RemoteNotFound,
            GitError::RepositoryNotFound,
            GitError::RefNotFound,
            GitError::NothingToCommit,
            GitError::IdentityUnset,
            GitError::RepositoryLocked,
        ];

        for error in actionable {
            assert_ne!(error.code(), "INTERNAL_ERROR", "{error}");
        }
    }

    #[test]
    fn diff_views_map_to_read_only_git_arguments() {
        assert_eq!(DiffView::WorkingVsIndex.args(), ["diff"]);
        assert_eq!(DiffView::IndexVsHead.args(), ["diff", "--cached"]);
        assert_eq!(DiffView::WorkingVsHead.args(), ["diff", "HEAD"]);
    }
}
