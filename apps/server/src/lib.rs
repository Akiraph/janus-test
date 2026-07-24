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
use modules::projects::interface::ProjectsInterface;
use modules::workspace_sync::interface::WorkspaceSyncInterface;
use platform::{
    database::Database,
    events::EventStore,
    managed_storage::BlobStore,
    operations::OperationInterface,
    secret::SecretCipher,
};

#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

struct AppStateInner {
    pub config: Config,
    pub database: Database,
    pub events: EventStore,
    pub secrets: SecretCipher,
    pub blobs: BlobStore,
    pub operations: OperationInterface,
    pub workspace_sync: WorkspaceSyncInterface,
    pub identity: IdentityInterface,
    pub models: ModelsInterface,
    pub projects: ProjectsInterface,
}

impl AppState {
    pub async fn initialize(config: Config) -> anyhow::Result<Self> {
        let database = Database::open(&config.data_root)
            .await
            .with_context(|| format!("initialize data root {}", config.data_root.display()))?;
        let pool = database.pool().clone();
        let events = EventStore::new(pool.clone());
        let secrets = SecretCipher::load(&config.data_root, config.mode)?;
        let blobs = BlobStore::new(pool.clone(), &config.data_root)?;
        let operations = OperationInterface::new(pool.clone());
        let workspace_sync = WorkspaceSyncInterface::new(pool.clone());
        let identity = IdentityInterface::new(pool.clone(), &config).await?;
        let models = ModelsInterface::new(pool.clone(), secrets.clone())?;
        let projects = ProjectsInterface::new(
            pool.clone(),
            secrets.clone(),
            operations.clone(),
            workspace_sync.clone(),
            &config.data_root,
        );
        Ok(Self {
            inner: Arc::new(AppStateInner {
                config,
                database,
                events,
                secrets,
                blobs,
                operations,
                workspace_sync,
                identity,
                models,
                projects,
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

    pub fn blobs(&self) -> &BlobStore {
        &self.inner.blobs
    }

    pub fn operations(&self) -> &OperationInterface {
        &self.inner.operations
    }

    pub fn workspace_sync(&self) -> &WorkspaceSyncInterface {
        &self.inner.workspace_sync
    }

    pub fn identity(&self) -> &IdentityInterface {
        &self.inner.identity
    }

    pub fn models(&self) -> &ModelsInterface {
        &self.inner.models
    }

    pub fn projects(&self) -> &ProjectsInterface {
        &self.inner.projects
    }
}

pub fn router(state: AppState) -> Router {
    transport::http::router(state)
}
