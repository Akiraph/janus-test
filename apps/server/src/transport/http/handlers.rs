use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header::HeaderName},
};

use crate::{
    AppState,
    config::RunMode,
    modules::identity::interface::InitializationState,
    modules::runtime::interface::{DeploymentCapabilityProbe, RuntimeCapabilityEvaluator},
    transport::http::{
        dto::{
            BootstrapData, BootstrapResponse, BootstrapState, DatabaseInfo, EventInfo,
            LiveResponse, PublicLimits, ReadyResponse, SystemInfo, SystemInfoResponse,
        },
        problem::Problem,
        request_id::RequestContext,
    },
};

pub static X_EVENT_CURSOR: HeaderName = HeaderName::from_static("x-janus-event-cursor");

#[utoipa::path(get, path = "/health/live", responses((status = 200, body = LiveResponse)))]
pub async fn live() -> Json<LiveResponse> {
    Json(LiveResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[utoipa::path(
    get,
    path = "/health/ready",
    responses((status = 200, body = ReadyResponse), (status = 503, body = Problem))
)]
pub async fn ready(State(state): State<AppState>) -> Result<Json<ReadyResponse>, Problem> {
    if !state.database().ready().await {
        return Err(Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVICE_NOT_READY",
            "Service not ready",
            "The database readiness probe failed.",
        ));
    }
    if !state.recovery_complete() {
        return Err(Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVICE_NOT_READY",
            "Service not ready",
            "Startup recovery has not finished.",
        ));
    }
    let schema_version = state.database().schema_version().await.map_err(|error| {
        tracing::error!(%error, "read schema version");
        Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVICE_NOT_READY",
            "Service not ready",
            "The schema version could not be read.",
        )
    })?;
    Ok(Json(ReadyResponse {
        status: "ready",
        database: "ok",
        schema_version,
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/bootstrap",
    responses((status = 200, body = BootstrapResponse))
)]
pub async fn bootstrap(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
) -> Result<(HeaderMap, Json<BootstrapResponse>), Problem> {
    let cursor = high_water(&state, &context).await?;
    let mut headers = HeaderMap::new();
    insert_cursor(&mut headers, cursor);
    Ok((
        headers,
        Json(BootstrapResponse {
            data: BootstrapData {
                state: match state.identity().initialization_state().await.map_err(|error| {
                    tracing::error!(request_id = %context.request_id, %error, "read initialization state");
                    internal_problem(&context, "The initialization state could not be read.")
                })? {
                    InitializationState::Uninitialized => BootstrapState::Uninitialized,
                    InitializationState::Initialized => BootstrapState::Initialized,
                },
                development_auth: state.config().development_auth,
                webauthn_rp_name: state.config().webauthn_rp_name.clone(),
                version: env!("CARGO_PKG_VERSION"),
                limits: PublicLimits {
                    max_file_bytes: 20 * 1024 * 1024,
                    max_message_bytes: 25 * 1024 * 1024,
                    max_attachments: 20,
                },
            },
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/system/info",
    responses((status = 200, body = SystemInfoResponse), (status = 500, body = Problem))
)]
pub async fn system_info(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
) -> Result<(HeaderMap, Json<SystemInfoResponse>), Problem> {
    let bounds = state.events().bounds().await.map_err(|error| {
        tracing::error!(request_id = %context.request_id, %error, "read event bounds");
        internal_problem(&context, "The event bounds could not be read.")
    })?;
    let schema_version = state.database().schema_version().await.map_err(|error| {
        tracing::error!(request_id = %context.request_id, %error, "read schema version");
        internal_problem(&context, "The schema version could not be read.")
    })?;
    let mut headers = HeaderMap::new();
    insert_cursor(&mut headers, bounds.max);
    Ok((
        headers,
        Json(SystemInfoResponse {
            data: SystemInfo {
                version: env!("CARGO_PKG_VERSION"),
                schema_version,
                mode: state.config().mode.as_str().into(),
                database: DatabaseInfo {
                    engine: "sqlite",
                    journal_mode: "wal",
                    ready: state.database().ready().await,
                },
                events: EventInfo {
                    min_cursor: bounds.min.to_string(),
                    max_cursor: bounds.max.to_string(),
                },
                capabilities: RuntimeCapabilityEvaluator::deployment(
                    &DeploymentCapabilityProbe::detect(),
                    state.config().mode == RunMode::Production,
                ),
                update_available: false,
            },
        }),
    ))
}

async fn high_water(state: &AppState, context: &RequestContext) -> Result<u64, Problem> {
    state
        .events()
        .bounds()
        .await
        .map(|bounds| bounds.max)
        .map_err(|error| {
            tracing::error!(request_id = %context.request_id, %error, "read event cursor");
            internal_problem(context, "The event cursor could not be read.")
        })
}

fn insert_cursor(headers: &mut HeaderMap, cursor: u64) {
    if let Ok(value) = HeaderValue::from_str(&cursor.to_string()) {
        headers.insert(&X_EVENT_CURSOR, value);
    }
}

fn internal_problem(context: &RequestContext, detail: &str) -> Problem {
    let mut problem = Problem::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "INTERNAL_ERROR",
        "Internal server error",
        detail,
    );
    problem.request_id = Some(context.request_id.clone());
    problem
}
