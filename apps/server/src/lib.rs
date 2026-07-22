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
use platform::{database::Database, events::EventStore};

#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

struct AppStateInner {
    pub config: Config,
    pub database: Database,
    pub events: EventStore,
}

impl AppState {
    pub async fn initialize(config: Config) -> anyhow::Result<Self> {
        let database = Database::open(&config.data_root)
            .await
            .with_context(|| format!("initialize data root {}", config.data_root.display()))?;
        let events = EventStore::new(database.pool().clone());
        Ok(Self {
            inner: Arc::new(AppStateInner {
                config,
                database,
                events,
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
}

pub fn router(state: AppState) -> Router {
    transport::http::router(state)
}
