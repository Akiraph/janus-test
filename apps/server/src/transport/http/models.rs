use crate::{
    AppState,
    transport::http::{
        auth::{authenticate, authorized},
        dto::DataResponse,
        problem::Problem,
        request_id::RequestContext,
    },
};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use janus_models::interface::{ModelsError, ProviderInput};

#[utoipa::path(get, path = "/api/v1/model-providers", responses((status = 200, body = DataResponse<Vec<janus_models::interface::ProviderView>>), (status = 401, body = Problem)))]
pub async fn providers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<Vec<janus_models::interface::ProviderView>>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .models()
            .providers(&auth.owner_id)
            .await
            .map_err(problem)?,
    }))
}
#[utoipa::path(post, path = "/api/v1/model-providers", request_body = ProviderInput, responses((status = 201, body = DataResponse<janus_models::interface::ProviderView>), (status = 422, body = Problem)))]
pub async fn create_provider(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    Json(input): Json<ProviderInput>,
) -> Result<
    (
        StatusCode,
        Json<DataResponse<janus_models::interface::ProviderView>>,
    ),
    Problem,
> {
    let auth = authorized(&state, &headers).await?;
    let view = state
        .models()
        .create_provider(&auth.owner_id, input, &context.request_id)
        .await
        .map_err(problem)?;
    Ok((StatusCode::CREATED, Json(DataResponse { data: view })))
}
#[utoipa::path(patch, path = "/api/v1/model-providers/{id}", params(("id" = String, Path)), request_body = ProviderInput, responses((status = 200, body = DataResponse<janus_models::interface::ProviderView>), (status = 404, body = Problem)))]
pub async fn update_provider(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ProviderInput>,
) -> Result<Json<DataResponse<janus_models::interface::ProviderView>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    let view = state
        .models()
        .update_provider(&auth.owner_id, &id, input, &context.request_id)
        .await
        .map_err(problem)?;
    Ok(Json(DataResponse { data: view }))
}
#[utoipa::path(delete, path = "/api/v1/model-providers/{id}", params(("id" = String, Path)), responses((status = 204), (status = 404, body = Problem)))]
pub async fn delete_provider(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, Problem> {
    let auth = authorized(&state, &headers).await?;
    state
        .models()
        .delete_provider(&auth.owner_id, &id, &context.request_id)
        .await
        .map_err(problem)?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(post, path = "/api/v1/model-providers/{id}/probe", params(("id" = String, Path)), responses((status = 200, body = DataResponse<janus_models::interface::ProbeResult>), (status = 404, body = Problem)))]
pub async fn probe_provider(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<janus_models::interface::ProbeResult>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    Ok(Json(DataResponse {
        data: state
            .models()
            .probe(&auth.owner_id, &id)
            .await
            .map_err(problem)?,
    }))
}

fn problem(error: ModelsError) -> Problem {
    match error {
        ModelsError::Validation(_) => Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            "Validation failed",
            error.to_string(),
        ),
        ModelsError::ProviderNotFound => Problem::new(
            StatusCode::NOT_FOUND,
            "RESOURCE_NOT_FOUND",
            "Resource not found",
            error.to_string(),
        ),
        ModelsError::Storage(_) | ModelsError::Data(_) | ModelsError::Internal(_) => Problem::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "Internal server error",
            "The model configuration operation could not be completed.",
        ),
    }
}
