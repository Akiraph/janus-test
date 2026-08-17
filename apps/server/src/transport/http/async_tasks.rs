//! Global asynchronous bash task transport.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use janus_infrastructure::id::AsyncTaskId;
use janus_runtime::interface::{AsyncTaskProjection, LogCursor, LogRange};
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
pub struct AsyncTaskLogQuery {
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[utoipa::path(
    get,
    path = "/api/v1/async-tasks",
    responses((status = 200, body = DataResponse<Vec<AsyncTaskProjection>>), (status = 401, body = Problem))
)]
pub async fn list_async_tasks(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<Vec<AsyncTaskProjection>>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let data = state
        .runtime()
        .async_tasks(200)
        .await
        .map_err(map_runtime_error)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    get,
    path = "/api/v1/async-tasks/{id}/log",
    params(("id" = String, Path), ("after" = Option<String>, Query), ("limit" = Option<usize>, Query)),
    responses((status = 200, body = DataResponse<LogRange>), (status = 401, body = Problem))
)]
pub async fn async_task_log(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<AsyncTaskLogQuery>,
) -> Result<Json<DataResponse<LogRange>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let task_id: AsyncTaskId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid async task id"))?;
    let task = state
        .runtime()
        .async_task(task_id)
        .await
        .map_err(map_runtime_error)?;
    let after = parse_cursor(query.after)?;
    let data = state
        .runtime()
        .log_range(
            task.log_stream_id,
            after,
            query.limit.unwrap_or(1024 * 1024),
        )
        .await
        .map_err(map_runtime_error)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    post,
    path = "/api/v1/async-tasks/{id}/cancel",
    params(("id" = String, Path)),
    responses((status = 200, body = DataResponse<AsyncTaskProjection>), (status = 401, body = Problem))
)]
pub async fn cancel_async_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<DataResponse<AsyncTaskProjection>>, Problem> {
    let _auth = authorized(&state, &headers).await?;
    let task_id: AsyncTaskId = id
        .parse()
        .map_err(|_| Problem::from_code(codes::VALIDATION_FAILED, "invalid async task id"))?;
    let data = state
        .runtime()
        .cancel_async_task(task_id)
        .await
        .map_err(map_runtime_error)?;
    Ok(Json(DataResponse { data }))
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
