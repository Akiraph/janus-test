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
    // Runtime cleanup first (Jobs/Services/Terminals/Runtime), then the durable
    // Session row + workspace copy. Lives in application so sessions does not
    // depend on the runtime module.
    crate::application::lifecycle::delete_session_with_runtime(&state, session_id, actor)
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

    // Route the message through the M4 state machine: started -> execute the
    // Turn now; queued behind an active Turn -> nothing to run (promoted later);
    // awaiting_handoff (active Turn is `waiting_for_job`) -> perform the atomic
    // Handoff in `application::session_flow` and execute the successor Turn.
    let content = body.content.clone();
    let owner_id = auth.owner_id.clone();
    let outcome = state
        .clone()
        .handle_message(session_id, data.clone(), &content, &owner_id)
        .await
        .map_err(sessions_problem)?;
    if let Some(run_turn) = outcome.run_turn {
        let supervisor = state.supervisor_for_owner(&owner_id);
        let sess_state = state.clone();
        let sess_session_id = session_id;
        let sess_owner_id = owner_id.clone();
        tokio::spawn(async move {
            if let Err(error) = supervisor.execute_turn(run_turn).await {
                tracing::error!(%error, turn_id = %run_turn, "execute_turn failed");
            }
            // After the Turn settles, drain the FIFO queue: completed/canceled
            // promote the next queued Turn (supervisor re-enters it); the queue
            // stays paused for failed/interrupted. promote_oldest_queued is a
            // no-op when the queue is empty or the slot is held.
            if let Some(next) = sess_state
                .sessions()
                .promote_oldest_queued(sess_session_id)
                .await
                .ok()
                .flatten()
            {
                let supervisor = sess_state.supervisor_for_owner(&sess_owner_id);
                if let Err(error) = supervisor.execute_turn(next).await {
                    tracing::error!(%error, turn_id = %next, "promoted execute_turn failed");
                }
            }
        });
    }

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
        "note": "Apply and sync controls are not available yet.",
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
