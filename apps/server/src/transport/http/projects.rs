//! HTTP transport for the Projects capability: project lifecycle, GitHub PAT
//! credentials and Main Workspace file read/write.
//!
//! Handlers follow the same shape as `transport/http/models.rs`: authenticate or
//! authorize, call the capability, map `ProjectsError` to a `Problem`, and emit a
//! `project.changed` / `project.main_revision_changed` event on mutating side
//! effects. Operations (clone, delete) return `202 + OperationView` because the
//! actual external side effect runs in the background worker; the Operation
//! capability already emits `operation.changed` from inside its transaction.
//!
//! Idempotency keys and `If-Match` are extracted via `transport/http/conditions`.

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::{
    AppState,
    transport::http::{
        auth::{authenticate, authorized},
        conditions::{RawBody, if_match_version, require_idempotency},
        dto::DataResponse,
        problem::Problem,
        request_id::RequestContext,
    },
};
use janus_infrastructure::id::CorrelationId;
use janus_projects::interface::{
    CreateGithubCredentialInput, CreateProjectInput, CredentialProbeResult, GithubCredentialView,
    ProjectView, ProjectsError, RetryProjectInput, UpdateGithubCredentialInput,
};
use janus_workspace::interface::{
    DeleteFileInput, FileMetaView, FileTreeView, MoveFileInput, RevisionRef, SaveTextInput,
};

// ----- Query and request bodies -------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListProjectsQuery {
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateProjectRequest {
    #[serde(default)]
    pub name: Option<String>,
    /// `null` clears the default model; omit to keep the current value.
    #[serde(default)]
    pub default_model_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct FileQuery {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct FileTreeQuery {
    #[serde(default)]
    pub path: Option<String>,
}

// ----- Project lifecycle --------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/projects",
    params(("limit" = Option<u32>, Query, description = "Page size (1-100, default 50)")),
    responses(
        (status = 200, body = DataResponse<Vec<ProjectView>>),
        (status = 401, body = Problem)
    )
)]
pub async fn list_projects(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListProjectsQuery>,
) -> Result<Json<DataResponse<Vec<ProjectView>>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    let limit = query.limit.unwrap_or(50);
    Ok(Json(DataResponse {
        data: state
            .projects()
            .list_projects(&auth.owner_id, limit)
            .await
            .map_err(problem)?,
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/projects",
    request_body = CreateProjectInput,
    responses(
        (status = 202, body = DataResponse<janus_infrastructure::operations::OperationView>),
        (status = 422, body = Problem),
        (status = 401, body = Problem),
        (status = 409, body = Problem)
    )
)]
pub async fn create_project(
    State(state): State<AppState>,
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
    let input: CreateProjectInput = serde_json::from_slice(body.as_slice()).map_err(|error| {
        Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            "Validation failed",
            error.to_string(),
        )
    })?;
    let correlation_id = CorrelationId::new();
    let idempotency = require_idempotency(
        &headers,
        &auth.owner_id,
        "POST",
        "/api/v1/projects",
        body.as_slice(),
    )?;
    let (_project, operation) = state
        .projects()
        .create_project(&auth.owner_id, input, correlation_id, Some(idempotency))
        .await
        .map_err(problem)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse { data: operation })))
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, body = DataResponse<ProjectView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem)
    )
)]
pub async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<ProjectView>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .projects()
            .get_project(&auth.owner_id, &id)
            .await
            .map_err(problem)?,
    }))
}

#[utoipa::path(
    patch,
    path = "/api/v1/projects/{id}",
    params(("id" = String, Path, description = "Project id")),
    request_body = UpdateProjectRequest,
    responses(
        (status = 200, body = DataResponse<ProjectView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 412, body = Problem),
        (status = 428, body = Problem)
    )
)]
pub async fn update_project(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<UpdateProjectRequest>,
) -> Result<Json<DataResponse<ProjectView>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    let expected_version = if_match_version(&headers)?;
    let view = state
        .projects()
        .update_project(
            &auth.owner_id,
            &id,
            &expected_version,
            input.name.as_deref(),
            input.default_model_id.as_ref().map(|m| m.as_deref()),
            &context.request_id,
        )
        .await
        .map_err(problem)?;
    Ok(Json(DataResponse { data: view }))
}

#[utoipa::path(
    delete,
    path = "/api/v1/projects/{id}",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 202, body = DataResponse<janus_infrastructure::operations::OperationView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 428, body = Problem)
    )
)]
pub async fn delete_project(
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
    let expected_version = if_match_version(&headers)?;
    let correlation_id = CorrelationId::new();
    // The DELETE Project contract requires an Idempotency-Key per `API-IDEM-01`;
    // the underlying capability currently records intent via correlation_id only,
    // so we validate presence here and forward the correlation_id.
    let idempotency = require_idempotency(
        &headers,
        &auth.owner_id,
        "DELETE",
        &format!("/api/v1/projects/{id}"),
        body.as_slice(),
    )?;
    let operation = state
        .projects()
        .delete_project(
            &auth.owner_id,
            &id,
            &expected_version,
            correlation_id,
            idempotency,
        )
        .await
        .map_err(problem)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse { data: operation })))
}

#[utoipa::path(
    post,
    path = "/api/v1/projects/{id}/retry",
    params(("id" = String, Path, description = "Project id")),
    request_body = RetryProjectInput,
    responses(
        (status = 202, body = DataResponse<janus_infrastructure::operations::OperationView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 409, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn retry_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<RetryProjectInput>,
) -> Result<
    (
        StatusCode,
        Json<DataResponse<janus_infrastructure::operations::OperationView>>,
    ),
    Problem,
> {
    let auth = authorized(&state, &headers).await?;
    let correlation_id = CorrelationId::new();
    let (_project, operation) = state
        .projects()
        .retry_project(&auth.owner_id, &id, input, correlation_id)
        .await
        .map_err(problem)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse { data: operation })))
}

// ----- GitHub credentials (PAT) -------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/github-credentials",
    responses(
        (status = 200, body = DataResponse<Vec<GithubCredentialView>>),
        (status = 401, body = Problem)
    )
)]
pub async fn list_credentials(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<Vec<GithubCredentialView>>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .projects()
            .list_credentials(&auth.owner_id)
            .await
            .map_err(problem)?,
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/github-credentials",
    request_body = CreateGithubCredentialInput,
    responses(
        (status = 201, body = DataResponse<GithubCredentialView>),
        (status = 401, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn create_credential(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    Json(input): Json<CreateGithubCredentialInput>,
) -> Result<(StatusCode, Json<DataResponse<GithubCredentialView>>), Problem> {
    let auth = authorized(&state, &headers).await?;
    let view = state
        .projects()
        .create_credential(&auth.owner_id, input, &context.request_id)
        .await
        .map_err(problem)?;
    Ok((StatusCode::CREATED, Json(DataResponse { data: view })))
}

#[utoipa::path(
    get,
    path = "/api/v1/github-credentials/{id}",
    params(("id" = String, Path, description = "Credential id")),
    responses(
        (status = 200, body = DataResponse<GithubCredentialView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem)
    )
)]
pub async fn get_credential(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<GithubCredentialView>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .projects()
            .get_credential(&auth.owner_id, &id)
            .await
            .map_err(problem)?,
    }))
}

#[utoipa::path(
    patch,
    path = "/api/v1/github-credentials/{id}",
    params(("id" = String, Path, description = "Credential id")),
    request_body = UpdateGithubCredentialInput,
    responses(
        (status = 200, body = DataResponse<GithubCredentialView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 412, body = Problem),
        (status = 428, body = Problem)
    )
)]
pub async fn update_credential(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<UpdateGithubCredentialInput>,
) -> Result<Json<DataResponse<GithubCredentialView>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    let expected_version = if_match_version(&headers)?;
    let view = state
        .projects()
        .update_credential(
            &auth.owner_id,
            &id,
            &expected_version,
            input,
            &context.request_id,
        )
        .await
        .map_err(problem)?;
    Ok(Json(DataResponse { data: view }))
}

#[utoipa::path(
    delete,
    path = "/api/v1/github-credentials/{id}",
    params(("id" = String, Path, description = "Credential id")),
    responses(
        (status = 204),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 409, body = Problem)
    )
)]
pub async fn delete_credential(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, Problem> {
    let auth = authorized(&state, &headers).await?;
    state
        .projects()
        .delete_credential(&auth.owner_id, &id, &context.request_id)
        .await
        .map_err(problem)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/github-credentials/{id}/probe",
    params(("id" = String, Path, description = "Credential id")),
    responses(
        (status = 200, body = DataResponse<CredentialProbeResult>),
        (status = 401, body = Problem),
        (status = 404, body = Problem)
    )
)]
pub async fn probe_credential(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<CredentialProbeResult>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .projects()
            .probe_credential(&auth.owner_id, &id)
            .await
            .map_err(problem)?,
    }))
}

// ----- Main Workspace files -----------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}/files/meta",
    params(
        ("id" = String, Path, description = "Project id"),
        ("path" = String, Query, description = "Workspace-relative file path")
    ),
    responses(
        (status = 200, body = DataResponse<FileMetaView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn file_meta(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
) -> Result<Json<DataResponse<FileMetaView>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .projects()
            .file_meta(&auth.owner_id, &id, &query.path)
            .await
            .map_err(problem)?,
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}/files/content",
    params(
        ("id" = String, Path, description = "Project id"),
        ("path" = String, Query, description = "Workspace-relative file path")
    ),
    responses(
        (status = 200, description = "Raw file bytes", content_type = "application/octet-stream"),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn file_content(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
) -> Result<Response, Problem> {
    let auth = authenticate(&state, &headers).await?;
    let bytes = state
        .projects()
        .file_content(&auth.owner_id, &id, &query.path)
        .await
        .map_err(problem)?;
    let mime = mime_from_path(&query.path);
    let mut response = (StatusCode::OK, bytes).into_response();
    if let Ok(value) = HeaderValue::from_str(mime) {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    Ok(response)
}

#[utoipa::path(
    put,
    path = "/api/v1/projects/{id}/files/text",
    params(("id" = String, Path, description = "Project id")),
    request_body = SaveTextInput,
    responses(
        (status = 200, body = DataResponse<RevisionRef>),
        (status = 401, body = Problem),
        (status = 412, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn save_text(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<SaveTextInput>,
) -> Result<Json<DataResponse<RevisionRef>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    let actor = serde_json::json!({ "kind": "owner", "id": auth.owner_id });
    let revision = state
        .projects()
        .save_text(&auth.owner_id, &id, input, actor, &context.request_id)
        .await
        .map_err(problem)?;
    Ok(Json(DataResponse { data: revision }))
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{id}/files/tree",
    params(
        ("id" = String, Path, description = "Project id"),
        ("path" = Option<String>, Query, description = "Workspace-relative directory; empty = root")
    ),
    responses(
        (status = 200, body = DataResponse<Vec<FileTreeView>>),
        (status = 401, body = Problem),
        (status = 404, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn file_tree(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<FileTreeQuery>,
) -> Result<Json<DataResponse<Vec<FileTreeView>>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .projects()
            .file_tree(&auth.owner_id, &id, query.path.as_deref().unwrap_or(""))
            .await
            .map_err(problem)?,
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/projects/{id}/files/move",
    params(("id" = String, Path, description = "Project id")),
    request_body = MoveFileInput,
    responses(
        (status = 200, body = DataResponse<RevisionRef>),
        (status = 401, body = Problem),
        (status = 412, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn move_file(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<MoveFileInput>,
) -> Result<Json<DataResponse<RevisionRef>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    let actor = serde_json::json!({ "kind": "owner", "id": auth.owner_id });
    let revision = state
        .projects()
        .move_file(&auth.owner_id, &id, input, actor, &context.request_id)
        .await
        .map_err(problem)?;
    Ok(Json(DataResponse { data: revision }))
}

#[utoipa::path(
    delete,
    path = "/api/v1/projects/{id}/files",
    params(("id" = String, Path, description = "Project id")),
    request_body = DeleteFileInput,
    responses(
        (status = 200, body = DataResponse<RevisionRef>),
        (status = 401, body = Problem),
        (status = 412, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn delete_file(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<DeleteFileInput>,
) -> Result<Json<DataResponse<RevisionRef>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    let actor = serde_json::json!({ "kind": "owner", "id": auth.owner_id });
    let revision = state
        .projects()
        .delete_file(&auth.owner_id, &id, input, actor, &context.request_id)
        .await
        .map_err(problem)?;
    Ok(Json(DataResponse { data: revision }))
}

// ----- Events --------------------------------------------------------------

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
        // GIT_* family — surface as conflict so clients refresh Git state and
        // resolve in the Git sidebar rather than retrying blindly.
        _ => StatusCode::CONFLICT,
    };
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        // Avoid leaking internal detail; the correlation id is in the request.
        return Problem::new(
            status,
            "INTERNAL_ERROR",
            "Internal server error",
            "The project operation could not be completed.",
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
        StatusCode::CONFLICT => "Operation conflict",
        _ => "Request failed",
    }
}

fn mime_from_path(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("rs") => "text/rust",
        Some("md") => "text/markdown",
        Some("toml") => "text/toml",
        Some("json") => "application/json",
        Some("html") | Some("htm") => "text/html",
        Some("css") => "text/css",
        Some("js") => "text/javascript",
        Some("ts") => "text/typescript",
        Some("txt") => "text/plain",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}
