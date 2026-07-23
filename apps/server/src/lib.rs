pub mod adapters;
pub mod application;
pub mod config;
pub mod modules;
pub mod platform;
pub mod transport;

use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use config::Config;
use modules::identity::interface::IdentityInterface;
use modules::models::interface::ModelsInterface;
use platform::{database::Database, events::EventStore, secret::SecretCipher};

#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

struct AppStateInner {
    pub config: Config,
    pub database: Database,
    pub events: EventStore,
    pub secrets: SecretCipher,
    pub identity: IdentityInterface,
    pub models: ModelsInterface,
}

impl AppState {
    pub async fn initialize(config: Config) -> anyhow::Result<Self> {
        let database = Database::open(&config.data_root)
            .await
            .with_context(|| format!("initialize data root {}", config.data_root.display()))?;
        let events = EventStore::new(database.pool().clone());
        let secrets = SecretCipher::load(&config.data_root, config.mode)?;
        let identity = IdentityInterface::new(database.pool().clone(), &config).await?;
        let models = ModelsInterface::new(database.pool().clone(), secrets.clone())?;
        Ok(Self {
            inner: Arc::new(AppStateInner {
                config,
                database,
                events,
                secrets,
                identity,
                models,
            }),
        })
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn database(&self) -> &Database {
        &self.inner.database
    }

    pub fn events(&self) -> &EventStore {
        &self.inner.events
    }

    pub fn secrets(&self) -> &SecretCipher {
        &self.inner.secrets
    }

    pub fn identity(&self) -> &IdentityInterface {
        &self.inner.identity
    }

    pub fn models(&self) -> &ModelsInterface {
        &self.inner.models
    }
}

pub fn router(state: AppState) -> Router {
    transport::http::router(state)
}
