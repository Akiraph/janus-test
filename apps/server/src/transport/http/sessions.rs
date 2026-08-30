//! HTTP transport for the Sessions capability.

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use janus_infrastructure::id::{AttachmentId, ProjectId, SessionId, TurnId, UploadId};
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

use crate::{
    AppState,
    application::{context::CompactContextRequest, session_flow::PostSessionMessage},
    transport::http::{
        auth::{authenticate, authorized},
        conditions::{RawBody, if_match_version, require_idempotency},
        dto::DataResponse,
        problem::{Problem, codes},
        request_id::RequestContext,
    },
};
use janus_infrastructure::{id::CorrelationId, operations::OperationView};
use janus_sessions::interface::{
    AttachmentView, CancelResult, MAX_ATTACHMENT_BYTES, MessageRouteResult, QueuedTurnItem,
    SessionModelPreference, SessionSummary, SessionsError, SteerResult, TimelinePage, TurnSummary,
    UploadAttachmentInput,
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

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ContextUsageView {
    pub estimated_input_tokens: i64,
    pub context_limit: i64,
    pub compact_status: String,
    pub created_at: String,
}

impl From<janus_execution::interface::ContextUsageView> for ContextUsageView {
    fn from(value: janus_execution::interface::ContextUsageView) -> Self {
        Self {
            estimated_input_tokens: value.estimated_input_tokens,
            context_limit: value.context_limit,
            compact_status: value.compact_status,
            created_at: value.created_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PostMessageRequest {
    pub content: String,
    pub expected_session_version: String,
    #[serde(default)]
    pub goal_mode: bool,
    #[serde(default)]
    pub attachment_ids: Vec<AttachmentId>,
    #[serde(default, deserialize_with = "deserialize_model_preference")]
    pub model_preference: Option<Option<SessionModelPreference>>,
}

#[derive(Debug, Deserialize)]
pub struct UploadAttachmentQuery {
    pub name: String,
}

fn deserialize_model_preference<'de, D>(
    deserializer: D,
) -> Result<Option<Option<SessionModelPreference>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<SessionModelPreference>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SteerRequest {
    pub content: String,
    pub expected_session_version: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CancelTurnRequest {
    pub expected_session_version: String,
    #[serde(default)]
    pub reason: Option<String>,
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
    responses((status = 202, body = DataResponse<OperationView>), (status = 401, body = Problem))
)]
pub async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Extension(context): Extension<RequestContext>,
    body: RawBody,
) -> Result<(StatusCode, Json<DataResponse<OperationView>>), Problem> {
    let auth = authorized(&state, &headers).await?;
    let input: CreateSessionRequest = serde_json::from_slice(body.as_slice()).map_err(|error| {
        Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            codes::VALIDATION_FAILED,
            "Validation failed",
            error.to_string(),
        )
    })?;
    let project_id: ProjectId = project_id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid project id"))?;
    let actor = serde_json::json!({
        "kind": "owner",
        "id": auth.owner_id,
        "request_id": context.request_id,
    });
    let idempotency = require_idempotency(
        &headers,
        &auth.owner_id,
        "POST",
        &format!("/api/v1/projects/{project_id}/sessions"),
        body.as_slice(),
    )?;
    let data = crate::application::lifecycle::request_session_creation(
        state.operations(),
        &auth.owner_id,
        project_id,
        input.title,
        actor,
        CorrelationId::new(),
        idempotency,
    )
    .await
    .map_err(lifecycle_problem)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse { data })))
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
    get,
    path = "/api/v1/sessions/{id}/context",
    params(("id" = String, Path)),
    responses((status = 200, body = DataResponse<Option<ContextUsageView>>), (status = 404, body = Problem))
)]
pub async fn session_context(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<DataResponse<Option<ContextUsageView>>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let data = state
        .session_context_usage(session_id)
        .await
        .map_err(sessions_problem)?
        .map(ContextUsageView::from);
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    post,
    path = "/api/v1/sessions/{id}/context/compact",
    params(("id" = String, Path)),
    responses((status = 202, body = DataResponse<OperationView>), (status = 404, body = Problem), (status = 409, body = Problem))
)]
pub async fn compact_context(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Extension(context): Extension<RequestContext>,
    RawBody(body): RawBody,
) -> Result<(StatusCode, Json<DataResponse<OperationView>>), Problem> {
    let auth = authorized(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let idempotency = require_idempotency(
        &headers,
        &auth.owner_id,
        "POST",
        &format!("/api/v1/sessions/{session_id}/context/compact"),
        body.as_ref(),
    )?;
    let data = state
        .application()
        .request_context_compact(CompactContextRequest {
            owner_id: auth.owner_id.clone(),
            session_id,
            actor: serde_json::json!({
                "kind": "owner",
                "id": auth.owner_id,
                "request_id": context.request_id,
            }),
            correlation_id: CorrelationId::new(),
            idempotency,
            context_limit: None,
        })
        .await
        .map_err(sessions_problem)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse { data })))
}

#[utoipa::path(
    delete,
    path = "/api/v1/sessions/{id}",
    params(("id" = String, Path)),
    responses((status = 202, body = DataResponse<OperationView>), (status = 404, body = Problem))
)]
pub async fn delete_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Extension(context): Extension<RequestContext>,
    body: RawBody,
) -> Result<(StatusCode, Json<DataResponse<OperationView>>), Problem> {
    let auth = authorized(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let expected_version = if_match_version(&headers)?;
    let idempotency = require_idempotency(
        &headers,
        &auth.owner_id,
        "DELETE",
        &format!("/api/v1/sessions/{session_id}"),
        body.as_slice(),
    )?;
    let actor = serde_json::json!({
        "kind": "owner",
        "id": auth.owner_id,
        "request_id": context.request_id,
    });
    let data = crate::application::lifecycle::request_session_deletion(
        state.operations(),
        session_id,
        expected_version,
        actor,
        CorrelationId::new(),
        idempotency,
    )
    .await
    .map_err(lifecycle_problem)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse { data })))
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
    let auth = authorized(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let request_bytes = serde_json::to_vec(&body).map_err(|error| {
        Problem::from_code(
            codes::VALIDATION_FAILED,
            format!("invalid message: {error}"),
        )
    })?;
    let idempotency = require_idempotency(
        &headers,
        &auth.owner_id,
        "POST",
        &format!("/api/v1/sessions/{session_id}/messages"),
        &request_bytes,
    )?;
    let actor = serde_json::json!({"kind": "owner", "id": auth.owner_id});
    let data = state
        .application()
        .post_session_message(PostSessionMessage {
            owner_id: &auth.owner_id,
            session_id,
            content: &body.content,
            expected_version: &body.expected_session_version,
            model_preference: body
                .model_preference
                .as_ref()
                .map(|preference| preference.as_ref()),
            attachment_ids: &body.attachment_ids,
            actor,
            goal_mode: body.goal_mode,
            idempotency: Some(idempotency),
        })
        .await
        .map_err(sessions_problem)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    post,
    path = "/api/v1/sessions/{id}/attachments",
    params(("id" = String, Path), ("name" = String, Query)),
    responses((status = 201, body = DataResponse<AttachmentView>), (status = 404, body = Problem), (status = 413, body = Problem), (status = 422, body = Problem))
)]
pub async fn upload_attachment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<UploadAttachmentQuery>,
    RawBody(body): RawBody,
) -> Result<(StatusCode, Json<DataResponse<AttachmentView>>), Problem> {
    let auth = authorized(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let name = query.name.trim();
    if name.is_empty()
        || name.len() > 255
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(Problem::from_code(
            codes::VALIDATION_FAILED,
            "attachment name is invalid",
        ));
    }
    if body.is_empty() {
        return Err(Problem::from_code(
            codes::VALIDATION_FAILED,
            "attachment is empty",
        ));
    }
    if body.len() as u64 > MAX_ATTACHMENT_BYTES {
        return Err(Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "PAYLOAD_TOO_LARGE",
            "Attachment too large",
            format!("an attachment may contain at most {MAX_ATTACHMENT_BYTES} bytes"),
        ));
    }
    let mime = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or("application/octet-stream");
    let upload_id = UploadId::new();
    let attachment_id = AttachmentId::new();
    let attachment = state
        .sessions()
        .create_upload_attachment(UploadAttachmentInput {
            owner_id: &auth.owner_id,
            session_id,
            upload_id,
            attachment_id,
            name,
            mime,
            byte_size: body.len() as u64,
            bytes: body.as_ref(),
        })
        .await
        .map_err(sessions_problem)?;
    Ok((StatusCode::CREATED, Json(DataResponse { data: attachment })))
}

#[utoipa::path(
    delete,
    path = "/api/v1/sessions/{id}/attachments/{attachment_id}",
    params(("id" = String, Path), ("attachment_id" = String, Path)),
    responses((status = 204), (status = 404, body = Problem), (status = 422, body = Problem))
)]
pub async fn delete_attachment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, attachment_id)): Path<(String, String)>,
) -> Result<StatusCode, Problem> {
    let _auth = authorized(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let attachment_id: AttachmentId = attachment_id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid attachment id"))?;
    state
        .sessions()
        .delete_draft_attachment(session_id, attachment_id)
        .await
        .map_err(sessions_problem)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/v1/sessions/{id}/queued-turns",
    params(("id" = String, Path)),
    responses(
        (status = 200, body = DataResponse<Vec<QueuedTurnItem>>),
        (status = 404, body = Problem)
    )
)]
pub async fn queued_turns(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<DataResponse<Vec<QueuedTurnItem>>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let data = state
        .sessions()
        .queued_turns(session_id)
        .await
        .map_err(sessions_problem)?;
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
        .application()
        .turn_summary(session_id, turn_id)
        .await
        .map_err(sessions_problem)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    post,
    path = "/api/v1/sessions/{id}/steer",
    request_body = SteerRequest,
    params(("id" = String, Path)),
    responses((status = 200, body = DataResponse<SteerResult>), (status = 409, body = Problem))
)]
pub async fn steer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Extension(context): Extension<RequestContext>,
    Json(body): Json<SteerRequest>,
) -> Result<Json<DataResponse<SteerResult>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let actor = serde_json::json!({
        "kind": "owner",
        "id": auth.owner_id,
        "request_id": context.request_id,
    });
    let data = state
        .sessions()
        .steer(
            session_id,
            &body.content,
            &body.expected_session_version,
            actor,
        )
        .await
        .map_err(sessions_problem)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    post,
    path = "/api/v1/sessions/{id}/turns/{turn_id}/cancel",
    request_body = CancelTurnRequest,
    params(("id" = String, Path), ("turn_id" = String, Path)),
    responses((status = 200, body = DataResponse<CancelResult>), (status = 409, body = Problem))
)]
pub async fn cancel_turn(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, turn_id)): Path<(String, String)>,
    Extension(context): Extension<RequestContext>,
    Json(body): Json<CancelTurnRequest>,
) -> Result<Json<DataResponse<CancelResult>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let turn_id: TurnId = turn_id
        .parse()
        .map_err(|_| Problem::from_code(codes::RESOURCE_NOT_FOUND, "invalid turn id"))?;
    let actor = serde_json::json!({
        "kind": "owner",
        "id": auth.owner_id,
        "request_id": context.request_id,
    });
    let reason = body.reason.as_deref().unwrap_or("user_cancel");
    let data = state
        .application()
        .cancel_active_turn(
            session_id,
            turn_id,
            reason,
            &body.expected_session_version,
            actor,
        )
        .await
        .map_err(sessions_problem)?;
    Ok(Json(DataResponse { data }))
}

fn sessions_problem(error: SessionsError) -> Problem {
    match error {
        SessionsError::NotFound => Problem::from_code(codes::SESSION_NOT_FOUND, error.to_string()),
        SessionsError::SessionDeleting => {
            Problem::from_code(codes::SESSION_DELETING, error.to_string())
        }
        SessionsError::ActiveTurnExists => {
            Problem::from_code(codes::ACTIVE_TURN_EXISTS, error.to_string())
        }
        SessionsError::TurnNotInteractive => {
            Problem::from_code(codes::TURN_NOT_INTERACTIVE, error.to_string())
        }
        SessionsError::TurnTerminal => Problem::from_code(codes::TURN_TERMINAL, error.to_string()),
        SessionsError::VersionMismatch { .. } => {
            Problem::from_code(codes::RESOURCE_VERSION_MISMATCH, error.to_string())
        }
        SessionsError::TimelineCursorInvalid => {
            Problem::from_code(codes::TIMELINE_CURSOR_INVALID, error.to_string())
        }
        SessionsError::ModelNotConfigured => {
            Problem::from_code(codes::MODEL_NOT_CONFIGURED, error.to_string())
        }
        SessionsError::InvalidModelPreference => {
            Problem::from_code(codes::VALIDATION_FAILED, error.to_string())
        }
        SessionsError::Validation(_) => {
            Problem::from_code(codes::VALIDATION_FAILED, error.to_string())
        }
        other => Problem::from_code(codes::INTERNAL_ERROR, other.to_string()),
    }
}

fn lifecycle_problem(error: crate::application::lifecycle::SessionLifecycleError) -> Problem {
    Problem::from_code(error.code(), error.to_string())
}
