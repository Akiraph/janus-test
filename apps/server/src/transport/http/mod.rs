pub mod dto;
mod handlers;
mod problem;
mod request_id;
mod sse;

use axum::{Router, middleware, routing::get};
use utoipa::OpenApi;

use crate::AppState;

pub use problem::Problem;

#[derive(OpenApi)]
#[openapi(
    paths(
        handlers::live,
        handlers::ready,
        handlers::bootstrap,
        handlers::system_info,
        sse::events
    ),
    components(schemas(
        dto::LiveResponse,
        dto::ReadyResponse,
        dto::BootstrapResponse,
        dto::BootstrapData,
        dto::BootstrapState,
        dto::PublicLimits,
        dto::SystemInfoResponse,
        dto::SystemInfo,
        dto::DatabaseInfo,
        dto::EventInfo,
        dto::RuntimeCapability,
        dto::CapabilityState,
        dto::CapabilityReason,
        problem::Problem,
        crate::platform::events::EventEnvelope
    )),
    tags((name = "system", description = "Janus system probes"))
)]
pub struct ApiDoc;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get(handlers::live))
        .route("/health/ready", get(handlers::ready))
        .route("/api/v1/bootstrap", get(handlers::bootstrap))
        .route("/api/v1/system/info", get(handlers::system_info))
        .route("/api/v1/events", get(sse::events))
        .layer(middleware::from_fn(request_id::middleware))
        .with_state(state)
}

pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}
