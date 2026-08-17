//! Public model-provider capability boundary.

use std::time::Duration;

use janus_infrastructure::clock::now_utc_str;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqliteConnection, SqlitePool};
use thiserror::Error;
use url::Url;
use utoipa::ToSchema;

use janus_infrastructure::{
    events::{EventStore, EventType, NewEvent},
    id::{AttemptId, ModelId, ProviderId, RoundId},
    secrets::{Secret, SecretCipher, fingerprint, mask_key},
    unit_of_work::{UnitOfWork, UnitOfWorkTransaction},
};

pub use super::openai_chat::OpenaiChatAssembler;
pub use super::stream_types::{
    ChatMessage, ChatRole, CompletedToolCall, ContentPart, ModelRequest, ModelStreamEvent,
    StreamChannel, TokenUsage, ToolCallDelta, ToolSpec,
};

pub(crate) struct AttemptFinalization<'a> {
    pub(crate) status: &'a str,
    pub(crate) input_tokens: Option<i64>,
    pub(crate) output_tokens: Option<i64>,
    pub(crate) cache_tokens: Option<i64>,
    pub(crate) error_json: Option<&'a serde_json::Value>,
    pub(crate) request: &'a ModelRequest,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Anthropic,
    OpenaiChat,
    OpenaiResponses,
}

/// The client surface a provider is intended to serve.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ModelClient {
    #[default]
    Supervisor,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderInput {
    #[serde(default)]
    pub client: ModelClient,
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
    pub client: ModelClient,
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

#[derive(Debug, Clone)]
pub struct ModelAttemptView {
    pub attempt: i64,
    pub status: String,
    pub detail: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelView {
    pub id: String,
    pub provider_id: String,
    pub display_name: String,
    pub upstream_model_id: String,
    pub context_limit: u32,
    pub supports_images: bool,
    pub supports_tools: bool,
    pub parameters: serde_json::Value,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub model_id: String,
    pub provider_id: String,
    pub display_name: String,
    pub upstream_model_id: String,
    pub context_limit: u32,
    pub supports_images: bool,
    pub supports_tools: bool,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ModelPreference<'a> {
    pub model_id: Option<&'a str>,
    pub provider_id: Option<&'a str>,
    pub upstream_model_id: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelFailoverView {
    pub primary_model_id: String,
    pub candidates: Vec<String>,
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
    unit_of_work: UnitOfWork,
    cipher: SecretCipher,
    client: reqwest::Client,
}

#[derive(FromRow)]
pub(crate) struct ProviderRow {
    pub id: String,
    pub client: String,
    pub kind: String,
    pub display_name: String,
    pub base_url: String,
    pub api_key_ciphertext: Option<Vec<u8>>,
    pub api_key_fingerprint: Option<String>,
    pub api_key_preview: Option<String>,
    pub models_json: String,
    pub enabled: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(FromRow)]
struct ModelRow {
    id: String,
    provider_id: String,
    display_name: String,
    upstream_model_id: String,
    context_limit: i64,
    supports_images: i64,
    supports_tools: i64,
    parameters_json: String,
    enabled: i64,
    created_at: String,
    updated_at: String,
}

impl ModelsInterface {
    pub fn new(pool: SqlitePool, cipher: SecretCipher, events: EventStore) -> anyhow::Result<Self> {
        let unit_of_work = UnitOfWork::new(pool.clone(), events);
        Ok(Self {
            pool,
            unit_of_work,
            cipher,
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }

    pub async fn providers(&self, owner_id: &str) -> Result<Vec<ProviderView>, ModelsError> {
        let rows = sqlx::query_as::<_, ProviderRow>("SELECT id, client, kind, display_name, base_url, api_key_ciphertext, api_key_fingerprint, api_key_preview, models_json, enabled, created_at, updated_at FROM model_providers WHERE owner_id = ? ORDER BY display_name")
            .bind(owner_id).fetch_all(&self.pool).await?;
        rows.into_iter().map(provider_view).collect()
    }

    pub async fn models(&self, owner_id: &str) -> Result<Vec<ModelView>, ModelsError> {
        let rows = sqlx::query_as::<_, ModelRow>(
            "SELECT model.id, model.provider_id, model.display_name, model.upstream_model_id, \
             model.context_limit, model.supports_images, model.supports_tools, model.parameters_json, \
             model.enabled, model.created_at, model.updated_at FROM models AS model \
             JOIN model_providers AS provider ON provider.id = model.provider_id \
             WHERE provider.owner_id = ? ORDER BY provider.display_name, model.display_name",
        )
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(model_view).collect()
    }

    pub async fn provider_kind_in_tx(
        &self,
        tx: &mut SqliteConnection,
        provider_id: &str,
    ) -> Result<ProviderKind, ModelsError> {
        let kind = sqlx::query_scalar::<_, String>(
            "SELECT kind FROM model_providers WHERE id = ? AND enabled = 1",
        )
        .bind(provider_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ModelsError::ProviderNotFound)?;
        parse_kind(&kind)
    }

    pub async fn resolve_for_turn_in_tx(
        &self,
        tx: &mut SqliteConnection,
        owner_id: &str,
        preference: ModelPreference<'_>,
    ) -> Result<Option<ResolvedModel>, ModelsError> {
        let row = if let (Some(provider_id), Some(upstream_model_id)) =
            (preference.provider_id, preference.upstream_model_id)
        {
            sqlx::query_as::<_, ModelRow>(
                "SELECT model.id, model.provider_id, model.display_name, model.upstream_model_id, \
                        model.context_limit, model.supports_images, model.supports_tools, \
                        model.parameters_json, model.enabled, model.created_at, model.updated_at \
                 FROM models AS model \
                 JOIN model_providers AS provider ON provider.id = model.provider_id \
                 WHERE provider.owner_id = ? AND provider.id = ? AND provider.client = 'supervisor' \
                   AND model.upstream_model_id = ? AND provider.enabled = 1 AND model.enabled = 1 \
                 ORDER BY model.display_name, model.id LIMIT 1",
            )
            .bind(owner_id)
            .bind(provider_id)
            .bind(upstream_model_id)
            .fetch_optional(&mut *tx)
            .await?
        } else if let Some(model_id) = preference.model_id {
            sqlx::query_as::<_, ModelRow>(
                "SELECT model.id, model.provider_id, model.display_name, model.upstream_model_id, \
                        model.context_limit, model.supports_images, model.supports_tools, \
                        model.parameters_json, model.enabled, model.created_at, model.updated_at \
                 FROM models AS model \
                 JOIN model_providers AS provider ON provider.id = model.provider_id \
                 WHERE provider.owner_id = ? AND model.id = ? AND provider.client = 'supervisor' \
                   AND provider.enabled = 1 AND model.enabled = 1",
            )
            .bind(owner_id)
            .bind(model_id)
            .fetch_optional(&mut *tx)
            .await?
        } else {
            sqlx::query_as::<_, ModelRow>(
                "SELECT model.id, model.provider_id, model.display_name, model.upstream_model_id, \
                        model.context_limit, model.supports_images, model.supports_tools, \
                        model.parameters_json, model.enabled, model.created_at, model.updated_at \
                 FROM models AS model \
                 JOIN model_providers AS provider ON provider.id = model.provider_id \
                 WHERE provider.owner_id = ? AND provider.client = 'supervisor' \
                   AND provider.enabled = 1 AND model.enabled = 1 \
                 ORDER BY provider.display_name, model.display_name, model.id LIMIT 1",
            )
            .bind(owner_id)
            .fetch_optional(&mut *tx)
            .await?
        };
        row.map(resolved_model).transpose()
    }

    /// Resolve the configured fallback chain at Turn creation time. The
    /// resulting ids are later embedded in the Turn snapshot so a running
    /// Turn does not silently change route when model configuration changes.
    pub async fn failover_candidates_in_tx(
        &self,
        tx: &mut SqliteConnection,
        owner_id: &str,
        primary_model_id: &str,
    ) -> Result<Vec<ResolvedModel>, ModelsError> {
        let candidate_ids: Vec<String> = sqlx::query_scalar(
            "SELECT candidate_model_id FROM model_failover \
             WHERE primary_model_id = ? ORDER BY ordinal",
        )
        .bind(primary_model_id)
        .fetch_all(&mut *tx)
        .await?;
        let mut candidates = Vec::with_capacity(candidate_ids.len());
        for candidate_id in candidate_ids {
            if let Some(model) = self
                .resolve_model_id_in_tx(tx, owner_id, &candidate_id)
                .await?
            {
                candidates.push(model);
            }
        }
        Ok(candidates)
    }

    async fn resolve_model_id_in_tx(
        &self,
        tx: &mut SqliteConnection,
        owner_id: &str,
        model_id: &str,
    ) -> Result<Option<ResolvedModel>, ModelsError> {
        sqlx::query_as::<_, ModelRow>(
            "SELECT model.id, model.provider_id, model.display_name, model.upstream_model_id, \
                    model.context_limit, model.supports_images, model.supports_tools, \
                    model.parameters_json, model.enabled, model.created_at, model.updated_at \
             FROM models AS model \
             JOIN model_providers AS provider ON provider.id = model.provider_id \
              WHERE provider.owner_id = ? AND model.id = ? AND provider.client = 'supervisor' \
               AND provider.enabled = 1 AND model.enabled = 1",
        )
        .bind(owner_id)
        .bind(model_id)
        .fetch_optional(&mut *tx)
        .await?
        .map(resolved_model)
        .transpose()
    }

    pub async fn set_failover(
        &self,
        owner_id: &str,
        primary_model_id: &str,
        candidates: Vec<String>,
        correlation_id: &str,
    ) -> Result<ModelFailoverView, ModelsError> {
        if candidates.len() > 2 {
            return Err(ModelsError::Validation(
                "a primary model supports at most two ordered fallback candidates".into(),
            ));
        }
        let mut unique = std::collections::BTreeSet::new();
        for candidate in &candidates {
            if candidate == primary_model_id || !unique.insert(candidate.as_str()) {
                return Err(ModelsError::Validation(
                    "failover candidates must be unique and cannot contain the primary".into(),
                ));
            }
        }
        let mut ids = Vec::with_capacity(candidates.len() + 1);
        ids.push(primary_model_id);
        ids.extend(candidates.iter().map(String::as_str));
        for id in ids {
            let belongs: i64 = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM models AS model JOIN model_providers AS provider \
                 ON provider.id = model.provider_id WHERE model.id = ? AND provider.owner_id = ? \
                  AND provider.client = 'supervisor' AND model.enabled = 1 AND provider.enabled = 1)",
            )
            .bind(id)
            .bind(owner_id)
            .fetch_one(&self.pool)
            .await?;
            if belongs == 0 {
                return Err(ModelsError::Validation(
                    "every failover model must be enabled and owned by the current owner".into(),
                ));
            }
        }
        let now = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        sqlx::query("DELETE FROM model_failover WHERE primary_model_id = ?")
            .bind(primary_model_id)
            .execute(work.connection())
            .await?;
        for (ordinal, candidate) in candidates.iter().enumerate() {
            sqlx::query(
                "INSERT INTO model_failover \
                 (primary_model_id, candidate_model_id, ordinal, created_at) VALUES (?, ?, ?, ?)",
            )
            .bind(primary_model_id)
            .bind(candidate)
            .bind(i64::try_from(ordinal).unwrap_or(i64::MAX))
            .bind(&now)
            .execute(work.connection())
            .await?;
        }
        self.append_config_changed_in_tx(
            &mut work,
            owner_id,
            primary_model_id,
            "model",
            "failover_updated",
            correlation_id,
        )
        .await?;
        work.commit().await?;
        Ok(ModelFailoverView {
            primary_model_id: primary_model_id.into(),
            candidates,
        })
    }

    pub async fn failover(
        &self,
        owner_id: &str,
        primary_model_id: &str,
    ) -> Result<ModelFailoverView, ModelsError> {
        let belongs: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM models AS model JOIN model_providers AS provider \
             ON provider.id = model.provider_id WHERE model.id = ? AND provider.owner_id = ? \
              AND provider.client = 'supervisor')",
        )
        .bind(primary_model_id)
        .bind(owner_id)
        .fetch_one(&self.pool)
        .await?;
        if belongs == 0 {
            return Err(ModelsError::ProviderNotFound);
        }
        let candidates = sqlx::query_scalar::<_, String>(
            "SELECT candidate_model_id FROM model_failover WHERE primary_model_id = ? ORDER BY ordinal",
        )
        .bind(primary_model_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(ModelFailoverView {
            primary_model_id: primary_model_id.into(),
            candidates,
        })
    }

    pub async fn create_provider(
        &self,
        owner_id: &str,
        input: ProviderInput,
        correlation_id: &str,
    ) -> Result<ProviderView, ModelsError> {
        validate_provider(&input)?;
        validate_models(&input)?;
        self.ensure_provider_name_available(owner_id, input.client, &input.display_name, None)
            .await?;
        let id = ProviderId::new().to_string();
        let now = now_utc_str();
        let (ciphertext, key_fingerprint, key_preview) =
            self.encrypt_key(owner_id, &id, input.api_key.as_deref())?;
        let models_json = serde_json::to_string(
            &input
                .models
                .iter()
                .map(EmbeddedModelInput::to_view)
                .collect::<Vec<_>>(),
        )?;
        let mut work = self.unit_of_work.begin().await?;
        sqlx::query("INSERT INTO model_providers (id, owner_id, client, kind, display_name, base_url, api_key_ciphertext, api_key_fingerprint, api_key_preview, models_json, enabled, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(&id).bind(owner_id).bind(client_str(input.client)).bind(kind_str(input.kind)).bind(input.display_name.trim()).bind(normalize_url(input.kind, &input.base_url)?).bind(ciphertext).bind(key_fingerprint).bind(key_preview).bind(models_json).bind(input.enabled).bind(&now).bind(&now).execute(work.connection()).await?;
        self.sync_normalized_models(work.connection(), &id, &input.models, &now)
            .await?;
        self.append_config_changed_in_tx(
            &mut work,
            owner_id,
            &id,
            "provider",
            "created",
            correlation_id,
        )
        .await?;
        work.commit().await?;
        self.provider(owner_id, &id).await
    }

    pub async fn update_provider(
        &self,
        owner_id: &str,
        id: &str,
        input: ProviderInput,
        correlation_id: &str,
    ) -> Result<ProviderView, ModelsError> {
        validate_provider(&input)?;
        validate_models(&input)?;
        let existing = self.provider_row(owner_id, id).await?;
        self.ensure_provider_name_available(owner_id, input.client, &input.display_name, Some(id))
            .await?;
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
        let now = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        let changed = sqlx::query("UPDATE model_providers SET client=?, kind=?, display_name=?, base_url=?, api_key_ciphertext=?, api_key_fingerprint=?, api_key_preview=?, models_json=?, enabled=?, updated_at=? WHERE id=? AND owner_id=?")
            .bind(client_str(input.client)).bind(kind_str(input.kind)).bind(input.display_name.trim()).bind(normalize_url(input.kind, &input.base_url)?).bind(ciphertext).bind(key_fingerprint).bind(key_preview).bind(models_json).bind(input.enabled).bind(&now).bind(id).bind(owner_id).execute(work.connection()).await?.rows_affected();
        if changed == 0 {
            work.rollback().await?;
            return Err(ModelsError::ProviderNotFound);
        }
        self.sync_normalized_models(work.connection(), id, &input.models, &now)
            .await?;
        self.append_config_changed_in_tx(
            &mut work,
            owner_id,
            id,
            "provider",
            "updated",
            correlation_id,
        )
        .await?;
        work.commit().await?;
        self.provider(owner_id, id).await
    }

    pub async fn delete_provider(
        &self,
        owner_id: &str,
        id: &str,
        correlation_id: &str,
    ) -> Result<(), ModelsError> {
        let mut work = self.unit_of_work.begin().await?;
        if sqlx::query("DELETE FROM model_providers WHERE id=? AND owner_id=?")
            .bind(id)
            .bind(owner_id)
            .execute(work.connection())
            .await?
            .rows_affected()
            == 0
        {
            work.rollback().await?;
            return Err(ModelsError::ProviderNotFound);
        }
        self.append_config_changed_in_tx(
            &mut work,
            owner_id,
            id,
            "provider",
            "deleted",
            correlation_id,
        )
        .await?;
        work.commit().await?;
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
    pub(crate) async fn provider_row(
        &self,
        owner_id: &str,
        id: &str,
    ) -> Result<ProviderRow, ModelsError> {
        sqlx::query_as::<_, ProviderRow>("SELECT id, client, kind, display_name, base_url, api_key_ciphertext, api_key_fingerprint, api_key_preview, models_json, enabled, created_at, updated_at FROM model_providers WHERE id=? AND owner_id=?").bind(id).bind(owner_id).fetch_optional(&self.pool).await?.ok_or(ModelsError::ProviderNotFound)
    }

    async fn ensure_provider_name_available(
        &self,
        owner_id: &str,
        client: ModelClient,
        display_name: &str,
        excluded_id: Option<&str>,
    ) -> Result<(), ModelsError> {
        let name = display_name.trim();
        let exists = if let Some(excluded_id) = excluded_id {
            sqlx::query_scalar::<_, i64>(
                "SELECT EXISTS(SELECT 1 FROM model_providers \
                 WHERE owner_id = ? AND client = ? AND display_name = ? AND id <> ?)",
            )
            .bind(owner_id)
            .bind(client_str(client))
            .bind(name)
            .bind(excluded_id)
            .fetch_one(&self.pool)
            .await?
        } else {
            sqlx::query_scalar::<_, i64>(
                "SELECT EXISTS(SELECT 1 FROM model_providers \
                 WHERE owner_id = ? AND client = ? AND display_name = ?)",
            )
            .bind(owner_id)
            .bind(client_str(client))
            .bind(name)
            .fetch_one(&self.pool)
            .await?
        };
        if exists != 0 {
            return Err(ModelsError::Validation(
                "provider display names must be unique for each client".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn cipher_ref(&self) -> &SecretCipher {
        &self.cipher
    }

    pub(crate) fn client_ref(&self) -> &reqwest::Client {
        &self.client
    }

    pub(crate) async fn provider_row_public(
        &self,
        owner_id: &str,
        id: &str,
    ) -> Result<ProviderRow, ModelsError> {
        self.provider_row(owner_id, id).await
    }

    pub(crate) async fn insert_attempt_running(
        &self,
        attempt_id: &str,
        round_id: &str,
        provider_id: &str,
        upstream_model_id: &str,
        candidate_order: i64,
        created_at: &str,
    ) -> Result<(), ModelsError> {
        sqlx::query(
            "INSERT INTO model_attempts \
             (id, round_id, candidate_order, provider_id, upstream_model_id, attempt_type, \
              status, normalized_error_json, upstream_request_id, input_tokens, output_tokens, \
              created_at, ended_at) \
             VALUES (?, ?, ?, ?, ?, 'normal', 'running', NULL, NULL, NULL, NULL, ?, NULL)",
        )
        .bind(attempt_id)
        .bind(round_id)
        .bind(candidate_order)
        .bind(provider_id)
        .bind(upstream_model_id)
        .bind(created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(crate) async fn finalize_attempt(
        &self,
        attempt_id: &str,
        finalization: AttemptFinalization<'_>,
    ) -> Result<(), ModelsError> {
        let AttemptFinalization {
            status,
            input_tokens,
            output_tokens,
            cache_tokens,
            error_json,
            request: req,
        } = finalization;
        let ended = now_utc_str();
        let changed = sqlx::query(
            "UPDATE model_attempts SET status = ?, input_tokens = ?, output_tokens = ?, \
             normalized_error_json = ?, ended_at = ? WHERE id = ? AND status = 'running'",
        )
        .bind(status)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(error_json.map(|v| v.to_string()))
        .bind(&ended)
        .bind(attempt_id)
        .execute(&self.pool)
        .await?
        .rows_affected();

        // Ledger only when usage was reported (including failed attempts with tokens).
        if changed == 1
            && let (Some(inp), Some(out)) = (input_tokens, output_tokens)
            && let (Some(project_id), Some(session_id), Some(turn_id), Some(round_id)) = (
                req.project_id.as_ref(),
                req.session_id.as_ref(),
                req.turn_id.as_ref(),
                req.round_id.as_ref(),
            )
        {
            let ledger_id = AttemptId::new().to_string();
            sqlx::query(
                "INSERT INTO model_usage_ledger \
                 (id, attempt_id, project_id, session_id, turn_id, round_id, provider_id, \
                  upstream_model_id, input_tokens, output_tokens, cache_tokens, \
                  attempt_result, occurred_at) \
                  VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&ledger_id)
            .bind(attempt_id)
            .bind(project_id)
            .bind(session_id)
            .bind(turn_id)
            .bind(round_id)
            .bind(&req.provider_id)
            .bind(&req.upstream_model_id)
            .bind(inp)
            .bind(out)
            .bind(cache_tokens.unwrap_or(0))
            .bind(status)
            .bind(&ended)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn interrupt_running_attempts_in_tx(
        &self,
        tx: &mut SqliteConnection,
        now: &str,
    ) -> Result<(), ModelsError> {
        sqlx::query(
            "UPDATE model_attempts SET status = 'interrupted', normalized_error_json = ?, \
                    ended_at = ? WHERE status = 'running'",
        )
        .bind(serde_json::json!({"code": "CONTROL_PLANE_RESTART"}).to_string())
        .bind(now)
        .execute(&mut *tx)
        .await?;
        Ok(())
    }

    pub async fn cancel_running_attempts_for_rounds_in_tx(
        &self,
        tx: &mut SqliteConnection,
        round_ids: &[RoundId],
        now: &str,
    ) -> Result<u64, ModelsError> {
        let mut canceled = 0;
        for round_id in round_ids {
            canceled += sqlx::query(
                "UPDATE model_attempts SET status = 'canceled', normalized_error_json = ?, \
                        ended_at = ? WHERE round_id = ? AND status = 'running'",
            )
            .bind(serde_json::json!({"code": "USER_CANCEL"}).to_string())
            .bind(now)
            .bind(round_id.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected();
        }
        Ok(canceled)
    }

    pub async fn delete_attempts_for_rounds_in_tx(
        &self,
        tx: &mut SqliteConnection,
        round_ids: &[RoundId],
    ) -> Result<u64, ModelsError> {
        let mut deleted = 0;
        for round_id in round_ids {
            deleted += sqlx::query("DELETE FROM model_attempts WHERE round_id = ?")
                .bind(round_id.to_string())
                .execute(&mut *tx)
                .await?
                .rows_affected();
        }
        Ok(deleted)
    }

    pub async fn latest_attempt_for_rounds(
        &self,
        round_ids: &[RoundId],
    ) -> Result<Option<ModelAttemptView>, ModelsError> {
        if round_ids.is_empty() {
            return Ok(None);
        }
        let placeholders = vec!["?"; round_ids.len()].join(", ");
        let statement = format!(
            "SELECT attempt_number, status, normalized_error_json FROM model_attempts \
             WHERE round_id IN ({placeholders}) ORDER BY created_at DESC, id DESC LIMIT 1"
        );
        let mut query = sqlx::query_as::<_, (i64, String, Option<String>)>(&statement);
        for round_id in round_ids {
            query = query.bind(round_id.to_string());
        }
        let Some((attempt, status, error_json)) = query.fetch_optional(&self.pool).await? else {
            return Ok(None);
        };
        let detail = error_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .and_then(|value| {
                value
                    .get("detail")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|detail| !detail.is_empty());
        Ok(Some(ModelAttemptView {
            attempt,
            status,
            detail,
        }))
    }

    async fn sync_normalized_models(
        &self,
        tx: &mut SqliteConnection,
        provider_id: &str,
        inputs: &[EmbeddedModelInput],
        now: &str,
    ) -> Result<(), ModelsError> {
        let existing: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT id, display_name, upstream_model_id FROM models WHERE provider_id = ?",
        )
        .bind(provider_id)
        .fetch_all(&mut *tx)
        .await?;
        let mut retained = std::collections::BTreeSet::new();
        for input in inputs {
            let display_name = input.display_name.trim();
            let upstream_model_id = input.upstream_model_id.trim();
            let existing_id = existing
                .iter()
                .find(|(_, _, upstream)| upstream == upstream_model_id)
                .or_else(|| {
                    existing
                        .iter()
                        .find(|(_, display, _)| display == display_name)
                })
                .map(|(id, _, _)| id.clone());
            let id = existing_id.unwrap_or_else(|| ModelId::new().to_string());
            retained.insert(id.clone());
            sqlx::query(
                "INSERT INTO models \
                 (id, provider_id, display_name, upstream_model_id, context_limit, supports_images, \
                  supports_tools, parameters_json, enabled, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, 1, '{}', ?, ?, ?) \
                 ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name, \
                  upstream_model_id=excluded.upstream_model_id, context_limit=excluded.context_limit, \
                  supports_images=excluded.supports_images, enabled=excluded.enabled, updated_at=excluded.updated_at",
            )
            .bind(&id)
            .bind(provider_id)
            .bind(display_name)
            .bind(upstream_model_id)
            .bind(if input.supports_1m { 1_000_000 } else { 200_000 })
            .bind(input.supports_images)
            .bind(input.enabled)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        for (id, _, _) in existing {
            if !retained.contains(&id) {
                sqlx::query("DELETE FROM models WHERE id = ?")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        Ok(())
    }

    async fn append_config_changed_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction<'_>,
        owner_id: &str,
        resource_id: &str,
        resource_kind: &str,
        operation: &str,
        correlation_id: &str,
    ) -> Result<(), ModelsError> {
        work.append_event(NewEvent {
            event_type: EventType::ModelConfigChanged,
            actor: serde_json::json!({"kind": "owner", "id": owner_id}),
            resource: Some(serde_json::json!({"kind": resource_kind, "id": resource_id})),
            correlation_id: correlation_id.to_owned(),
            causation_id: None,
            payload: serde_json::json!({"operation": operation}),
        })
        .await?;
        Ok(())
    }

    fn encrypt_key(
        &self,
        owner_id: &str,
        id: &str,
        key: Option<&str>,
    ) -> Result<EncryptedKeyMaterial, ModelsError> {
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

type EncryptedKeyMaterial = (Option<Vec<u8>>, Option<String>, Option<String>);

fn default_true() -> bool {
    true
}
fn client_str(client: ModelClient) -> &'static str {
    match client {
        ModelClient::Supervisor => "supervisor",
    }
}
fn parse_client(value: &str) -> Result<ModelClient, ModelsError> {
    match value {
        "supervisor" => Ok(ModelClient::Supervisor),
        _ => Err(ModelsError::Validation("unknown model client".into())),
    }
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
        client: parse_client(&row.client)?,
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

fn model_view(row: ModelRow) -> Result<ModelView, ModelsError> {
    Ok(ModelView {
        id: row.id,
        provider_id: row.provider_id,
        display_name: row.display_name,
        upstream_model_id: row.upstream_model_id,
        context_limit: u32::try_from(row.context_limit)
            .map_err(|_| ModelsError::Validation("invalid stored context limit".into()))?,
        supports_images: row.supports_images != 0,
        supports_tools: row.supports_tools != 0,
        parameters: serde_json::from_str(&row.parameters_json)?,
        enabled: row.enabled != 0,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn resolved_model(row: ModelRow) -> Result<ResolvedModel, ModelsError> {
    Ok(ResolvedModel {
        model_id: row.id,
        provider_id: row.provider_id,
        display_name: row.display_name,
        upstream_model_id: row.upstream_model_id,
        context_limit: u32::try_from(row.context_limit)
            .map_err(|_| ModelsError::Validation("invalid stored context limit".into()))?,
        supports_images: row.supports_images != 0,
        supports_tools: row.supports_tools != 0,
        parameters: serde_json::from_str(&row.parameters_json)?,
    })
}
