//! Public model-provider Module boundary.

use std::{collections::HashSet, time::Duration};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use thiserror::Error;
use url::Url;
use utoipa::ToSchema;

use crate::platform::{
    clock::format_utc,
    id::{ModelId, ProviderId},
    secret::{Secret, SecretCipher, fingerprint},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Anthropic,
    OpenaiCompatible,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderInput {
    pub kind: ProviderKind,
    pub display_name: String,
    pub base_url: String,
    #[schema(write_only)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub supports_1m: bool,
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
    pub supports_1m: bool,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProbeResult {
    pub status: ProbeStatus,
    pub http_status: Option<u16>,
    pub latency_ms: u64,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Ready,
    AuthenticationFailed,
    Unreachable,
    UpstreamError,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ContextWindow {
    #[serde(rename = "200k")]
    TwoHundredK,
    #[serde(rename = "1m")]
    OneMillion,
}

impl ContextWindow {
    fn as_str(self) -> &'static str {
        match self {
            Self::TwoHundredK => "200k",
            Self::OneMillion => "1m",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelInput {
    pub provider_id: String,
    pub display_name: String,
    pub upstream_model_id: String,
    #[serde(default = "default_context")]
    pub context_window: ContextWindow,
    #[serde(default)]
    pub supports_images: bool,
    #[serde(default)]
    pub supports_tools: bool,
    pub max_output_tokens: u32,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelView {
    pub id: String,
    pub provider_id: String,
    pub display_name: String,
    pub upstream_model_id: String,
    pub context_window: ContextWindow,
    pub supports_images: bool,
    pub supports_tools: bool,
    pub max_output_tokens: u32,
    pub reasoning_effort: Option<String>,
    pub enabled: bool,
    pub failover: FailoverView,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Default)]
pub struct FailoverInput {
    pub enabled: bool,
    pub candidate_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema, Default)]
pub struct FailoverView {
    pub enabled: bool,
    pub candidate_ids: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ModelsError {
    #[error("the model configuration is invalid: {0}")]
    Validation(String),
    #[error("the provider was not found")]
    ProviderNotFound,
    #[error("the model was not found")]
    ModelNotFound,
    #[error("the provider still has configured models")]
    ProviderInUse,
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
    capabilities_json: String,
    enabled: i64,
    created_at: String,
    updated_at: String,
}

#[derive(FromRow)]
struct ModelRow {
    id: String,
    provider_id: String,
    display_name: String,
    upstream_model_id: String,
    context_window: String,
    supports_images: i64,
    supports_tools: i64,
    max_output_tokens: i64,
    reasoning_json: String,
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
        let rows = sqlx::query_as::<_, ProviderRow>("SELECT id, kind, display_name, base_url, api_key_ciphertext, api_key_fingerprint, capabilities_json, enabled, created_at, updated_at FROM model_providers WHERE owner_id = ? ORDER BY display_name")
            .bind(owner_id).fetch_all(&self.pool).await?;
        rows.into_iter().map(provider_view).collect()
    }

    pub async fn create_provider(
        &self,
        owner_id: &str,
        input: ProviderInput,
    ) -> Result<ProviderView, ModelsError> {
        validate_provider(&input)?;
        let id = ProviderId::new().to_string();
        let now = format_utc(Utc::now());
        let (ciphertext, key_fingerprint) =
            self.encrypt_key(owner_id, &id, input.api_key.as_deref())?;
        sqlx::query("INSERT INTO model_providers (id, owner_id, kind, display_name, base_url, api_key_ciphertext, api_key_fingerprint, capabilities_json, enabled, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(&id).bind(owner_id).bind(kind_str(input.kind)).bind(input.display_name.trim()).bind(normalize_url(&input.base_url)?).bind(ciphertext).bind(key_fingerprint).bind(serde_json::json!({"supports_1m": input.supports_1m}).to_string()).bind(input.enabled).bind(&now).bind(&now).execute(&self.pool).await?;
        self.provider(owner_id, &id).await
    }

    pub async fn update_provider(
        &self,
        owner_id: &str,
        id: &str,
        input: ProviderInput,
    ) -> Result<ProviderView, ModelsError> {
        validate_provider(&input)?;
        let exists = self.provider(owner_id, id).await?;
        let (ciphertext, key_fingerprint) = if let Some(key) = input.api_key.as_deref() {
            self.encrypt_key(owner_id, id, Some(key))?
        } else {
            (None, None)
        };
        let changed = if input.api_key.is_some() {
            sqlx::query("UPDATE model_providers SET kind=?, display_name=?, base_url=?, api_key_ciphertext=?, api_key_fingerprint=?, capabilities_json=?, enabled=?, updated_at=? WHERE id=? AND owner_id=?")
                .bind(kind_str(input.kind)).bind(input.display_name.trim()).bind(normalize_url(&input.base_url)?).bind(ciphertext).bind(key_fingerprint).bind(serde_json::json!({"supports_1m": input.supports_1m}).to_string()).bind(input.enabled).bind(format_utc(Utc::now())).bind(id).bind(owner_id).execute(&self.pool).await?.rows_affected()
        } else {
            sqlx::query("UPDATE model_providers SET kind=?, display_name=?, base_url=?, capabilities_json=?, enabled=?, updated_at=? WHERE id=? AND owner_id=?")
                .bind(kind_str(input.kind)).bind(input.display_name.trim()).bind(normalize_url(&input.base_url)?).bind(serde_json::json!({"supports_1m": input.supports_1m}).to_string()).bind(input.enabled).bind(format_utc(Utc::now())).bind(id).bind(owner_id).execute(&self.pool).await?.rows_affected()
        };
        let _ = exists;
        if changed == 0 {
            return Err(ModelsError::ProviderNotFound);
        }
        self.provider(owner_id, id).await
    }

    pub async fn delete_provider(&self, owner_id: &str, id: &str) -> Result<(), ModelsError> {
        let used = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM models WHERE provider_id=? AND owner_id=?",
        )
        .bind(id)
        .bind(owner_id)
        .fetch_one(&self.pool)
        .await?;
        if used > 0 {
            return Err(ModelsError::ProviderInUse);
        }
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
        let url = Url::parse(&row.base_url)
            .map_err(|_| ModelsError::Validation("base_url is invalid".into()))?
            .join("models")
            .map_err(|_| {
                ModelsError::Validation("base_url cannot resolve the models endpoint".into())
            })?;
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

    pub async fn models(&self, owner_id: &str) -> Result<Vec<ModelView>, ModelsError> {
        let rows = sqlx::query_as::<_, ModelRow>("SELECT id, provider_id, display_name, upstream_model_id, context_window, supports_images, supports_tools, max_output_tokens, reasoning_json, enabled, created_at, updated_at FROM models WHERE owner_id=? ORDER BY display_name").bind(owner_id).fetch_all(&self.pool).await?;
        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            result.push(self.model_view(row).await?);
        }
        Ok(result)
    }

    pub async fn create_model(
        &self,
        owner_id: &str,
        input: ModelInput,
    ) -> Result<ModelView, ModelsError> {
        self.validate_model(owner_id, &input).await?;
        let id = ModelId::new().to_string();
        let now = format_utc(Utc::now());
        sqlx::query("INSERT INTO models (id, owner_id, provider_id, display_name, upstream_model_id, context_window, supports_images, supports_tools, max_output_tokens, reasoning_json, enabled, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(&id).bind(owner_id).bind(&input.provider_id).bind(input.display_name.trim()).bind(input.upstream_model_id.trim()).bind(input.context_window.as_str()).bind(input.supports_images).bind(input.supports_tools).bind(input.max_output_tokens).bind(reasoning_json(&input)?).bind(input.enabled).bind(&now).bind(&now).execute(&self.pool).await?;
        self.model(owner_id, &id).await
    }

    pub async fn update_model(
        &self,
        owner_id: &str,
        id: &str,
        input: ModelInput,
    ) -> Result<ModelView, ModelsError> {
        self.validate_model(owner_id, &input).await?;
        let changed = sqlx::query("UPDATE models SET provider_id=?, display_name=?, upstream_model_id=?, context_window=?, supports_images=?, supports_tools=?, max_output_tokens=?, reasoning_json=?, enabled=?, updated_at=? WHERE id=? AND owner_id=?")
            .bind(&input.provider_id).bind(input.display_name.trim()).bind(input.upstream_model_id.trim()).bind(input.context_window.as_str()).bind(input.supports_images).bind(input.supports_tools).bind(input.max_output_tokens).bind(reasoning_json(&input)?).bind(input.enabled).bind(format_utc(Utc::now())).bind(id).bind(owner_id).execute(&self.pool).await?.rows_affected();
        if changed == 0 {
            return Err(ModelsError::ModelNotFound);
        }
        self.model(owner_id, id).await
    }

    pub async fn delete_model(&self, owner_id: &str, id: &str) -> Result<(), ModelsError> {
        if sqlx::query("DELETE FROM models WHERE id=? AND owner_id=?")
            .bind(id)
            .bind(owner_id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            == 0
        {
            return Err(ModelsError::ModelNotFound);
        }
        Ok(())
    }

    pub async fn set_failover(
        &self,
        owner_id: &str,
        id: &str,
        input: FailoverInput,
    ) -> Result<FailoverView, ModelsError> {
        let primary = self.model(owner_id, id).await?;
        let mut unique = HashSet::new();
        let mut warnings = Vec::new();
        for candidate_id in &input.candidate_ids {
            if candidate_id == id {
                return Err(ModelsError::Validation(
                    "failover cannot contain the primary model".into(),
                ));
            }
            if !unique.insert(candidate_id) {
                return Err(ModelsError::Validation(
                    "failover contains a duplicate model".into(),
                ));
            }
            let candidate = self.model(owner_id, candidate_id).await?;
            if !candidate.enabled {
                return Err(ModelsError::Validation(format!(
                    "model '{}' is disabled",
                    candidate.display_name
                )));
            }
            if primary.supports_images && !candidate.supports_images {
                warnings.push(format!(
                    "{} does not support images",
                    candidate.display_name
                ));
            }
            if primary.supports_tools && !candidate.supports_tools {
                warnings.push(format!("{} does not support tools", candidate.display_name));
            }
            if primary.context_window == ContextWindow::OneMillion
                && candidate.context_window != ContextWindow::OneMillion
            {
                warnings.push(format!(
                    "{} has a smaller context window",
                    candidate.display_name
                ));
            }
        }
        sqlx::query("INSERT INTO model_failover (model_id, enabled, candidate_ids_json, updated_at) VALUES (?, ?, ?, ?) ON CONFLICT(model_id) DO UPDATE SET enabled=excluded.enabled, candidate_ids_json=excluded.candidate_ids_json, updated_at=excluded.updated_at")
            .bind(id).bind(input.enabled).bind(serde_json::to_string(&input.candidate_ids)?).bind(format_utc(Utc::now())).execute(&self.pool).await?;
        Ok(FailoverView {
            enabled: input.enabled,
            candidate_ids: input.candidate_ids,
            warnings,
        })
    }

    async fn provider(&self, owner_id: &str, id: &str) -> Result<ProviderView, ModelsError> {
        provider_view(self.provider_row(owner_id, id).await?)
    }
    async fn provider_row(&self, owner_id: &str, id: &str) -> Result<ProviderRow, ModelsError> {
        sqlx::query_as::<_, ProviderRow>("SELECT id, kind, display_name, base_url, api_key_ciphertext, api_key_fingerprint, capabilities_json, enabled, created_at, updated_at FROM model_providers WHERE id=? AND owner_id=?").bind(id).bind(owner_id).fetch_optional(&self.pool).await?.ok_or(ModelsError::ProviderNotFound)
    }
    async fn model(&self, owner_id: &str, id: &str) -> Result<ModelView, ModelsError> {
        let row=sqlx::query_as::<_, ModelRow>("SELECT id, provider_id, display_name, upstream_model_id, context_window, supports_images, supports_tools, max_output_tokens, reasoning_json, enabled, created_at, updated_at FROM models WHERE id=? AND owner_id=?").bind(id).bind(owner_id).fetch_optional(&self.pool).await?.ok_or(ModelsError::ModelNotFound)?;
        self.model_view(row).await
    }
    async fn model_view(&self, row: ModelRow) -> Result<ModelView, ModelsError> {
        let failover = sqlx::query_as::<_, (i64, String)>(
            "SELECT enabled,candidate_ids_json FROM model_failover WHERE model_id=?",
        )
        .bind(&row.id)
        .fetch_optional(&self.pool)
        .await?;
        let reasoning: serde_json::Value = serde_json::from_str(&row.reasoning_json)?;
        Ok(ModelView {
            id: row.id,
            provider_id: row.provider_id,
            display_name: row.display_name,
            upstream_model_id: row.upstream_model_id,
            context_window: parse_context(&row.context_window)?,
            supports_images: row.supports_images != 0,
            supports_tools: row.supports_tools != 0,
            max_output_tokens: u32::try_from(row.max_output_tokens).map_err(|_| {
                ModelsError::Data(serde_json::Error::io(std::io::Error::other(
                    "invalid token count",
                )))
            })?,
            reasoning_effort: reasoning
                .get("effort")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            enabled: row.enabled != 0,
            failover: match failover {
                Some((enabled, ids)) => FailoverView {
                    enabled: enabled != 0,
                    candidate_ids: serde_json::from_str(&ids)?,
                    warnings: Vec::new(),
                },
                None => FailoverView::default(),
            },
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
    async fn validate_model(&self, owner_id: &str, input: &ModelInput) -> Result<(), ModelsError> {
        if input.display_name.trim().is_empty()
            || input.upstream_model_id.trim().is_empty()
            || input.max_output_tokens == 0
        {
            return Err(ModelsError::Validation(
                "name, upstream model ID, and max output tokens are required".into(),
            ));
        }
        let provider = self.provider(owner_id, &input.provider_id).await?;
        if input.context_window == ContextWindow::OneMillion && !provider.supports_1m {
            return Err(ModelsError::Validation(
                "the provider has not confirmed 1m context support".into(),
            ));
        }
        if !provider.enabled && input.enabled {
            return Err(ModelsError::Validation(
                "an enabled model requires an enabled provider".into(),
            ));
        }
        Ok(())
    }
    fn encrypt_key(
        &self,
        owner_id: &str,
        id: &str,
        key: Option<&str>,
    ) -> Result<(Option<Vec<u8>>, Option<String>), ModelsError> {
        match key {
            Some(value) if !value.trim().is_empty() => Ok((
                Some(
                    self.cipher
                        .encrypt(&Secret::new(value.into()), &key_aad(owner_id, id))?,
                ),
                Some(fingerprint(value)),
            )),
            Some(_) => Err(ModelsError::Validation("api_key cannot be empty".into())),
            None => Ok((None, None)),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_context() -> ContextWindow {
    ContextWindow::TwoHundredK
}
fn kind_str(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::OpenaiCompatible => "openai_compatible",
    }
}
fn parse_kind(value: &str) -> Result<ProviderKind, ModelsError> {
    match value {
        "anthropic" => Ok(ProviderKind::Anthropic),
        "openai_compatible" => Ok(ProviderKind::OpenaiCompatible),
        _ => Err(ModelsError::Validation("unknown provider kind".into())),
    }
}
fn parse_context(value: &str) -> Result<ContextWindow, ModelsError> {
    match value {
        "200k" => Ok(ContextWindow::TwoHundredK),
        "1m" => Ok(ContextWindow::OneMillion),
        _ => Err(ModelsError::Validation("unknown context window".into())),
    }
}
fn validate_provider(input: &ProviderInput) -> Result<(), ModelsError> {
    if input.display_name.trim().is_empty() {
        return Err(ModelsError::Validation("display_name is required".into()));
    }
    normalize_url(&input.base_url)?;
    Ok(())
}
fn normalize_url(value: &str) -> Result<String, ModelsError> {
    let mut url = Url::parse(value)
        .map_err(|_| ModelsError::Validation("base_url must be an absolute URL".into()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(ModelsError::Validation(
            "base_url must use http or https".into(),
        ));
    }
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url.to_string())
}
fn key_aad(owner_id: &str, id: &str) -> String {
    format!("v1/{owner_id}/model_providers/{id}/api_key")
}
fn reasoning_json(input: &ModelInput) -> Result<String, ModelsError> {
    if let Some(value) = &input.reasoning_effort
        && !matches!(value.as_str(), "low" | "medium" | "high")
    {
        return Err(ModelsError::Validation(
            "reasoning_effort must be low, medium, or high".into(),
        ));
    }
    Ok(serde_json::json!({"effort":input.reasoning_effort}).to_string())
}
fn provider_view(row: ProviderRow) -> Result<ProviderView, ModelsError> {
    let caps: serde_json::Value = serde_json::from_str(&row.capabilities_json)?;
    Ok(ProviderView {
        id: row.id,
        kind: parse_kind(&row.kind)?,
        display_name: row.display_name,
        base_url: row.base_url,
        api_key_is_set: row.api_key_ciphertext.is_some(),
        api_key_fingerprint: row.api_key_fingerprint,
        supports_1m: caps
            .get("supports_1m")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        enabled: row.enabled != 0,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}
