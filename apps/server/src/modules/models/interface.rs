//! Public model-provider Module boundary.

use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use thiserror::Error;
use url::Url;
use utoipa::ToSchema;

use crate::platform::{
    clock::format_utc,
    id::ProviderId,
    secret::{Secret, SecretCipher, fingerprint, mask_key},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Anthropic,
    OpenaiChat,
    OpenaiResponses,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderInput {
    pub kind: ProviderKind,
    pub display_name: String,
    pub base_url: String,
    #[schema(write_only)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub models: Vec<EmbeddedModelInput>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderView {
    pub id: String,
    pub kind: ProviderKind,
    pub display_name: String,
    pub base_url: String,
    pub api_key_is_set: bool,
    pub api_key_fingerprint: Option<String>,
    pub api_key_preview: Option<String>,
    pub models: Vec<EmbeddedModelView>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Ready,
    AuthenticationFailed,
    Unreachable,
    UpstreamError,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProbeResult {
    pub status: ProbeStatus,
    pub http_status: Option<u16>,
    pub latency_ms: u64,
    pub detail: String,
}

/// A model embedded inside its provider. Stored as an element of the
/// provider's `models_json` array; no standalone identity outside the parent.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EmbeddedModelInput {
    pub display_name: String,
    pub upstream_model_id: String,
    #[serde(default)]
    pub supports_1m: bool,
    #[serde(default)]
    pub supports_images: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EmbeddedModelView {
    pub display_name: String,
    pub upstream_model_id: String,
    pub supports_1m: bool,
    pub supports_images: bool,
    pub enabled: bool,
}

impl EmbeddedModelInput {
    fn to_view(&self) -> EmbeddedModelView {
        EmbeddedModelView {
            display_name: self.display_name.trim().to_owned(),
            upstream_model_id: self.upstream_model_id.trim().to_owned(),
            supports_1m: self.supports_1m,
            supports_images: self.supports_images,
            enabled: self.enabled,
        }
    }
}

#[derive(Debug, Error)]
pub enum ModelsError {
    #[error("the model configuration is invalid: {0}")]
    Validation(String),
    #[error("the provider was not found")]
    ProviderNotFound,
    #[error("model storage failed")]
    Storage(#[from] sqlx::Error),
    #[error("model data is invalid")]
    Data(#[from] serde_json::Error),
    #[error("model operation failed")]
    Internal(#[from] anyhow::Error),
}

#[derive(Clone)]
pub struct ModelsInterface {
    pool: SqlitePool,
    cipher: SecretCipher,
    client: reqwest::Client,
}

#[derive(FromRow)]
struct ProviderRow {
    id: String,
    kind: String,
    display_name: String,
    base_url: String,
    api_key_ciphertext: Option<Vec<u8>>,
    api_key_fingerprint: Option<String>,
    api_key_preview: Option<String>,
    models_json: String,
    enabled: i64,
    created_at: String,
    updated_at: String,
}

impl ModelsInterface {
    pub fn new(pool: SqlitePool, cipher: SecretCipher) -> anyhow::Result<Self> {
        Ok(Self {
            pool,
            cipher,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }

    pub async fn providers(&self, owner_id: &str) -> Result<Vec<ProviderView>, ModelsError> {
        let rows = sqlx::query_as::<_, ProviderRow>("SELECT id, kind, display_name, base_url, api_key_ciphertext, api_key_fingerprint, api_key_preview, models_json, enabled, created_at, updated_at FROM model_providers WHERE owner_id = ? ORDER BY display_name")
            .bind(owner_id).fetch_all(&self.pool).await?;
        rows.into_iter().map(provider_view).collect()
    }

    pub async fn create_provider(
        &self,
        owner_id: &str,
        input: ProviderInput,
    ) -> Result<ProviderView, ModelsError> {
        validate_provider(&input)?;
        validate_models(&input)?;
        let id = ProviderId::new().to_string();
        let now = format_utc(Utc::now());
        let (ciphertext, key_fingerprint, key_preview) =
            self.encrypt_key(owner_id, &id, input.api_key.as_deref())?;
        let models_json = serde_json::to_string(
            &input
                .models
                .iter()
                .map(EmbeddedModelInput::to_view)
                .collect::<Vec<_>>(),
        )?;
        sqlx::query("INSERT INTO model_providers (id, owner_id, kind, display_name, base_url, api_key_ciphertext, api_key_fingerprint, api_key_preview, models_json, enabled, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(&id).bind(owner_id).bind(kind_str(input.kind)).bind(input.display_name.trim()).bind(normalize_url(input.kind, &input.base_url)?).bind(ciphertext).bind(key_fingerprint).bind(key_preview).bind(models_json).bind(input.enabled).bind(&now).bind(&now).execute(&self.pool).await?;
        self.provider(owner_id, &id).await
    }

    pub async fn update_provider(
        &self,
        owner_id: &str,
        id: &str,
        input: ProviderInput,
    ) -> Result<ProviderView, ModelsError> {
        validate_provider(&input)?;
        validate_models(&input)?;
        let existing = self.provider_row(owner_id, id).await?;
        // When no new key is supplied, keep the existing key material (ciphertext,
        // fingerprint, preview) unchanged.
        let (ciphertext, key_fingerprint, key_preview) = if let Some(key) = input.api_key.as_deref()
        {
            self.encrypt_key(owner_id, id, Some(key))?
        } else {
            (
                existing.api_key_ciphertext.clone(),
                existing.api_key_fingerprint.clone(),
                existing.api_key_preview.clone(),
            )
        };
        let models_json = serde_json::to_string(
            &input
                .models
                .iter()
                .map(EmbeddedModelInput::to_view)
                .collect::<Vec<_>>(),
        )?;
        let changed = sqlx::query("UPDATE model_providers SET kind=?, display_name=?, base_url=?, api_key_ciphertext=?, api_key_fingerprint=?, api_key_preview=?, models_json=?, enabled=?, updated_at=? WHERE id=? AND owner_id=?")
            .bind(kind_str(input.kind)).bind(input.display_name.trim()).bind(normalize_url(input.kind, &input.base_url)?).bind(ciphertext).bind(key_fingerprint).bind(key_preview).bind(models_json).bind(input.enabled).bind(format_utc(Utc::now())).bind(id).bind(owner_id).execute(&self.pool).await?.rows_affected();
        if changed == 0 {
            return Err(ModelsError::ProviderNotFound);
        }
        self.provider(owner_id, id).await
    }

    pub async fn delete_provider(&self, owner_id: &str, id: &str) -> Result<(), ModelsError> {
        if sqlx::query("DELETE FROM model_providers WHERE id=? AND owner_id=?")
            .bind(id)
            .bind(owner_id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            == 0
        {
            return Err(ModelsError::ProviderNotFound);
        }
        Ok(())
    }

    pub async fn probe(&self, owner_id: &str, id: &str) -> Result<ProbeResult, ModelsError> {
        let row = self.provider_row(owner_id, id).await?;
        let key = row
            .api_key_ciphertext
            .as_deref()
            .map(|stored| self.cipher.decrypt(stored, &key_aad(owner_id, id)))
            .transpose()?;
        // Append the models probe path without Url::join, which would replace the
        // final segment of a non-trailing-slash base (e.g. .../v1 + models → .../models).
        let url = append_path_segment(&row.base_url, "models")?;
        let mut request = self.client.get(url).header("accept", "application/json");
        if let Some(key) = key {
            request = if row.kind == "anthropic" {
                request
                    .header("x-api-key", key.expose())
                    .header("anthropic-version", "2023-06-01")
            } else {
                request.bearer_auth(key.expose())
            };
        }
        let started = std::time::Instant::now();
        match request.send().await {
            Ok(response) => {
                let status = response.status();
                let probe_status = if status.is_success() {
                    ProbeStatus::Ready
                } else if matches!(status.as_u16(), 401 | 403) {
                    ProbeStatus::AuthenticationFailed
                } else {
                    ProbeStatus::UpstreamError
                };
                Ok(ProbeResult {
                    status: probe_status,
                    http_status: Some(status.as_u16()),
                    latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    detail: if status.is_success() {
                        "Provider is reachable and accepted the credentials.".into()
                    } else if matches!(status.as_u16(), 401 | 403) {
                        "The provider rejected the configured credentials.".into()
                    } else {
                        format!("The provider returned HTTP {}.", status.as_u16())
                    },
                })
            }
            Err(error) => Ok(ProbeResult {
                status: ProbeStatus::Unreachable,
                http_status: None,
                latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                detail: if error.is_timeout() {
                    "The provider probe timed out.".into()
                } else {
                    "The provider could not be reached.".into()
                },
            }),
        }
    }

    async fn provider(&self, owner_id: &str, id: &str) -> Result<ProviderView, ModelsError> {
        provider_view(self.provider_row(owner_id, id).await?)
    }
    async fn provider_row(&self, owner_id: &str, id: &str) -> Result<ProviderRow, ModelsError> {
        sqlx::query_as::<_, ProviderRow>("SELECT id, kind, display_name, base_url, api_key_ciphertext, api_key_fingerprint, api_key_preview, models_json, enabled, created_at, updated_at FROM model_providers WHERE id=? AND owner_id=?").bind(id).bind(owner_id).fetch_optional(&self.pool).await?.ok_or(ModelsError::ProviderNotFound)
    }
    fn encrypt_key(
        &self,
        owner_id: &str,
        id: &str,
        key: Option<&str>,
    ) -> Result<(Option<Vec<u8>>, Option<String>, Option<String>), ModelsError> {
        match key {
            Some(value) if !value.trim().is_empty() => Ok((
                Some(
                    self.cipher
                        .encrypt(&Secret::new(value.into()), &key_aad(owner_id, id))?,
                ),
                Some(fingerprint(value)),
                Some(mask_key(value)),
            )),
            Some(_) => Err(ModelsError::Validation("api_key cannot be empty".into())),
            None => Ok((None, None, None)),
        }
    }
}

fn default_true() -> bool {
    true
}
fn kind_str(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::OpenaiChat => "openai_chat",
        ProviderKind::OpenaiResponses => "openai_responses",
    }
}
fn parse_kind(value: &str) -> Result<ProviderKind, ModelsError> {
    match value {
        "anthropic" => Ok(ProviderKind::Anthropic),
        "openai_chat" => Ok(ProviderKind::OpenaiChat),
        "openai_responses" => Ok(ProviderKind::OpenaiResponses),
        _ => Err(ModelsError::Validation("unknown provider kind".into())),
    }
}
fn validate_provider(input: &ProviderInput) -> Result<(), ModelsError> {
    if input.display_name.trim().is_empty() {
        return Err(ModelsError::Validation("display_name is required".into()));
    }
    normalize_url(input.kind, &input.base_url)?;
    Ok(())
}
fn validate_models(input: &ProviderInput) -> Result<(), ModelsError> {
    let mut names = std::collections::HashSet::new();
    for model in &input.models {
        if model.display_name.trim().is_empty() || model.upstream_model_id.trim().is_empty() {
            return Err(ModelsError::Validation(
                "each model needs a display name and an upstream model ID".into(),
            ));
        }
        if !input.enabled && model.enabled {
            return Err(ModelsError::Validation(
                "an enabled model requires an enabled provider".into(),
            ));
        }
        if !names.insert(model.display_name.trim().to_owned()) {
            return Err(ModelsError::Validation(
                "model display names must be unique within a provider".into(),
            ));
        }
    }
    Ok(())
}
/// Normalize a provider base URL for storage.
///
/// Rules:
/// - absolute http(s) URL only
/// - never ends with `/`
/// - Anthropic Messages: host/path as provided (no forced `/v1`)
/// - OpenAI Chat Completions / Responses: empty path becomes `/v1`
fn normalize_url(kind: ProviderKind, value: &str) -> Result<String, ModelsError> {
    let mut url = Url::parse(value.trim())
        .map_err(|_| ModelsError::Validation("base_url must be an absolute URL".into()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(ModelsError::Validation(
            "base_url must use http or https".into(),
        ));
    }
    url.set_query(None);
    url.set_fragment(None);

    let mut path = url.path().trim_end_matches('/').to_owned();
    match kind {
        ProviderKind::Anthropic => {
            // Keep whatever path the user supplied (after stripping trailing slashes).
        }
        ProviderKind::OpenaiChat | ProviderKind::OpenaiResponses => {
            // Host-only bases resolve to `/v1` (no trailing slash).
            if path.is_empty() {
                path = "/v1".into();
            }
        }
    }
    if path.is_empty() {
        // Origin-only URL: set path to `/` for Url, then strip the slash on serialize.
        url.set_path("/");
    } else {
        url.set_path(&path);
    }

    let mut serialized = url.to_string();
    // Product rule: stored base_url never ends with `/`.
    // Do not strip the `//` after the scheme.
    if let Some(scheme_end) = serialized.find("://") {
        let rest_start = scheme_end + 3;
        while serialized.len() > rest_start && serialized.ends_with('/') {
            serialized.pop();
        }
    }
    Ok(serialized)
}

fn append_path_segment(base: &str, segment: &str) -> Result<Url, ModelsError> {
    let mut url =
        Url::parse(base).map_err(|_| ModelsError::Validation("base_url is invalid".into()))?;
    let base_path = url.path().trim_end_matches('/');
    let next = if base_path.is_empty() || base_path == "/" {
        format!("/{segment}")
    } else {
        format!("{base_path}/{segment}")
    };
    url.set_path(&next);
    Ok(url)
}
fn key_aad(owner_id: &str, id: &str) -> String {
    format!("v1/{owner_id}/model_providers/{id}/api_key")
}
fn provider_view(row: ProviderRow) -> Result<ProviderView, ModelsError> {
    let models: Vec<EmbeddedModelView> = serde_json::from_str(&row.models_json)?;
    Ok(ProviderView {
        id: row.id,
        kind: parse_kind(&row.kind)?,
        display_name: row.display_name,
        base_url: row.base_url,
        api_key_is_set: row.api_key_ciphertext.is_some(),
        api_key_fingerprint: row.api_key_fingerprint,
        api_key_preview: row.api_key_preview,
        models,
        enabled: row.enabled != 0,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}
