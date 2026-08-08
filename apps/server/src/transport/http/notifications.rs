//! HTTP transport for deployment-owner notification channels.

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use janus_notifications::interface::{
    NotificationChannelInput, NotificationChannelView, NotificationsError,
};

use crate::{
    AppState,
    transport::http::{
        auth::{authenticate, authorized},
        dto::DataResponse,
        problem::{Problem, codes},
        request_id::RequestContext,
    },
};

#[utoipa::path(
    get,
    path = "/api/v1/notification-channels",
    responses((status = 200, body = DataResponse<Vec<NotificationChannelView>>), (status = 401, body = Problem))
)]
pub async fn list_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<Vec<NotificationChannelView>>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    let data = state
        .notifications()
        .channels(&auth.owner_id)
        .await
        .map_err(problem)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    post,
    path = "/api/v1/notification-channels",
    request_body = NotificationChannelInput,
    responses((status = 201, body = DataResponse<NotificationChannelView>), (status = 422, body = Problem))
)]
pub async fn create_channel(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    Json(input): Json<NotificationChannelInput>,
) -> Result<(StatusCode, Json<DataResponse<NotificationChannelView>>), Problem> {
    let auth = authorized(&state, &headers).await?;
    let data = state
        .notifications()
        .create_channel(&auth.owner_id, input, &context.request_id)
        .await
        .map_err(problem)?;
    Ok((StatusCode::CREATED, Json(DataResponse { data })))
}

#[utoipa::path(
    patch,
    path = "/api/v1/notification-channels/{id}",
    params(("id" = String, Path)),
    request_body = NotificationChannelInput,
    responses((status = 200, body = DataResponse<NotificationChannelView>), (status = 404, body = Problem))
)]
pub async fn update_channel(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<NotificationChannelInput>,
) -> Result<Json<DataResponse<NotificationChannelView>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    let data = state
        .notifications()
        .update_channel(&auth.owner_id, &id, input, &context.request_id)
        .await
        .map_err(problem)?;
    Ok(Json(DataResponse { data }))
}

#[utoipa::path(
    delete,
    path = "/api/v1/notification-channels/{id}",
    params(("id" = String, Path)),
    responses((status = 204), (status = 404, body = Problem))
)]
pub async fn delete_channel(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, Problem> {
    let auth = authorized(&state, &headers).await?;
    state
        .notifications()
        .delete_channel(&auth.owner_id, &id, &context.request_id)
        .await
        .map_err(problem)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/notification-channels/{id}/test",
    params(("id" = String, Path)),
    responses((status = 204), (status = 404, body = Problem))
)]
pub async fn test_channel(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, Problem> {
    let auth = authorized(&state, &headers).await?;
    state
        .notifications()
        .test_channel(&auth.owner_id, &id)
        .await
        .map_err(problem)?;
    Ok(StatusCode::NO_CONTENT)
}

fn problem(error: NotificationsError) -> Problem {
    match error {
        NotificationsError::Validation(detail) => {
            Problem::from_code(codes::VALIDATION_FAILED, detail)
        }
        NotificationsError::ChannelNotFound => {
            Problem::from_code(codes::RESOURCE_NOT_FOUND, "notification channel not found")
        }
        NotificationsError::Delivery(detail) => Problem::new(
            StatusCode::BAD_GATEWAY,
            "NOTIFICATION_DELIVERY_FAILED",
            "Notification delivery failed",
            detail,
        ),
        NotificationsError::Storage(error) => Problem::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            codes::INTERNAL_ERROR,
            "Internal server error",
            error.to_string(),
        ),
        NotificationsError::Data(error) => Problem::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            codes::INTERNAL_ERROR,
            "Internal server error",
            error.to_string(),
        ),
        NotificationsError::Internal(error) => Problem::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            codes::INTERNAL_ERROR,
            "Internal server error",
            error.to_string(),
        ),
    }
}
