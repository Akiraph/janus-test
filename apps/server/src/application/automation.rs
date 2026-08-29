//! Optional webhook-driven pull-request automation.
//!
//! The transport turns an email/webhook payload into a validated PR reference;
//! this module owns the durable orchestration that clones the repository,
//! creates a Session, and injects a Supervisor task. No email body or token is
//! persisted as executable prompt input.

use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};
use janus_infrastructure::{
    clock::{format_utc, now_utc_str},
    id::{AttachmentId, CorrelationId, ProjectId, SessionId},
    operations::{
        CreateOperation, CreateWork, IdempotencyRequest, OperationCompletion, OperationError,
        OperationStatus, OperationView, WorkClaim,
    },
};
use janus_models::interface::ModelClient;
use janus_projects::interface::{CreateProjectInput, ProjectsError, RepoAccess, RepositoryInput};
use janus_sessions::interface::{SessionModelPreference, SessionsError, TurnStatus};
use janus_source_control::interface::{GitStatus, SourceControlError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use crate::application::session_flow::PostSessionMessage;
use crate::application::{
    Application, lifecycle,
    operation_kinds::{KIND_FORK_SYNC_BATCH, KIND_PULL_REQUEST_AUTOMATION},
};

const GITHUB_HOST: &str = "github.com";
const AUTOMATION_WAIT: StdDuration = StdDuration::from_secs(20 * 60);

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct PullRequestAutomationRequest {
    pub(crate) owner_id: String,
    pub(crate) workflow: String,
    pub(crate) source: String,
    pub(crate) pull_request_url: String,
    pub(crate) repository_url: String,
    pub(crate) branch: Option<String>,
    pub(crate) project_name: String,
    pub(crate) github_credential_id: Option<String>,
    pub(crate) github_token: Option<String>,
    pub(crate) actor: Value,
    pub(crate) correlation_id: CorrelationId,
    pub(crate) idempotency: IdempotencyRequest,
}

/// One repository repair item delivered by the fork-sync webhook. The item is
/// deliberately small: the email/report body is parsed at the HTTP boundary,
/// while the durable work payload contains only canonical repository metadata.
#[derive(Debug, Clone)]
pub(crate) struct ForkSyncAutomationItem {
    pub(crate) pull_request_url: String,
    pub(crate) repository_url: String,
    pub(crate) parent_repository_url: Option<String>,
    pub(crate) default_branch: Option<String>,
    pub(crate) parent_default_branch: Option<String>,
    pub(crate) message: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) project_name: String,
    pub(crate) github_credential_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ForkSyncAutomationRequest {
    pub(crate) owner_id: String,
    pub(crate) workflow: String,
    pub(crate) source: String,
    pub(crate) items: Vec<ForkSyncAutomationItem>,
    pub(crate) github_credential_id: Option<String>,
    pub(crate) github_token: Option<String>,
    pub(crate) actor: Value,
    pub(crate) correlation_id: CorrelationId,
    pub(crate) idempotency: IdempotencyRequest,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AutomationError {
    #[error("automation validation failed: {0}")]
    Validation(String),
    #[error("automation timed out: {0}")]
    Timeout(String),
    #[error("repository clone failed: {0}")]
    RepositoryClone(String),
    #[error(transparent)]
    Operation(#[from] OperationError),
    #[error(transparent)]
    Projects(#[from] ProjectsError),
    #[error(transparent)]
    Models(#[from] janus_models::interface::ModelsError),
    #[error(transparent)]
    Sessions(#[from] SessionsError),
    #[error(transparent)]
    SourceControl(#[from] SourceControlError),
    #[error(transparent)]
    Storage(#[from] mongodb::error::Error),
    #[error("automation serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
}

impl AutomationError {
    /// Stable problem code recorded on the durable Operation. Without it every
    /// automation failure reads the same, so a client cannot tell a rejected
    /// request from a stalled clone from a broken control plane.
    fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) | Self::Models(_) => "VALIDATION_FAILED",
            Self::Timeout(_) => "AUTOMATION_TIMED_OUT",
            Self::RepositoryClone(_) => "PROJECT_CLONE_FAILED",
            Self::Projects(error) => error.code(),
            Self::SourceControl(error) => error.code(),
            Self::Sessions(error) => sessions_code(error),
            Self::Operation(OperationError::StaleWorkClaim) => "OPERATION_LEASE_STALE",
            Self::Operation(_) | Self::Storage(_) | Self::Serde(_) => "INTERNAL_ERROR",
        }
    }

    /// A failure that leaves a Project, Session or Turn half-built needs a human
    /// to reconcile it; a decided rejection or clone failure is simply failed.
    fn requires_attention(&self) -> bool {
        !matches!(
            self,
            Self::Validation(_)
                | Self::Models(_)
                | Self::RepositoryClone(_)
                | Self::Projects(ProjectsError::Validation(_) | ProjectsError::NotFound)
        )
    }
}

/// Session failures an automation run can hit, mapped to the same codes the
/// HTTP session routes publish so both surfaces name them identically.
fn sessions_code(error: &SessionsError) -> &'static str {
    match error {
        SessionsError::NotFound => "SESSION_NOT_FOUND",
        SessionsError::SessionDeleting => "SESSION_DELETING",
        SessionsError::ActiveTurnExists => "ACTIVE_TURN_EXISTS",
        SessionsError::VersionMismatch { .. } => "RESOURCE_VERSION_MISMATCH",
        SessionsError::ModelNotConfigured => "MODEL_NOT_CONFIGURED",
        SessionsError::InvalidModelPreference => "VALIDATION_FAILED",
        SessionsError::Validation(_) => "VALIDATION_FAILED",
        _ => "INTERNAL_ERROR",
    }
}

#[derive(Debug, Deserialize)]
struct PullRequestAutomationWork {
    operation_id: String,
    owner_id: String,
    workflow: String,
    source: String,
    pull_request_url: String,
    repository_url: String,
    #[serde(default)]
    parent_repository_url: Option<String>,
    #[serde(default)]
    default_branch: Option<String>,
    #[serde(default)]
    parent_default_branch: Option<String>,
    #[serde(default)]
    message: Option<String>,
    branch: Option<String>,
    project_name: String,
    github_credential_id: Option<String>,
    #[serde(default)]
    model_preference: Option<SessionModelPreference>,
    #[serde(default)]
    child_scope: String,
}

#[derive(Debug, Deserialize)]
struct ForkSyncAutomationWork {
    operation_id: String,
    owner_id: String,
    workflow: String,
    source: String,
    items: Vec<PullRequestAutomationItemWork>,
    #[serde(default)]
    model_preference: Option<SessionModelPreference>,
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequestAutomationItemWork {
    pull_request_url: String,
    repository_url: String,
    #[serde(default)]
    parent_repository_url: Option<String>,
    #[serde(default)]
    default_branch: Option<String>,
    #[serde(default)]
    parent_default_branch: Option<String>,
    #[serde(default)]
    message: Option<String>,
    branch: Option<String>,
    project_name: String,
    github_credential_id: Option<String>,
    /// Stable per-repository scope used for child idempotency keys. It is
    /// derived from the parent operation and repository URL at enqueue time.
    child_scope: String,
}

#[derive(Debug, Serialize)]
struct AutomationResult {
    workflow: String,
    source: String,
    project_id: String,
    session_id: String,
    pull_request_url: String,
    message_id: Option<String>,
    turn_id: Option<String>,
    turn_status: String,
    git_status: GitStatus,
    push_enabled: bool,
    push_status: String,
}

#[derive(Debug, Serialize)]
struct ForkSyncItemResult {
    repository_url: String,
    pull_request_url: String,
    project_id: Option<String>,
    session_id: Option<String>,
    status: String,
    push_status: String,
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub(crate) struct AutomationRepositoryView {
    pub repository_url: String,
    pub pull_request_url: String,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub status: String,
    pub push_status: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct AutomationRunView {
    pub operation: OperationView,
    pub workflow: String,
    pub source: String,
    pub pull_request_url: Option<String>,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub push_enabled: bool,
    pub push_status: String,
    pub repositories: Vec<AutomationRepositoryView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct AutomationSettingsView {
    pub model_provider_id: Option<String>,
    pub model_upstream_id: Option<String>,
    pub model_display_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub(crate) struct UpdateAutomationSettingsInput {
    #[serde(default)]
    pub model_provider_id: Option<String>,
    #[serde(default)]
    pub model_upstream_id: Option<String>,
}

impl Application {
    pub(crate) async fn get_automation_settings(
        &self,
        owner_id: &str,
    ) -> Result<AutomationSettingsView, AutomationError> {
        let Some(selection) = self.models().automation_model_selection(owner_id).await? else {
            return Ok(AutomationSettingsView {
                model_provider_id: None,
                model_upstream_id: None,
                model_display_name: None,
            });
        };
        let display_name = self
            .model_display_name(
                owner_id,
                selection.model_provider_id.as_deref(),
                selection.model_upstream_id.as_deref(),
            )
            .await?;
        Ok(AutomationSettingsView {
            model_provider_id: selection.model_provider_id,
            model_upstream_id: selection.model_upstream_id,
            model_display_name: display_name,
        })
    }

    pub(crate) async fn update_automation_settings(
        &self,
        owner_id: &str,
        input: UpdateAutomationSettingsInput,
    ) -> Result<AutomationSettingsView, AutomationError> {
        let provider_id = input
            .model_provider_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let upstream_id = input
            .model_upstream_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if provider_id.is_some() != upstream_id.is_some() {
            return Err(AutomationError::Validation(
                "model_provider_id and model_upstream_id must be set together".into(),
            ));
        }
        let display_name = self
            .model_display_name(owner_id, provider_id.as_deref(), upstream_id.as_deref())
            .await?;
        let now = now_utc_str();
        self.models()
            .set_automation_model_selection(
                owner_id,
                provider_id.as_deref(),
                upstream_id.as_deref(),
                &now,
            )
            .await?;
        Ok(AutomationSettingsView {
            model_provider_id: provider_id,
            model_upstream_id: upstream_id,
            model_display_name: display_name,
        })
    }

    async fn model_display_name(
        &self,
        owner_id: &str,
        provider_id: Option<&str>,
        upstream_id: Option<&str>,
    ) -> Result<Option<String>, AutomationError> {
        let (Some(provider_id), Some(upstream_id)) = (provider_id, upstream_id) else {
            return Ok(None);
        };
        let providers = self.models().providers(owner_id).await?;
        let provider = providers
            .iter()
            .find(|provider| {
                provider.id == provider_id
                    && provider.enabled
                    && provider.client == ModelClient::Supervisor
            })
            .ok_or_else(|| {
                AutomationError::Validation("selected model provider is unavailable".into())
            })?;
        let model = self
            .models()
            .models(owner_id)
            .await?
            .into_iter()
            .find(|model| {
                model.provider_id == provider_id
                    && model.upstream_model_id == upstream_id
                    && model.enabled
            })
            .ok_or_else(|| AutomationError::Validation("selected model is unavailable".into()))?;
        Ok(Some(format!(
            "{} / {}",
            provider.display_name, model.display_name
        )))
    }

    async fn validate_automation_credential(
        &self,
        owner_id: &str,
        credential_id: &str,
    ) -> Result<(), AutomationError> {
        let credential = self
            .projects()
            .get_credential(owner_id, credential_id)
            .await?;
        if !credential.automation_enabled {
            return Err(AutomationError::Validation(
                "github credential is not enabled for Automation pushes".into(),
            ));
        }
        if !credential.pat_is_set {
            return Err(AutomationError::Validation(
                "github credential has no PAT; add a classic token before enabling Automation"
                    .into(),
            ));
        }
        Ok(())
    }

    async fn resolve_automation_credential(
        &self,
        owner_id: &str,
        explicit_id: Option<String>,
        token: Option<&str>,
        required: bool,
        correlation_id: &CorrelationId,
    ) -> Result<Option<String>, AutomationError> {
        let credential_id = match explicit_id {
            Some(id) => {
                self.validate_automation_credential(owner_id, &id).await?;
                Some(id)
            }
            None => match token {
                Some(token) if !token.trim().is_empty() => Some(
                    self.projects()
                        .ensure_automation_credential(
                            owner_id,
                            GITHUB_HOST,
                            token,
                            &correlation_id.to_string(),
                        )
                        .await?,
                ),
                _ => {
                    let credentials = self
                        .projects()
                        .list_automation_credentials(owner_id)
                        .await?;
                    if credentials.len() == 1 && credentials[0].pat_is_set {
                        Some(credentials[0].id.clone())
                    } else {
                        None
                    }
                }
            },
        };
        if let Some(id) = credential_id.as_deref() {
            self.validate_automation_credential(owner_id, id).await?;
        } else if required {
            return Err(AutomationError::Validation(
                "fork-sync Automation requires exactly one Automation-enabled GitHub classic PAT"
                    .into(),
            ));
        }
        Ok(credential_id)
    }

    /// Enqueue one durable parent operation for a fork-sync report. The parent
    /// worker processes `items` in order, so each repository gets its own
    /// Project and Session while the report remains one auditable run.
    pub(crate) async fn request_fork_sync_automation(
        &self,
        input: ForkSyncAutomationRequest,
    ) -> Result<OperationView, AutomationError> {
        if input.items.is_empty() {
            return Err(AutomationError::Validation(
                "fork-sync webhook contains no repository items".into(),
            ));
        }
        let workflow = normalize_label(&input.workflow, "fork-sync");
        let source = normalize_label(&input.source, "webhook");
        let settings = self.get_automation_settings(&input.owner_id).await?;
        let model_preference = match (settings.model_provider_id, settings.model_upstream_id) {
            (Some(provider_id), Some(upstream_model_id)) => Some(SessionModelPreference {
                provider_id,
                upstream_model_id,
                reasoning_effort: Default::default(),
            }),
            _ => None,
        };
        let default_credential = self
            .resolve_automation_credential(
                &input.owner_id,
                input.github_credential_id.clone(),
                input.github_token.as_deref(),
                true,
                &input.correlation_id,
            )
            .await?;
        let mut item_values = Vec::with_capacity(input.items.len());
        for item in input.items {
            let credential_id = match item.github_credential_id {
                Some(id) => {
                    self.validate_automation_credential(&input.owner_id, &id)
                        .await?;
                    Some(id)
                }
                None => default_credential.clone(),
            };
            item_values.push(json!({
                "pull_request_url": item.pull_request_url,
                "repository_url": item.repository_url,
                "parent_repository_url": item.parent_repository_url,
                "default_branch": item.default_branch,
                "parent_default_branch": item.parent_default_branch,
                "message": item.message,
                "branch": item.branch,
                "project_name": item.project_name,
                "github_credential_id": credential_id,
                "child_scope": normalize_repository_url(&item.repository_url),
            }));
        }
        let target_id = format!("fork-sync:{}", input.idempotency.key);
        let payload = json!({
            "owner_id": input.owner_id,
            "workflow": workflow.clone(),
            "source": source.clone(),
            "items": item_values,
            "model_preference": model_preference,
        });
        let created = self
            .operations()
            .create(
                CreateOperation {
                    kind: KIND_FORK_SYNC_BATCH,
                    actor: input.actor,
                    target_kind: "fork_sync_batch",
                    target_id: Some(&target_id),
                    conditions: json!({
                        "workflow": workflow,
                        "source": source,
                        "items": payload.get("items"),
                    }),
                    correlation_id: input.correlation_id,
                    idempotency: Some(input.idempotency),
                },
                Some(CreateWork {
                    handler_kind: KIND_FORK_SYNC_BATCH,
                    payload,
                }),
            )
            .await?;
        Ok(created.operation)
    }

    /// Enqueue a webhook automation operation. The PAT is used only to create
    /// or reuse an encrypted project credential; it never enters this payload.
    #[allow(dead_code)]
    pub(crate) async fn request_pull_request_automation(
        &self,
        input: PullRequestAutomationRequest,
    ) -> Result<OperationView, AutomationError> {
        let workflow = normalize_label(&input.workflow, "github-pr-repair");
        let source = normalize_label(&input.source, "webhook");
        let settings = self.get_automation_settings(&input.owner_id).await?;
        let model_preference = match (settings.model_provider_id, settings.model_upstream_id) {
            (Some(provider_id), Some(upstream_model_id)) => Some(SessionModelPreference {
                provider_id,
                upstream_model_id,
                reasoning_effort: Default::default(),
            }),
            _ => None,
        };
        let credential_id = match input.github_credential_id.clone() {
            Some(id) => {
                let credential = self.projects().get_credential(&input.owner_id, &id).await?;
                if !credential.automation_enabled {
                    return Err(AutomationError::Validation(
                        "github credential is not enabled for Automation pushes".into(),
                    ));
                }
                Some(id)
            }
            None => match input.github_token.as_deref() {
                Some(token) => Some(
                    self.projects()
                        .ensure_automation_credential(
                            &input.owner_id,
                            GITHUB_HOST,
                            token,
                            &input.correlation_id.to_string(),
                        )
                        .await?,
                ),
                None => {
                    let credentials = self
                        .projects()
                        .list_automation_credentials(&input.owner_id)
                        .await?;
                    (credentials.len() == 1)
                        .then(|| {
                            credentials[0]
                                .pat_is_set
                                .then_some(credentials[0].id.clone())
                        })
                        .flatten()
                }
            },
        };
        let access = if credential_id.is_some() {
            RepoAccess::GithubPrivate
        } else {
            RepoAccess::PublicHttps
        };
        let target_id = input.pull_request_url.clone();
        let payload = json!({
            "owner_id": input.owner_id,
            "workflow": workflow.clone(),
            "source": source.clone(),
            "pull_request_url": input.pull_request_url,
            "repository_url": input.repository_url,
            "branch": input.branch,
            "project_name": input.project_name,
            "github_credential_id": credential_id,
            "model_preference": model_preference,
            "repository_access": access,
            "child_scope": normalize_repository_url(&input.repository_url),
        });
        let created = self
            .operations()
            .create(
                CreateOperation {
                    kind: KIND_PULL_REQUEST_AUTOMATION,
                    actor: input.actor,
                    target_kind: "pull_request",
                    target_id: Some(&target_id),
                    conditions: json!({
                        "workflow": workflow.clone(),
                        "source": source.clone(),
                        "pull_request_url": target_id,
                        "repository_url": input.repository_url,
                        "branch": input.branch,
                    }),
                    correlation_id: input.correlation_id,
                    idempotency: Some(input.idempotency),
                },
                Some(CreateWork {
                    handler_kind: KIND_PULL_REQUEST_AUTOMATION,
                    payload,
                }),
            )
            .await?;
        Ok(created.operation)
    }

    pub(crate) async fn list_automation_runs(
        &self,
        owner_id: &str,
        limit: i64,
    ) -> Result<Vec<AutomationRunView>, AutomationError> {
        let mut operations = self
            .operations()
            .list_by_kind_owner(KIND_PULL_REQUEST_AUTOMATION, owner_id, limit)
            .await?;
        let mut batches = self
            .operations()
            .list_by_kind_owner(KIND_FORK_SYNC_BATCH, owner_id, limit)
            .await?;
        operations.append(&mut batches);
        operations.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        operations.truncate(limit.clamp(1, 200) as usize);
        Ok(operations.into_iter().map(automation_run_view).collect())
    }
}

/// Run one durable automation work item. Child operations use deterministic
/// idempotency keys derived from the outer operation, so a reclaimed work item
/// can safely resume after any process boundary.
pub(crate) async fn run_pull_request_automation(
    state: &Application,
    payload: &Value,
    work_id: &str,
    work_nonce: &str,
) -> Result<(), anyhow::Error> {
    let input: PullRequestAutomationWork = serde_json::from_value(payload.clone())?;
    let claim = WorkClaim {
        id: work_id,
        nonce: work_nonce,
    };
    let result = execute_pull_request_automation(state, &input, claim).await;
    let completion = match result {
        Ok(result) => OperationCompletion {
            status: OperationStatus::Succeeded,
            result: Some(serde_json::to_value(result)?),
            problem: None,
            correlation_id: CorrelationId::new(),
        },
        Err(error) => OperationCompletion {
            status: if error.requires_attention() {
                OperationStatus::NeedsAttention
            } else {
                OperationStatus::Failed
            },
            result: None,
            problem: Some(json!({
                "code": error.code(),
                "detail": error.to_string(),
            })),
            correlation_id: CorrelationId::new(),
        },
    };
    state
        .operations()
        .finish_claimed(&input.operation_id, work_id, work_nonce, completion)
        .await?;
    Ok(())
}

/// Run a fork-sync batch. The parent owns the lease for the complete report,
/// so repository items are intentionally awaited one at a time. Each item gets
/// a stable child idempotency scope and its Project/Session IDs are published
/// immediately for the UI.
pub(crate) async fn run_fork_sync_batch(
    state: &Application,
    payload: &Value,
    work_id: &str,
    work_nonce: &str,
) -> Result<(), anyhow::Error> {
    let input: ForkSyncAutomationWork = serde_json::from_value(payload.clone())?;
    let claim = WorkClaim {
        id: work_id,
        nonce: work_nonce,
    };
    let mut results = Vec::with_capacity(input.items.len());
    let mut failures = 0usize;
    for (index, item) in input.items.iter().cloned().enumerate() {
        let child = PullRequestAutomationWork {
            operation_id: input.operation_id.clone(),
            owner_id: input.owner_id.clone(),
            workflow: input.workflow.clone(),
            source: input.source.clone(),
            pull_request_url: item.pull_request_url.clone(),
            repository_url: item.repository_url.clone(),
            parent_repository_url: item.parent_repository_url.clone(),
            default_branch: item.default_branch.clone(),
            parent_default_branch: item.parent_default_branch.clone(),
            message: item.message.clone(),
            branch: item.branch.clone(),
            project_name: item.project_name.clone(),
            github_credential_id: item.github_credential_id.clone(),
            model_preference: input.model_preference.clone(),
            child_scope: format!("{}:{index}", item.child_scope),
        };
        publish_progress(
            state,
            claim,
            &input.operation_id,
            "repository_started",
            json!({
                "workflow": input.workflow,
                "source": input.source,
                "current_index": index,
                "total": input.items.len(),
                "repository_url": child.repository_url,
                "pull_request_url": child.pull_request_url,
                "items": results,
            }),
        )
        .await?;
        match execute_pull_request_automation(state, &child, claim).await {
            Ok(result) => {
                results.push(ForkSyncItemResult {
                    repository_url: child.repository_url.clone(),
                    pull_request_url: result.pull_request_url.clone(),
                    project_id: Some(result.project_id.clone()),
                    session_id: Some(result.session_id.clone()),
                    status: "succeeded".into(),
                    push_status: result.push_status.clone(),
                    detail: None,
                });
            }
            Err(error) => {
                failures += 1;
                results.push(ForkSyncItemResult {
                    repository_url: child.repository_url.clone(),
                    pull_request_url: child.pull_request_url.clone(),
                    project_id: None,
                    session_id: None,
                    status: "failed".into(),
                    push_status: "needs_attention".into(),
                    detail: Some(error.to_string()),
                });
            }
        }
        publish_progress(
            state,
            claim,
            &input.operation_id,
            "repository_completed",
            json!({
                "workflow": input.workflow,
                "source": input.source,
                "current_index": index + 1,
                "total": input.items.len(),
                "items": results,
            }),
        )
        .await?;
    }
    let status = if failures == 0 {
        "succeeded"
    } else {
        "needs_attention"
    };
    let completion = OperationCompletion {
        status: if failures == 0 {
            OperationStatus::Succeeded
        } else {
            OperationStatus::NeedsAttention
        },
        result: Some(json!({
            "workflow": input.workflow,
            "source": input.source,
            "status": status,
            "items": results,
        })),
        problem: (failures > 0).then(|| {
            json!({
                "code": "FORK_SYNC_PARTIAL_FAILURE",
                "detail": format!("{failures} of {} repository item(s) need attention", input.items.len()),
            })
        }),
        correlation_id: CorrelationId::new(),
    };
    state
        .operations()
        .finish_claimed(&input.operation_id, work_id, work_nonce, completion)
        .await?;
    Ok(())
}

async fn execute_pull_request_automation(
    state: &Application,
    input: &PullRequestAutomationWork,
    claim: WorkClaim<'_>,
) -> Result<AutomationResult, AutomationError> {
    let child_scope = if input.child_scope.trim().is_empty() {
        input.operation_id.clone()
    } else {
        format!("{}:{}", input.operation_id, input.child_scope)
    };
    let project = if let Some(mut existing) =
        find_matching_project(state, &input.owner_id, &input.repository_url).await?
    {
        if let Some(credential_id) = input.github_credential_id.as_deref()
            && existing.repository.github_credential_id.as_deref() != Some(credential_id)
        {
            existing = state
                .projects()
                .set_project_github_credential(
                    &input.owner_id,
                    &existing.id,
                    credential_id,
                    &CorrelationId::new().to_string(),
                )
                .await?;
        }
        wait_for_project_ready(state, &input.owner_id, &existing.id, None).await?
    } else {
        let project_idem = project_idempotency(&input.owner_id, &input.repository_url);
        let (project, clone_operation) = state
            .projects()
            .create_project(
                &input.owner_id,
                CreateProjectInput {
                    name: input.project_name.clone(),
                    repository: RepositoryInput {
                        access: RepoAccess::GithubPrivate,
                        url: input.repository_url.clone(),
                        branch: input
                            .default_branch
                            .clone()
                            .or_else(|| input.branch.clone()),
                        github_credential_id: input.github_credential_id.clone(),
                    },
                },
                CorrelationId::new(),
                Some(project_idem),
            )
            .await?;
        wait_for_project_ready(
            state,
            &input.owner_id,
            &project.id,
            Some(clone_operation.id.as_str()),
        )
        .await?
    };

    publish_progress(
        state,
        claim,
        &input.operation_id,
        "project_ready",
        json!({
                "workflow": input.workflow,
                "source": input.source,
                "pull_request_url": input.pull_request_url,
                "project_id": project.id,
                "push_enabled": input.github_credential_id.is_some(),
                "push_status": if input.github_credential_id.is_some() { "enabled" } else { "read_only" },
            }),
    )
    .await?;

    let project_id: ProjectId = project
        .id
        .parse()
        .map_err(|error| AutomationError::Validation(format!("project id: {error}")))?;
    let session_operation = lifecycle::request_session_creation(
        state.operations(),
        &input.owner_id,
        project_id,
        Some(format!("PR automation: {}", input.pull_request_url)),
        json!({
            "kind": "automation",
            "source": "github_webhook",
            "pull_request_url": input.pull_request_url,
        }),
        CorrelationId::new(),
        child_idempotency(
            &input.owner_id,
            &child_scope,
            &format!("/api/v1/projects/{project_id}/sessions"),
            "session",
        ),
    )
    .await
    .map_err(|error| AutomationError::Validation(format!("session creation: {error}")))?;
    let session_id = session_operation
        .target_id
        .as_deref()
        .ok_or_else(|| AutomationError::Validation("session operation has no target".into()))?
        .parse::<SessionId>()
        .map_err(|error| AutomationError::Validation(format!("session id: {error}")))?;
    wait_for_operation(state, &session_operation.id).await?;

    publish_progress(
        state,
        claim,
        &input.operation_id,
        "session_ready",
        json!({
                "workflow": input.workflow,
                "source": input.source,
                "pull_request_url": input.pull_request_url,
                "project_id": project.id,
                "session_id": session_id,
                "push_enabled": input.github_credential_id.is_some(),
                "push_status": if input.github_credential_id.is_some() { "enabled" } else { "read_only" },
            }),
    )
    .await?;

    let session = state.sessions().get_session(session_id).await?;
    let prompt = supervisor_prompt(input);
    let message = state
        .post_session_message(PostSessionMessage {
            owner_id: &input.owner_id,
            session_id,
            content: &prompt,
            expected_version: &session.version,
            model_preference: input.model_preference.as_ref().map(Some),
            attachment_ids: &[] as &[AttachmentId],
            actor: json!({
                "kind": "automation",
                "source": "github_webhook",
                "pull_request_url": input.pull_request_url,
            }),
            goal_mode: true,
            idempotency: Some(child_idempotency(
                &input.owner_id,
                &child_scope,
                &format!("/api/v1/sessions/{session_id}/messages"),
                "message",
            )),
        })
        .await?;
    let turn_id = message
        .turn_id
        .parse::<janus_infrastructure::id::TurnId>()
        .map_err(|error| AutomationError::Validation(format!("turn id: {error}")))?;
    let turn = wait_for_turn(state, session_id, turn_id).await?;
    let git_status = state
        .source_control()
        .git_status(&input.owner_id, &project.id)
        .await?;

    publish_progress(
        state,
        claim,
        &input.operation_id,
        "turn_completed",
        json!({
                "workflow": input.workflow,
                "source": input.source,
                "pull_request_url": input.pull_request_url,
                "project_id": project.id,
                "session_id": session_id,
                "turn_id": turn.id,
                "push_enabled": input.github_credential_id.is_some(),
                "push_status": if input.github_credential_id.is_some() { "requested" } else { "read_only" },
            }),
    )
    .await?;

    Ok(AutomationResult {
        workflow: input.workflow.clone(),
        source: input.source.clone(),
        project_id: project.id,
        session_id: session_id.to_string(),
        pull_request_url: input.pull_request_url.clone(),
        message_id: Some(message.message_id),
        turn_id: Some(message.turn_id),
        turn_status: turn.status,
        git_status,
        push_enabled: input.github_credential_id.is_some(),
        push_status: if input.github_credential_id.is_some() {
            "requested".to_owned()
        } else {
            "read_only".to_owned()
        },
    })
}

async fn publish_progress(
    state: &Application,
    claim: WorkClaim<'_>,
    operation_id: &str,
    current_step: &str,
    progress: Value,
) -> Result<(), AutomationError> {
    let changed = state
        .operations()
        .update_progress_claimed(
            claim,
            operation_id,
            current_step,
            progress,
            CorrelationId::new(),
        )
        .await?;
    if !changed {
        return Err(AutomationError::Operation(OperationError::StaleWorkClaim));
    }
    Ok(())
}

fn automation_run_view(operation: OperationView) -> AutomationRunView {
    let progress = operation.progress.as_ref();
    let result = operation.result.as_ref();
    let string_field = |name: &str| {
        progress
            .and_then(|value| value.get(name))
            .and_then(Value::as_str)
            .or_else(|| {
                result
                    .and_then(|value| value.get(name))
                    .and_then(Value::as_str)
            })
            .map(ToOwned::to_owned)
    };
    let bool_field = |name: &str| {
        progress
            .and_then(|value| value.get(name))
            .and_then(Value::as_bool)
            .or_else(|| {
                result
                    .and_then(|value| value.get(name))
                    .and_then(Value::as_bool)
            })
            .unwrap_or(false)
    };
    let item_value = progress
        .and_then(|value| value.get("items"))
        .or_else(|| result.and_then(|value| value.get("items")));
    let repositories = item_value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    serde_json::from_value::<AutomationRepositoryView>(item.clone()).ok()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let first_repository = repositories.first();
    AutomationRunView {
        pull_request_url: string_field("pull_request_url")
            .or_else(|| first_repository.map(|item| item.pull_request_url.clone())),
        workflow: string_field("workflow").unwrap_or_else(|| "github-pr-repair".to_owned()),
        source: string_field("source").unwrap_or_else(|| "webhook".to_owned()),
        project_id: string_field("project_id")
            .or_else(|| first_repository.and_then(|item| item.project_id.clone())),
        session_id: string_field("session_id")
            .or_else(|| first_repository.and_then(|item| item.session_id.clone())),
        push_enabled: bool_field("push_enabled") || !repositories.is_empty(),
        push_status: string_field("push_status")
            .or_else(|| first_repository.map(|item| item.push_status.clone()))
            .unwrap_or_else(|| "pending".to_owned()),
        repositories,
        operation,
    }
}

fn normalize_label(value: &str, fallback: &str) -> String {
    let value = value
        .trim()
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || *character == '-' || *character == '_'
        })
        .take(64)
        .collect::<String>();
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value
    }
}

fn normalize_repository_url(value: &str) -> String {
    let mut normalized = value.trim().to_ascii_lowercase();
    if let Some(rest) = normalized.strip_prefix("git@github.com:") {
        normalized = format!("github.com/{rest}");
    }
    for prefix in ["https://", "http://", "ssh://", "git://"] {
        if let Some(rest) = normalized.strip_prefix(prefix) {
            normalized = rest.to_owned();
            break;
        }
    }
    normalized = normalized
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches('/')
        .to_owned();
    normalized
}

fn project_idempotency(owner_id: &str, repository_url: &str) -> IdempotencyRequest {
    let normalized = normalize_repository_url(repository_url);
    let digest = format!("automation-project:{normalized}");
    IdempotencyRequest {
        key: format!("automation-project:{owner_id}:{normalized}"),
        owner_id: owner_id.to_owned(),
        method: "POST".into(),
        normalized_route: "/api/v1/projects".into(),
        digest,
        expires_at: format_utc(Utc::now() + Duration::days(3650)),
    }
}

async fn find_matching_project(
    state: &Application,
    owner_id: &str,
    repository_url: &str,
) -> Result<Option<janus_projects::interface::ProjectView>, AutomationError> {
    let normalized = normalize_repository_url(repository_url);
    let projects = state.projects().list_projects(owner_id, 100).await?;
    Ok(projects.into_iter().find(|project| {
        normalize_repository_url(&project.repository.url) == normalized
            && project.state != "deleting"
    }))
}

async fn wait_for_project_ready(
    state: &Application,
    owner_id: &str,
    project_id: &str,
    clone_operation_id: Option<&str>,
) -> Result<janus_projects::interface::ProjectView, AutomationError> {
    let deadline = tokio::time::Instant::now() + AUTOMATION_WAIT;
    loop {
        let project = state.projects().get_project(owner_id, project_id).await?;
        match project.state.as_str() {
            "ready" => return Ok(project),
            "error" => {
                let detail = project
                    .restrictions
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "project clone failed".into());
                return Err(AutomationError::RepositoryClone(detail));
            }
            "deleting" => {
                return Err(AutomationError::Validation(
                    "matching project is being deleted".into(),
                ));
            }
            _ => {}
        }
        // A clone that dead-letters can leave the Project in `creating`
        // forever. Report the clone Operation's own problem instead of waiting
        // out the full deadline and blaming a timeout.
        if let Some(operation_id) = clone_operation_id
            && let Some(operation) = state.operations().get(operation_id).await?
            && matches!(
                operation.status.as_str(),
                "failed" | "canceled" | "needs_attention"
            )
        {
            let status = operation.status.as_str();
            let detail = operation_problem_detail(&operation);
            return Err(AutomationError::RepositoryClone(format!(
                "clone operation {operation_id} ended with {status}: {detail}"
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AutomationError::Timeout(format!(
                "project {project_id} was still {} after {} seconds",
                project.state,
                AUTOMATION_WAIT.as_secs()
            )));
        }
        tokio::time::sleep(StdDuration::from_millis(500)).await;
    }
}

async fn wait_for_turn(
    state: &Application,
    session_id: SessionId,
    turn_id: janus_infrastructure::id::TurnId,
) -> Result<janus_sessions::interface::TurnSummary, AutomationError> {
    let deadline = tokio::time::Instant::now() + AUTOMATION_WAIT;
    loop {
        let turn = state.turn_summary(session_id, turn_id).await?;
        let status = turn
            .status
            .parse::<TurnStatus>()
            .map_err(AutomationError::Validation)?;
        match status {
            TurnStatus::Completed => return Ok(turn),
            TurnStatus::Failed | TurnStatus::Canceled | TurnStatus::Interrupted => {
                return Err(AutomationError::Validation(format!(
                    "Supervisor turn {} ended with {}",
                    turn.id, turn.status
                )));
            }
            TurnStatus::Queued | TurnStatus::Running | TurnStatus::Canceling => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AutomationError::Timeout(format!(
                "Supervisor turn {} did not finish within {} seconds",
                turn.id,
                AUTOMATION_WAIT.as_secs()
            )));
        }
        tokio::time::sleep(StdDuration::from_millis(500)).await;
    }
}

async fn wait_for_operation(
    state: &Application,
    operation_id: &str,
) -> Result<OperationView, AutomationError> {
    let deadline = tokio::time::Instant::now() + AUTOMATION_WAIT;
    loop {
        let operation = state
            .operations()
            .get(operation_id)
            .await?
            .ok_or_else(|| AutomationError::Validation("child operation disappeared".into()))?;
        match operation.status.as_str() {
            "succeeded" => return Ok(operation),
            "failed" | "canceled" | "needs_attention" => {
                // Carry the child's own problem forward: without it the parent
                // run only says "ended with failed" and the cause is buried in
                // an Operation the client never sees.
                let status = operation.status.as_str();
                let detail = operation_problem_detail(&operation);
                return Err(AutomationError::Validation(format!(
                    "child operation {operation_id} ended with {status}: {detail}"
                )));
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AutomationError::Timeout(format!(
                "child operation {operation_id} did not finish within {} seconds",
                AUTOMATION_WAIT.as_secs()
            )));
        }
        tokio::time::sleep(StdDuration::from_millis(500)).await;
    }
}

/// The `detail` a worker recorded on an Operation's problem, or an explicit
/// marker when the Operation failed without one.
fn operation_problem_detail(operation: &OperationView) -> &str {
    operation
        .problem
        .as_ref()
        .and_then(|problem| problem.get("detail"))
        .and_then(Value::as_str)
        .unwrap_or("no problem detail was recorded")
}

fn child_idempotency(
    owner_id: &str,
    operation_id: &str,
    route: &str,
    step: &str,
) -> IdempotencyRequest {
    IdempotencyRequest {
        key: format!("automation:{operation_id}:{step}"),
        owner_id: owner_id.to_owned(),
        method: "POST".into(),
        normalized_route: route.to_owned(),
        digest: format!("automation:{operation_id}:{step}"),
        expires_at: format_utc(Utc::now() + Duration::days(30)),
    }
}

fn supervisor_prompt(input: &PullRequestAutomationWork) -> String {
    let branch = input
        .default_branch
        .as_deref()
        .or(input.branch.as_deref())
        .unwrap_or("the repository default branch");
    let parent = input
        .parent_repository_url
        .as_deref()
        .unwrap_or("the upstream parent repository from the conflict report");
    let parent_branch = input
        .parent_default_branch
        .as_deref()
        .unwrap_or("the parent repository default branch");
    let message = input
        .message
        .as_deref()
        .unwrap_or("No additional conflict message was provided.");
    format!(
        "A fork-sync webhook identified a GitHub pull request whose repository needs an automated repair.\n\n\
Pull request: {pr}\nFork repository: {repo}\nFork default branch: {branch}\nParent repository: {parent}\nParent default branch: {parent_branch}\nConflict report: {message}\n\n\
Work directly in the current Main workspace. Begin with `git status --short` and preserve every existing change: never run `git reset --hard`, `git clean`, or any command that discards/stashes local work. Inspect the pull request with `gh pr view {pr}`. Fetch the parent repository and merge/reconcile its `{parent_branch}` into the fork's `{branch}` as needed to resolve the reported conflict. Run the relevant tests, create a focused commit containing the repair (and any pre-existing changes that are clearly part of this repair), then push directly to the fork repository default branch with `git push origin HEAD:{branch}`. Do not push the PR head branch or the parent repository. If existing changes are unrelated and cannot be safely separated, stop without deleting or pushing them and report that the Session needs attention. When GH_TOKEN or GITHUB_TOKEN is available, run `gh auth setup-git` before pushing. Do not only explain the fix; complete the edit, commit, and push or leave an explicit needs-attention report.",
        pr = input.pull_request_url,
        repo = input.repository_url,
        branch = branch,
        parent = parent,
        parent_branch = parent_branch,
        message = message,
    )
}

#[cfg(test)]
mod tests {
    use super::AutomationError;
    use janus_projects::interface::ProjectsError;

    #[test]
    fn failure_codes_separate_a_stall_from_a_rejection() {
        assert_eq!(
            AutomationError::Timeout("clone".into()).code(),
            "AUTOMATION_TIMED_OUT"
        );
        assert_eq!(
            AutomationError::Validation("bad repository url".into()).code(),
            "VALIDATION_FAILED"
        );
        assert_eq!(
            AutomationError::Projects(ProjectsError::NotFound).code(),
            "RESOURCE_NOT_FOUND"
        );
        assert_eq!(
            AutomationError::RepositoryClone("remote unavailable".into()).code(),
            "PROJECT_CLONE_FAILED"
        );
    }

    #[test]
    fn only_a_half_built_run_needs_attention() {
        assert!(AutomationError::Timeout("turn".into()).requires_attention());
        assert!(!AutomationError::Validation("bad repository url".into()).requires_attention());
        assert!(!AutomationError::Projects(ProjectsError::NotFound).requires_attention());
    }
}
