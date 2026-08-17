//! Optional webhook-driven pull-request automation.
//!
//! The transport turns an email/webhook payload into a validated PR reference;
//! this module owns the durable orchestration that clones the repository,
//! creates a Session, and injects a Supervisor task. No email body or token is
//! persisted as executable prompt input.

use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};
use janus_infrastructure::{
    clock::format_utc,
    id::{AttachmentId, CorrelationId, ProjectId, SessionId},
    operations::{
        CreateOperation, CreateWork, IdempotencyRequest, OperationCompletion, OperationError,
        OperationStatus, OperationView,
    },
};
use janus_projects::interface::{CreateProjectInput, ProjectsError, RepoAccess, RepositoryInput};
use janus_sessions::interface::{SessionModelPreference, SessionsError, TurnStatus};
use janus_source_control::interface::{GitStatus, SourceControlError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::application::session_flow::PostSessionMessage;
use crate::application::{Application, lifecycle, operation_kinds::KIND_PULL_REQUEST_AUTOMATION};

const GITHUB_HOST: &str = "github.com";
const AUTOMATION_WAIT: StdDuration = StdDuration::from_secs(20 * 60);

#[derive(Debug, Clone)]
pub(crate) struct PullRequestAutomationRequest {
    pub(crate) owner_id: String,
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
    Sessions(#[from] SessionsError),
    #[error(transparent)]
    SourceControl(#[from] SourceControlError),
    #[error("automation serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
}

#[derive(Debug, Deserialize)]
struct PullRequestAutomationWork {
    operation_id: String,
    owner_id: String,
    pull_request_url: String,
    repository_url: String,
    branch: Option<String>,
    project_name: String,
    github_credential_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct AutomationResult {
    project_id: String,
    session_id: String,
    pull_request_url: String,
    message_id: Option<String>,
    turn_id: Option<String>,
    turn_status: String,
    git_status: GitStatus,
}

impl Application {
    /// Enqueue a webhook automation operation. The PAT is used only to create
    /// or reuse an encrypted project credential; it never enters this payload.
    pub(crate) async fn request_pull_request_automation(
        &self,
        input: PullRequestAutomationRequest,
    ) -> Result<OperationView, AutomationError> {
        let credential_id = match input.github_credential_id.clone() {
            Some(id) => {
                self.projects().get_credential(&input.owner_id, &id).await?;
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
                None => None,
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
            "pull_request_url": input.pull_request_url,
            "repository_url": input.repository_url,
            "branch": input.branch,
            "project_name": input.project_name,
            "github_credential_id": credential_id,
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
    let result = execute_pull_request_automation(state, &input).await;
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

    let session = state.sessions().get_session(session_id).await?;
    let prompt = supervisor_prompt(input);
    let message = state
        .post_session_message(PostSessionMessage {
            owner_id: &input.owner_id,
            session_id,
            content: &prompt,
            expected_version: &session.version,
            model_preference: None::<Option<&SessionModelPreference>>,
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

    Ok(AutomationResult {
        project_id: project.id,
        session_id: session_id.to_string(),
        pull_request_url: input.pull_request_url.clone(),
        message_id: Some(message.message_id),
        turn_id: Some(message.turn_id),
        turn_status: turn.status,
        git_status,
    })
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
