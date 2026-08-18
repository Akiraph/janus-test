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
use crate::application::{Application, lifecycle, operation_kinds::KIND_PULL_REQUEST_AUTOMATION};

const GITHUB_HOST: &str = "github.com";
const AUTOMATION_WAIT: StdDuration = StdDuration::from_secs(20 * 60);

#[derive(Debug, Clone)]
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

#[derive(Debug, thiserror::Error)]
pub(crate) enum AutomationError {
    #[error("automation validation failed: {0}")]
    Validation(String),
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
    Storage(#[from] sqlx::Error),
    #[error("automation serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
}

#[derive(Debug, Deserialize)]
struct PullRequestAutomationWork {
    operation_id: String,
    owner_id: String,
    workflow: String,
    source: String,
    pull_request_url: String,
    repository_url: String,
    branch: Option<String>,
    project_name: String,
    github_credential_id: Option<String>,
    #[serde(default)]
    model_preference: Option<SessionModelPreference>,
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
        let mut work = self.unit_of_work().begin().await?;
        let row = sqlx::query_as::<_, (Option<String>, Option<String>)>(
            "SELECT model_provider_id, model_upstream_id FROM automation_settings WHERE owner_id = ?",
        )
        .bind(owner_id)
        .fetch_optional(work.connection())
        .await?;
        work.rollback().await?;
        let Some((provider_id, upstream_id)) = row else {
            return Ok(AutomationSettingsView {
                model_provider_id: None,
                model_upstream_id: None,
                model_display_name: None,
            });
        };
        let display_name = self
            .model_display_name(owner_id, provider_id.as_deref(), upstream_id.as_deref())
            .await?;
        Ok(AutomationSettingsView {
            model_provider_id: provider_id,
            model_upstream_id: upstream_id,
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
        let mut work = self.unit_of_work().begin().await?;
        sqlx::query(
            "INSERT INTO automation_settings (owner_id, model_provider_id, model_upstream_id, updated_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT(owner_id) DO UPDATE SET model_provider_id = excluded.model_provider_id, model_upstream_id = excluded.model_upstream_id, updated_at = excluded.updated_at",
        )
        .bind(owner_id)
        .bind(provider_id.as_deref())
        .bind(upstream_id.as_deref())
        .bind(&now)
        .execute(work.connection())
        .await?;
        work.commit().await?;
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

    /// Enqueue a webhook automation operation. The PAT is used only to create
    /// or reuse an encrypted project credential; it never enters this payload.
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
        let operations = self
            .operations()
            .list_by_kind_owner(KIND_PULL_REQUEST_AUTOMATION, owner_id, limit)
            .await?;
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
            status: OperationStatus::Failed,
            result: None,
            problem: Some(json!({
                "code": "PULL_REQUEST_AUTOMATION_FAILED",
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

async fn execute_pull_request_automation(
    state: &Application,
    input: &PullRequestAutomationWork,
    claim: WorkClaim<'_>,
) -> Result<AutomationResult, AutomationError> {
    let project_idem = child_idempotency(
        &input.owner_id,
        &input.operation_id,
        "/api/v1/projects",
        "project",
    );
    let (project, clone_operation) = state
        .projects()
        .create_project(
            &input.owner_id,
            CreateProjectInput {
                name: input.project_name.clone(),
                repository: RepositoryInput {
                    access: if input.github_credential_id.is_some() {
                        RepoAccess::GithubPrivate
                    } else {
                        RepoAccess::PublicHttps
                    },
                    url: input.repository_url.clone(),
                    branch: input.branch.clone(),
                    github_credential_id: input.github_credential_id.clone(),
                },
            },
            CorrelationId::new(),
            Some(project_idem),
        )
        .await?;
    wait_for_operation(state, &clone_operation.id).await?;

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
            &input.operation_id,
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
                &input.operation_id,
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
    AutomationRunView {
        pull_request_url: operation.target_id.clone(),
        workflow: string_field("workflow").unwrap_or_else(|| "github-pr-repair".to_owned()),
        source: string_field("source").unwrap_or_else(|| "webhook".to_owned()),
        project_id: string_field("project_id"),
        session_id: string_field("session_id"),
        push_enabled: bool_field("push_enabled"),
        push_status: string_field("push_status").unwrap_or_else(|| "pending".to_owned()),
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
            return Err(AutomationError::Validation(format!(
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
                return Err(AutomationError::Validation(format!(
                    "child operation {} ended with {}",
                    operation.id, operation.status
                )));
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AutomationError::Validation(format!(
                "child operation {operation_id} did not finish within {} seconds",
                AUTOMATION_WAIT.as_secs()
            )));
        }
        tokio::time::sleep(StdDuration::from_millis(500)).await;
    }
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
        .branch
        .as_deref()
        .unwrap_or("the current checked-out branch");
    format!(
        "A webhook identified a GitHub pull request that needs an automated repair.\n\n\
Pull request: {pr}\nRepository: {repo}\nWorking branch: {branch}\n\n\
Work directly in the current Main workspace. First inspect the pull request with `gh pr view {pr}` and, when possible, use `gh pr checkout {pr}` so the actual PR head repository and branch are selected; do not blindly push the repository default branch. Resolve the repository/PR address conflict represented by this report while preserving the intended behavior. Run the relevant tests, create a focused git commit, and push it to the PR head branch. When GH_TOKEN or GITHUB_TOKEN is available, run `gh auth setup-git` before `git push`. Do not only explain the fix; complete the edit, commit, and push.",
        pr = input.pull_request_url,
        repo = input.repository_url,
        branch = branch,
    )
}
