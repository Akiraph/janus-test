//! Public Project capability boundary.
//!
//! Owns project lifecycle, credentials, runtime policy, the read-only Git
//! projection, and the Main Workspace facade for Project routes.
//! Workspace content and copy state belong to `janus-workspace`; git state,
//! conflicts, and git orchestration belong to `janus-source-control`.
//!
//! Clone/update/push/delete are durable Operations: this module records intent
//! and enqueues work items; the background worker (registered in the
//! application layer) executes them through the Operation interface so a
//! process restart cannot silently drop a half-done clone.

use janus_infrastructure::clock::now_utc_str;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use utoipa::ToSchema;

use janus_infrastructure::{
    events::{EventStore, EventType, NewEvent},
    id::{CorrelationId, GithubCredentialId, ProjectId},
    operations::{
        CreateOperation, CreateWork, IdempotencyOutcome, IdempotencyRequest, OperationInterface,
        OperationView,
    },
    secrets::{Secret, SecretCipher, fingerprint},
    unit_of_work::{UnitOfWork, UnitOfWorkTransaction},
};
use janus_workspace::interface::{
    DeleteFileInput, FileMetaView, FileMutation, FileMutationEventContext, FileMutationRequest,
    FileTreeView, MoveFileInput, PathError, RevisionRef, SaveTextInput, WorkspaceError,
    WorkspaceHandle, WorkspaceInterface,
};

const REPO_KIND_PUBLIC: &str = "public_https";
const REPO_KIND_GITHUB_PRIVATE: &str = "github_private";

pub const KIND_CLONE: &str = "project.clone";
pub const KIND_DELETE_PROJECT: &str = "project.delete";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RepoAccess {
    PublicHttps,
    GithubPrivate,
}

impl RepoAccess {
    fn as_str(self) -> &'static str {
        match self {
            Self::PublicHttps => REPO_KIND_PUBLIC,
            Self::GithubPrivate => REPO_KIND_GITHUB_PRIVATE,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            REPO_KIND_PUBLIC => Self::PublicHttps,
            REPO_KIND_GITHUB_PRIVATE => Self::GithubPrivate,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateProjectInput {
    pub name: String,
    pub repository: RepositoryInput,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RepositoryInput {
    pub access: RepoAccess,
    pub url: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub github_credential_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    pub state: String,
    pub restrictions: Vec<String>,
    pub repository: RepositoryView,
    pub current_branch: Option<String>,
    pub main_revision: Option<String>,
    pub git_state_version: Option<String>,
    pub default_model_id: Option<String>,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MemoryView {
    pub key: String,
    pub content: String,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct ProjectModelPreference {
    pub owner_id: String,
    pub default_model_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RepositoryView {
    pub access: RepoAccess,
    pub url: String,
    pub branch: Option<String>,
    /// Only present for `github_private`; never exposes the PAT.
    pub github_credential_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateGithubCredentialInput {
    pub name: String,
    pub github_host: String,
    #[serde(default)]
    pub pat: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GithubCredentialView {
    pub id: String,
    pub name: String,
    pub github_host: String,
    pub pat_is_set: bool,
    pub pat_fingerprint: Option<String>,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CredentialProbeResult {
    pub status: String,
    pub http_status: Option<u16>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RetryProjectInput {
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub github_credential_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateGithubCredentialInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub github_host: Option<String>,
    /// When set, replaces the stored PAT. When omitted, the existing PAT is kept.
    #[serde(default)]
    pub pat: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectsError {
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
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("file is not editable: {0}")]
    NotEditable(String),
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

impl From<PathError> for ProjectsError {
    fn from(error: PathError) -> Self {
        Self::InvalidPath(error.to_string())
    }
}

fn map_workspace_path_error(error: WorkspaceError, not_found: &'static str) -> ProjectsError {
    match error {
        WorkspaceError::InvalidPath(error) => ProjectsError::InvalidPath(error.to_string()),
        WorkspaceError::PathNotFound(_) => ProjectsError::Validation(not_found.into()),
        error => ProjectsError::Workspace(error),
    }
}

/// Stable error codes; transport maps these to RFC 9457 Problems.
impl ProjectsError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "VALIDATION_FAILED",
            Self::NotFound | Self::CredentialNotFound | Self::ConflictNotFound => {
                "RESOURCE_NOT_FOUND"
            }
            Self::RevisionMismatch { .. } => "RESOURCE_VERSION_MISMATCH",
            Self::InvalidPath(_) => "INVALID_PATH",
            Self::NotEditable(_) => "FILE_NOT_EDITABLE",
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

#[derive(Clone)]
pub struct ProjectsInterface {
    pool: SqlitePool,
    unit_of_work: UnitOfWork,
    cipher: SecretCipher,
    operations: OperationInterface,
    workspace: WorkspaceInterface,
    workspaces_root: std::path::PathBuf,
}

#[derive(FromRow)]
struct ProjectRow {
    id: String,
    name: String,
    state: String,
    repo_access: String,
    repo_url: String,
    repo_branch: Option<String>,
    github_credential_id: Option<String>,
    default_model_id: Option<String>,
    main_workspace_handle: Option<String>,
    clone_error: Option<String>,
    version: String,
    created_at: String,
    updated_at: String,
}

/// The event metadata shared by every Main Workspace file mutation.
struct MainRevisionEvent<'a> {
    owner_id: &'a str,
    project_id: &'a str,
    correlation_id: &'a str,
}

impl ProjectsInterface {
    pub fn new(
        pool: SqlitePool,
        cipher: SecretCipher,
        operations: OperationInterface,
        workspace: WorkspaceInterface,
        events: EventStore,
        data_root: &std::path::Path,
    ) -> Self {
        let unit_of_work = UnitOfWork::new(pool.clone(), events);
        Self {
            workspaces_root: data_root.join("workspaces"),
            pool,
            unit_of_work,
            cipher,
            operations,
            workspace,
        }
    }

    pub async fn owner_id(&self, project_id: ProjectId) -> Result<String, ProjectsError> {
        sqlx::query_scalar("SELECT owner_id FROM projects WHERE id = ?")
            .bind(project_id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(ProjectsError::NotFound)
    }

    pub async fn list_memories(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<MemoryView>, ProjectsError> {
        let rows = sqlx::query_as::<_, (String, String, String, String, String)>(
            "SELECT memory_key, content, version, created_at, updated_at \
             FROM memories WHERE project_id = ? ORDER BY memory_key LIMIT 200",
        )
        .bind(project_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(key, content, version, created_at, updated_at)| MemoryView {
                    key,
                    content,
                    version,
                    created_at,
                    updated_at,
                },
            )
            .collect())
    }

    pub async fn memory_context(&self, project_id: ProjectId) -> Result<String, ProjectsError> {
        let memories = self.list_memories(project_id).await?;
        if memories.is_empty() {
            return Ok(String::new());
        }
        let mut context =
            String::from("Persistent project memory (authoritative when relevant):\n");
        for memory in memories {
            context.push_str("- ");
            context.push_str(&memory.key);
            context.push_str(": ");
            context.push_str(&memory.content);
            context.push('\n');
        }
        Ok(context)
    }

    pub async fn set_memory(
        &self,
        project_id: ProjectId,
        key: &str,
        content: &str,
    ) -> Result<MemoryView, ProjectsError> {
        let key = key.trim();
        let content = content.trim();
        if key.is_empty() || key.len() > 200 {
            return Err(ProjectsError::Validation(
                "memory key must be 1-200 characters".into(),
            ));
        }
        if content.is_empty() || content.len() > 100_000 {
            return Err(ProjectsError::Validation(
                "memory content must be 1-100000 bytes".into(),
            ));
        }
        let now = now_utc_str();
        let version = format!("v_{}", ProjectId::new());
        sqlx::query(
            "INSERT INTO memories (project_id, memory_key, content, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(project_id, memory_key) DO UPDATE SET content = excluded.content, \
             version = excluded.version, updated_at = excluded.updated_at",
        )
        .bind(project_id.to_string())
        .bind(key)
        .bind(content)
        .bind(&version)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(MemoryView {
            key: key.to_owned(),
            content: content.to_owned(),
            version,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub async fn delete_memory(
        &self,
        project_id: ProjectId,
        key: &str,
    ) -> Result<bool, ProjectsError> {
        Ok(
            sqlx::query("DELETE FROM memories WHERE project_id = ? AND memory_key = ?")
                .bind(project_id.to_string())
                .bind(key.trim())
                .execute(&self.pool)
                .await?
                .rows_affected()
                == 1,
        )
    }

    /// Return the PAT for a ready project's GitHub credential to the
    /// application execution boundary. Callers must place it in a secret
    /// environment value and must never persist or log the returned string.
    pub async fn git_token_for_project(
        &self,
        owner_id: &str,
        project_id: ProjectId,
    ) -> Result<Option<String>, ProjectsError> {
        let row = self
            .fetch_project(owner_id, &project_id.to_string())
            .await?;
        let Some(credential_id) = row.github_credential_id else {
            return Ok(None);
        };
        self.pat_for(owner_id, &credential_id).await
    }

    pub async fn model_preference_in_tx(
        &self,
        tx: &mut sqlx::SqliteConnection,
        project_id: ProjectId,
    ) -> Result<ProjectModelPreference, ProjectsError> {
        let row: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT owner_id, default_model_id FROM projects WHERE id = ?")
                .bind(project_id.to_string())
                .fetch_optional(&mut *tx)
                .await?;
        row.map(|(owner_id, default_model_id)| ProjectModelPreference {
            owner_id,
            default_model_id,
        })
        .ok_or(ProjectsError::NotFound)
    }

    pub async fn ensure_ready(
        &self,
        owner_id: &str,
        project_id: ProjectId,
    ) -> Result<(), ProjectsError> {
        self.require_ready(owner_id, &project_id.to_string())
            .await
            .map(|_| ())
    }

    pub async fn main_workspace_root(
        &self,
        owner_id: &str,
        project_id: ProjectId,
    ) -> Result<std::path::PathBuf, ProjectsError> {
        self.require_ready(owner_id, &project_id.to_string())
            .await?;
        Ok(tokio::fs::canonicalize(self.main_repo_dir(&project_id.to_string())).await?)
    }

    /// Create a Project record in `creating` state and enqueue a clone work
    /// item. The actual clone runs in the worker so it survives HTTP disconnect.
    /// Returns the new Project view and the clone Operation.
    pub async fn create_project(
        &self,
        owner_id: &str,
        input: CreateProjectInput,
        correlation_id: CorrelationId,
        idempotency: Option<IdempotencyRequest>,
    ) -> Result<(ProjectView, OperationView), ProjectsError> {
        validate_repository_input(&input.repository)?;
        if input.name.trim().is_empty() {
            return Err(ProjectsError::Validation("name is required".into()));
        }

        // Allocate the Project id up front so a *new* Operation can point at it.
        // On an idempotency hit, this provisional id is discarded and the stored
        // Operation's `target_id` is used instead — no ghost Project row is written.
        let provisional_id = ProjectId::new();
        let project_id = provisional_id.to_string();
        let event_correlation_id = correlation_id.to_string();
        let mut work = self.unit_of_work.begin().await?;
        let created = self
            .operations
            .create_in_tx(
                &mut work,
                CreateOperation {
                    kind: KIND_CLONE,
                    actor: serde_json::json!({"kind": "owner", "id": owner_id}),
                    target_kind: "project",
                    target_id: Some(&project_id),
                    conditions: serde_json::json!({"project_id": project_id}),
                    correlation_id,
                    idempotency,
                },
                Some(CreateWork {
                    handler_kind: KIND_CLONE,
                    payload: serde_json::json!({
                        "project_id": project_id,
                        "url": input.repository.url,
                        "branch": input.repository.branch,
                        "access": input.repository.access.as_str(),
                        "github_credential_id": input.repository.github_credential_id,
                    }),
                }),
            )
            .await?;

        if !matches!(created.outcome, IdempotencyOutcome::New) {
            work.commit().await?;
            let project_id = created.operation.target_id.as_deref().ok_or_else(|| {
                ProjectsError::Internal(anyhow::anyhow!(
                    "idempotent clone operation missing target_id"
                ))
            })?;
            let view = self.get_project(owner_id, project_id).await?;
            return Ok((view, created.operation));
        }

        let now = now_utc_str();
        let version = format!("v_{}", ProjectId::new());
        sqlx::query("INSERT INTO projects (id, owner_id, name, state, repo_access, repo_url, repo_branch, github_credential_id, default_model_id, main_workspace_handle, clone_error, version, created_at, updated_at, last_activity_at) VALUES (?, ?, ?, 'creating', ?, ?, ?, ?, NULL, NULL, NULL, ?, ?, ?, ?)")
            .bind(provisional_id.to_string())
            .bind(owner_id)
            .bind(input.name.trim())
            .bind(input.repository.access.as_str())
            .bind(&input.repository.url)
            .bind(input.repository.branch.as_deref())
            .bind(input.repository.github_credential_id.as_deref())
            .bind(&version)
            .bind(&now)
            .bind(&now)
            .bind(&now)
            .execute(work.connection())
            .await?;
        self.append_project_changed_in_tx(
            &mut work,
            owner_id,
            &project_id,
            "project",
            "created",
            &event_correlation_id,
        )
        .await?;
        work.commit().await?;

        let view = self
            .get_project(owner_id, &provisional_id.to_string())
            .await?;
        Ok((view, created.operation))
    }

    /// List the owner's Projects ordered by most recent activity.
    pub async fn list_projects(
        &self,
        owner_id: &str,
        limit: u32,
    ) -> Result<Vec<ProjectView>, ProjectsError> {
        let limit = i64::from(limit.clamp(1, 100));
        let rows = sqlx::query_as::<_, ProjectRow>(
            "SELECT id, name, state, repo_access, repo_url, repo_branch, github_credential_id, default_model_id, main_workspace_handle, clone_error, version, created_at, updated_at FROM projects WHERE owner_id = ? ORDER BY last_activity_at DESC LIMIT ?",
        )
        .bind(owner_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let mut views = Vec::with_capacity(rows.len());
        for row in rows {
            views.push(self.view_from_row(owner_id, row).await?);
        }
        Ok(views)
    }

    pub async fn get_project(
        &self,
        owner_id: &str,
        id: &str,
    ) -> Result<ProjectView, ProjectsError> {
        let row = self.fetch_project(owner_id, id).await?;
        self.view_from_row(owner_id, row).await
    }

    async fn fetch_project(&self, owner_id: &str, id: &str) -> Result<ProjectRow, ProjectsError> {
        sqlx::query_as::<_, ProjectRow>(
            "SELECT id, name, state, repo_access, repo_url, repo_branch, github_credential_id, default_model_id, main_workspace_handle, clone_error, version, created_at, updated_at FROM projects WHERE id = ? AND owner_id = ?",
        )
        .bind(id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ProjectsError::NotFound)
    }

    async fn view_from_row(
        &self,
        owner_id: &str,
        row: ProjectRow,
    ) -> Result<ProjectView, ProjectsError> {
        let main_revision = match &row.main_workspace_handle {
            Some(handle) => {
                let handle = WorkspaceHandle(handle.clone());
                self.workspace
                    .current_revision(&handle)
                    .await
                    .map(|r| r.0)
                    .ok()
            }
            None => None,
        };
        let (current_branch, git_state_version) = self.git_projection(owner_id, &row.id).await;
        Ok(ProjectView {
            id: row.id,
            name: row.name,
            state: row.state,
            restrictions: row.clone_error.map(|e| vec![e]).unwrap_or_default(),
            repository: RepositoryView {
                access: RepoAccess::parse(&row.repo_access).unwrap_or(RepoAccess::PublicHttps),
                url: row.repo_url,
                branch: row.repo_branch,
                github_credential_id: row.github_credential_id,
            },
            current_branch,
            main_revision,
            git_state_version,
            default_model_id: row.default_model_id,
            version: row.version,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }

    async fn git_projection(
        &self,
        _owner_id: &str,
        project_id: &str,
    ) -> (Option<String>, Option<String>) {
        let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT branch, git_state_version FROM project_git_state WHERE project_id = ?",
        )
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        match row {
            Some((branch, version)) => (branch, version),
            None => (None, None),
        }
    }

    /// Rename a Project or change its default model. Requires the current
    /// `version` via `If-Match` (enforced at transport).
    pub async fn update_project(
        &self,
        owner_id: &str,
        id: &str,
        expected_version: &str,
        name: Option<&str>,
        default_model_id: Option<Option<&str>>,
        correlation_id: &str,
    ) -> Result<ProjectView, ProjectsError> {
        let now = now_utc_str();
        let new_version = format!("v_{}", ProjectId::new());
        let mut work = self.unit_of_work.begin().await?;
        let changed = if let Some(model) = default_model_id {
            sqlx::query("UPDATE projects SET name = COALESCE(?, name), default_model_id = ?, version = ?, updated_at = ? WHERE id = ? AND owner_id = ? AND version = ?")
                .bind(name.map(str::trim))
                .bind(model)
                .bind(&new_version)
                .bind(&now)
                .bind(id)
                .bind(owner_id)
                .bind(expected_version)
                .execute(work.connection())
                .await?
                .rows_affected()
        } else {
            sqlx::query("UPDATE projects SET name = COALESCE(?, name), version = ?, updated_at = ? WHERE id = ? AND owner_id = ? AND version = ?")
                .bind(name.map(str::trim))
                .bind(&new_version)
                .bind(&now)
                .bind(id)
                .bind(owner_id)
                .bind(expected_version)
                .execute(work.connection())
                .await?
                .rows_affected()
        };
        if changed == 0 {
            work.rollback().await?;
            return Err(ProjectsError::NotFound);
        }
        self.append_project_changed_in_tx(
            &mut work,
            owner_id,
            id,
            "project",
            "updated",
            correlation_id,
        )
        .await?;
        work.commit().await?;
        self.get_project(owner_id, id).await
    }

    /// Mark a Project `deleting` and enqueue the cascade delete Operation. The
    /// worker stops/cleans Workspace, Git metadata and config; it never touches
    /// the remote repo or global models.
    pub async fn delete_project(
        &self,
        owner_id: &str,
        id: &str,
        expected_version: &str,
        correlation_id: CorrelationId,
        idempotency: IdempotencyRequest,
    ) -> Result<OperationView, ProjectsError> {
        let now = now_utc_str();
        let new_version = format!("v_{}", ProjectId::new());
        let event_correlation_id = correlation_id.to_string();
        let mut work = self.unit_of_work.begin().await?;
        let created = self
            .operations
            .create_in_tx(
                &mut work,
                CreateOperation {
                    kind: KIND_DELETE_PROJECT,
                    actor: serde_json::json!({"kind": "owner", "id": owner_id}),
                    target_kind: "project",
                    target_id: Some(id),
                    conditions: serde_json::json!({"project_id": id, "version": new_version}),
                    correlation_id,
                    idempotency: Some(idempotency),
                },
                Some(CreateWork {
                    handler_kind: KIND_DELETE_PROJECT,
                    payload: serde_json::json!({"project_id": id}),
                }),
            )
            .await?;
        if !matches!(created.outcome, IdempotencyOutcome::New) {
            work.commit().await?;
            return Ok(created.operation);
        }
        let changed = sqlx::query("UPDATE projects SET state = 'deleting', version = ?, updated_at = ? WHERE id = ? AND owner_id = ? AND version = ? AND state != 'deleting'")
            .bind(&new_version)
            .bind(&now)
            .bind(id)
            .bind(owner_id)
            .bind(expected_version)
            .execute(work.connection())
            .await?
            .rows_affected();
        if changed == 0 {
            work.rollback().await?;
            return Err(ProjectsError::NotFound);
        }
        self.append_project_changed_in_tx(
            &mut work,
            owner_id,
            id,
            "project",
            "deleted",
            &event_correlation_id,
        )
        .await?;
        work.commit().await?;
        Ok(created.operation)
    }

    /// Retry a clone for an `error` Project: reuse the saved repository input and
    /// current credential reference, optionally replacing the branch or
    /// credential id, but never the source URL (`API` retry).
    pub async fn retry_project(
        &self,
        owner_id: &str,
        id: &str,
        input: RetryProjectInput,
        correlation_id: CorrelationId,
    ) -> Result<(ProjectView, OperationView), ProjectsError> {
        let row = self.fetch_project(owner_id, id).await?;
        if row.state != "error" {
            return Err(ProjectsError::Validation(format!(
                "only error projects can retry (state: {})",
                row.state
            )));
        }
        let now = now_utc_str();
        let new_version = format!("v_{}", ProjectId::new());
        let branch = input.branch.or(row.repo_branch);
        let cred = input.github_credential_id.or(row.github_credential_id);
        let event_correlation_id = correlation_id.to_string();
        let mut work = self.unit_of_work.begin().await?;
        let changed = sqlx::query("UPDATE projects SET state = 'creating', repo_branch = ?, github_credential_id = ?, clone_error = NULL, version = ?, updated_at = ?, last_activity_at = ? WHERE id = ? AND owner_id = ? AND state = 'error'")
            .bind(branch.as_deref())
            .bind(cred.as_deref())
            .bind(&new_version)
            .bind(&now)
            .bind(&now)
            .bind(id)
            .bind(owner_id)
            .execute(work.connection())
            .await?
            .rows_affected();
        if changed == 0 {
            work.rollback().await?;
            return Err(ProjectsError::NotFound);
        }
        let created = self
            .operations
            .create_in_tx(
                &mut work,
                CreateOperation {
                    kind: KIND_CLONE,
                    actor: serde_json::json!({"kind": "owner", "id": owner_id}),
                    target_kind: "project",
                    target_id: Some(id),
                    conditions: serde_json::json!({"project_id": id}),
                    correlation_id,
                    idempotency: None,
                },
                Some(CreateWork {
                    handler_kind: KIND_CLONE,
                    payload: serde_json::json!({
                        "project_id": id,
                        "url": row.repo_url,
                        "branch": branch,
                        "access": row.repo_access,
                        "github_credential_id": cred,
                    }),
                }),
            )
            .await?;
        self.append_project_changed_in_tx(
            &mut work,
            owner_id,
            id,
            "project",
            "retry",
            &event_correlation_id,
        )
        .await?;
        work.commit().await?;
        let view = self.get_project(owner_id, id).await?;
        Ok((view, created.operation))
    }

    // ----- GitHub credentials (PAT) -----

    pub async fn list_credentials(
        &self,
        owner_id: &str,
    ) -> Result<Vec<GithubCredentialView>, ProjectsError> {
        let rows = sqlx::query_as::<_, CredentialRow>(
            "SELECT id, name, github_host, pat_ciphertext, pat_fingerprint, version, created_at, updated_at FROM github_credentials WHERE owner_id = ? ORDER BY name",
        )
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(credential_view).collect()
    }

    pub async fn create_credential(
        &self,
        owner_id: &str,
        input: CreateGithubCredentialInput,
        correlation_id: &str,
    ) -> Result<GithubCredentialView, ProjectsError> {
        if input.name.trim().is_empty() {
            return Err(ProjectsError::Validation("name is required".into()));
        }
        let id = GithubCredentialId::new();
        let now = now_utc_str();
        let version = format!("v_{}", GithubCredentialId::new());
        let (ciphertext, fingerprint) = self.encrypt_pat(owner_id, &id.to_string(), input.pat)?;
        let mut work = self.unit_of_work.begin().await?;
        sqlx::query("INSERT INTO github_credentials (id, owner_id, name, github_host, pat_ciphertext, pat_fingerprint, probe_summary_json, state, version, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, NULL, 'ready', ?, ?, ?)")
            .bind(id.to_string())
            .bind(owner_id)
            .bind(input.name.trim())
            .bind(input.github_host.trim())
            .bind(ciphertext)
            .bind(fingerprint)
            .bind(&version)
            .bind(&now)
            .bind(&now)
            .execute(work.connection())
            .await?;
        self.append_project_changed_in_tx(
            &mut work,
            owner_id,
            &id.to_string(),
            "github_credential",
            "created",
            correlation_id,
        )
        .await?;
        work.commit().await?;
        let row = self.fetch_credential(owner_id, &id.to_string()).await?;
        credential_view(row)
    }

    /// Reuse the automation PAT when the same deployment token is seen again;
    /// this keeps repeated webhook deliveries from creating one credential per
    /// email while keeping the plaintext token out of operation payloads.
    pub async fn ensure_automation_credential(
        &self,
        owner_id: &str,
        github_host: &str,
        pat: &str,
        correlation_id: &str,
    ) -> Result<String, ProjectsError> {
        let pat = pat.trim();
        if pat.is_empty() {
            return Err(ProjectsError::Validation("github PAT is required".into()));
        }
        let name = "Janus webhook automation";
        let pat_fingerprint = fingerprint(pat);
        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM github_credentials \
             WHERE owner_id = ? AND name = ? AND github_host = ? AND pat_fingerprint = ? \
             ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(owner_id)
        .bind(name)
        .bind(github_host.trim())
        .bind(pat_fingerprint)
        .fetch_optional(&self.pool)
        .await?;
        if let Some((id,)) = existing {
            return Ok(id);
        }
        Ok(self
            .create_credential(
                owner_id,
                CreateGithubCredentialInput {
                    name: name.into(),
                    github_host: github_host.trim().into(),
                    pat: Some(pat.into()),
                },
                correlation_id,
            )
            .await?
            .id)
    }

    pub async fn get_credential(
        &self,
        owner_id: &str,
        id: &str,
    ) -> Result<GithubCredentialView, ProjectsError> {
        let row = self.fetch_credential(owner_id, id).await?;
        credential_view(row)
    }

    /// Replace name/host and optionally the PAT. Updating without a new PAT
    /// keeps the existing ciphertext (same semantics as model provider keys).
    pub async fn update_credential(
        &self,
        owner_id: &str,
        id: &str,
        expected_version: &str,
        input: UpdateGithubCredentialInput,
        correlation_id: &str,
    ) -> Result<GithubCredentialView, ProjectsError> {
        let existing = self.fetch_credential(owner_id, id).await?;
        if existing.version != expected_version {
            return Err(ProjectsError::RevisionMismatch {
                expected: expected_version.into(),
                current: existing.version,
            });
        }
        let now = now_utc_str();
        let new_version = format!("v_{}", GithubCredentialId::new());
        let name = input
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(existing.name.as_str());
        let host = input
            .github_host
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(existing.github_host.as_str());
        let (ciphertext, fingerprint) = if let Some(pat) = input.pat {
            self.encrypt_pat(owner_id, id, Some(pat))?
        } else {
            (
                existing.pat_ciphertext.clone(),
                existing.pat_fingerprint.clone(),
            )
        };
        let mut work = self.unit_of_work.begin().await?;
        let changed = sqlx::query(
            "UPDATE github_credentials SET name = ?, github_host = ?, pat_ciphertext = ?, pat_fingerprint = ?, version = ?, updated_at = ? WHERE id = ? AND owner_id = ? AND version = ?",
        )
        .bind(name)
        .bind(host)
        .bind(ciphertext)
        .bind(fingerprint)
        .bind(&new_version)
        .bind(&now)
        .bind(id)
        .bind(owner_id)
        .bind(expected_version)
        .execute(work.connection())
        .await?
        .rows_affected();
        if changed == 0 {
            work.rollback().await?;
            return Err(ProjectsError::CredentialNotFound);
        }
        self.append_project_changed_in_tx(
            &mut work,
            owner_id,
            id,
            "github_credential",
            "updated",
            correlation_id,
        )
        .await?;
        work.commit().await?;
        self.get_credential(owner_id, id).await
    }

    /// Delete a credential. Refuses if a `github_private` Project still
    /// references it (the user must reassign or delete the Project first), so a
    /// PAT is never removed while a clone/push still depends on it.
    pub async fn delete_credential(
        &self,
        owner_id: &str,
        id: &str,
        correlation_id: &str,
    ) -> Result<(), ProjectsError> {
        let dependent: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM projects WHERE owner_id = ? AND github_credential_id = ? AND state != 'deleting' LIMIT 1",
        )
        .bind(owner_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        if dependent.is_some() {
            return Err(ProjectsError::Validation(
                "credential is in use by a project; reassign or delete the project first".into(),
            ));
        }
        let mut work = self.unit_of_work.begin().await?;
        let changed = sqlx::query("DELETE FROM github_credentials WHERE id = ? AND owner_id = ?")
            .bind(id)
            .bind(owner_id)
            .execute(work.connection())
            .await?
            .rows_affected();
        if changed == 0 {
            work.rollback().await?;
            return Err(ProjectsError::CredentialNotFound);
        }
        self.append_project_changed_in_tx(
            &mut work,
            owner_id,
            id,
            "github_credential",
            "deleted",
            correlation_id,
        )
        .await?;
        work.commit().await?;
        Ok(())
    }

    /// Bounded GitHub auth probe. Uses the stored PAT against the host's
    /// `/user` API; never returns the key or unbounded upstream response body.
    pub async fn probe_credential(
        &self,
        owner_id: &str,
        id: &str,
    ) -> Result<CredentialProbeResult, ProjectsError> {
        let row = self.fetch_credential(owner_id, id).await?;
        let Some(pat) = self.pat_for(owner_id, id).await? else {
            return Ok(CredentialProbeResult {
                status: "authentication_failed".into(),
                http_status: None,
                detail: "No PAT is set for this credential.".into(),
            });
        };
        let host = row.github_host.trim().trim_end_matches('/');
        let url = if host.starts_with("http://") || host.starts_with("https://") {
            format!("{host}/user")
        } else {
            format!("https://{host}/user")
        };
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ProjectsError::Internal(anyhow::anyhow!(e)))?;
        match client
            .get(&url)
            .header("authorization", format!("Bearer {pat}"))
            .header("user-agent", "janus-probe")
            .header("accept", "application/vnd.github+json")
            .send()
            .await
        {
            Ok(response) => {
                let status = response.status();
                let probe_status = if status.is_success() {
                    "ready"
                } else if matches!(status.as_u16(), 401 | 403) {
                    "authentication_failed"
                } else {
                    "upstream_error"
                };
                Ok(CredentialProbeResult {
                    status: probe_status.into(),
                    http_status: Some(status.as_u16()),
                    detail: if status.is_success() {
                        "GitHub accepted the credentials.".into()
                    } else if matches!(status.as_u16(), 401 | 403) {
                        "GitHub rejected the credentials.".into()
                    } else {
                        format!("GitHub returned HTTP {}.", status.as_u16())
                    },
                })
            }
            Err(error) => Ok(CredentialProbeResult {
                status: "unreachable".into(),
                http_status: None,
                detail: if error.is_timeout() {
                    "The GitHub probe timed out.".into()
                } else {
                    "GitHub could not be reached.".into()
                },
            }),
        }
    }

    async fn fetch_credential(
        &self,
        owner_id: &str,
        id: &str,
    ) -> Result<CredentialRow, ProjectsError> {
        sqlx::query_as::<_, CredentialRow>(
            "SELECT id, name, github_host, pat_ciphertext, pat_fingerprint, version, created_at, updated_at FROM github_credentials WHERE id = ? AND owner_id = ?",
        )
        .bind(id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ProjectsError::CredentialNotFound)
    }

    fn encrypt_pat(
        &self,
        owner_id: &str,
        id: &str,
        pat: Option<String>,
    ) -> Result<(Option<Vec<u8>>, Option<String>), ProjectsError> {
        match pat {
            Some(value) if !value.trim().is_empty() => {
                let fingerprint = fingerprint(&value);
                let ciphertext = self
                    .cipher
                    .encrypt(&Secret::new(value), &pat_aad(owner_id, id))?;
                Ok((Some(ciphertext), Some(fingerprint)))
            }
            _ => Ok((None, None)),
        }
    }

    /// Resolve a credential id to its decrypted PAT, for short-lived use in a
    /// clone/fetch/push helper. The plaintext is never logged or stored again.
    async fn pat_for(
        &self,
        owner_id: &str,
        credential_id: &str,
    ) -> Result<Option<String>, ProjectsError> {
        let row = self.fetch_credential(owner_id, credential_id).await?;
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

    // ----- Worker-facing operations (called by the background worker) -----

    /// Complete a successful clone: establish the Main content copy + first
    /// Content Revision and flip the Project to `ready`. Called by the clone
    /// orchestration in the application layer after the git clone itself has
    /// produced the repo directory (`source_control.clone_project`).
    pub async fn complete_clone(&self, project_id: &str) -> Result<(), ProjectsError> {
        let owner_id = self.project_owner(project_id).await?;
        let row = self.fetch_project(&owner_id, project_id).await?;
        if row.state != "creating" {
            return Err(ProjectsError::Validation(format!(
                "project is not creating (state: {})",
                row.state
            )));
        }
        let project_id_typed: ProjectId = project_id
            .parse()
            .map_err(|_| ProjectsError::Internal(anyhow::anyhow!("bad project id")))?;
        let managed_dir = format!("workspaces/main/{project_id}/repo");
        self.workspace
            .ensure_main_copy(
                project_id_typed,
                &managed_dir,
                "clone",
                serde_json::json!({"kind": "owner", "id": owner_id}),
            )
            .await?;
        self.mark_project_ready(project_id).await?;
        Ok(())
    }

    /// Record a failed clone: leave the Project in `error` so it can be retried
    /// or deleted. Only used when the git clone itself failed.
    pub async fn fail_clone(&self, project_id: &str, error: &str) -> Result<(), ProjectsError> {
        self.mark_project_state(project_id, "error", Some(error.to_owned()))
            .await
    }

    /// Execute the cascade delete for a `deleting` Project: remove the
    /// workspace directory and all Project rows. Never touches the remote repo
    /// or global model and identity state.
    pub async fn run_delete(&self, project_id: &str) -> Result<(), ProjectsError> {
        let owner_id = self.project_owner(project_id).await?;
        let _lock = self
            .workspace
            .acquire_project_mutation_lock(project_id)
            .await?;
        // Remove the workspace dir before metadata deletion. A failed
        // filesystem delete leaves the Project in `deleting` so the durable
        // Operation can retry instead of orphaning its Main copy.
        self.remove_main_workspace(project_id).await?;
        // Cascade FKs (ON DELETE CASCADE) remove github_credentials refs,
        // project_git_state and git_update_conflicts*.
        sqlx::query("DELETE FROM projects WHERE id = ? AND owner_id = ?")
            .bind(project_id)
            .bind(&owner_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn project_owner(&self, project_id: &str) -> Result<String, ProjectsError> {
        let row: Option<(String,)> = sqlx::query_as("SELECT owner_id FROM projects WHERE id = ?")
            .bind(project_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|(o,)| o).ok_or(ProjectsError::NotFound)
    }

    async fn mark_project_state(
        &self,
        project_id: &str,
        state: &str,
        error: Option<String>,
    ) -> Result<(), ProjectsError> {
        let now = now_utc_str();
        let version = format!("v_{}", ProjectId::new());
        sqlx::query("UPDATE projects SET state = ?, clone_error = ?, version = ?, updated_at = ?, last_activity_at = ? WHERE id = ?")
            .bind(state)
            .bind(error)
            .bind(&version)
            .bind(&now)
            .bind(&now)
            .bind(project_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn mark_project_ready(&self, project_id: &str) -> Result<(), ProjectsError> {
        let handle = format!("main:{project_id}");
        let now = now_utc_str();
        let version = format!("v_{}", ProjectId::new());
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE projects SET state = 'ready', clone_error = NULL, main_workspace_handle = ?, version = ?, updated_at = ?, last_activity_at = ? WHERE id = ?")
            .bind(&handle)
            .bind(&version)
            .bind(&now)
            .bind(&now)
            .bind(project_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    // ----- Main Workspace path helpers -----

    fn main_repo_dir(&self, project_id: &str) -> std::path::PathBuf {
        self.workspaces_root
            .join("main")
            .join(project_id)
            .join("repo")
    }

    async fn remove_main_workspace(&self, project_id: &str) -> Result<(), ProjectsError> {
        let repo = self.main_repo_dir(project_id);
        match tokio::fs::remove_dir_all(repo.parent().unwrap_or(&repo)).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ProjectsError::Io(error)),
        }
    }

    async fn require_ready(&self, owner_id: &str, id: &str) -> Result<ProjectRow, ProjectsError> {
        let row = self.fetch_project(owner_id, id).await?;
        if row.state != "ready" {
            return Err(ProjectsError::Validation(format!(
                "project is not ready (state: {})",
                row.state
            )));
        }
        Ok(row)
    }

    // ----- Main Workspace file read/write -----

    pub async fn file_meta(
        &self,
        owner_id: &str,
        project_id: &str,
        raw_path: &str,
    ) -> Result<FileMetaView, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        self.workspace
            .file_meta(&self.main_handle_for(project_id)?, raw_path)
            .await
            .map_err(|error| map_workspace_path_error(error, "file not found"))
    }

    pub async fn file_content(
        &self,
        owner_id: &str,
        project_id: &str,
        raw_path: &str,
    ) -> Result<Vec<u8>, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        self.workspace
            .file_content(&self.main_handle_for(project_id)?, raw_path)
            .await
            .map_err(|error| map_workspace_path_error(error, "file not found"))
    }

    pub async fn save_text(
        &self,
        owner_id: &str,
        project_id: &str,
        input: SaveTextInput,
        actor: serde_json::Value,
        correlation_id: &str,
    ) -> Result<RevisionRef, ProjectsError> {
        self.apply_main_file_mutation(
            MainRevisionEvent {
                owner_id,
                project_id,
                correlation_id,
            },
            FileMutation::Write {
                path: input.path,
                content: input.content.into_bytes(),
            },
            input.expected_main_revision.map(RevisionRef),
            "editor.save",
            actor,
        )
        .await
    }

    /// List a directory of the Main Workspace. Non-recursive one level listing;
    /// the client walks the tree by calling again with sub-paths (`API` files/tree).
    pub async fn file_tree(
        &self,
        owner_id: &str,
        project_id: &str,
        raw_path: &str,
    ) -> Result<Vec<FileTreeView>, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        self.workspace
            .file_tree(&self.main_handle_for(project_id)?, raw_path)
            .await
            .map_err(|error| map_workspace_path_error(error, "path not found"))
    }

    /// Atomic rename of one managed file/dir within the Main Workspace. Cannot
    /// escape the workspace root (`API` files/move).
    pub async fn move_file(
        &self,
        owner_id: &str,
        project_id: &str,
        input: MoveFileInput,
        actor: serde_json::Value,
        correlation_id: &str,
    ) -> Result<RevisionRef, ProjectsError> {
        self.apply_main_file_mutation(
            MainRevisionEvent {
                owner_id,
                project_id,
                correlation_id,
            },
            FileMutation::Move {
                from: input.from,
                to: input.to,
            },
            input.expected_main_revision.map(RevisionRef),
            "editor.move",
            actor,
        )
        .await
    }

    /// Delete one managed file, or a directory when `recursive` is true (`API` files delete).
    pub async fn delete_file(
        &self,
        owner_id: &str,
        project_id: &str,
        input: DeleteFileInput,
        actor: serde_json::Value,
        correlation_id: &str,
    ) -> Result<RevisionRef, ProjectsError> {
        let mutation = if input.recursive {
            FileMutation::DeleteTree { path: input.path }
        } else {
            FileMutation::Delete { path: input.path }
        };
        self.apply_main_file_mutation(
            MainRevisionEvent {
                owner_id,
                project_id,
                correlation_id,
            },
            mutation,
            input.expected_main_revision.map(RevisionRef),
            "editor.delete",
            actor,
        )
        .await
    }

    async fn apply_main_file_mutation(
        &self,
        event: MainRevisionEvent<'_>,
        mutation: FileMutation,
        expected: Option<RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
    ) -> Result<RevisionRef, ProjectsError> {
        self.require_ready(event.owner_id, event.project_id).await?;
        let handle = self.main_handle_for(event.project_id)?;
        let lock = self.workspace.acquire_mutation_lock(&handle).await?;
        let prepared = self
            .workspace
            .prepare_file_mutation(
                &lock,
                FileMutationRequest {
                    handle: &handle,
                    mutation,
                    expected: expected.as_ref(),
                    cause,
                    actor,
                    event: Some(main_revision_event_context(
                        event.owner_id,
                        event.project_id,
                        event.correlation_id,
                    )),
                },
            )
            .await?;
        let applied = self
            .workspace
            .apply_prepared_file_mutation(&lock, &prepared)
            .await?;
        let mut work = self.unit_of_work.begin().await?;
        let revision = self
            .workspace
            .finalize_file_mutation_in_tx(&lock, work.connection(), &prepared, &applied)
            .await?;
        self.append_main_revision_changed_in_tx(
            &mut work,
            event.owner_id,
            event.project_id,
            &revision,
            event.correlation_id,
        )
        .await?;
        self.workspace
            .acknowledge_file_mutation_event_in_tx(
                work.connection(),
                prepared.intent_id(),
                &revision,
            )
            .await?;
        work.commit().await?;
        Ok(revision)
    }

    fn main_handle_for(&self, project_id: &str) -> Result<WorkspaceHandle, ProjectsError> {
        let project_id_typed: ProjectId = project_id
            .parse()
            .map_err(|_| ProjectsError::Internal(anyhow::anyhow!("bad project id")))?;
        Ok(WorkspaceHandle::main(project_id_typed))
    }

    async fn append_project_changed_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        owner_id: &str,
        resource_id: &str,
        resource_kind: &str,
        operation: &str,
        correlation_id: &str,
    ) -> Result<(), ProjectsError> {
        work.append_event(NewEvent {
            event_type: EventType::ProjectChanged,
            actor: serde_json::json!({"kind": "owner", "id": owner_id}),
            resource: Some(serde_json::json!({"kind": resource_kind, "id": resource_id})),
            correlation_id: correlation_id.to_owned(),
            causation_id: None,
            payload: serde_json::json!({"operation": operation}),
        })
        .await?;
        Ok(())
    }

    async fn append_main_revision_changed_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        owner_id: &str,
        project_id: &str,
        revision: &RevisionRef,
        correlation_id: &str,
    ) -> Result<(), ProjectsError> {
        work.append_event(NewEvent {
            event_type: EventType::ProjectMainRevisionChanged,
            actor: serde_json::json!({"kind": "owner", "id": owner_id}),
            resource: Some(serde_json::json!({"kind": "project", "id": project_id})),
            correlation_id: correlation_id.to_owned(),
            causation_id: None,
            payload: serde_json::json!({
                "main_revision": revision.0,
                "source": "editor",
            }),
        })
        .await?;
        Ok(())
    }
}

fn main_revision_event_context(
    owner_id: &str,
    project_id: &str,
    correlation_id: &str,
) -> FileMutationEventContext {
    FileMutationEventContext {
        event_type: EventType::ProjectMainRevisionChanged,
        actor: serde_json::json!({"kind": "owner", "id": owner_id}),
        resource: serde_json::json!({"kind": "project", "id": project_id}),
        correlation_id: correlation_id.to_owned(),
        causation_id: None,
        payload: serde_json::json!({"source": "editor"}),
    }
}

#[derive(FromRow)]
struct CredentialRow {
    id: String,
    name: String,
    github_host: String,
    pat_ciphertext: Option<Vec<u8>>,
    pat_fingerprint: Option<String>,
    version: String,
    created_at: String,
    updated_at: String,
}

fn credential_view(row: CredentialRow) -> Result<GithubCredentialView, ProjectsError> {
    Ok(GithubCredentialView {
        id: row.id,
        name: row.name,
        github_host: row.github_host,
        pat_is_set: row.pat_ciphertext.is_some(),
        pat_fingerprint: row.pat_fingerprint,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn pat_aad(owner_id: &str, id: &str) -> String {
    format!("v1/{owner_id}/github_credentials/{id}/pat")
}

fn validate_repository_input(repo: &RepositoryInput) -> Result<(), ProjectsError> {
    let url = url::Url::parse(repo.url.trim())
        .map_err(|_| ProjectsError::Validation("repository url must be an absolute URL".into()))?;
    // Production paths are http(s). `file://` is accepted for local bare-repo
    // fixtures and offline development clones; the access enum still documents
    // the product-facing public_https / github_private distinction.
    if !matches!(url.scheme(), "http" | "https" | "file")
        || url.host_str().is_none() && url.scheme() != "file"
    {
        return Err(ProjectsError::Validation(
            "repository url must use http(s) or file".into(),
        ));
    }
    if repo.access == RepoAccess::GithubPrivate && repo.github_credential_id.is_none() {
        return Err(ProjectsError::Validation(
            "github_private requires a github_credential_id".into(),
        ));
    }
    Ok(())
}
