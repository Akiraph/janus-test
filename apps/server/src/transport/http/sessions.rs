//! HTTP transport for Sessions (M3 Stage 5 minimal surface).

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Deserializer};
use utoipa::ToSchema;

use crate::{
    AppState,
    application::session_flow::PostSessionMessage,
    modules::sessions::interface::{
        AttachmentView, CancelResult, MAX_ATTACHMENT_BYTES, MessageRouteResult, QueuedTurnItem,
        SessionModelPreference, SessionSummary, SessionsError, SteerResult, TimelinePage,
        TurnSummary,
    },
    platform::{
        id::{AskId, AttachmentId, CorrelationId, ProjectId, SessionId, TurnId},
        managed_storage::BlobReference,
        operations::OperationView,
    },
    transport::http::{
        auth::authenticate,
        conditions::{RawBody, if_match_version, require_idempotency},
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

#[derive(Debug, Deserialize, ToSchema)]
pub struct AnswerAskRequest {
    pub answer: serde_json::Value,
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
    let auth = authenticate(&state, &headers).await?;
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
        &state,
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
    responses((status = 200, body = DataResponse<Option<crate::modules::supervisor::interface::ContextUsageView>>), (status = 404, body = Problem))
)]
pub async fn session_context(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<
    Json<DataResponse<Option<crate::modules::supervisor::interface::ContextUsageView>>>,
    Problem,
> {
    let _auth = authenticate(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    state
        .sessions()
        .get_session(session_id)
        .await
        .map_err(sessions_problem)?;
    let data = state
        .supervisor()
        .latest_context_usage(session_id)
        .await
        .map_err(|_| {
            Problem::from_code(codes::INTERNAL_ERROR, "context usage could not be read")
        })?;
    Ok(Json(DataResponse { data }))
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
    let auth = authenticate(&state, &headers).await?;
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
        &state,
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
    let auth = authenticate(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let actor = serde_json::json!({"kind": "owner", "id": auth.owner_id});
    let data = state
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
        })
        .await
        .map_err(sessions_problem)?;
    if matches!(data.route.as_str(), "started" | "handed_off") {
        let run_turn: TurnId = data
            .turn_id
            .parse()
            .map_err(|_| Problem::from_code(codes::INTERNAL_ERROR, "invalid accepted Turn id"))?;
        state.turn_runner().schedule(run_turn);
    }

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
    let auth = authenticate(&state, &headers).await?;
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
    let upload_id = crate::platform::id::UploadId::new();
    let attachment_id = AttachmentId::new();
    let reference = BlobReference::new(
        "sessions",
        "attachment",
        &attachment_id.to_string(),
        "content",
    );
    let blob_sha = state
        .blobs()
        .write(body.as_ref(), reference.clone())
        .await
        .map_err(|error| {
            tracing::error!(%error, "store attachment bytes");
            Problem::from_code(codes::INTERNAL_ERROR, "attachment could not be stored")
        })?;
    let attachment = match state
        .sessions()
        .create_upload_attachment(
            &auth.owner_id,
            session_id,
            upload_id,
            attachment_id,
            name,
            mime,
            body.len() as u64,
            blob_sha.as_str(),
        )
        .await
    {
        Ok(attachment) => attachment,
        Err(error) => {
            let _ = state.blobs().drop_reference(&reference).await;
            return Err(sessions_problem(error));
        }
    };
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
    let _auth = authenticate(&state, &headers).await?;
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
    let reference = BlobReference::new(
        "sessions",
        "attachment",
        &attachment_id.to_string(),
        "content",
    );
    if let Err(error) = state.blobs().drop_reference(&reference).await {
        tracing::warn!(%error, %attachment_id, "drop deleted attachment reference");
    }
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
    let auth = authenticate(&state, &headers).await?;
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
    let auth = authenticate(&state, &headers).await?;
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

#[utoipa::path(
    post,
    path = "/api/v1/asks/{ask_id}/answer",
    request_body = AnswerAskRequest,
    params(("ask_id" = String, Path)),
    responses((status = 200, body = DataResponse<serde_json::Value>), (status = 404, body = Problem))
)]
pub async fn answer_ask(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(ask_id): Path<String>,
    Extension(context): Extension<RequestContext>,
    Json(body): Json<AnswerAskRequest>,
) -> Result<Json<DataResponse<serde_json::Value>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    let ask_id: AskId = ask_id
        .parse()
        .map_err(|_| Problem::from_code(codes::ASK_NOT_FOUND, "invalid ask id"))?;
    let actor = serde_json::json!({
        "kind": "owner",
        "id": auth.owner_id,
        "request_id": context.request_id,
    });
    let (turn_id, route_or_status, session_version) = state
        .answer_ask(&auth.owner_id, ask_id, &body.answer, actor)
        .await
        .map_err(sessions_problem)?;
    Ok(Json(DataResponse {
        data: serde_json::json!({
            "ask_id": ask_id.to_string(),
            "turn_id": turn_id.to_string(),
            "route_or_status": route_or_status,
            "session_version": session_version,
        }),
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/sessions/{id}/turns/{turn_id}/retry-model",
    params(("id" = String, Path), ("turn_id" = String, Path)),
    responses((status = 200, body = DataResponse<serde_json::Value>), (status = 404, body = Problem))
)]
pub async fn retry_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, turn_id)): Path<(String, String)>,
) -> Result<Json<DataResponse<serde_json::Value>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "invalid session id"))?;
    let turn_id: TurnId = turn_id
        .parse()
        .map_err(|_| Problem::from_code(codes::RESOURCE_NOT_FOUND, "invalid turn id"))?;
    // Ensure the Turn belongs to the Session before scheduling retry.
    let _ = state
        .sessions()
        .get_turn(session_id, turn_id)
        .await
        .map_err(sessions_problem)?;
    let scheduled = state
        .retry_waiting_model(turn_id)
        .await
        .map_err(sessions_problem)?;
    Ok(Json(DataResponse {
        data: serde_json::json!({
            "turn_id": turn_id.to_string(),
            "scheduled": scheduled,
        }),
    }))
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
        SessionsError::AskNotFound => Problem::from_code(codes::ASK_NOT_FOUND, error.to_string()),
        SessionsError::AskNotOpen => Problem::from_code(codes::ASK_NOT_OPEN, error.to_string()),
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
