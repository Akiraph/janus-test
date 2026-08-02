//! HTTP transport for user Git queries and commands under a Project's Main
//! Workspace (`/api/v1/projects/{id}/git/...`).
//!
//! Queries (`status`/`diff`/`log`/`branches`/`remotes`) are read-only and use
//! `authenticate`. Commands under `/git/commands/*` are user-exclusive writes
//! (`TST-GIT-03`): they use `authorized`, and the durable ones (`fetch`/`push`/
//! `update`) return `202 + OperationView`. `stage`/`unstage`/`commit` are short
//! synchronous commands. Update conflicts are listed and resolved under
//! `/git/update-conflicts/*`.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    AppState,
    adapters::git::{DiffView, GitLogEntry, GitStatus},
    modules::projects::interface::{
        GitUpdateConflictView, GitUpdateInput, ProjectsError, ResolveGitUpdateConflictInput,
    },
    transport::http::{
        auth::{authenticate, authorized},
        conditions::{RawBody, if_match_version, require_idempotency},
        dto::DataResponse,
        problem::Problem,
    },
};
use janus_infrastructure::id::CorrelationId;

// ----- Transport DTOs (adapters::git types do not derive ToSchema) --------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GitStatusView {
    pub head_sha: Option<String>,
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub working: Vec<String>,
    pub index: Vec<String>,
    pub untracked: Vec<String>,
}

impl From<GitStatus> for GitStatusView {
    fn from(status: GitStatus) -> Self {
        Self {
            head_sha: status.head_sha,
            branch: status.branch,
            ahead: status.ahead,
            behind: status.behind,
            working: status.working,
            index: status.index,
            untracked: status.untracked,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GitLogEntryView {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub committed_at: String,
    pub message: String,
    pub changed_files: u64,
    pub insertions: u64,
    pub deletions: u64,
}

impl From<GitLogEntry> for GitLogEntryView {
    fn from(entry: GitLogEntry) -> Self {
        Self {
            sha: entry.sha,
            parents: entry.parents,
            author: entry.author,
            committed_at: entry.committed_at,
            message: entry.message,
            changed_files: entry.changed_files,
            insertions: entry.insertions,
            deletions: entry.deletions,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GitLogResponse {
    pub entries: Vec<GitLogEntryView>,
}

// ----- Query and request bodies -------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DiffQuery {
    #[serde(default = "default_diff_view")]
    pub view: DiffViewParam,
}

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiffViewParam {
    WorkingVsIndex,
    IndexVsHead,
    WorkingVsHead,
}

impl DiffViewParam {
    fn into_diff_view(self) -> DiffView {
        match self {
            Self::WorkingVsIndex => DiffView::WorkingVsIndex,
            Self::IndexVsHead => DiffView::IndexVsHead,
            Self::WorkingVsHead => DiffView::WorkingVsHead,
        }
    }
}

fn default_diff_view() -> DiffViewParam {
    DiffViewParam::WorkingVsIndex
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct GitFetchRequest {
    pub remote: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct GitStageRequest {
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct GitCommitRequest {
    pub message: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct GitPushRequest {
    pub remote: String,
    pub branch: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct GitUpdateRequest {
    pub remote: String,
    pub branch: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolveConflictRequest {
    pub paths: Vec<ResolveConflictPathRequest>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolveConflictPathRequest {
    pub path: String,
    pub choice: String,
    #[serde(default)]
    pub edited_text: Option<String>,
}

// ----- Git queries --------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}/git/status",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = DataResponse<GitStatusView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 409, body = Problem)
    )
)]
pub async fn git_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<GitStatusView>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    let status = state
        .projects()
        .git_status(&auth.owner_id, &id)
        .await
        .map_err(problem)?;
    Ok(Json(DataResponse {
        data: status.into(),
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}/git/diff",
    params(
        ("id" = String, Path, description = "Project id"),
        ("view" = Option<DiffViewParam>, Query, description = "Diff view (working_vs_index | index_vs_head | working_vs_head; default working_vs_index)")
    ),
    responses(
        (status = 200, description = "Unified diff text", content_type = "text/plain"),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 409, body = Problem)
    )
)]
pub async fn git_diff(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<DiffQuery>,
) -> Result<Response, Problem> {
    let auth = authenticate(&state, &headers).await?;
    let text = state
        .projects()
        .git_diff(&auth.owner_id, &id, query.view.into_diff_view())
        .await
        .map_err(problem)?;
    let mut response = (StatusCode::OK, text).into_response();
    if let Ok(value) = HeaderValue::from_str("text/plain; charset=utf-8") {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    Ok(response)
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}/git/log",
    params(
        ("id" = String, Path, description = "Project id"),
        ("limit" = Option<u32>, Query, description = "Number of entries (1-200, default 50)")
    ),
    responses(
        (status = 200, body = DataResponse<GitLogResponse>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 409, body = Problem)
    )
)]
pub async fn git_log(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<LogQuery>,
) -> Result<Json<DataResponse<GitLogResponse>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    let limit = query.limit.unwrap_or(50);
    let entries = state
        .projects()
        .git_log(&auth.owner_id, &id, limit)
        .await
        .map_err(problem)?;
    let entries: Vec<GitLogEntryView> = entries.into_iter().map(GitLogEntryView::from).collect();
    Ok(Json(DataResponse {
        data: GitLogResponse { entries },
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}/git/branches",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = DataResponse<Vec<String>>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 409, body = Problem)
    )
)]
pub async fn git_branches(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<Vec<String>>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .projects()
            .git_branches(&auth.owner_id, &id)
            .await
            .map_err(problem)?,
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}/git/remotes",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = DataResponse<Vec<String>>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 409, body = Problem)
    )
)]
pub async fn git_remotes(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<Vec<String>>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .projects()
            .git_remotes(&auth.owner_id, &id)
            .await
            .map_err(problem)?,
    }))
}

// ----- Git commands -------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/v1/projects/{id}/git/commands/fetch",
    params(("id" = String, Path, description = "Project id")),
    request_body = GitFetchRequest,
    responses(
        (status = 202, body = DataResponse<janus_infrastructure::operations::OperationView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 409, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn git_fetch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: RawBody,
) -> Result<
    (
        StatusCode,
        Json<DataResponse<janus_infrastructure::operations::OperationView>>,
    ),
    Problem,
> {
    let auth = authorized(&state, &headers).await?;
    let input: GitFetchRequest = serde_json::from_slice(body.as_slice()).map_err(|error| {
        Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            "Validation failed",
            error.to_string(),
        )
    })?;
    let correlation_id = CorrelationId::new();
    let _idempotency = require_idempotency(
        &headers,
        &auth.owner_id,
        "POST",
        &format!("/api/v1/projects/{id}/git/commands/fetch"),
        body.as_slice(),
    )?;
    let operation = state
        .projects()
        .git_fetch(&auth.owner_id, &id, &input.remote, correlation_id)
        .await
        .map_err(problem)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse { data: operation })))
}

#[utoipa::path(
    post,
    path = "/api/v1/projects/{id}/git/commands/stage",
    params(("id" = String, Path, description = "Project id")),
    request_body = GitStageRequest,
    responses(
        (status = 204),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 409, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn git_stage(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<GitStageRequest>,
) -> Result<StatusCode, Problem> {
    let auth = authorized(&state, &headers).await?;
    state
        .projects()
        .git_stage(&auth.owner_id, &id, &input.paths)
        .await
        .map_err(problem)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/projects/{id}/git/commands/unstage",
    params(("id" = String, Path, description = "Project id")),
    request_body = GitStageRequest,
    responses(
        (status = 204),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 409, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn git_unstage(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<GitStageRequest>,
) -> Result<StatusCode, Problem> {
    let auth = authorized(&state, &headers).await?;
    state
        .projects()
        .git_unstage(&auth.owner_id, &id, &input.paths)
        .await
        .map_err(problem)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/projects/{id}/git/commands/commit",
    params(("id" = String, Path, description = "Project id")),
    request_body = GitCommitRequest,
    responses(
        (status = 200, body = DataResponse<String>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 409, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn git_commit(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<GitCommitRequest>,
) -> Result<Json<DataResponse<String>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    let correlation_id = CorrelationId::new();
    let sha = state
        .projects()
        .git_commit(&auth.owner_id, &id, &input.message, correlation_id)
        .await
        .map_err(problem)?;
    Ok(Json(DataResponse { data: sha }))
}

#[utoipa::path(
    post,
    path = "/api/v1/projects/{id}/git/commands/push",
    params(("id" = String, Path, description = "Project id")),
    request_body = GitPushRequest,
    responses(
        (status = 202, body = DataResponse<janus_infrastructure::operations::OperationView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 409, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn git_push(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: RawBody,
) -> Result<
    (
        StatusCode,
        Json<DataResponse<janus_infrastructure::operations::OperationView>>,
    ),
    Problem,
> {
    let auth = authorized(&state, &headers).await?;
    let input: GitPushRequest = serde_json::from_slice(body.as_slice()).map_err(|error| {
        Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            "Validation failed",
            error.to_string(),
        )
    })?;
    let correlation_id = CorrelationId::new();
    let _idempotency = require_idempotency(
        &headers,
        &auth.owner_id,
        "POST",
        &format!("/api/v1/projects/{id}/git/commands/push"),
        body.as_slice(),
    )?;
    // Resolve the Project's GitHub PAT (if any) inside the Module so private
    // push works without the transport layer ever seeing the secret.
    let operation = state
        .projects()
        .git_push(
            &auth.owner_id,
            &id,
            &input.remote,
            &input.branch,
            correlation_id,
        )
        .await
        .map_err(problem)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse { data: operation })))
}

#[utoipa::path(
    post,
    path = "/api/v1/projects/{id}/git/commands/update",
    params(("id" = String, Path, description = "Project id")),
    request_body = GitUpdateRequest,
    responses(
        (status = 202, body = DataResponse<janus_infrastructure::operations::OperationView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 409, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn git_update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: RawBody,
) -> Result<
    (
        StatusCode,
        Json<DataResponse<janus_infrastructure::operations::OperationView>>,
    ),
    Problem,
> {
    let auth = authorized(&state, &headers).await?;
    let input: GitUpdateRequest = serde_json::from_slice(body.as_slice()).map_err(|error| {
        Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            "Validation failed",
            error.to_string(),
        )
    })?;
    let correlation_id = CorrelationId::new();
    let _idempotency = require_idempotency(
        &headers,
        &auth.owner_id,
        "POST",
        &format!("/api/v1/projects/{id}/git/commands/update"),
        body.as_slice(),
    )?;
    let operation = state
        .projects()
        .git_update(
            &auth.owner_id,
            &id,
            GitUpdateInput {
                remote: input.remote,
                branch: input.branch,
            },
            correlation_id,
        )
        .await
        .map_err(problem)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse { data: operation })))
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}/git/update-conflicts",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = DataResponse<Vec<GitUpdateConflictView>>),
        (status = 401, body = Problem),
        (status = 404, body = Problem)
    )
)]
pub async fn list_update_conflicts(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<Vec<GitUpdateConflictView>>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .projects()
            .list_update_conflicts(&auth.owner_id, &id)
            .await
            .map_err(problem)?,
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}/git/update-conflicts/{conflict_id}",
    params(
        ("id" = String, Path, description = "Project id"),
        ("conflict_id" = String, Path, description = "Conflict id")
    ),
    responses(
        (status = 200, body = DataResponse<GitUpdateConflictView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem)
    )
)]
pub async fn get_update_conflict(
    State(state): State<AppState>,
    Path((id, conflict_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<GitUpdateConflictView>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .projects()
            .get_update_conflict(&auth.owner_id, &id, &conflict_id)
            .await
            .map_err(problem)?,
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/projects/{id}/git/update-conflicts/{conflict_id}/resolve",
    params(
        ("id" = String, Path, description = "Project id"),
        ("conflict_id" = String, Path, description = "Conflict id")
    ),
    request_body = ResolveConflictRequest,
    responses(
        (status = 200, body = DataResponse<GitUpdateConflictView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 409, body = Problem),
        (status = 412, body = Problem),
        (status = 428, body = Problem)
    )
)]
pub async fn resolve_update_conflict(
    State(state): State<AppState>,
    Path((id, conflict_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<ResolveConflictRequest>,
) -> Result<Json<DataResponse<GitUpdateConflictView>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    let expected_version = if_match_version(&headers)?;
    let correlation_id = CorrelationId::new();
    let view = state
        .projects()
        .resolve_update_conflict(
            &auth.owner_id,
            &id,
            &conflict_id,
            &expected_version,
            ResolveGitUpdateConflictInput {
                paths: input
                    .paths
                    .into_iter()
                    .map(
                        |p| crate::modules::projects::interface::ResolveGitUpdateConflictPath {
                            path: p.path,
                            choice: p.choice,
                            edited_text: p.edited_text,
                        },
                    )
                    .collect(),
            },
            correlation_id,
        )
        .await
        .map_err(problem)?;
    Ok(Json(DataResponse { data: view }))
}

// ----- Problem mapping -----------------------------------------------------

fn problem(error: ProjectsError) -> Problem {
    let code = error.code();
    let status = match code {
        "VALIDATION_FAILED" | "INVALID_PATH" | "FILE_NOT_EDITABLE" => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        "RESOURCE_NOT_FOUND" => StatusCode::NOT_FOUND,
        "RESOURCE_VERSION_MISMATCH" => StatusCode::PRECONDITION_FAILED,
        "INTERNAL_ERROR" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::CONFLICT,
    };
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        return Problem::new(
            status,
            "INTERNAL_ERROR",
            "Internal server error",
            "The git operation could not be completed.",
        );
    }
    Problem::new(
        status,
        code,
        status_canonical_title(status),
        error.to_string(),
    )
}

fn status_canonical_title(status: StatusCode) -> &'static str {
    match status {
        StatusCode::UNPROCESSABLE_ENTITY => "Validation failed",
        StatusCode::NOT_FOUND => "Resource not found",
        StatusCode::PRECONDITION_FAILED => "Resource version mismatch",
        StatusCode::CONFLICT => "Git operation conflict",
        _ => "Request failed",
    }
}
