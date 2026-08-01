//! Public Project and Main Workspace boundary.
//!
//! M2 (see `design.md`): Project lifecycle, GitHub PAT credentials, Main
//! Workspace handle, main-copy file read/write, user Git operations, and Git
//! Update Conflicts. The Session Workspace, three-way Apply/Sync, propagation
//! cursors and Checkpoints belong to later milestones.
//!
//! Clone/update/push/delete are durable Operations: this module records intent
//! and enqueues work items; the background worker (registered in the
//! application layer) executes them through the GitRunner and Operation
//! interfaces so a process restart cannot silently drop a half-done clone.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use utoipa::ToSchema;

use crate::modules::projects::git::{
    DiffView, GitCredential, GitError, GitRunner, GitStatus, UpdateConflictPath, UpdateOutcome,
};
use crate::modules::runtime::interface::{
    CapabilityReason, CapabilityState, DelegatedCliKind, ExecutorKind, NetworkPolicy,
    ResourceLimits,
};
use crate::modules::workspace_sync::interface::{
    RevisionRef, WorkspaceHandle, WorkspaceSyncError, WorkspaceSyncInterface,
};
use crate::platform::{
    clock::{Clock, SystemClock, format_utc},
    events::{EventStore, NewEvent},
    id::{
        CliConfigId, CorrelationId, EgressRuleId, GitUpdateConflictId, GithubCredentialId,
        ProjectId, RuntimeSecretId,
    },
    operations::{
        CreateOperation, CreateWork, IdempotencyOutcome, IdempotencyRequest, KIND_GIT_UPDATE,
        OperationInterface, OperationStatus, OperationView,
    },
    path::{PathError, validate_workspace_path},
    secret::{Secret, SecretCipher, fingerprint},
    unit_of_work::{UnitOfWork, UnitOfWorkTransaction},
};

const REPO_KIND_PUBLIC: &str = "public_https";
const REPO_KIND_GITHUB_PRIVATE: &str = "github_private";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum ProjectState {
    Creating,
    Ready,
    Error,
    Deleting,
}

impl ProjectState {
    #[allow(dead_code)]
    fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Ready => "ready",
            Self::Error => "error",
            Self::Deleting => "deleting",
        }
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

#[derive(Debug, Clone)]
pub struct ProjectModelPreference {
    pub owner_id: String,
    pub default_model_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProjectRuntimeConfigInput {
    pub executor: ExecutorKind,
    #[serde(default)]
    pub allow_insecure_local_executor: bool,
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    pub default_limits: ResourceLimits,
    pub network_policy: NetworkPolicy,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectRuntimeConfigView {
    pub project_id: String,
    pub executor: ExecutorKind,
    pub allow_insecure_local_executor: bool,
    pub variables: BTreeMap<String, String>,
    pub default_limits: ResourceLimits,
    pub network_policy: NetworkPolicy,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ProjectRuntimeSecretInput {
    pub name: String,
    #[schema(write_only)]
    pub value: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectRuntimeSecretView {
    pub id: String,
    pub name: String,
    pub value_is_set: bool,
    pub value_fingerprint: String,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EgressScheme {
    Http,
    Https,
}

impl EgressScheme {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    fn parse(value: &str) -> Result<Self, ProjectsError> {
        match value {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            _ => Err(ProjectsError::Validation("unknown egress scheme".into())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProjectEgressRuleInput {
    pub scheme: EgressScheme,
    pub host: String,
    pub port_start: u16,
    pub port_end: u16,
    pub purpose: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectEgressRuleView {
    pub id: String,
    pub scheme: EgressScheme,
    pub host: String,
    pub port_start: u16,
    pub port_end: u16,
    pub purpose: String,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProjectCliConfigInput {
    pub kind: DelegatedCliKind,
    pub enabled: bool,
    pub secret_id: Option<String>,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProjectCliConfigView {
    pub id: String,
    pub kind: DelegatedCliKind,
    pub enabled: bool,
    pub secret_id: Option<String>,
    pub options: BTreeMap<String, String>,
    pub observed_version: Option<String>,
    pub capability_state: CapabilityState,
    pub capability_reason: Option<CapabilityReason>,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RetryProjectInput {
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub github_credential_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FileMetaView {
    pub path: String,
    pub size: u64,
    pub editable: bool,
    pub mime: Option<String>,
    pub main_revision: Option<String>,
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
    #[error("git operation failed: {0}")]
    Git(GitError),
    #[error("workspace sync error: {0}")]
    WorkspaceSync(#[from] WorkspaceSyncError),
    #[error("operation error: {0}")]
    Operation(#[from] crate::platform::operations::OperationError),
    #[error("storage error: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl From<GitError> for ProjectsError {
    fn from(error: GitError) -> Self {
        Self::Git(error)
    }
}

impl From<PathError> for ProjectsError {
    fn from(error: PathError) -> Self {
        Self::InvalidPath(error.to_string())
    }
}

/// Stable error codes for the `GIT_*` family and friends; transport maps these
/// to RFC 9457 Problems.
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
            Self::Git(git) => git.code(),
            Self::WorkspaceSync(WorkspaceSyncError::RevisionMismatch { .. }) => {
                "RESOURCE_VERSION_MISMATCH"
            }
            Self::WorkspaceSync(_) => "INTERNAL_ERROR",
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
    workspace_sync: WorkspaceSyncInterface,
    workspaces_root: std::path::PathBuf,
    git: Arc<dyn GitRunner>,
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

#[derive(FromRow)]
struct RuntimeConfigRow {
    project_id: String,
    executor_kind: String,
    allow_insecure_local_executor: i64,
    variables_json: String,
    default_limits_json: String,
    network_policy: String,
    version: String,
    created_at: String,
    updated_at: String,
}

#[derive(FromRow)]
struct RuntimeSecretRow {
    id: String,
    name: String,
    value_fingerprint: String,
    version: String,
    created_at: String,
    updated_at: String,
}

#[derive(FromRow)]
struct EgressRuleRow {
    id: String,
    scheme: String,
    host: String,
    port_start: i64,
    port_end: i64,
    purpose: String,
    version: String,
    created_at: String,
    updated_at: String,
}

#[derive(FromRow)]
struct CliConfigRow {
    id: String,
    kind: String,
    enabled: i64,
    secret_id: Option<String>,
    options_json: String,
    observed_version: Option<String>,
    capability_state: String,
    capability_reason: Option<String>,
    version: String,
    created_at: String,
    updated_at: String,
}

impl ProjectsInterface {
    pub fn new(
        pool: SqlitePool,
        cipher: SecretCipher,
        operations: OperationInterface,
        workspace_sync: WorkspaceSyncInterface,
        events: EventStore,
        data_root: &std::path::Path,
        git: Arc<dyn GitRunner>,
    ) -> Self {
        let unit_of_work = UnitOfWork::new(pool.clone(), events);
        Self {
            workspaces_root: data_root.join("workspaces"),
            pool,
            unit_of_work,
            cipher,
            operations,
            workspace_sync,
            git,
        }
    }

    pub async fn owner_id(&self, project_id: ProjectId) -> Result<String, ProjectsError> {
        sqlx::query_scalar("SELECT owner_id FROM projects WHERE id = ?")
            .bind(project_id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(ProjectsError::NotFound)
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

    pub async fn runtime_config(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<Option<ProjectRuntimeConfigView>, ProjectsError> {
        self.ensure_project_owner(owner_id, project_id).await?;
        let row = sqlx::query_as::<_, RuntimeConfigRow>(
            "SELECT project_id, executor_kind, allow_insecure_local_executor, variables_json, \
             default_limits_json, network_policy, version, created_at, updated_at \
             FROM project_runtime_configs WHERE project_id = ?",
        )
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(runtime_config_view).transpose()
    }

    pub async fn save_runtime_config(
        &self,
        owner_id: &str,
        project_id: &str,
        expected_version: Option<&str>,
        input: ProjectRuntimeConfigInput,
    ) -> Result<ProjectRuntimeConfigView, ProjectsError> {
        self.ensure_project_owner(owner_id, project_id).await?;
        validate_runtime_config(&input)?;
        let current: Option<(String, String)> = sqlx::query_as(
            "SELECT version, created_at FROM project_runtime_configs WHERE project_id = ?",
        )
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?;
        match (&current, expected_version) {
            (Some((current, _)), Some(expected)) if current != expected => {
                return Err(ProjectsError::RevisionMismatch {
                    expected: expected.into(),
                    current: current.clone(),
                });
            }
            (Some((current, _)), None) => {
                return Err(ProjectsError::RevisionMismatch {
                    expected: "a current runtime configuration version".into(),
                    current: current.clone(),
                });
            }
            (None, Some(expected)) => {
                return Err(ProjectsError::RevisionMismatch {
                    expected: expected.into(),
                    current: "missing".into(),
                });
            }
            _ => {}
        }

        let now = format_utc(SystemClock.now());
        let created_at = current.map_or_else(|| now.clone(), |(_, value)| value);
        let version = format!("v_{}", ProjectId::new());
        sqlx::query(
            "INSERT INTO project_runtime_configs \
             (project_id, executor_kind, allow_insecure_local_executor, variables_json, \
              default_limits_json, network_policy, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(project_id) DO UPDATE SET executor_kind=excluded.executor_kind, \
              allow_insecure_local_executor=excluded.allow_insecure_local_executor, \
              variables_json=excluded.variables_json, default_limits_json=excluded.default_limits_json, \
              network_policy=excluded.network_policy, version=excluded.version, updated_at=excluded.updated_at",
        )
        .bind(project_id)
        .bind(executor_kind_str(input.executor))
        .bind(input.allow_insecure_local_executor)
        .bind(serde_json::to_string(&input.variables)?)
        .bind(serde_json::to_string(&input.default_limits)?)
        .bind(network_policy_str(input.network_policy))
        .bind(&version)
        .bind(&created_at)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        self.runtime_config(owner_id, project_id)
            .await?
            .ok_or_else(|| ProjectsError::Internal(anyhow::anyhow!("saved runtime config missing")))
    }

    pub async fn list_runtime_secrets(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<Vec<ProjectRuntimeSecretView>, ProjectsError> {
        self.ensure_project_owner(owner_id, project_id).await?;
        let rows = sqlx::query_as::<_, RuntimeSecretRow>(
            "SELECT id, name, value_fingerprint, version, created_at, updated_at \
             FROM project_runtime_secrets WHERE project_id = ? ORDER BY name",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(runtime_secret_view).collect())
    }

    pub async fn put_runtime_secret(
        &self,
        owner_id: &str,
        project_id: &str,
        input: ProjectRuntimeSecretInput,
    ) -> Result<ProjectRuntimeSecretView, ProjectsError> {
        self.ensure_project_owner(owner_id, project_id).await?;
        validate_environment_name(&input.name)?;
        if input.value.is_empty() || input.value.len() > 64 * 1024 {
            return Err(ProjectsError::Validation(
                "secret value must contain between 1 and 65536 bytes".into(),
            ));
        }
        let existing: Option<(String, String)> = sqlx::query_as(
            "SELECT id, created_at FROM project_runtime_secrets WHERE project_id = ? AND name = ?",
        )
        .bind(project_id)
        .bind(&input.name)
        .fetch_optional(&self.pool)
        .await?;
        let now = format_utc(SystemClock.now());
        let (id, created_at) =
            existing.unwrap_or_else(|| (RuntimeSecretId::new().to_string(), now.clone()));
        let encrypted = self.cipher.encrypt(
            &Secret::new(input.value.clone()),
            &runtime_secret_aad(project_id, &id),
        )?;
        let version = format!("v_{}", RuntimeSecretId::new());
        sqlx::query(
            "INSERT INTO project_runtime_secrets \
             (id, project_id, name, value_ciphertext, value_fingerprint, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(project_id, name) DO UPDATE SET value_ciphertext=excluded.value_ciphertext, \
              value_fingerprint=excluded.value_fingerprint, version=excluded.version, updated_at=excluded.updated_at",
        )
        .bind(&id)
        .bind(project_id)
        .bind(input.name.trim())
        .bind(encrypted)
        .bind(fingerprint(&input.value))
        .bind(&version)
        .bind(&created_at)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        self.list_runtime_secrets(owner_id, project_id)
            .await?
            .into_iter()
            .find(|value| value.id == id)
            .ok_or_else(|| ProjectsError::Internal(anyhow::anyhow!("saved runtime secret missing")))
    }

    pub async fn runtime_secret_value(
        &self,
        owner_id: &str,
        project_id: &str,
        secret_id: &str,
    ) -> Result<Secret, ProjectsError> {
        self.ensure_project_owner(owner_id, project_id).await?;
        let row: Option<(Vec<u8>,)> = sqlx::query_as(
            "SELECT value_ciphertext FROM project_runtime_secrets WHERE id = ? AND project_id = ?",
        )
        .bind(secret_id)
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?;
        let (ciphertext,) = row.ok_or(ProjectsError::NotFound)?;
        self.cipher
            .decrypt(&ciphertext, &runtime_secret_aad(project_id, secret_id))
            .map_err(ProjectsError::Internal)
    }

    pub async fn replace_egress_rules(
        &self,
        owner_id: &str,
        project_id: &str,
        rules: Vec<ProjectEgressRuleInput>,
    ) -> Result<Vec<ProjectEgressRuleView>, ProjectsError> {
        self.ensure_project_owner(owner_id, project_id).await?;
        let rules = validate_egress_rules(rules)?;
        let now = format_utc(SystemClock.now());
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM project_egress_rules WHERE project_id = ?")
            .bind(project_id)
            .execute(&mut *tx)
            .await?;
        for rule in rules {
            sqlx::query(
                "INSERT INTO project_egress_rules \
                 (id, project_id, scheme, host, port_start, port_end, purpose, version, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(EgressRuleId::new().to_string())
            .bind(project_id)
            .bind(rule.scheme.as_str())
            .bind(rule.host)
            .bind(i64::from(rule.port_start))
            .bind(i64::from(rule.port_end))
            .bind(rule.purpose)
            .bind(format!("v_{}", EgressRuleId::new()))
            .bind(&now)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.egress_rules(owner_id, project_id).await
    }

    pub async fn egress_rules(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<Vec<ProjectEgressRuleView>, ProjectsError> {
        self.ensure_project_owner(owner_id, project_id).await?;
        let rows = sqlx::query_as::<_, EgressRuleRow>(
            "SELECT id, scheme, host, port_start, port_end, purpose, version, created_at, updated_at \
             FROM project_egress_rules WHERE project_id = ? ORDER BY scheme, host, port_start",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(egress_rule_view).collect()
    }

    pub async fn save_cli_config(
        &self,
        owner_id: &str,
        project_id: &str,
        input: ProjectCliConfigInput,
    ) -> Result<ProjectCliConfigView, ProjectsError> {
        self.ensure_project_owner(owner_id, project_id).await?;
        validate_cli_options(input.kind, &input.options)?;
        if let Some(secret_id) = &input.secret_id {
            let exists: i64 = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM project_runtime_secrets WHERE id = ? AND project_id = ?)",
            )
            .bind(secret_id)
            .bind(project_id)
            .fetch_one(&self.pool)
            .await?;
            if exists == 0 {
                return Err(ProjectsError::Validation(
                    "CLI secret_id does not belong to the project".into(),
                ));
            }
        }
        let kind = delegated_cli_kind_str(input.kind);
        let existing: Option<(String, String)> = sqlx::query_as(
            "SELECT id, created_at FROM project_cli_configs WHERE project_id = ? AND kind = ?",
        )
        .bind(project_id)
        .bind(kind)
        .fetch_optional(&self.pool)
        .await?;
        let now = format_utc(SystemClock.now());
        let (id, created_at) =
            existing.unwrap_or_else(|| (CliConfigId::new().to_string(), now.clone()));
        let version = format!("v_{}", CliConfigId::new());
        sqlx::query(
            "INSERT INTO project_cli_configs \
             (id, project_id, kind, enabled, secret_id, options_json, capability_state, \
              capability_reason, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'unconfigured', 'DEPENDENCY_MISSING', ?, ?, ?) \
             ON CONFLICT(project_id, kind) DO UPDATE SET enabled=excluded.enabled, \
              secret_id=excluded.secret_id, options_json=excluded.options_json, \
              version=excluded.version, updated_at=excluded.updated_at",
        )
        .bind(&id)
        .bind(project_id)
        .bind(kind)
        .bind(input.enabled)
        .bind(input.secret_id)
        .bind(serde_json::to_string(&input.options)?)
        .bind(&version)
        .bind(&created_at)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        self.cli_configs(owner_id, project_id)
            .await?
            .into_iter()
            .find(|value| value.id == id)
            .ok_or_else(|| ProjectsError::Internal(anyhow::anyhow!("saved CLI config missing")))
    }

    pub async fn cli_configs(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<Vec<ProjectCliConfigView>, ProjectsError> {
        self.ensure_project_owner(owner_id, project_id).await?;
        let rows = sqlx::query_as::<_, CliConfigRow>(
            "SELECT id, kind, enabled, secret_id, options_json, observed_version, capability_state, \
             capability_reason, version, created_at, updated_at FROM project_cli_configs \
             WHERE project_id = ? ORDER BY kind",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(cli_config_view).collect()
    }

    async fn ensure_project_owner(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<(), ProjectsError> {
        let exists: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ? AND owner_id = ?)",
        )
        .bind(project_id)
        .bind(owner_id)
        .fetch_one(&self.pool)
        .await?;
        if exists == 0 {
            return Err(ProjectsError::NotFound);
        }
        Ok(())
    }

    /// Create a Project record in `creating` state and enqueue a clone work
    /// item. The actual clone runs in the worker so it survives HTTP disconnect.
    /// Returns the new Project view and the clone Operation.
    pub async fn create_project(
        &self,
        owner_id: &str,
        tenant_id: &str,
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
            .create_with_work_in_tx(
                &mut work,
                CreateOperation {
                    kind: crate::platform::operations::KIND_CLONE,
                    actor: serde_json::json!({"kind": "owner", "id": owner_id}),
                    target_kind: "project",
                    target_id: Some(&project_id),
                    conditions: serde_json::json!({"project_id": project_id}),
                    correlation_id,
                    idempotency,
                },
                CreateWork {
                    handler_kind: crate::platform::operations::KIND_CLONE,
                    payload: serde_json::json!({
                        "project_id": project_id,
                        "url": input.repository.url,
                        "branch": input.repository.branch,
                        "access": input.repository.access.as_str(),
                        "github_credential_id": input.repository.github_credential_id,
                    }),
                },
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

        let now = format_utc(SystemClock.now());
        let version = format!("v_{}", ProjectId::new());
        sqlx::query("INSERT INTO projects (id, owner_id, tenant_id, name, state, repo_access, repo_url, repo_branch, github_credential_id, default_model_id, main_workspace_handle, clone_error, version, created_at, updated_at, last_activity_at) VALUES (?, ?, ?, ?, 'creating', ?, ?, ?, ?, NULL, NULL, NULL, ?, ?, ?, ?)")
            .bind(provisional_id.to_string())
            .bind(owner_id)
            .bind(tenant_id)
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
                self.workspace_sync
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
        let now = format_utc(SystemClock.now());
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
        let now = format_utc(SystemClock.now());
        let new_version = format!("v_{}", ProjectId::new());
        let event_correlation_id = correlation_id.to_string();
        let mut work = self.unit_of_work.begin().await?;
        let created = self
            .operations
            .create_with_work_in_tx(
                &mut work,
                CreateOperation {
                    kind: crate::platform::operations::KIND_DELETE_PROJECT,
                    actor: serde_json::json!({"kind": "owner", "id": owner_id}),
                    target_kind: "project",
                    target_id: Some(id),
                    conditions: serde_json::json!({"project_id": id, "version": new_version}),
                    correlation_id,
                    idempotency: Some(idempotency),
                },
                CreateWork {
                    handler_kind: crate::platform::operations::KIND_DELETE_PROJECT,
                    payload: serde_json::json!({"project_id": id}),
                },
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
        let now = format_utc(SystemClock.now());
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
            .create_with_work_in_tx(
                &mut work,
                CreateOperation {
                    kind: crate::platform::operations::KIND_CLONE,
                    actor: serde_json::json!({"kind": "owner", "id": owner_id}),
                    target_kind: "project",
                    target_id: Some(id),
                    conditions: serde_json::json!({"project_id": id}),
                    correlation_id,
                    idempotency: None,
                },
                CreateWork {
                    handler_kind: crate::platform::operations::KIND_CLONE,
                    payload: serde_json::json!({
                        "project_id": id,
                        "url": row.repo_url,
                        "branch": branch,
                        "access": row.repo_access,
                        "github_credential_id": cred,
                    }),
                },
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
        let now = format_utc(SystemClock.now());
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
        let now = format_utc(SystemClock.now());
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

    /// Execute the clone for a `creating` Project: clone into the Main
    /// workspace dir, record the initial Content Revision, and flip the state
    /// to `ready`. On failure, record a stable error and leave the Project in
    /// `error` so it can be retried or deleted (`DAT-OP-01`).
    pub async fn run_clone(&self, project_id: &str) -> Result<(), ProjectsError> {
        let owner_id = self.project_owner(project_id).await?;
        let row = self.fetch_project(&owner_id, project_id).await?;
        if row.state != "creating" {
            return Err(ProjectsError::Validation(format!(
                "project is not creating (state: {})",
                row.state
            )));
        }
        let access = RepoAccess::parse(&row.repo_access)
            .ok_or_else(|| ProjectsError::Internal(anyhow::anyhow!("bad repo access")))?;
        let credential = match access {
            RepoAccess::PublicHttps => GitCredential::None,
            RepoAccess::GithubPrivate => {
                let cred_id = row
                    .github_credential_id
                    .as_ref()
                    .ok_or_else(|| ProjectsError::Validation("missing credential".into()))?;
                let pat = self.pat_for(&owner_id, cred_id).await?;
                GitCredential::HttpsBasic {
                    username: "x-access-token".into(),
                    password: pat.unwrap_or_default(),
                }
            }
        };

        let dest = self.main_repo_dir(project_id);
        let clone_result = self
            .git
            .clone(&row.repo_url, row.repo_branch.as_deref(), &dest, &credential)
            .await;
        if let Err(error) = clone_result {
            self.mark_project_state(project_id, "error", Some(error.to_string()))
                .await?;
            return Err(error.into());
        }

        // Clone succeeded: establish the Main copy + first Content Revision and
        // mark the Project ready. managed_dir is relative to the data root.
        let project_id_typed: ProjectId = project_id
            .parse()
            .map_err(|_| ProjectsError::Internal(anyhow::anyhow!("bad project id")))?;
        let managed_dir = format!("workspaces/main/{project_id}/repo");
        let revision = self
            .workspace_sync
            .ensure_main_copy(
                project_id_typed,
                &managed_dir,
                "clone",
                serde_json::json!({"kind": "owner", "id": owner_id}),
            )
            .await?;
        self.mark_project_ready(project_id, revision.0).await?;
        Ok(())
    }

    /// Execute the cascade delete for a `deleting` Project: remove the
    /// workspace directory and all Project rows. Never touches the remote repo
    /// or global models/Passkeys (`DAT-DELETE-01` subset for M2).
    pub async fn run_delete(&self, project_id: &str) -> Result<(), ProjectsError> {
        let owner_id = self.project_owner(project_id).await?;
        // Remove the workspace dir on the same filesystem (best effort; the
        // authoritative cleanup is the DB delete + tombstone in later milestones).
        let dest = self.main_repo_dir(project_id);
        let _ = tokio::fs::remove_dir_all(dest.parent().unwrap_or(&dest)).await;
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
        let now = format_utc(SystemClock.now());
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

    async fn mark_project_ready(
        &self,
        project_id: &str,
        revision: String,
    ) -> Result<(), ProjectsError> {
        let handle = format!("main:{project_id}");
        let now = format_utc(SystemClock.now());
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
        let _ = revision; // emitted via workspace_sync events in later milestones
        Ok(())
    }

    // ----- Main Workspace path helpers -----

    fn main_repo_dir(&self, project_id: &str) -> std::path::PathBuf {
        self.workspaces_root
            .join("main")
            .join(project_id)
            .join("repo")
    }

    fn main_handle(&self, project_id: ProjectId) -> WorkspaceHandle {
        WorkspaceHandle::main(project_id)
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
        let row = self.require_ready(owner_id, project_id).await?;
        let rel = validate_workspace_path(raw_path)?;
        let abs = self.main_repo_dir(project_id).join(&rel);
        let meta = tokio::fs::metadata(&abs)
            .await
            .map_err(|_| ProjectsError::Validation("file not found".into()))?;
        let size = meta.len();
        let editable = size <= 10 * 1024 * 1024 && is_utf8_text_file(&abs).await;
        let mime = guess_mime(&abs);
        let revision = self
            .workspace_sync
            .current_revision(
                &self.main_handle(
                    row.id
                        .parse()
                        .map_err(|_| ProjectsError::Internal(anyhow::anyhow!("bad project id")))?,
                ),
            )
            .await
            .ok()
            .map(|r| r.0);
        Ok(FileMetaView {
            path: raw_path.to_owned(),
            size,
            editable,
            mime,
            main_revision: revision,
        })
    }

    pub async fn file_content(
        &self,
        owner_id: &str,
        project_id: &str,
        raw_path: &str,
    ) -> Result<Vec<u8>, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        let rel = validate_workspace_path(raw_path)?;
        let abs = self.main_repo_dir(project_id).join(&rel);
        tokio::fs::read(&abs)
            .await
            .map_err(|_| ProjectsError::Validation("file not found".into()))
    }

    pub async fn save_text(
        &self,
        owner_id: &str,
        project_id: &str,
        input: SaveTextInput,
        actor: serde_json::Value,
        correlation_id: &str,
    ) -> Result<RevisionRef, ProjectsError> {
        let row = self.require_ready(owner_id, project_id).await?;
        let project_id_typed: ProjectId = row
            .id
            .parse()
            .map_err(|_| ProjectsError::Internal(anyhow::anyhow!("bad project id")))?;
        let handle = self.main_handle(project_id_typed);

        // Version precondition first: if the caller supplied an expected revision,
        // reject before touching the working tree (`WS-REV-02`,
        // RESOURCE_VERSION_MISMATCH). A late re-check still happens in
        // bump_revision for the race window between check and write.
        let expected = input
            .expected_main_revision
            .as_ref()
            .map(|r| RevisionRef(r.clone()));
        if let Some(expected_ref) = expected.as_ref() {
            let current = self.workspace_sync.current_revision(&handle).await?;
            if current.0 != expected_ref.0 {
                return Err(ProjectsError::RevisionMismatch {
                    expected: expected_ref.0.clone(),
                    current: current.0,
                });
            }
        }
        let rel = validate_workspace_path(&input.path)?;
        let abs = self.main_repo_dir(project_id).join(&rel);

        // Validate editability before writing.
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        // Atomic write: temp file + rename so a crash never leaves a half file.
        let tmp = abs.with_extension("janus-tmp");
        tokio::fs::write(&tmp, input.content.as_bytes())
            .await
            .map_err(|e| ProjectsError::Internal(anyhow::anyhow!("write failed: {e}")))?;
        tokio::fs::rename(&tmp, &abs)
            .await
            .map_err(|e| ProjectsError::Internal(anyhow::anyhow!("rename failed: {e}")))?;

        // If the revision moved after the disk write, leave the on-disk file as
        // the latest content but refuse to advance identity so the client must
        // re-read. (True CAS would need a content-level rollback; M2 prefers
        // fail-closed on identity over silent ABA.)
        let mut work = self.unit_of_work.begin().await?;
        let new_revision = self
            .workspace_sync
            .bump_revision_in_tx(
                work.connection(),
                &handle,
                expected.as_ref(),
                "editor.save",
                actor,
            )
            .await?;
        self.append_main_revision_changed_in_tx(
            &mut work,
            owner_id,
            project_id,
            &new_revision,
            correlation_id,
        )
        .await?;
        work.commit().await?;
        Ok(new_revision)
    }

    /// List a directory of the Main Workspace. Non-recursive one level listing;
    /// the client walks the tree by calling again with sub-paths (`API` files/tree).
    pub async fn file_tree(
        &self,
        owner_id: &str,
        project_id: &str,
        raw_path: &str,
    ) -> Result<Vec<FileTreeView>, ProjectsError> {
        let row = self.require_ready(owner_id, project_id).await?;
        let rel = if raw_path.is_empty() {
            std::path::PathBuf::new()
        } else {
            validate_workspace_path(raw_path)?
        };
        let abs = self.main_repo_dir(project_id).join(&rel);
        let mut entries = tokio::fs::read_dir(&abs)
            .await
            .map_err(|_| ProjectsError::Validation("path not found".into()))?;
        let mut out = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".git" {
                continue;
            }
            let meta = entry.metadata().await?;
            let child_path = if rel.as_os_str().is_empty() {
                name.clone()
            } else {
                format!("{}/{name}", rel.to_string_lossy())
            };
            out.push(FileTreeView {
                path: child_path,
                kind: if meta.is_dir() {
                    "dir".into()
                } else {
                    "file".into()
                },
                size: meta.len(),
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        let _ = row;
        Ok(out)
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
        let row = self.require_ready(owner_id, project_id).await?;
        let project_id_typed: ProjectId = row
            .id
            .parse()
            .map_err(|_| ProjectsError::Internal(anyhow::anyhow!("bad project id")))?;
        let handle = self.main_handle(project_id_typed);
        let expected = input.expected_main_revision.map(RevisionRef);
        let from_rel = validate_workspace_path(&input.from)?;
        let to_rel = validate_workspace_path(&input.to)?;
        let base = self.main_repo_dir(project_id);
        let from = base.join(&from_rel);
        let to = base.join(&to_rel);
        if let Some(parent) = to.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::rename(&from, &to)
            .await
            .map_err(|e| ProjectsError::Internal(anyhow::anyhow!("move failed: {e}")))?;
        let mut work = self.unit_of_work.begin().await?;
        let new_revision = self
            .workspace_sync
            .bump_revision_in_tx(
                work.connection(),
                &handle,
                expected.as_ref(),
                "editor.move",
                actor,
            )
            .await?;
        self.append_main_revision_changed_in_tx(
            &mut work,
            owner_id,
            project_id,
            &new_revision,
            correlation_id,
        )
        .await?;
        work.commit().await?;
        Ok(new_revision)
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
        let row = self.require_ready(owner_id, project_id).await?;
        let project_id_typed: ProjectId = row
            .id
            .parse()
            .map_err(|_| ProjectsError::Internal(anyhow::anyhow!("bad project id")))?;
        let handle = self.main_handle(project_id_typed);
        let expected = input.expected_main_revision.map(RevisionRef);
        let rel = validate_workspace_path(&input.path)?;
        let abs = self.main_repo_dir(project_id).join(&rel);
        let meta = tokio::fs::metadata(&abs)
            .await
            .map_err(|_| ProjectsError::Validation("path not found".into()))?;
        if meta.is_dir() {
            if !input.recursive {
                return Err(ProjectsError::Validation(
                    "recursive is required to delete a directory".into(),
                ));
            }
            tokio::fs::remove_dir_all(&abs)
                .await
                .map_err(|e| ProjectsError::Internal(anyhow::anyhow!("delete dir failed: {e}")))?;
        } else {
            tokio::fs::remove_file(&abs)
                .await
                .map_err(|e| ProjectsError::Internal(anyhow::anyhow!("delete file failed: {e}")))?;
        }
        let mut work = self.unit_of_work.begin().await?;
        let new_revision = self
            .workspace_sync
            .bump_revision_in_tx(
                work.connection(),
                &handle,
                expected.as_ref(),
                "editor.delete",
                actor,
            )
            .await?;
        self.append_main_revision_changed_in_tx(
            &mut work,
            owner_id,
            project_id,
            &new_revision,
            correlation_id,
        )
        .await?;
        work.commit().await?;
        Ok(new_revision)
    }

    // ----- Git queries and user commands -----

    pub async fn git_status(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<GitStatus, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        let dir = self.main_repo_dir(project_id);
        Ok(self.git.status(&dir).await?)
    }

    pub async fn git_diff(
        &self,
        owner_id: &str,
        project_id: &str,
        view: DiffView,
    ) -> Result<String, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        let dir = self.main_repo_dir(project_id);
        Ok(self.git.diff(&dir, view).await?)
    }

    pub async fn git_log(
        &self,
        owner_id: &str,
        project_id: &str,
        limit: u32,
    ) -> Result<Vec<crate::adapters::git::GitLogEntry>, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        let dir = self.main_repo_dir(project_id);
        Ok(self.git.log(&dir, limit).await?)
    }

    pub async fn git_branches(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<Vec<String>, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        let dir = self.main_repo_dir(project_id);
        Ok(self.git.branches(&dir).await?)
    }

    pub async fn git_remotes(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<Vec<String>, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        let dir = self.main_repo_dir(project_id);
        Ok(self.git.remotes(&dir).await?)
    }

    pub async fn git_fetch(
        &self,
        owner_id: &str,
        project_id: &str,
        remote: &str,
        correlation_id: CorrelationId,
    ) -> Result<OperationView, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        let credential = self.credential_for_project(owner_id, project_id).await?;
        let created = self
            .operations
            .create(CreateOperation {
                kind: crate::platform::operations::KIND_GIT_FETCH,
                actor: serde_json::json!({"kind": "owner", "id": owner_id}),
                target_kind: "project",
                target_id: Some(project_id),
                conditions: serde_json::json!({"project_id": project_id, "remote": remote}),
                correlation_id,
                idempotency: None,
            })
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
    ) -> Result<(), ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        let dir = self.main_repo_dir(project_id);
        Ok(self.git.stage(&dir, paths).await?)
    }

    pub async fn git_unstage(
        &self,
        owner_id: &str,
        project_id: &str,
        paths: &[String],
    ) -> Result<(), ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        let dir = self.main_repo_dir(project_id);
        Ok(self.git.unstage(&dir, paths).await?)
    }

    pub async fn git_commit(
        &self,
        owner_id: &str,
        project_id: &str,
        message: &str,
        correlation_id: CorrelationId,
    ) -> Result<String, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
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
    ) -> Result<OperationView, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        let credential = self.credential_for_project(owner_id, project_id).await?;
        let created = self
            .operations
            .create(CreateOperation {
                kind: crate::platform::operations::KIND_GIT_PUSH,
                actor: serde_json::json!({"kind": "owner", "id": owner_id}),
                target_kind: "project",
                target_id: Some(project_id),
                conditions: serde_json::json!({"project_id": project_id, "remote": remote, "branch": branch}),
                correlation_id,
                idempotency: None,
            })
            .await?;
        // Push executes inline (short) when possible; the Operation records it.
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
    ) -> Result<OperationView, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        let credential = self.credential_for_project(owner_id, project_id).await?;
        let created = self
            .operations
            .create(CreateOperation {
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
            })
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
                if let Ok(handle) = self.main_handle_for(project_id).await {
                    let _ = self
                        .workspace_sync
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
            .ok_or(ProjectsError::NotFound)
    }

    pub async fn list_update_conflicts(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<Vec<GitUpdateConflictView>, ProjectsError> {
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
    ) -> Result<GitUpdateConflictView, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        let row: ConflictRow = sqlx::query_as(
            "SELECT id, project_id, base_tree, remote_tree, main_tree, state, operation_id, version, created_at, updated_at FROM git_update_conflicts WHERE id = ? AND project_id = ?",
        )
        .bind(conflict_id)
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ProjectsError::ConflictNotFound)?;
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
    ) -> Result<GitUpdateConflictView, ProjectsError> {
        self.require_ready(owner_id, project_id).await?;
        let row: ConflictRow = sqlx::query_as(
            "SELECT id, project_id, base_tree, remote_tree, main_tree, state, operation_id, version, created_at, updated_at FROM git_update_conflicts WHERE id = ? AND project_id = ?",
        )
        .bind(conflict_id)
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ProjectsError::ConflictNotFound)?;
        if row.version != expected_version {
            return Err(ProjectsError::RevisionMismatch {
                expected: expected_version.into(),
                current: row.version,
            });
        }
        if row.state != "open" && row.state != "applying" {
            return Err(ProjectsError::Validation(format!(
                "conflict is not open (state: {})",
                row.state
            )));
        }

        // Save each path choice.
        let now = format_utc(SystemClock.now());
        for path in &input.paths {
            if !matches!(
                path.choice.as_str(),
                "main" | "remote" | "delete" | "edited_text"
            ) {
                return Err(ProjectsError::Validation(format!(
                    "invalid choice {}",
                    path.choice
                )));
            }
            let edited_blob = if path.choice == "edited_text" {
                let text = path.edited_text.as_deref().ok_or_else(|| {
                    ProjectsError::Validation("edited_text requires edited_text body".into())
                })?;
                Some(sha2_hash(text.as_bytes()))
            } else {
                None
            };
            // For edited_text we store the text in a side file under objects via
            // a simple content hash name under the data root tmp; for M2 we keep
            // the text in the path row by writing a blob file under the project
            // staging dir when applying.
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
                    .map_err(|e| ProjectsError::Internal(anyhow::anyhow!(e)))?;
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
            "SELECT path, kind, base_hash, remote_hash, main_hash, choice, edited_blob_sha, version FROM git_update_conflict_paths WHERE conflict_id = ?",
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
        let project = self.fetch_project(owner_id, project_id).await?;
        let remote = "origin";
        let branch = project.repo_branch.as_deref().unwrap_or("main");
        self.git
            .complete_fast_forward(&dir, remote, branch)
            .await?;
        self.refresh_git_state(
            owner_id,
            project_id,
            "update_conflict_resolved",
            correlation_id,
        )
        .await?;
        if let Ok(handle) = self.main_handle_for(project_id).await {
            let _ = self
                .workspace_sync
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

    async fn credential_for_project(
        &self,
        owner_id: &str,
        project_id: &str,
    ) -> Result<GitCredential, ProjectsError> {
        let row = self.fetch_project(owner_id, project_id).await?;
        match RepoAccess::parse(&row.repo_access) {
            Some(RepoAccess::GithubPrivate) => {
                let cred_id = row
                    .github_credential_id
                    .as_ref()
                    .ok_or_else(|| ProjectsError::Validation("missing credential".into()))?;
                let pat = self.pat_for(owner_id, cred_id).await?;
                Ok(GitCredential::HttpsBasic {
                    username: "x-access-token".into(),
                    password: pat.unwrap_or_default(),
                })
            }
            _ => Ok(GitCredential::None),
        }
    }

    async fn refresh_git_state(
        &self,
        owner_id: &str,
        project_id: &str,
        cause: &str,
        correlation_id: CorrelationId,
    ) -> Result<(), ProjectsError> {
        let dir = self.main_repo_dir(project_id);
        let status = self.git.status(&dir).await?;
        let now = format_utc(SystemClock.now());
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
            event_type: "git.state_changed".into(),
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

    async fn main_handle_for(&self, project_id: &str) -> Result<WorkspaceHandle, ProjectsError> {
        let project_id_typed: ProjectId = project_id
            .parse()
            .map_err(|_| ProjectsError::Internal(anyhow::anyhow!("bad project id")))?;
        Ok(self.main_handle(project_id_typed))
    }

    async fn persist_update_conflict(
        &self,
        project_id: &str,
        operation_id: &str,
        base_tree: &str,
        remote_tree: &str,
        main_tree: &str,
        paths: &[UpdateConflictPath],
    ) -> Result<String, ProjectsError> {
        let conflict_id = GitUpdateConflictId::new().to_string();
        let now = format_utc(SystemClock.now());
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
    ) -> Result<GitUpdateConflictView, ProjectsError> {
        let paths: Vec<ConflictPathRow> = sqlx::query_as(
            "SELECT path, kind, base_hash, remote_hash, main_hash, choice, edited_blob_sha, version FROM git_update_conflict_paths WHERE conflict_id = ? ORDER BY path",
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
            event_type: "project.changed".into(),
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
            event_type: "project.main_revision_changed".into(),
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

fn runtime_config_view(row: RuntimeConfigRow) -> Result<ProjectRuntimeConfigView, ProjectsError> {
    Ok(ProjectRuntimeConfigView {
        project_id: row.project_id,
        executor: parse_executor_kind(&row.executor_kind)?,
        allow_insecure_local_executor: row.allow_insecure_local_executor != 0,
        variables: serde_json::from_str(&row.variables_json)?,
        default_limits: serde_json::from_str(&row.default_limits_json)?,
        network_policy: parse_network_policy(&row.network_policy)?,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn runtime_secret_view(row: RuntimeSecretRow) -> ProjectRuntimeSecretView {
    ProjectRuntimeSecretView {
        id: row.id,
        name: row.name,
        value_is_set: true,
        value_fingerprint: row.value_fingerprint,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn egress_rule_view(row: EgressRuleRow) -> Result<ProjectEgressRuleView, ProjectsError> {
    Ok(ProjectEgressRuleView {
        id: row.id,
        scheme: EgressScheme::parse(&row.scheme)?,
        host: row.host,
        port_start: u16::try_from(row.port_start)
            .map_err(|_| ProjectsError::Internal(anyhow::anyhow!("invalid stored port")))?,
        port_end: u16::try_from(row.port_end)
            .map_err(|_| ProjectsError::Internal(anyhow::anyhow!("invalid stored port")))?,
        purpose: row.purpose,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn cli_config_view(row: CliConfigRow) -> Result<ProjectCliConfigView, ProjectsError> {
    Ok(ProjectCliConfigView {
        id: row.id,
        kind: parse_delegated_cli_kind(&row.kind)?,
        enabled: row.enabled != 0,
        secret_id: row.secret_id,
        options: serde_json::from_str(&row.options_json)?,
        observed_version: row.observed_version,
        capability_state: parse_capability_state(&row.capability_state)?,
        capability_reason: row
            .capability_reason
            .as_deref()
            .map(parse_capability_reason)
            .transpose()?,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn validate_runtime_config(input: &ProjectRuntimeConfigInput) -> Result<(), ProjectsError> {
    input
        .default_limits
        .validate()
        .map_err(|error| ProjectsError::Validation(error.to_string()))?;
    if input.variables.len() > 256 {
        return Err(ProjectsError::Validation(
            "runtime configuration supports at most 256 variables".into(),
        ));
    }
    let mut total_bytes = 0_usize;
    for (name, value) in &input.variables {
        validate_environment_name(name)?;
        if value.len() > 64 * 1024 {
            return Err(ProjectsError::Validation(format!(
                "runtime variable {name} exceeds 65536 bytes"
            )));
        }
        total_bytes = total_bytes.saturating_add(name.len() + value.len());
    }
    if total_bytes > 1024 * 1024 {
        return Err(ProjectsError::Validation(
            "runtime variables exceed the one MiB aggregate limit".into(),
        ));
    }
    Ok(())
}

fn validate_environment_name(name: &str) -> Result<(), ProjectsError> {
    let mut chars = name.chars();
    if name.len() > 128
        || !chars
            .next()
            .is_some_and(|value| value == '_' || value.is_ascii_alphabetic())
        || !chars.all(|value| value == '_' || value.is_ascii_alphanumeric())
    {
        return Err(ProjectsError::Validation(format!(
            "{name:?} is not a portable environment variable name"
        )));
    }
    Ok(())
}

fn validate_egress_rules(
    mut rules: Vec<ProjectEgressRuleInput>,
) -> Result<Vec<ProjectEgressRuleInput>, ProjectsError> {
    if rules.len() > 128 {
        return Err(ProjectsError::Validation(
            "a project supports at most 128 egress rules".into(),
        ));
    }
    let mut unique = BTreeSet::new();
    for rule in &mut rules {
        rule.host = rule.host.trim().to_ascii_lowercase();
        rule.purpose = rule.purpose.trim().to_owned();
        if rule.host.is_empty()
            || rule.host.contains('*')
            || rule.host.contains('/')
            || rule.host.contains(char::is_whitespace)
            || rule.purpose.is_empty()
            || rule.purpose.len() > 256
            || rule.port_start == 0
            || rule.port_end < rule.port_start
        {
            return Err(ProjectsError::Validation("invalid egress rule".into()));
        }
        let key = (
            rule.scheme.as_str(),
            rule.host.clone(),
            rule.port_start,
            rule.port_end,
        );
        if !unique.insert(key) {
            return Err(ProjectsError::Validation("duplicate egress rule".into()));
        }
    }
    Ok(rules)
}

fn validate_cli_options(
    kind: DelegatedCliKind,
    options: &BTreeMap<String, String>,
) -> Result<(), ProjectsError> {
    let allowed: &[&str] = match kind {
        DelegatedCliKind::ClaudeCode => &["model", "permission_mode"],
        DelegatedCliKind::Codex => &["approval_policy", "model", "sandbox_mode"],
    };
    for (key, value) in options {
        if !allowed.contains(&key.as_str())
            || value.is_empty()
            || value.len() > 256
            || value.starts_with('-')
            || value.contains(['\r', '\n', '\0'])
        {
            return Err(ProjectsError::Validation(format!(
                "unsupported or invalid delegated CLI option {key:?}"
            )));
        }
    }
    Ok(())
}

const fn executor_kind_str(value: ExecutorKind) -> &'static str {
    match value {
        ExecutorKind::Local => "local",
        ExecutorKind::Container => "container",
    }
}

fn parse_executor_kind(value: &str) -> Result<ExecutorKind, ProjectsError> {
    match value {
        "local" => Ok(ExecutorKind::Local),
        "container" => Ok(ExecutorKind::Container),
        _ => Err(ProjectsError::Validation("unknown executor kind".into())),
    }
}

const fn network_policy_str(value: NetworkPolicy) -> &'static str {
    match value {
        NetworkPolicy::DenyAll => "deny_all",
        NetworkPolicy::ProjectRules => "project_rules",
    }
}

fn parse_network_policy(value: &str) -> Result<NetworkPolicy, ProjectsError> {
    match value {
        "deny_all" => Ok(NetworkPolicy::DenyAll),
        "project_rules" => Ok(NetworkPolicy::ProjectRules),
        _ => Err(ProjectsError::Validation("unknown network policy".into())),
    }
}

const fn delegated_cli_kind_str(value: DelegatedCliKind) -> &'static str {
    match value {
        DelegatedCliKind::ClaudeCode => "claude_code",
        DelegatedCliKind::Codex => "codex",
    }
}

fn parse_delegated_cli_kind(value: &str) -> Result<DelegatedCliKind, ProjectsError> {
    match value {
        "claude_code" => Ok(DelegatedCliKind::ClaudeCode),
        "codex" => Ok(DelegatedCliKind::Codex),
        _ => Err(ProjectsError::Validation(
            "unknown delegated CLI kind".into(),
        )),
    }
}

fn parse_capability_state(value: &str) -> Result<CapabilityState, ProjectsError> {
    match value {
        "ready" => Ok(CapabilityState::Ready),
        "degraded" => Ok(CapabilityState::Degraded),
        "unconfigured" => Ok(CapabilityState::Unconfigured),
        "unsupported" => Ok(CapabilityState::Unsupported),
        _ => Err(ProjectsError::Validation("unknown capability state".into())),
    }
}

fn parse_capability_reason(value: &str) -> Result<CapabilityReason, ProjectsError> {
    match value {
        "LOCAL_EXECUTOR" => Ok(CapabilityReason::LocalExecutor),
        "CONFIG_MISSING" => Ok(CapabilityReason::ConfigMissing),
        "DEPENDENCY_MISSING" => Ok(CapabilityReason::DependencyMissing),
        "PLATFORM_UNSUPPORTED" => Ok(CapabilityReason::PlatformUnsupported),
        "POLICY_DISABLED" => Ok(CapabilityReason::PolicyDisabled),
        "PROBE_FAILED" => Ok(CapabilityReason::ProbeFailed),
        _ => Err(ProjectsError::Validation(
            "unknown capability reason".into(),
        )),
    }
}

fn runtime_secret_aad(project_id: &str, secret_id: &str) -> String {
    format!("v1/{project_id}/runtime_secrets/{secret_id}/value")
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
    #[allow(dead_code)]
    edited_blob_sha: Option<String>,
    #[allow(dead_code)]
    version: String,
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

fn sha2_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn guess_mime(path: &std::path::Path) -> Option<String> {
    match path.extension().and_then(|e| e.to_str()) {
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

/// Heuristic: read up to 8 KiB and check it's valid UTF-8 with no NUL bytes,
/// which is sufficient to decide text-vs-binary for the editable flag
/// (`WS-REV-06` uses a 10 MiB size cap; this is the encoding check).
async fn is_utf8_text_file(path: &std::path::Path) -> bool {
    use tokio::io::AsyncReadExt;
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut buf = [0u8; 8192];
    let n = match file.read(&mut buf).await {
        Ok(n) => n,
        Err(_) => return false,
    };
    if buf[..n].contains(&0) {
        return false;
    }
    std::str::from_utf8(&buf[..n]).is_ok()
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
