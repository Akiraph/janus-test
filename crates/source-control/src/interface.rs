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

use janus_infrastructure::{
    clock::now_utc_str,
    events::{EventStore, EventType, NewEvent},
    id::{CorrelationId, GitUpdateConflictId, ProjectId},
    operations::{CreateOperation, OperationInterface, OperationStatus, OperationView},
    secrets::SecretCipher,
    unit_of_work::UnitOfWork,
};
use janus_workspace::interface::{WorkspaceError, WorkspaceHandle, WorkspaceInterface};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
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
    Storage(#[from] sqlx::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
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
            | Self::Internal(_) => "INTERNAL_ERROR",
        }
    }
}

/// Repo config the git capability reads (read-only) from the `projects` table.
#[derive(FromRow)]
struct RepoConfigRow {
    state: String,
    repo_url: String,
    repo_branch: Option<String>,
    repo_access: String,
    github_credential_id: Option<String>,
}

#[derive(FromRow)]
struct CredentialRow {
    pat_ciphertext: Option<Vec<u8>>,
}

#[derive(FromRow)]
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

#[derive(FromRow)]
struct ConflictPathRow {
    path: String,
    kind: String,
    base_hash: Option<String>,
    remote_hash: Option<String>,
    main_hash: Option<String>,
    choice: Option<String>,
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

/// The git capability: drives the `GitRunner` over a Project's Main repo and
/// owns the git state/conflict tables. Reads the Project's repo config and
/// GitHub credential from `projects` (read-only) so orchestration stays here;
/// it never writes project metadata or project states.
#[derive(Clone)]
pub struct SourceControlInterface {
    pool: SqlitePool,
    unit_of_work: UnitOfWork,
    cipher: SecretCipher,
    operations: OperationInterface,
    workspace: WorkspaceInterface,
    workspaces_root: PathBuf,
    git: Arc<dyn GitRunner>,
}

impl SourceControlInterface {
    pub fn new(
        pool: SqlitePool,
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
        sqlx::query_as::<_, RepoConfigRow>(
            "SELECT state, repo_url, repo_branch, repo_access, github_credential_id \
             FROM projects WHERE id = ? AND owner_id = ?",
        )
        .bind(id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(SourceControlError::NotFound)
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
        let row: Option<CredentialRow> = sqlx::query_as(
            "SELECT pat_ciphertext FROM github_credentials WHERE id = ? AND owner_id = ?",
        )
        .bind(credential_id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Err(SourceControlError::CredentialNotFound);
        };
        match row.pat_ciphertext {
            Some(stored) => {
                let secret = self.cipher.decrypt(&stored, &pat_aad(owner_id, credential_id))?;
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

    pub async fn git_fetch(
        &self,
        owner_id: &str,
        project_id: &str,
        remote: &str,
        correlation_id: CorrelationId,
    ) -> Result<OperationView, SourceControlError> {
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
                    idempotency: None,
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
                    Some(serde_json::json!({"code": error.code()})),
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
    ) -> Result<(), SourceControlError> {
        self.require_ready(owner_id, project_id).await?;
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let dir = self.main_repo_dir(project_id);
        Ok(self.git.stage(&dir, paths).await?)
    }

    pub async fn git_unstage(
        &self,
        owner_id: &str,
        project_id: &str,
        paths: &[String],
    ) -> Result<(), SourceControlError> {
        self.require_ready(owner_id, project_id).await?;
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        let dir = self.main_repo_dir(project_id);
        Ok(self.git.unstage(&dir, paths).await?)
    }

    pub async fn git_commit(
        &self,
        owner_id: &str,
        project_id: &str,
        message: &str,
        correlation_id: CorrelationId,
    ) -> Result<String, SourceControlError> {
        self.require_ready(owner_id, project_id).await?;
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
    ) -> Result<OperationView, SourceControlError> {
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
                    idempotency: None,
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
                    Some(serde_json::json!({"code": error.code()})),
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
    ) -> Result<OperationView, SourceControlError> {
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
                    idempotency: None,
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
                        Some(
                            serde_json::json!({"code": error.code(), "detail": error.to_string()}),
                        ),
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
        let rows: Vec<ConflictRow> = sqlx::query_as(
            "SELECT id, project_id, base_tree, remote_tree, main_tree, state, operation_id, version, created_at, updated_at FROM git_update_conflicts WHERE project_id = ? AND state IN ('open', 'applying') ORDER BY created_at DESC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
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
        let row: ConflictRow = sqlx::query_as(
            "SELECT id, project_id, base_tree, remote_tree, main_tree, state, operation_id, version, created_at, updated_at FROM git_update_conflicts WHERE id = ? AND project_id = ?",
        )
        .bind(conflict_id)
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(SourceControlError::ConflictNotFound)?;
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
        let row: ConflictRow = sqlx::query_as(
            "SELECT id, project_id, base_tree, remote_tree, main_tree, state, operation_id, version, created_at, updated_at FROM git_update_conflicts WHERE id = ? AND project_id = ?",
        )
        .bind(conflict_id)
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(SourceControlError::ConflictNotFound)?;
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

        // Save each path choice.
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
            sqlx::query(
                "UPDATE git_update_conflict_paths SET choice = ?, edited_blob_sha = ?, version = ? WHERE conflict_id = ? AND path = ?",
            )
            .bind(&path.choice)
            .bind(edited_blob.as_deref())
            .bind(format!("v_{}", GitUpdateConflictId::new()))
            .bind(conflict_id)
            .bind(&path.path)
            .execute(&self.pool)
            .await?;
            if path.choice == "edited_text"
                && let Some(text) = &path.edited_text
            {
                let staging = self
                    .workspaces_root
                    .join("main")
                    .join(project_id)
                    .join(".janus-conflict-edits");
                tokio::fs::create_dir_all(&staging).await.ok();
                let safe = path.path.replace('/', "__");
                tokio::fs::write(staging.join(safe), text.as_bytes())
                    .await
                    .map_err(|e| SourceControlError::Internal(anyhow::anyhow!(e)))?;
            }
        }

        // Check whether every path now has a choice.
        let unresolved: Option<(i64,)> = sqlx::query_as(
            "SELECT COUNT(*) FROM git_update_conflict_paths WHERE conflict_id = ? AND choice IS NULL",
        )
        .bind(conflict_id)
        .fetch_optional(&self.pool)
        .await?;
        let unresolved = unresolved.map(|(c,)| c).unwrap_or(0);
        if unresolved > 0 {
            // Partial save — stay open.
            let new_version = format!("v_{}", GitUpdateConflictId::new());
            sqlx::query("UPDATE git_update_conflicts SET version = ?, updated_at = ? WHERE id = ?")
                .bind(&new_version)
                .bind(&now)
                .bind(conflict_id)
                .execute(&self.pool)
                .await?;
            return self
                .get_update_conflict(owner_id, project_id, conflict_id)
                .await;
        }

        // All paths chosen: apply and complete.
        sqlx::query(
            "UPDATE git_update_conflicts SET state = 'applying', version = ?, updated_at = ? WHERE id = ?",
        )
        .bind(format!("v_{}", GitUpdateConflictId::new()))
        .bind(&now)
        .bind(conflict_id)
        .execute(&self.pool)
        .await?;

        let path_rows: Vec<ConflictPathRow> = sqlx::query_as(
            "SELECT path, kind, base_hash, remote_hash, main_hash, choice FROM git_update_conflict_paths WHERE conflict_id = ?",
        )
        .bind(conflict_id)
        .fetch_all(&self.pool)
        .await?;
        let dir = self.main_repo_dir(project_id);
        for path_row in &path_rows {
            let choice = path_row.choice.as_deref().unwrap_or("main");
            let edited_bytes = if choice == "edited_text" {
                let safe = path_row.path.replace('/', "__");
                let staging = self
                    .workspaces_root
                    .join("main")
                    .join(project_id)
                    .join(".janus-conflict-edits")
                    .join(safe);
                Some(tokio::fs::read(&staging).await.unwrap_or_default())
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
        sqlx::query(
            "UPDATE git_update_conflicts SET state = 'resolved', version = ?, updated_at = ? WHERE id = ?",
        )
        .bind(format!("v_{}", GitUpdateConflictId::new()))
        .bind(&now)
        .bind(conflict_id)
        .execute(&self.pool)
        .await?;
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
        sqlx::query(
            "INSERT INTO project_git_state (project_id, git_state_version, head_sha, branch, ahead, behind, last_scan_at, version, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(project_id) DO UPDATE SET
               git_state_version = excluded.git_state_version,
               head_sha = excluded.head_sha,
               branch = excluded.branch,
               ahead = excluded.ahead,
               behind = excluded.behind,
               last_scan_at = excluded.last_scan_at,
               version = excluded.version,
               updated_at = excluded.updated_at",
        )
        .bind(project_id)
        .bind(&version)
        .bind(status.head_sha.as_deref())
        .bind(status.branch.as_deref())
        .bind(i64::from(status.ahead))
        .bind(i64::from(status.behind))
        .bind(&now)
        .bind(&version)
        .bind(&now)
        .execute(work.connection())
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
        // Supersede any previous open conflict for this project.
        sqlx::query(
            "UPDATE git_update_conflicts SET state = 'superseded', updated_at = ? WHERE project_id = ? AND state IN ('open', 'applying')",
        )
        .bind(&now)
        .bind(project_id)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "INSERT INTO git_update_conflicts (id, project_id, base_tree, remote_tree, main_tree, state, operation_id, prev_conflict_id, version, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 'open', ?, NULL, ?, ?, ?)",
        )
        .bind(&conflict_id)
        .bind(project_id)
        .bind(base_tree)
        .bind(remote_tree)
        .bind(main_tree)
        .bind(operation_id)
        .bind(&version)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        for path in paths {
            sqlx::query(
                "INSERT INTO git_update_conflict_paths (conflict_id, path, kind, base_hash, remote_hash, main_hash, choice, edited_blob_sha, version) VALUES (?, ?, ?, ?, ?, ?, NULL, NULL, ?)",
            )
            .bind(&conflict_id)
            .bind(&path.path)
            .bind(&path.kind)
            .bind(path.base_hash.as_deref())
            .bind(path.remote_hash.as_deref())
            .bind(path.main_hash.as_deref())
            .bind(format!("v_{}", GitUpdateConflictId::new()))
            .execute(&self.pool)
            .await?;
        }
        Ok(conflict_id)
    }

    async fn conflict_view(
        &self,
        row: ConflictRow,
    ) -> Result<GitUpdateConflictView, SourceControlError> {
        let paths: Vec<ConflictPathRow> = sqlx::query_as(
            "SELECT path, kind, base_hash, remote_hash, main_hash, choice FROM git_update_conflict_paths WHERE conflict_id = ? ORDER BY path",
        )
        .bind(&row.id)
        .fetch_all(&self.pool)
        .await?;
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
            (GitError::CommandFailed("exit 1".into()), "INTERNAL_ERROR"),
            (GitError::BadOutput("invalid".into()), "INTERNAL_ERROR"),
        ];

        for (error, code) in cases {
            assert_eq!(error.code(), code);
        }
    }

    #[test]
    fn diff_views_map_to_read_only_git_arguments() {
        assert_eq!(DiffView::WorkingVsIndex.args(), ["diff"]);
        assert_eq!(DiffView::IndexVsHead.args(), ["diff", "--cached"]);
        assert_eq!(DiffView::WorkingVsHead.args(), ["diff", "HEAD"]);
    }
}
