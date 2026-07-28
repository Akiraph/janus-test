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
use modules::runtime::interface::RuntimeInterface;
use modules::sessions::interface::SessionsInterface;
use modules::supervisor::interface::SupervisorInterface;
use modules::workspace_sync::interface::WorkspaceSyncInterface;
use platform::{
    database::Database, events::EventStore, managed_storage::BlobStore,
    operations::OperationInterface, secret::SecretCipher,
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
    pub runtime: RuntimeInterface,
    pub sessions: SessionsInterface,
    pub supervisor: SupervisorInterface,
    /// Set once startup recovery (runtime + supervisor + blob/ops) has finished.
    /// `/health/ready` stays 503 until this is true so clients never land on a
    /// half-recovered control plane.
    pub recovery_complete: std::sync::atomic::AtomicBool,
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
        let workspace_sync =
            WorkspaceSyncInterface::new(pool.clone(), &config.data_root, blobs.clone());
        let identity = IdentityInterface::new(pool.clone(), &config).await?;
        let models = ModelsInterface::new(pool.clone(), secrets.clone())?;
        let projects = ProjectsInterface::new(
            pool.clone(),
            secrets.clone(),
            operations.clone(),
            workspace_sync.clone(),
            &config.data_root,
        );
        let runtime_logs =
            modules::runtime::log_store::LogStore::new(pool.clone(), &config.data_root);
        let local_executor = Arc::new(adapters::runtime::local::LocalExecutor::new(runtime_logs));
        let runtime = RuntimeInterface::new(
            pool.clone(),
            events.clone(),
            &config.data_root,
            local_executor,
        );
        runtime.recover_uncertain().await?;
        let sessions = SessionsInterface::new(pool.clone(), events.clone(), workspace_sync.clone());
        // Owner id used when spawning background turns; HTTP message handlers
        // rebuild a request-scoped supervisor with the authenticated owner.
        let supervisor = SupervisorInterface::new(
            pool.clone(),
            events.clone(),
            models.clone(),
            workspace_sync.clone(),
            "owner-bootstrap".into(),
        )
        .with_runtime(runtime.clone());
        if let Err(error) = supervisor.recover_running_on_startup().await {
            tracing::warn!(%error, "supervisor startup recovery failed");
        }
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
                runtime,
                sessions,
                supervisor,
                // main() flips this after the remaining recovery steps
                // (incoming blobs + stale operations) complete, so unit tests
                // that only call initialize() still see ready=true by default.
                recovery_complete: std::sync::atomic::AtomicBool::new(true),
            }),
        })
    }

    /// Mark startup recovery finished. Called from `main` after blob/ops
    /// cleanup so `/health/ready` can flip to 200.
    pub fn mark_recovery_complete(&self) {
        self.inner
            .recovery_complete
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// True once startup recovery has finished. Used by `/health/ready`.
    pub fn recovery_complete(&self) -> bool {
        self.inner
            .recovery_complete
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Begin process startup in the "not yet recovered" posture so a freshly
    /// constructed AppState used by `main` keeps `/health/ready` at 503 until
    /// `mark_recovery_complete` runs. Tests leave the default `true`.
    pub fn begin_startup_recovery(&self) {
        self.inner
            .recovery_complete
            .store(false, std::sync::atomic::Ordering::SeqCst);
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

    pub fn runtime(&self) -> &RuntimeInterface {
        &self.inner.runtime
    }

    pub fn sessions(&self) -> &SessionsInterface {
        &self.inner.sessions
    }

    pub fn supervisor(&self) -> &SupervisorInterface {
        &self.inner.supervisor
    }

    /// Build a request-scoped supervisor bound to the authenticated owner id
    /// (provider credentials / model selection).
    pub fn supervisor_for_owner(&self, owner_id: &str) -> SupervisorInterface {
        SupervisorInterface::new(
            self.inner.database.pool().clone(),
            self.inner.events.clone(),
            self.inner.models.clone(),
            self.inner.workspace_sync.clone(),
            owner_id.to_owned(),
        )
        .with_runtime(self.inner.runtime.clone())
    }
}

pub fn router(state: AppState) -> Router {
    transport::http::router(state)
}
