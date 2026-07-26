//! HTTP transport for Sessions (M3 Stage 5 minimal surface).

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::{
    AppState,
    modules::sessions::types::{
        MessageRouteResult, SessionSummary, SessionsError, TimelinePage, TurnSummary,
    },
    platform::id::{ProjectId, SessionId, TurnId},
    transport::http::{
        auth::authenticate,
        dto::DataResponse,
        problem::{Problem, codes},
        request_id::RequestContext,
    },
};

#[derive(Debug, Deserialize)]
pub struct ListSessionsQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSessionRequest {
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PostMessageRequest {
    pub content: String,
    pub expected_session_version: String,
}

#[derive(Debug, Deserialize)]
pub struct TimelineQuery {
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/api/v1/projects/{project_id}/sessions",
    params(("project_id" = String, Path), ("limit" = Option<i64>, Query)),
    responses((status = 200, body = DataResponse<Vec<SessionSummary>>), (status = 401, body = Problem))
)]
pub async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Query(query): Query<ListSessionsQuery>,
) -> Result<Json<DataResponse<Vec<SessionSummary>>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let project_id: ProjectId = project_id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid project id"))?;
    let data = state
        .sessions()
        .list_sessions(project_id, query.limit.unwrap_or(50))
        .await
        .map_err(sessions_problem)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    post,
    path = "/api/v1/projects/{project_id}/sessions",
    request_body = CreateSessionRequest,
    params(("project_id" = String, Path)),
    responses((status = 201, body = DataResponse<SessionSummary>), (status = 401, body = Problem))
)]
pub async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Extension(context): Extension<RequestContext>,
    Json(body): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<DataResponse<SessionSummary>>), Problem> {
    let auth = authenticate(&state, &headers).await?;
    let project_id: ProjectId = project_id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid project id"))?;
    let actor = serde_json::json!({
        "kind": "owner",
        "id": auth.owner_id,
        "request_id": context.request_id,
    });
    let data = state
        .sessions()
        .create_session(project_id, body.title, actor)
        .await
        .map_err(sessions_problem)?;
    Ok((StatusCode::CREATED, Json(DataResponse { data })))
}

#[utoipa::path(
    get,
    path = "/api/v1/sessions/{id}",
    params(("id" = String, Path)),
    responses((status = 200, body = DataResponse<SessionSummary>), (status = 404, body = Problem))
)]
pub async fn get_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<DataResponse<SessionSummary>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let data = state
        .sessions()
        .get_session(session_id)
        .await
        .map_err(sessions_problem)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    delete,
    path = "/api/v1/sessions/{id}",
    params(("id" = String, Path)),
    responses((status = 204), (status = 404, body = Problem))
)]
pub async fn delete_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, Problem> {
    let auth = authenticate(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let actor = serde_json::json!({"kind": "owner", "id": auth.owner_id});
    state
        .sessions()
        .delete_session(session_id, actor)
        .await
        .map_err(sessions_problem)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/sessions/{id}/messages",
    request_body = PostMessageRequest,
    params(("id" = String, Path)),
    responses((status = 200, body = DataResponse<MessageRouteResult>), (status = 409, body = Problem))
)]
pub async fn post_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Extension(_context): Extension<RequestContext>,
    Json(body): Json<PostMessageRequest>,
) -> Result<Json<DataResponse<MessageRouteResult>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let actor = serde_json::json!({"kind": "owner", "id": auth.owner_id});
    let data = state
        .sessions()
        .post_message(
            session_id,
            &body.content,
            &body.expected_session_version,
            actor,
        )
        .await
        .map_err(sessions_problem)?;

    // Spawn turn execution in the background with the authenticated owner.
    let turn_id: TurnId = data
        .turn_id
        .parse()
        .map_err(|_| Problem::from_code(codes::INTERNAL_ERROR, "invalid turn id"))?;
    let supervisor = state.supervisor_for_owner(&auth.owner_id);
    tokio::spawn(async move {
        if let Err(error) = supervisor.execute_turn(turn_id).await {
            tracing::error!(%error, %turn_id, "execute_turn failed");
        }
    });

    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    get,
    path = "/api/v1/sessions/{id}/timeline",
    params(
        ("id" = String, Path),
        ("before" = Option<String>, Query),
        ("after" = Option<String>, Query),
        ("limit" = Option<i64>, Query)
    ),
    responses((status = 200, body = DataResponse<TimelinePage>), (status = 404, body = Problem))
)]
pub async fn timeline(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<TimelineQuery>,
) -> Result<Json<DataResponse<TimelinePage>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let data = state
        .sessions()
        .timeline(
            session_id,
            query.before.as_deref(),
            query.after.as_deref(),
            query.limit.unwrap_or(50),
        )
        .await
        .map_err(sessions_problem)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    get,
    path = "/api/v1/sessions/{id}/turns/{turn_id}",
    params(("id" = String, Path), ("turn_id" = String, Path)),
    responses((status = 200, body = DataResponse<TurnSummary>), (status = 404, body = Problem))
)]
pub async fn get_turn(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, turn_id)): Path<(String, String)>,
) -> Result<Json<DataResponse<TurnSummary>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let turn_id: TurnId = turn_id
        .parse()
        .map_err(|_| Problem::from_code(codes::RESOURCE_NOT_FOUND, "invalid turn id"))?;
    let data = state
        .sessions()
        .get_turn(session_id, turn_id)
        .await
        .map_err(sessions_problem)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    get,
    path = "/api/v1/sessions/{id}/diff",
    params(("id" = String, Path)),
    responses((status = 200, body = DataResponse<serde_json::Value>), (status = 404, body = Problem))
)]
pub async fn session_diff(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<DataResponse<serde_json::Value>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    // Ensure session exists.
    let _ = state
        .sessions()
        .get_session(session_id)
        .await
        .map_err(sessions_problem)?;
    let summary = state
        .workspace_sync()
        .diff_summary(session_id)
        .await
        .map_err(|e| Problem::from_code(codes::INTERNAL_ERROR, format!("diff failed: {e}")))?;
    let data = serde_json::json!({
        "apply_enabled": false,
        "sync_enabled": false,
        "note": "Apply/Sync land in M5",
        "summary": summary,
    });
    Ok(Json(DataResponse { data }))
}

fn sessions_problem(error: SessionsError) -> Problem {
    match error {
        SessionsError::NotFound => Problem::from_code(codes::SESSION_NOT_FOUND, error.to_string()),
        SessionsError::ProjectNotFound => {
            Problem::from_code(codes::RESOURCE_NOT_FOUND, error.to_string())
        }
        SessionsError::ProjectNotReady => {
            Problem::from_code(codes::VALIDATION_FAILED, error.to_string())
        }
        SessionsError::SessionDeleting => {
            Problem::from_code(codes::SESSION_DELETING, error.to_string())
        }
        SessionsError::ActiveTurnExists => {
            Problem::from_code(codes::ACTIVE_TURN_EXISTS, error.to_string())
        }
        SessionsError::VersionMismatch { .. } => {
            Problem::from_code(codes::RESOURCE_VERSION_MISMATCH, error.to_string())
        }
        SessionsError::TimelineCursorInvalid => {
            Problem::from_code(codes::TIMELINE_CURSOR_INVALID, error.to_string())
        }
        SessionsError::ModelNotConfigured => {
            Problem::from_code(codes::MODEL_NOT_CONFIGURED, error.to_string())
        }
        other => Problem::from_code(codes::INTERNAL_ERROR, other.to_string()),
    }
}
