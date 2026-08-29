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
use futures_util::TryStreamExt;
use mongodb::{
    bson::{Bson, Document, doc},
    options::UpdateOptions,
};
use serde::{Deserialize, Serialize};
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
    /// Explicit opt-in for use by webhook-driven Automation pushes.
    #[serde(default)]
    pub automation_enabled: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GithubCredentialView {
    pub id: String,
    pub name: String,
    pub github_host: String,
    pub pat_is_set: bool,
    pub pat_fingerprint: Option<String>,
    pub automation_enabled: bool,
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
    /// When set, changes whether Automation may use this PAT for pushes.
    #[serde(default)]
    pub automation_enabled: Option<bool>,
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
    Storage(#[from] mongodb::error::Error),
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
        // Name the path: these routes are also driven by agent tools, where the
        // caller cannot see which of its paths the workspace rejected.
        WorkspaceError::PathNotFound(path) => {
            let detail = format!("{not_found}: {path}");
            ProjectsError::Validation(detail)
        }
        WorkspaceError::NotEditable(path) => ProjectsError::NotEditable(path),
        // A filesystem refusal is legible and actionable, so keep its reason
        // instead of folding it into an opaque internal error.
        denied @ WorkspaceError::PermissionDenied(_) => {
            let detail = denied.to_string();
            ProjectsError::Validation(detail)
        }
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
    pool: mongodb::Database,
    unit_of_work: UnitOfWork,
    cipher: SecretCipher,
    operations: OperationInterface,
    workspace: WorkspaceInterface,
    workspaces_root: std::path::PathBuf,
}

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

impl ProjectRow {
    fn from_doc(document: &Document) -> Result<Self, ProjectsError> {
        Ok(Self {
            id: document.get_str("_id")?.to_owned(),
            name: document.get_str("name")?.to_owned(),
            state: document.get_str("state")?.to_owned(),
            repo_access: document.get_str("repo_access")?.to_owned(),
            repo_url: document.get_str("repo_url")?.to_owned(),
            repo_branch: document.get("repo_branch").and_then(Bson::as_str).map(str::to_owned),
            github_credential_id: document
                .get("github_credential_id")
                .and_then(Bson::as_str)
                .map(str::to_owned),
            default_model_id: document
                .get("default_model_id")
                .and_then(Bson::as_str)
                .map(str::to_owned),
            main_workspace_handle: document
                .get("main_workspace_handle")
                .and_then(Bson::as_str)
                .map(str::to_owned),
            clone_error: document.get("clone_error").and_then(Bson::as_str).map(str::to_owned),
            version: document.get_str("version")?.to_owned(),
            created_at: document.get_str("created_at")?.to_owned(),
            updated_at: document.get_str("updated_at")?.to_owned(),
        })
    }
}

/// The event metadata shared by every Main Workspace file mutation.
struct MainRevisionEvent<'a> {
    owner_id: &'a str,
    project_id: &'a str,
    correlation_id: &'a str,
}

impl ProjectsInterface {
    pub fn new(
        pool: mongodb::Database,
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
        let document = self
            .pool
            .collection::<Document>("projects")
            .find_one(doc! {"_id": project_id.to_string()})
            .await?;
        let Some(document) = document else {
            return Err(ProjectsError::NotFound);
        };
        Ok(document.get_str("owner_id")?.to_owned())
    }

    pub async fn list_memories(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<MemoryView>, ProjectsError> {
        let mut cursor = self
            .pool
            .collection::<Document>("memories")
            .find(doc! {"project_id": project_id.to_string()})
            .sort(doc! {"memory_key": 1})
            .limit(200)
            .await?;
        let mut views = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            views.push(MemoryView {
                key: document.get_str("memory_key")?.to_owned(),
                content: document.get_str("content")?.to_owned(),
                version: document.get_str("version")?.to_owned(),
                created_at: document.get_str("created_at")?.to_owned(),
                updated_at: document.get_str("updated_at")?.to_owned(),
            });
        }
        Ok(views)
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
        self.pool
            .collection::<Document>("memories")
            .update_one(
                doc! {"project_id": project_id.to_string(), "memory_key": key},
                doc! {
                    "$set": {"content": content, "version": &version, "updated_at": &now},
                    "$setOnInsert": {"created_at": &now},
                },
                UpdateOptions::builder().upsert(true).build(),
            )
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
        let result = self
            .pool
            .collection::<Document>("memories")
            .delete_one(doc! {"project_id": project_id.to_string(), "memory_key": key.trim()})
            .await?;
        Ok(result.deleted_count > 0)
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
        session: &mut mongodb::ClientSession,
        project_id: ProjectId,
    ) -> Result<ProjectModelPreference, ProjectsError> {
        let document = self
            .pool
            .collection::<Document>("projects")
            .find_one(doc! {"_id": project_id.to_string()})
            .session(&mut *session)
            .await?;
        let Some(document) = document else {
            return Err(ProjectsError::NotFound);
        };
        Ok(ProjectModelPreference {
            owner_id: document.get_str("owner_id")?.to_owned(),
            default_model_id: document
                .get("default_model_id")
                .and_then(Bson::as_str)
                .map(str::to_owned),
        })
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
        let branch = input.repository.branch.as_deref();
        let credential_id = input.repository.github_credential_id.as_deref();
        self.pool
            .collection::<Document>("projects")
            .insert_one(doc! {
                "_id": &project_id,
                "owner_id": owner_id,
                "name": input.name.trim(),
                "state": "creating",
                "repo_access": input.repository.access.as_str(),
                "repo_url": &input.repository.url,
                "repo_branch": branch,
                "github_credential_id": credential_id,
                "default_model_id": null,
                "main_workspace_handle": null,
                "clone_error": null,
                "version": &version,
                "created_at": &now,
                "updated_at": &now,
                "last_activity_at": &now,
            })
            .session(&mut *work.connection())
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
        let mut cursor = self
            .pool
            .collection::<Document>("projects")
            .find(doc! {"owner_id": owner_id})
            .sort(doc! {"last_activity_at": -1})
            .limit(limit)
            .await?;
        let mut rows = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            rows.push(ProjectRow::from_doc(&document)?);
        }
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
        let document = self
            .pool
            .collection::<Document>("projects")
            .find_one(doc! {"_id": id, "owner_id": owner_id})
            .await?;
        let Some(document) = document else {
            return Err(ProjectsError::NotFound);
        };
        ProjectRow::from_doc(&document)
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
        let document = self
            .pool
            .collection::<Document>("project_git_state")
            .find_one(doc! {"_id": project_id})
            .await
            .ok()
            .flatten();
        match document {
            Some(document) => (
                document.get("branch").and_then(Bson::as_str).map(str::to_owned),
                document
                    .get("git_state_version")
                    .and_then(Bson::as_str)
                    .map(str::to_owned),
            ),
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
        let mut set = doc! {"version": &new_version, "updated_at": &now};
        // SQL `name = COALESCE(?, name)`: only touch the column when a new name
        // is supplied. The default model, by contrast, is always written (even
        // when explicitly cleared), matching the always-set column.
        if let Some(name) = name {
            set.insert("name", name.trim());
        }
        if let Some(model) = default_model_id {
            match model {
                Some(value) => {
                    set.insert("default_model_id", value);
                }
                None => {
                    set.insert("default_model_id", Bson::Null);
                }
            }
        }
        let mut work = self.unit_of_work.begin().await?;
        let changed = self
            .pool
            .collection::<Document>("projects")
            .update_one(
                doc! {"_id": id, "owner_id": owner_id, "version": expected_version},
                doc! {"$set": set},
            )
            .session(&mut *work.connection())
            .await?
            .matched_count;
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

    /// Attach an explicitly Automation-enabled GitHub credential to an
    /// existing Project without recloning its workspace. Fork-sync reuses
    /// Projects by repository URL, so an older public clone may need the
    /// dedicated push credential before its repair Session runs.
    pub async fn set_project_github_credential(
        &self,
        owner_id: &str,
        id: &str,
        credential_id: &str,
        correlation_id: &str,
    ) -> Result<ProjectView, ProjectsError> {
        let credential = self.fetch_credential(owner_id, credential_id).await?;
        if credential.pat_ciphertext.is_none() || !credential.automation_enabled {
            return Err(ProjectsError::Validation(
                "github credential must have a PAT and Automation enabled".into(),
            ));
        }
        let now = now_utc_str();
        let new_version = format!("v_{}", ProjectId::new());
        let mut work = self.unit_of_work.begin().await?;
        let changed = self
            .pool
            .collection::<Document>("projects")
            .update_one(
                doc! {"_id": id, "owner_id": owner_id, "state": {"$ne": "deleting"}},
                doc! {"$set": {
                    "repo_access": REPO_KIND_GITHUB_PRIVATE,
                    "github_credential_id": credential_id,
                    "version": &new_version,
                    "updated_at": &now,
                    "last_activity_at": &now,
                }},
            )
            .session(&mut *work.connection())
            .await?
            .matched_count;
        if changed == 0 {
            work.rollback().await?;
            return Err(ProjectsError::NotFound);
        }
        self.append_project_changed_in_tx(
            &mut work,
            owner_id,
            id,
            "project",
            "github_credential_attached",
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
        let changed = self
            .pool
            .collection::<Document>("projects")
            .update_one(
                doc! {
                    "_id": id,
                    "owner_id": owner_id,
                    "version": expected_version,
                    "state": {"$ne": "deleting"},
                },
                doc! {"$set": {"state": "deleting", "version": &new_version, "updated_at": &now}},
            )
            .session(&mut *work.connection())
            .await?
            .matched_count;
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
        let mut set = doc! {
            "state": "creating",
            "clone_error": null,
            "version": &new_version,
            "updated_at": &now,
            "last_activity_at": &now,
        };
        match &branch {
            Some(branch) => {
                set.insert("repo_branch", branch.as_str());
            }
            None => {
                set.insert("repo_branch", Bson::Null);
            }
        }
        match &cred {
            Some(cred) => {
                set.insert("github_credential_id", cred.as_str());
            }
            None => {
                set.insert("github_credential_id", Bson::Null);
            }
        }
        let mut work = self.unit_of_work.begin().await?;
        let changed = self
            .pool
            .collection::<Document>("projects")
            .update_one(
                doc! {"_id": id, "owner_id": owner_id, "state": "error"},
                doc! {"$set": set},
            )
            .session(&mut *work.connection())
            .await?
            .matched_count;
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
        let mut cursor = self
            .pool
            .collection::<Document>("github_credentials")
            .find(doc! {"owner_id": owner_id})
            .sort(doc! {"name": 1})
            .await?;
        let mut rows = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            rows.push(CredentialRow::from_doc(&document)?);
        }
        rows.into_iter().map(credential_view).collect()
    }

    /// List only credentials explicitly allowed to perform Automation pushes.
    /// Project/private-repository credentials remain outside this selection by
    /// default, even when they are the owner's only GitHub PAT.
    pub async fn list_automation_credentials(
        &self,
        owner_id: &str,
    ) -> Result<Vec<GithubCredentialView>, ProjectsError> {
        let mut cursor = self
            .pool
            .collection::<Document>("github_credentials")
            .find(doc! {"owner_id": owner_id, "automation_enabled": true})
            .sort(doc! {"name": 1})
            .await?;
        let mut rows = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            rows.push(CredentialRow::from_doc(&document)?);
        }
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
        let mut document = doc! {
            "_id": id.to_string(),
            "owner_id": owner_id,
            "name": input.name.trim(),
            "github_host": input.github_host.trim(),
            "pat_fingerprint": fingerprint,
            "automation_enabled": input.automation_enabled,
            "state": "ready",
            "version": &version,
            "created_at": &now,
            "updated_at": &now,
        };
        if let Some(ciphertext) = ciphertext {
            document.insert("pat_ciphertext", ciphertext);
        }
        let mut work = self.unit_of_work.begin().await?;
        self.pool
            .collection::<Document>("github_credentials")
            .insert_one(document)
            .session(&mut *work.connection())
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
        let existing = self
            .pool
            .collection::<Document>("github_credentials")
            .find_one(doc! {
                "owner_id": owner_id,
                "name": name,
                "github_host": github_host.trim(),
                "pat_fingerprint": pat_fingerprint,
            })
            .sort(doc! {"updated_at": -1})
            .await?;
        let existing = existing
            .map(|document| -> Result<(String, bool), ProjectsError> {
                let id = document.get_str("_id")?.to_owned();
                let automation_enabled = document
                    .get("automation_enabled")
                    .and_then(Bson::as_bool)
                    .unwrap_or(false);
                Ok((id, automation_enabled))
            })
            .transpose()?;
        if let Some((id, automation_enabled)) = existing {
            if !automation_enabled {
                self.set_automation_enabled(owner_id, &id, true, correlation_id)
                    .await?;
            }
            return Ok(id);
        }
        Ok(self
            .create_credential(
                owner_id,
                CreateGithubCredentialInput {
                    name: name.into(),
                    github_host: github_host.trim().into(),
                    pat: Some(pat.into()),
                    automation_enabled: true,
                },
                correlation_id,
            )
            .await?
            .id)
    }

    async fn set_automation_enabled(
        &self,
        owner_id: &str,
        id: &str,
        enabled: bool,
        correlation_id: &str,
    ) -> Result<(), ProjectsError> {
        let existing = self.fetch_credential(owner_id, id).await?;
        if existing.automation_enabled == enabled {
            return Ok(());
        }
        let now = now_utc_str();
        let version = format!("v_{}", GithubCredentialId::new());
        let mut work = self.unit_of_work.begin().await?;
        let changed = self
            .pool
            .collection::<Document>("github_credentials")
            .update_one(
                doc! {"_id": id, "owner_id": owner_id, "version": &existing.version},
                doc! {"$set": {
                    "automation_enabled": enabled,
                    "version": &version,
                    "updated_at": &now,
                }},
            )
            .session(&mut *work.connection())
            .await?
            .matched_count;
        if changed == 0 {
            work.rollback().await?;
            return Err(ProjectsError::CredentialNotFound);
        }
        self.append_project_changed_in_tx(
            &mut work,
            owner_id,
            id,
            "github_credential",
            "automation_scope_updated",
            correlation_id,
        )
        .await?;
        work.commit().await?;
        Ok(())
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
        let automation_enabled = input
            .automation_enabled
            .unwrap_or(existing.automation_enabled);
        let mut work = self.unit_of_work.begin().await?;
        let changed = self
            .pool
            .collection::<Document>("github_credentials")
            .update_one(
                doc! {"_id": id, "owner_id": owner_id, "version": expected_version},
                doc! {"$set": {
                    "name": name,
                    "github_host": host,
                    "pat_ciphertext": ciphertext,
                    "pat_fingerprint": fingerprint,
                    "automation_enabled": automation_enabled,
                    "version": &new_version,
                    "updated_at": &now,
                }},
            )
            .session(&mut *work.connection())
            .await?
            .matched_count;
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
        let dependent = self
            .pool
            .collection::<Document>("projects")
            .find_one(doc! {
                "owner_id": owner_id,
                "github_credential_id": id,
                "state": {"$ne": "deleting"},
            })
            .await?;
        if dependent.is_some() {
            return Err(ProjectsError::Validation(
                "credential is in use by a project; reassign or delete the project first".into(),
            ));
        }
        let mut work = self.unit_of_work.begin().await?;
        let deleted = self
            .pool
            .collection::<Document>("github_credentials")
            .delete_one(doc! {"_id": id, "owner_id": owner_id})
            .session(&mut *work.connection())
            .await?
            .deleted_count;
        if deleted == 0 {
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
        let document = self
            .pool
            .collection::<Document>("github_credentials")
            .find_one(doc! {"_id": id, "owner_id": owner_id})
            .await?;
        let Some(document) = document else {
            return Err(ProjectsError::CredentialNotFound);
        };
        CredentialRow::from_doc(&document)
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
    /// workspace directory and the Project-owned rows. Mongo has no ON DELETE
    /// CASCADE, and the collection-ownership rules forbid `projects` from
    /// writing git-state/conflict/workspace/session rows, so the application
    /// layer must additionally call `source_control.delete_project_state` and
    /// clear workspace copies for a fully consistent delete.
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
        self.pool
            .collection::<Document>("memories")
            .delete_many(doc! {"project_id": project_id})
            .await?;
        self.pool
            .collection::<Document>("projects")
            .delete_one(doc! {"_id": project_id, "owner_id": &owner_id})
            .await?;
        Ok(())
    }

    async fn project_owner(&self, project_id: &str) -> Result<String, ProjectsError> {
        let document = self
            .pool
            .collection::<Document>("projects")
            .find_one(doc! {"_id": project_id})
            .await?;
        let Some(document) = document else {
            return Err(ProjectsError::NotFound);
        };
        Ok(document.get_str("owner_id")?.to_owned())
    }

    async fn mark_project_state(
        &self,
        project_id: &str,
        state: &str,
        error: Option<String>,
    ) -> Result<(), ProjectsError> {
        let now = now_utc_str();
        let version = format!("v_{}", ProjectId::new());
        let mut set = doc! {
            "state": state,
            "version": &version,
            "updated_at": &now,
            "last_activity_at": &now,
        };
        match &error {
            Some(error) => {
                set.insert("clone_error", error.as_str());
            }
            None => {
                set.insert("clone_error", Bson::Null);
            }
        }
        self.pool
            .collection::<Document>("projects")
            .update_one(doc! {"_id": project_id}, doc! {"$set": set})
            .await?;
        Ok(())
    }

    async fn mark_project_ready(&self, project_id: &str) -> Result<(), ProjectsError> {
        let handle = format!("main:{project_id}");
        let now = now_utc_str();
        let version = format!("v_{}", ProjectId::new());
        self.pool
            .collection::<Document>("projects")
            .update_one(
                doc! {"_id": project_id},
                doc! {"$set": {
                    "state": "ready",
                    "clone_error": null,
                    "main_workspace_handle": &handle,
                    "version": &version,
                    "updated_at": &now,
                    "last_activity_at": &now,
                }},
            )
            .await?;
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
            .await
            .map_err(|error| map_workspace_path_error(error, "path not found"))?;
        let applied = self
            .workspace
            .apply_prepared_file_mutation(&lock, &prepared)
            .await
            .map_err(|error| map_workspace_path_error(error, "path not found"))?;
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
        work: &mut UnitOfWorkTransaction,
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
        work: &mut UnitOfWorkTransaction,
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

struct CredentialRow {
    id: String,
    name: String,
    github_host: String,
    pat_ciphertext: Option<Vec<u8>>,
    pat_fingerprint: Option<String>,
    automation_enabled: bool,
    version: String,
    created_at: String,
    updated_at: String,
}

impl CredentialRow {
    fn from_doc(document: &Document) -> Result<Self, ProjectsError> {
        Ok(Self {
            id: document.get_str("_id")?.to_owned(),
            name: document.get_str("name")?.to_owned(),
            github_host: document.get_str("github_host")?.to_owned(),
            pat_ciphertext: document
                .get("pat_ciphertext")
                .and_then(Bson::as_binary)
                .map(|binary| binary.bytes.clone()),
            pat_fingerprint: document
                .get("pat_fingerprint")
                .and_then(Bson::as_str)
                .map(str::to_owned),
            automation_enabled: document
                .get("automation_enabled")
                .and_then(Bson::as_bool)
                .unwrap_or(false),
            version: document.get_str("version")?.to_owned(),
            created_at: document.get_str("created_at")?.to_owned(),
            updated_at: document.get_str("updated_at")?.to_owned(),
        })
    }
}

fn credential_view(row: CredentialRow) -> Result<GithubCredentialView, ProjectsError> {
    Ok(GithubCredentialView {
        id: row.id,
        name: row.name,
        github_host: row.github_host,
        pat_is_set: row.pat_ciphertext.is_some(),
        pat_fingerprint: row.pat_fingerprint,
        automation_enabled: row.automation_enabled,
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
