//! Public model-provider capability boundary.

use std::time::Duration;

use futures_util::TryStreamExt;
use janus_infrastructure::clock::now_utc_str;
use mongodb::{
    ClientSession,
    bson::{Bson, Document, doc},
    options::UpdateOptions,
};
use serde::{Deserialize, Serialize};
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

pub(crate) struct AttemptRunningRecord<'a> {
    pub(crate) attempt_id: &'a str,
    pub(crate) round_id: &'a str,
    pub(crate) provider_id: &'a str,
    pub(crate) upstream_model_id: &'a str,
    pub(crate) candidate_order: i64,
    pub(crate) attempt_type: AttemptType,
    pub(crate) created_at: &'a str,
}

pub(crate) struct AttemptFinalization<'a> {
    pub(crate) status: &'a str,
    pub(crate) input_tokens: Option<i64>,
    pub(crate) output_tokens: Option<i64>,
    pub(crate) cache_tokens: Option<i64>,
    pub(crate) error_json: Option<&'a serde_json::Value>,
    pub(crate) request: &'a ModelRequest,
}

/// Ledger classification for one Provider stream attempt. Mirrors the
/// `model_attempts.attempt_type` CHECK constraint.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttemptType {
    Normal,
    RecoveryProbe,
    Compact,
}

impl AttemptType {
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptType::Normal => "normal",
            AttemptType::RecoveryProbe => "recovery_probe",
            AttemptType::Compact => "compact",
        }
    }
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
    Storage(#[from] mongodb::error::Error),
    #[error("model data is invalid")]
    Data(#[from] serde_json::Error),
    #[error("model operation failed")]
    Internal(#[from] anyhow::Error),
}

#[derive(Clone)]
pub struct ModelsInterface {
    pool: mongodb::Database,
    unit_of_work: UnitOfWork,
    cipher: SecretCipher,
    client: reqwest::Client,
}

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
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl ProviderRow {
    fn from_doc(doc: &Document) -> Result<Self, ModelsError> {
        Ok(Self {
            id: doc.get_str("_id")?.to_owned(),
            client: doc.get_str("client")?.to_owned(),
            kind: doc.get_str("kind")?.to_owned(),
            display_name: doc.get_str("display_name")?.to_owned(),
            base_url: doc.get_str("base_url")?.to_owned(),
            api_key_ciphertext: doc
                .get("api_key_ciphertext")
                .and_then(Bson::as_binary)
                .map(|binary| binary.bytes.clone()),
            api_key_fingerprint: doc
                .get("api_key_fingerprint")
                .and_then(Bson::as_str)
                .map(str::to_owned),
            api_key_preview: doc
                .get("api_key_preview")
                .and_then(Bson::as_str)
                .map(str::to_owned),
            models_json: doc.get_str("models_json")?.to_owned(),
            enabled: doc.get("enabled").and_then(Bson::as_bool).unwrap_or(false),
            created_at: doc.get_str("created_at")?.to_owned(),
            updated_at: doc.get_str("updated_at")?.to_owned(),
        })
    }
}

struct ModelRow {
    id: String,
    provider_id: String,
    display_name: String,
    upstream_model_id: String,
    context_limit: i64,
    supports_images: bool,
    supports_tools: bool,
    parameters_json: String,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

impl ModelRow {
    fn from_doc(doc: &Document) -> Result<Self, ModelsError> {
        Ok(Self {
            id: doc.get_str("_id")?.to_owned(),
            provider_id: doc.get_str("provider_id")?.to_owned(),
            display_name: doc.get_str("display_name")?.to_owned(),
            upstream_model_id: doc.get_str("upstream_model_id")?.to_owned(),
            context_limit: doc.get("context_limit").and_then(Bson::as_i64).unwrap_or_default(),
            supports_images: doc
                .get("supports_images")
                .and_then(Bson::as_bool)
                .unwrap_or(false),
            supports_tools: doc
                .get("supports_tools")
                .and_then(Bson::as_bool)
                .unwrap_or(false),
            parameters_json: doc.get_str("parameters_json")?.to_owned(),
            enabled: doc.get("enabled").and_then(Bson::as_bool).unwrap_or(false),
            created_at: doc.get_str("created_at")?.to_owned(),
            updated_at: doc.get_str("updated_at")?.to_owned(),
        })
    }
}

/// The model an owner picked for Automation runs. Both fields are nullable
/// because the row also carries `reasoning_effort`, so a row can exist with no
/// model chosen.
#[derive(Debug, Clone)]
pub struct AutomationModelSelection {
    pub model_provider_id: Option<String>,
    pub model_upstream_id: Option<String>,
}

impl ModelsInterface {
    pub fn new(
        pool: mongodb::Database,
        cipher: SecretCipher,
        events: EventStore,
    ) -> anyhow::Result<Self> {
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
        let mut cursor = self
            .pool
            .collection::<Document>("model_providers")
            .find(doc! {"owner_id": owner_id})
            .sort(doc! {"display_name": 1, "_id": 1})
            .await?;
        let mut views = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            views.push(provider_view(ProviderRow::from_doc(&document)?)?);
        }
        Ok(views)
    }

    pub async fn models(&self, owner_id: &str) -> Result<Vec<ModelView>, ModelsError> {
        let mut provider_cursor = self
            .pool
            .collection::<Document>("model_providers")
            .find(doc! {"owner_id": owner_id})
            .sort(doc! {"display_name": 1, "_id": 1})
            .await?;
        let mut provider_names = Vec::new();
        while let Some(document) = provider_cursor.try_next().await? {
            let id = document.get_str("_id")?.to_owned();
            let display_name = document.get_str("display_name")?.to_owned();
            provider_names.push((id, display_name));
        }
        if provider_names.is_empty() {
            return Ok(Vec::new());
        }
        let provider_name: std::collections::HashMap<String, String> =
            provider_names.into_iter().collect();
        let provider_ids: Vec<&str> = provider_name.keys().map(String::as_str).collect();
        let mut model_cursor = self
            .pool
            .collection::<Document>("models")
            .find(doc! {"provider_id": {"$in": provider_ids}})
            .await?;
        let mut documents = Vec::new();
        while let Some(document) = model_cursor.try_next().await? {
            documents.push(document);
        }
        documents.sort_by(|a, b| {
            let an = provider_name
                .get(a.get_str("provider_id").unwrap_or_default())
                .map(String::as_str)
                .unwrap_or_default();
            let bn = provider_name
                .get(b.get_str("provider_id").unwrap_or_default())
                .map(String::as_str)
                .unwrap_or_default();
            an.cmp(bn)
                .then_with(|| {
                    a.get_str("display_name")
                        .unwrap_or_default()
                        .cmp(b.get_str("display_name").unwrap_or_default())
                })
                .then_with(|| {
                    a.get_str("_id")
                        .unwrap_or_default()
                        .cmp(b.get_str("_id").unwrap_or_default())
                })
        });
        documents
            .into_iter()
            .map(|document| model_view(ModelRow::from_doc(&document)?))
            .collect()
    }

    pub async fn automation_model_selection(
        &self,
        owner_id: &str,
    ) -> Result<Option<AutomationModelSelection>, ModelsError> {
        let document = self
            .pool
            .collection::<Document>("automation_settings")
            .find_one(doc! {"_id": owner_id})
            .await?;
        Ok(document.map(|document| AutomationModelSelection {
            model_provider_id: document
                .get("model_provider_id")
                .and_then(Bson::as_str)
                .map(str::to_owned),
            model_upstream_id: document
                .get("model_upstream_id")
                .and_then(Bson::as_str)
                .map(str::to_owned),
        }))
    }

    pub async fn set_automation_model_selection(
        &self,
        owner_id: &str,
        model_provider_id: Option<&str>,
        model_upstream_id: Option<&str>,
        now: &str,
    ) -> Result<(), ModelsError> {
        let mut set = doc! {"updated_at": now};
        match model_provider_id {
            Some(value) => {
                set.insert("model_provider_id", value);
            }
            None => {
                set.insert("model_provider_id", Bson::Null);
            }
        }
        match model_upstream_id {
            Some(value) => {
                set.insert("model_upstream_id", value);
            }
            None => {
                set.insert("model_upstream_id", Bson::Null);
            }
        }
        self.pool
            .collection::<Document>("automation_settings")
            .update_one(
                doc! {"_id": owner_id},
                doc! {"$set": set},
                UpdateOptions::builder().upsert(true).build(),
            )
            .await?;
        Ok(())
    }

    pub async fn provider_kind_in_tx(
        &self,
        session: &mut ClientSession,
        provider_id: &str,
    ) -> Result<ProviderKind, ModelsError> {
        let kind = self
            .pool
            .collection::<Document>("model_providers")
            .find_one(doc! {"_id": provider_id, "enabled": true})
            .session(&mut *session)
            .await?
            .ok_or(ModelsError::ProviderNotFound)?
            .get_str("kind")?
            .to_owned();
        parse_kind(&kind)
    }

    pub async fn resolve_for_turn_in_tx(
        &self,
        session: &mut ClientSession,
        owner_id: &str,
        preference: ModelPreference<'_>,
    ) -> Result<Option<ResolvedModel>, ModelsError> {
        let (provider_filter, model_filter) =
            if let (Some(provider_id), Some(upstream_model_id)) =
                (preference.provider_id, preference.upstream_model_id)
            {
                (
                    doc! {
                        "_id": provider_id,
                        "owner_id": owner_id,
                        "client": "supervisor",
                        "enabled": true,
                    },
                    doc! {"upstream_model_id": upstream_model_id},
                )
            } else if let Some(model_id) = preference.model_id {
                (
                    doc! {"owner_id": owner_id, "client": "supervisor", "enabled": true},
                    doc! {"_id": model_id},
                )
            } else {
                (
                    doc! {"owner_id": owner_id, "client": "supervisor", "enabled": true},
                    doc! {},
                )
            };
        Ok(self
            .resolve_models_in_tx(session, provider_filter, model_filter)
            .await?
            .into_iter()
            .next())
    }

    /// Resolve the configured fallback chain at Turn creation time. The
    /// resulting ids are later embedded in the Turn snapshot so a running
    /// Turn does not silently change route when model configuration changes.
    pub async fn failover_candidates_in_tx(
        &self,
        session: &mut ClientSession,
        owner_id: &str,
        primary_model_id: &str,
    ) -> Result<Vec<ResolvedModel>, ModelsError> {
        let mut cursor = self
            .pool
            .collection::<Document>("model_failover")
            .find(doc! {"primary_model_id": primary_model_id})
            .sort(doc! {"ordinal": 1})
            .session(&mut *session)
            .await?;
        let mut candidate_ids = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            candidate_ids.push(document.get_str("candidate_model_id")?.to_owned());
        }
        let mut candidates = Vec::with_capacity(candidate_ids.len());
        for candidate_id in candidate_ids {
            if let Some(model) = self
                .resolve_model_id_in_tx(session, owner_id, &candidate_id)
                .await?
            {
                candidates.push(model);
            }
        }
        Ok(candidates)
    }

    async fn resolve_model_id_in_tx(
        &self,
        session: &mut ClientSession,
        owner_id: &str,
        model_id: &str,
    ) -> Result<Option<ResolvedModel>, ModelsError> {
        Ok(self
            .resolve_models_in_tx(
                session,
                doc! {"owner_id": owner_id, "client": "supervisor", "enabled": true},
                doc! {"_id": model_id},
            )
            .await?
            .into_iter()
            .next())
    }

    /// Enabled supervisor models for an owner, joined with their provider
    /// display names and ordered exactly like the old `ORDER BY
    /// provider.display_name, model.display_name, model.id`.
    async fn resolve_models_in_tx(
        &self,
        session: &mut ClientSession,
        provider_filter: Document,
        model_filter: Document,
    ) -> Result<Vec<ResolvedModel>, ModelsError> {
        let mut provider_cursor = self
            .pool
            .collection::<Document>("model_providers")
            .find(provider_filter)
            .sort(doc! {"display_name": 1, "_id": 1})
            .session(&mut *session)
            .await?;
        let mut provider_names = Vec::new();
        while let Some(document) = provider_cursor.try_next().await? {
            let id = document.get_str("_id")?.to_owned();
            let display_name = document.get_str("display_name")?.to_owned();
            provider_names.push((id, display_name));
        }
        if provider_names.is_empty() {
            return Ok(Vec::new());
        }
        let provider_name: std::collections::HashMap<String, String> =
            provider_names.into_iter().collect();
        let provider_ids: Vec<&str> = provider_name.keys().map(String::as_str).collect();
        let mut model_cursor = self
            .pool
            .collection::<Document>("models")
            .find(doc! {"provider_id": {"$in": provider_ids}, "enabled": true, ...model_filter})
            .session(&mut *session)
            .await?;
        let mut documents = Vec::new();
        while let Some(document) = model_cursor.try_next().await? {
            documents.push(document);
        }
        documents.sort_by(|a, b| {
            let an = provider_name
                .get(a.get_str("provider_id").unwrap_or_default())
                .map(String::as_str)
                .unwrap_or_default();
            let bn = provider_name
                .get(b.get_str("provider_id").unwrap_or_default())
                .map(String::as_str)
                .unwrap_or_default();
            an.cmp(bn)
                .then_with(|| {
                    a.get_str("display_name")
                        .unwrap_or_default()
                        .cmp(b.get_str("display_name").unwrap_or_default())
                })
                .then_with(|| {
                    a.get_str("_id")
                        .unwrap_or_default()
                        .cmp(b.get_str("_id").unwrap_or_default())
                })
        });
        let mut resolved = Vec::with_capacity(documents.len());
        for document in documents {
            resolved.push(resolved_model(ModelRow::from_doc(&document)?)?);
        }
        Ok(resolved)
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
            let model = self
                .pool
                .collection::<Document>("models")
                .find_one(doc! {"_id": id, "enabled": true})
                .await?;
            let Some(model) = model else {
                return Err(ModelsError::Validation(
                    "every failover model must be enabled and owned by the current owner".into(),
                ));
            };
            let provider_id = model.get_str("provider_id")?;
            let provider = self
                .pool
                .collection::<Document>("model_providers")
                .find_one(doc! {
                    "_id": provider_id,
                    "owner_id": owner_id,
                    "client": "supervisor",
                    "enabled": true,
                })
                .await?;
            if provider.is_none() {
                return Err(ModelsError::Validation(
                    "every failover model must be enabled and owned by the current owner".into(),
                ));
            }
        }
        let now = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        self.pool
            .collection::<Document>("model_failover")
            .delete_many(doc! {"primary_model_id": primary_model_id})
            .session(work.connection())
            .await?;
        for (ordinal, candidate) in candidates.iter().enumerate() {
            self.pool
                .collection::<Document>("model_failover")
                .insert_one(doc! {
                    "primary_model_id": primary_model_id,
                    "candidate_model_id": candidate,
                    "ordinal": i64::try_from(ordinal).unwrap_or(i64::MAX),
                    "created_at": &now,
                })
                .session(work.connection())
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
        let model = self
            .pool
            .collection::<Document>("models")
            .find_one(doc! {"_id": primary_model_id})
            .await?;
        let Some(model) = model else {
            return Err(ModelsError::ProviderNotFound);
        };
        let provider_id = model.get_str("provider_id")?;
        let provider = self
            .pool
            .collection::<Document>("model_providers")
            .find_one(doc! {
                "_id": provider_id,
                "owner_id": owner_id,
                "client": "supervisor",
            })
            .await?;
        if provider.is_none() {
            return Err(ModelsError::ProviderNotFound);
        }
        let mut cursor = self
            .pool
            .collection::<Document>("model_failover")
            .find(doc! {"primary_model_id": primary_model_id})
            .sort(doc! {"ordinal": 1})
            .await?;
        let mut candidates = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            candidates.push(document.get_str("candidate_model_id")?.to_owned());
        }
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
        let base_url = normalize_url(input.kind, &input.base_url)?;
        let mut work = self.unit_of_work.begin().await?;
        let mut document = doc! {
            "_id": &id,
            "owner_id": owner_id,
            "client": client_str(input.client),
            "kind": kind_str(input.kind),
            "display_name": input.display_name.trim(),
            "base_url": &base_url,
            "api_key_fingerprint": &key_fingerprint,
            "api_key_preview": &key_preview,
            "models_json": &models_json,
            "enabled": input.enabled,
            "created_at": &now,
            "updated_at": &now,
        };
        if let Some(bytes) = &ciphertext {
            document.insert(
                "api_key_ciphertext",
                Bson::Binary(mongodb::bson::Binary {
                    subtype: mongodb::bson::BinarySubtype::Generic,
                    bytes: bytes.clone(),
                }),
            );
        }
        self.pool
            .collection::<Document>("model_providers")
            .insert_one(document)
            .session(work.connection())
            .await?;
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
        let base_url = normalize_url(input.kind, &input.base_url)?;
        let mut work = self.unit_of_work.begin().await?;
        let mut set = doc! {
            "client": client_str(input.client),
            "kind": kind_str(input.kind),
            "display_name": input.display_name.trim(),
            "base_url": &base_url,
            "api_key_fingerprint": &key_fingerprint,
            "api_key_preview": &key_preview,
            "models_json": &models_json,
            "enabled": input.enabled,
            "updated_at": &now,
        };
        match &ciphertext {
            Some(bytes) => {
                set.insert(
                    "api_key_ciphertext",
                    Bson::Binary(mongodb::bson::Binary {
                        subtype: mongodb::bson::BinarySubtype::Generic,
                        bytes: bytes.clone(),
                    }),
                );
            }
            None => {
                set.insert("api_key_ciphertext", Bson::Null);
            }
        }
        let changed = self
            .pool
            .collection::<Document>("model_providers")
            .update_one(
                doc! {"_id": id, "owner_id": owner_id},
                doc! {"$set": set},
            )
            .session(work.connection())
            .await?
            .matched_count;
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
        let deleted = self
            .pool
            .collection::<Document>("model_providers")
            .delete_one(doc! {"_id": id, "owner_id": owner_id})
            .session(work.connection())
            .await?
            .deleted_count;
        if deleted == 0 {
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
        let document = self
            .pool
            .collection::<Document>("model_providers")
            .find_one(doc! {"_id": id, "owner_id": owner_id})
            .await?
            .ok_or(ModelsError::ProviderNotFound)?;
        ProviderRow::from_doc(&document)
    }

    async fn ensure_provider_name_available(
        &self,
        owner_id: &str,
        client: ModelClient,
        display_name: &str,
        excluded_id: Option<&str>,
    ) -> Result<(), ModelsError> {
        let name = display_name.trim();
        let mut filter = doc! {
            "owner_id": owner_id,
            "client": client_str(client),
            "display_name": name,
        };
        if let Some(excluded_id) = excluded_id {
            filter.insert("_id", doc! {"$ne": excluded_id});
        }
        let exists = self
            .pool
            .collection::<Document>("model_providers")
            .find_one(filter)
            .await?
            .is_some();
        if exists {
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
        record: AttemptRunningRecord<'_>,
    ) -> Result<(), ModelsError> {
        let AttemptRunningRecord {
            attempt_id,
            round_id,
            provider_id,
            upstream_model_id,
            candidate_order,
            attempt_type,
            created_at,
        } = record;
        // The retry index has to survive a reload: the UI reads it back from
        // this ledger to keep its reconnect counter after the announcing SSE
        // event is gone. Deriving it from the attempts already recorded for
        // this Round and candidate keeps the column authoritative without the
        // caller having to thread the loop counter through.
        let count = self
            .pool
            .collection::<Document>("model_attempts")
            .count_documents(doc! {"round_id": round_id, "candidate_order": candidate_order})
            .await?;
        let attempt_number = count_to_i64(count, "attempt")?;
        self.pool
            .collection::<Document>("model_attempts")
            .insert_one(doc! {
                "_id": attempt_id,
                "round_id": round_id,
                "candidate_order": candidate_order,
                "attempt_number": attempt_number,
                "provider_id": provider_id,
                "upstream_model_id": upstream_model_id,
                "attempt_type": attempt_type.as_str(),
                "status": "running",
                "created_at": created_at,
            })
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
        let mut set = doc! {
            "status": status,
            "ended_at": &ended,
        };
        match input_tokens {
            Some(value) => {
                set.insert("input_tokens", value);
            }
            None => {
                set.insert("input_tokens", Bson::Null);
            }
        }
        match output_tokens {
            Some(value) => {
                set.insert("output_tokens", value);
            }
            None => {
                set.insert("output_tokens", Bson::Null);
            }
        }
        match error_json {
            Some(value) => {
                set.insert("normalized_error_json", value.to_string());
            }
            None => {
                set.insert("normalized_error_json", Bson::Null);
            }
        }
        let changed = self
            .pool
            .collection::<Document>("model_attempts")
            .update_one(
                doc! {"_id": attempt_id, "status": "running"},
                doc! {"$set": set},
            )
            .await?
            .matched_count;

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
            self.pool
                .collection::<Document>("model_usage_ledger")
                .insert_one(doc! {
                    "_id": &ledger_id,
                    "attempt_id": attempt_id,
                    "project_id": project_id,
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "round_id": round_id,
                    "provider_id": &req.provider_id,
                    "upstream_model_id": &req.upstream_model_id,
                    "input_tokens": inp,
                    "output_tokens": out,
                    "cache_tokens": cache_tokens.unwrap_or(0),
                    "attempt_result": status,
                    "occurred_at": &ended,
                })
                .await?;
        }
        Ok(())
    }

    pub async fn interrupt_running_attempts_in_tx(
        &self,
        session: &mut ClientSession,
        now: &str,
    ) -> Result<(), ModelsError> {
        let error_json = serde_json::json!({"code": "CONTROL_PLANE_RESTART"}).to_string();
        self.pool
            .collection::<Document>("model_attempts")
            .update_many(
                doc! {"status": "running"},
                doc! {
                    "$set": {
                        "status": "interrupted",
                        "normalized_error_json": &error_json,
                        "ended_at": now,
                    }
                },
            )
            .session(&mut *session)
            .await?;
        Ok(())
    }

    pub async fn cancel_running_attempts_for_rounds_in_tx(
        &self,
        session: &mut ClientSession,
        round_ids: &[RoundId],
        now: &str,
    ) -> Result<u64, ModelsError> {
        let error_json = serde_json::json!({"code": "USER_CANCEL"}).to_string();
        let mut canceled = 0i64;
        for round_id in round_ids {
            let updated = self
                .pool
                .collection::<Document>("model_attempts")
                .update_many(
                    doc! {"round_id": round_id.to_string(), "status": "running"},
                    doc! {
                        "$set": {
                            "status": "canceled",
                            "normalized_error_json": &error_json,
                            "ended_at": now,
                        }
                    },
                )
                .session(&mut *session)
                .await?;
            canceled += updated.matched_count;
        }
        Ok(u64::try_from(canceled).map_err(|_| {
            ModelsError::Internal(anyhow::anyhow!("canceled attempt count overflow"))
        })?)
    }

    pub async fn delete_attempts_for_rounds_in_tx(
        &self,
        session: &mut ClientSession,
        round_ids: &[RoundId],
    ) -> Result<u64, ModelsError> {
        let mut deleted = 0i64;
        for round_id in round_ids {
            let result = self
                .pool
                .collection::<Document>("model_attempts")
                .delete_many(doc! {"round_id": round_id.to_string()})
                .session(&mut *session)
                .await?;
            deleted += result.deleted_count;
        }
        Ok(u64::try_from(deleted).map_err(|_| {
            ModelsError::Internal(anyhow::anyhow!("deleted attempt count overflow"))
        })?)
    }

    pub async fn latest_attempt_for_rounds(
        &self,
        round_ids: &[RoundId],
    ) -> Result<Option<ModelAttemptView>, ModelsError> {
        if round_ids.is_empty() {
            return Ok(None);
        }
        let round_id_strs: Vec<String> = round_ids.iter().map(|id| id.to_string()).collect();
        let document = self
            .pool
            .collection::<Document>("model_attempts")
            .find_one(doc! {"round_id": {"$in": &round_id_strs}})
            .sort(doc! {"created_at": -1, "_id": -1})
            .await?;
        let Some(document) = document else {
            return Ok(None);
        };
        let attempt = document
            .get("attempt_number")
            .and_then(Bson::as_i64)
            .unwrap_or_default();
        let status = document.get_str("status")?.to_owned();
        let detail = document
            .get("normalized_error_json")
            .and_then(Bson::as_str)
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
        session: &mut ClientSession,
        provider_id: &str,
        inputs: &[EmbeddedModelInput],
        now: &str,
    ) -> Result<(), ModelsError> {
        let mut existing_cursor = self
            .pool
            .collection::<Document>("models")
            .find(doc! {"provider_id": provider_id})
            .session(&mut *session)
            .await?;
        let mut existing = Vec::new();
        while let Some(document) = existing_cursor.try_next().await? {
            let id = document.get_str("_id")?.to_owned();
            let display_name = document.get_str("display_name")?.to_owned();
            let upstream_model_id = document.get_str("upstream_model_id")?.to_owned();
            existing.push((id, display_name, upstream_model_id));
        }
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
            let context_limit = if input.supports_1m { 1_000_000i64 } else { 200_000i64 };
            self.pool
                .collection::<Document>("models")
                .update_one(
                    doc! {"_id": &id},
                    doc! {
                        "$set": {
                            "display_name": display_name,
                            "upstream_model_id": upstream_model_id,
                            "context_limit": context_limit,
                            "supports_images": input.supports_images,
                            "enabled": input.enabled,
                            "updated_at": now,
                        },
                        "$setOnInsert": {
                            "provider_id": provider_id,
                            "supports_tools": true,
                            "parameters_json": "{}",
                            "created_at": now,
                        }
                    },
                    UpdateOptions::builder().upsert(true).build(),
                )
                .session(&mut *session)
                .await?;
        }
        for (id, _, _) in existing {
            if !retained.contains(&id) {
                self.pool
                    .collection::<Document>("models")
                    .delete_one(doc! {"_id": &id})
                    .session(&mut *session)
                    .await?;
            }
        }
        Ok(())
    }

    async fn append_config_changed_in_tx(
        &self,
        work: &mut UnitOfWorkTransaction,
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

fn count_to_i64(count: u64, what: &str) -> Result<i64, ModelsError> {
    i64::try_from(count)
        .map_err(|_| ModelsError::Internal(anyhow::anyhow!("{what} count overflow")))
}

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
        enabled: row.enabled,
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
        supports_images: row.supports_images,
        supports_tools: row.supports_tools,
        parameters: serde_json::from_str(&row.parameters_json)?,
        enabled: row.enabled,
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
        supports_images: row.supports_images,
        supports_tools: row.supports_tools,
        parameters: serde_json::from_str(&row.parameters_json)?,
    })
}
