//! HTTP projection and log transport for Runtime-owned asynchronous Jobs.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use janus_infrastructure::id::{JobId, SessionId};
use janus_runtime::interface::{JobProjection, LogCursor, LogRange};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::{
    AppState,
    transport::http::{
        auth::{authenticate, authorized},
        dto::DataResponse,
        problem::{Problem, codes, map_runtime_error},
    },
};

#[derive(Debug, Deserialize, ToSchema)]
pub struct JobLogQuery {
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[utoipa::path(
    get,
    path = "/api/v1/sessions/{id}/jobs",
    params(("id" = String, Path)),
    responses((status = 200, body = DataResponse<Vec<JobProjection>>), (status = 401, body = Problem))
)]
pub async fn list_jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<DataResponse<Vec<JobProjection>>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let session_id: SessionId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid session id"))?;
    state
        .sessions()
        .get_session(session_id)
        .await
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "session not found"))?;
    let data = state
        .runtime()
        .jobs(session_id)
        .await
        .map_err(map_runtime_error)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    get,
    path = "/api/v1/jobs/{id}/log",
    params(("id" = String, Path), ("after" = Option<String>, Query), ("limit" = Option<usize>, Query)),
    responses((status = 200, body = DataResponse<LogRange>), (status = 401, body = Problem))
)]
pub async fn job_log(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<JobLogQuery>,
) -> Result<Json<DataResponse<LogRange>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let job_id: JobId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid job id"))?;
    let job = state
        .runtime()
        .job(job_id)
        .await
        .map_err(map_runtime_error)?;
    ensure_session_exists(&state, job.session_id).await?;
    let after = parse_cursor(query.after)?;
    let data = state
        .runtime()
        .log_range(job.log_stream_id, after, query.limit.unwrap_or(1024 * 1024))
        .await
        .map_err(map_runtime_error)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    post,
    path = "/api/v1/jobs/{id}/cancel",
    params(("id" = String, Path)),
    responses((status = 200, body = DataResponse<JobProjection>), (status = 401, body = Problem))
)]
pub async fn cancel_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<DataResponse<JobProjection>>, Problem> {
    let _auth = authorized(&state, &headers).await?;
    let job_id: JobId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid job id"))?;
    let job = state
        .runtime()
        .job(job_id)
        .await
        .map_err(map_runtime_error)?;
    ensure_session_exists(&state, job.session_id).await?;
    let data = state
        .runtime()
        .cancel_job(job_id)
        .await
        .map_err(map_runtime_error)?;
    Ok(Json(DataResponse { data }))
}

async fn ensure_session_exists(state: &AppState, session_id: SessionId) -> Result<(), Problem> {
    state
        .sessions()
        .get_session(session_id)
        .await
        .map(|_| ())
        .map_err(|_| Problem::from_code(codes::SESSION_NOT_FOUND, "session not found"))
}

fn parse_cursor(value: Option<String>) -> Result<LogCursor, Problem> {
    match value {
        None => Ok(LogCursor::ZERO),
        Some(raw) => raw
            .parse::<u64>()
            .map(LogCursor::new)
            .map_err(|_| Problem::from_code(codes::TIMELINE_CURSOR_INVALID, "invalid cursor")),
    }
}
