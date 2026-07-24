//! HTTP transport for durable Operations: the read side a client polls after a
//! `202` from clone/delete/git operations. The Operation Module already emits
//! `operation.changed` from its own transaction; this only exposes the current
//! projection.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};

use crate::{
    AppState,
    platform::operations::OperationView,
    transport::http::{auth::authenticate, dto::DataResponse, problem::Problem},
};

#[utoipa::path(
    get,
    path = "/api/v1/operations/{id}",
    params(("id" = String, Path, description = "Operation id")),
    responses(
        (status = 200, body = DataResponse<OperationView>),
        (status = 401, body = Problem),
        (status = 404, body = Problem)
    )
)]
pub async fn get_operation(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<OperationView>>, Problem> {
    let _auth = authenticate(&state, &headers).await?;
    let view = state
        .operations()
        .get(&id)
        .await
        .map_err(|_| {
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Internal server error",
                "The operation could not be read.",
            )
        })?
        .ok_or_else(|| {
            Problem::new(
                StatusCode::NOT_FOUND,
                "RESOURCE_NOT_FOUND",
                "Resource not found",
                "The operation does not exist.",
            )
        })?;
    Ok(Json(DataResponse { data: view }))
}
