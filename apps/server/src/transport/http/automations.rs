//! Owner-scoped read model for durable automation runs.

use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    AppState,
    application::automation::{
        AutomationRunView, AutomationSettingsView, UpdateAutomationSettingsInput,
    },
    transport::http::{
        auth::{authenticate, authorized},
        dto::DataResponse,
        problem::Problem,
    },
};

#[derive(Debug, Deserialize)]
pub struct AutomationListQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct AutomationWebhookConfigQuery {
    #[serde(default)]
    pub reveal: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AutomationWebhookConfigView {
    pub enabled: bool,
    pub endpoint: String,
    pub secret_configured: bool,
    /// One-time revealed plaintext; only present with `?reveal=true` and only
    /// when a secret is configured.
    pub secret: Option<String>,
    /// Where the effective secret comes from: "generated" (stored in the
    /// database) or "env" (JANUS_AUTOMATION_WEBHOOK_SECRET at process start).
    pub secret_source: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/v1/automations",
    params(("limit" = Option<i64>, Query, description = "Maximum number of recent runs")),
    responses(
        (status = 200, body = DataResponse<Vec<AutomationRunView>>),
        (status = 401, body = Problem)
    )
)]
pub async fn list_automations(
    State(state): State<AppState>,
    Query(query): Query<AutomationListQuery>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<Vec<AutomationRunView>>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    let runs = state
        .application()
        .list_automation_runs(&auth.owner_id, query.limit.unwrap_or(50))
        .await
        .map_err(|error| Problem::from_code("INTERNAL_ERROR", error.to_string()))?;
    Ok(Json(DataResponse { data: runs }))
}

#[utoipa::path(
    get,
    path = "/api/v1/automation/webhook/config",
    params(("reveal" = Option<bool>, Query, description = "Reveal the configured secret to the authenticated owner")),
    responses(
        (status = 200, body = DataResponse<AutomationWebhookConfigView>),
        (status = 401, body = Problem)
    )
)]
pub async fn webhook_config(
    State(state): State<AppState>,
    Query(query): Query<AutomationWebhookConfigQuery>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<AutomationWebhookConfigView>>, Problem> {
    authenticate(&state, &headers).await?;
    let effective = state
        .application()
        .effective_automation_webhook_secret(state.config().automation_webhook_secret.as_deref())
        .await
        .map_err(|error| Problem::from_code("INTERNAL_ERROR", error.to_string()))?;
    let endpoint = format!(
        "{}/api/v1/automation/webhook",
        state.config().public_origin.as_str().trim_end_matches('/')
    );
    Ok(Json(DataResponse {
        data: AutomationWebhookConfigView {
            enabled: state.config().automation_webhook_enabled,
            endpoint,
            secret_configured: effective.secret.is_some(),
            secret: query.reveal.then_some(effective.secret).flatten(),
            secret_source: effective.source.map(str::to_owned),
        },
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/automation/webhook/secret",
    responses(
        (status = 200, body = DataResponse<AutomationWebhookConfigView>),
        (status = 401, body = Problem)
    )
)]
pub async fn generate_webhook_secret(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<AutomationWebhookConfigView>>, Problem> {
    authenticate(&state, &headers).await?;
    let secret = state
        .application()
        .generate_automation_webhook_secret()
        .await
        .map_err(|error| Problem::from_code("INTERNAL_ERROR", error.to_string()))?;
    let endpoint = format!(
        "{}/api/v1/automation/webhook",
        state.config().public_origin.as_str().trim_end_matches('/')
    );
    Ok(Json(DataResponse {
        data: AutomationWebhookConfigView {
            enabled: state.config().automation_webhook_enabled,
            endpoint,
            secret_configured: true,
            secret: Some(secret),
            secret_source: Some("generated".into()),
        },
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/automation/settings",
    responses(
        (status = 200, body = DataResponse<AutomationSettingsView>),
        (status = 401, body = Problem)
    )
)]
pub async fn get_automation_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DataResponse<AutomationSettingsView>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    let settings = state
        .application()
        .get_automation_settings(&auth.owner_id)
        .await
        .map_err(|error| Problem::from_code("INTERNAL_ERROR", error.to_string()))?;
    Ok(Json(DataResponse { data: settings }))
}

#[utoipa::path(
    patch,
    path = "/api/v1/automation/settings",
    request_body = UpdateAutomationSettingsInput,
    responses(
        (status = 200, body = DataResponse<AutomationSettingsView>),
        (status = 401, body = Problem),
        (status = 422, body = Problem)
    )
)]
pub async fn update_automation_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<UpdateAutomationSettingsInput>,
) -> Result<Json<DataResponse<AutomationSettingsView>>, Problem> {
    let auth = authorized(&state, &headers).await?;
    let settings = state
        .application()
        .update_automation_settings(&auth.owner_id, input)
        .await
        .map_err(|error| match error {
            crate::application::automation::AutomationError::Validation(detail) => {
                Problem::from_code("VALIDATION_FAILED", detail)
            }
            error => Problem::from_code("INTERNAL_ERROR", error.to_string()),
        })?;
    Ok(Json(DataResponse { data: settings }))
}
