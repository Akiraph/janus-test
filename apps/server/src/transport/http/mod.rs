mod auth;
pub mod dto;
mod handlers;
mod models;
mod problem;
mod request_id;
mod sse;

use axum::{
    Router, middleware,
    routing::{get, patch, post, put},
};
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
        , auth::initialize_options, auth::initialize_complete, auth::login_options,
        auth::login_complete, auth::me, auth::logout, auth::passkeys, auth::passkey_options,
        auth::passkey_complete, auth::rename_passkey, auth::revoke_passkey,
        auth::regenerate_recovery_codes, auth::recovery_exchange,
        auth::recovery_passkey_options, auth::recovery_passkey_complete,
        models::providers, models::create_provider, models::update_provider,
        models::delete_provider, models::probe_provider, models::models,
        models::create_model, models::update_model, models::delete_model, models::set_failover
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
        dto::InitializeOptionsRequest,
        dto::CeremonyCompleteRequest,
        dto::PasskeyOptionsRequest,
        dto::RenamePasskeyRequest,
        dto::RecoveryExchangeRequest,
        crate::modules::identity::interface::CeremonyOptions,
        crate::modules::identity::interface::OwnerView,
        crate::modules::identity::interface::AuthenticationMode,
        crate::modules::identity::interface::PasskeyView,
        crate::modules::models::interface::ProviderInput,
        crate::modules::models::interface::ProviderView,
        crate::modules::models::interface::ProviderKind,
        crate::modules::models::interface::ProbeResult,
        crate::modules::models::interface::ProbeStatus,
        crate::modules::models::interface::ModelInput,
        crate::modules::models::interface::ModelView,
        crate::modules::models::interface::ContextWindow,
        crate::modules::models::interface::FailoverInput,
        crate::modules::models::interface::FailoverView,
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
        .route(
            "/api/v1/auth/initialize/options",
            post(auth::initialize_options),
        )
        .route(
            "/api/v1/auth/initialize/complete",
            post(auth::initialize_complete),
        )
        .route("/api/v1/auth/passkey/options", post(auth::login_options))
        .route("/api/v1/auth/passkey/complete", post(auth::login_complete))
        .route("/api/v1/auth/logout", post(auth::logout))
        .route("/api/v1/me", get(auth::me))
        .route("/api/v1/me/passkeys", get(auth::passkeys))
        .route("/api/v1/me/passkeys/options", post(auth::passkey_options))
        .route("/api/v1/me/passkeys/complete", post(auth::passkey_complete))
        .route(
            "/api/v1/me/passkeys/{id}",
            patch(auth::rename_passkey).delete(auth::revoke_passkey),
        )
        .route(
            "/api/v1/me/recovery-codes/regenerate",
            post(auth::regenerate_recovery_codes),
        )
        .route(
            "/api/v1/auth/recovery/exchange",
            post(auth::recovery_exchange),
        )
        .route(
            "/api/v1/auth/recovery/passkey/options",
            post(auth::recovery_passkey_options),
        )
        .route(
            "/api/v1/auth/recovery/passkey/complete",
            post(auth::recovery_passkey_complete),
        )
        .route(
            "/api/v1/model-providers",
            get(models::providers).post(models::create_provider),
        )
        .route(
            "/api/v1/model-providers/{id}",
            patch(models::update_provider).delete(models::delete_provider),
        )
        .route(
            "/api/v1/model-providers/{id}/probe",
            post(models::probe_provider),
        )
        .route(
            "/api/v1/models",
            get(models::models).post(models::create_model),
        )
        .route(
            "/api/v1/models/{id}",
            patch(models::update_model).delete(models::delete_model),
        )
        .route("/api/v1/models/{id}/failover", put(models::set_failover))
        .layer(middleware::from_fn(request_id::middleware))
        .with_state(state)
}

pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}
