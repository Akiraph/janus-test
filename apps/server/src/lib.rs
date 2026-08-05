//! Server composition root: wire infrastructure, capabilities, adapters, and
//! public transports without owning capability business state.

pub mod adapters;
pub mod application;
pub mod config;
pub mod transport;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::Context;
use application::{Application, ApplicationDependencies, execution::ExecutionCoordinator};
use axum::Router;
use config::Config;
use janus_execution::interface::{ExecutionDependencies, ExecutionInterface};
use janus_identity::IdentityInterface;
use janus_infrastructure::{
    database::Database, events::EventStore, managed_storage::BlobStore,
    operations::OperationInterface, secrets::SecretCipher, unit_of_work::UnitOfWork,
};
use janus_models::interface::ModelsInterface;
use janus_projects::interface::ProjectsInterface;
use janus_runtime::interface::RuntimeInterface;
use janus_sessions::interface::SessionsInterface;
use janus_workspace::interface::WorkspaceInterface;
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub const fn migrator() -> &'static sqlx::migrate::Migrator {
    &MIGRATOR
}

#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

struct AppStateInner {
    config: Config,
    database: Database,
    events: EventStore,
    blobs: BlobStore,
    identity: IdentityInterface,
    application: Application,
    /// Set once startup recovery (runtime + execution + blob/ops) has finished.
    /// `/health/ready` stays 503 until this is true so clients never land on a
    /// half-recovered control plane.
    recovery_complete: AtomicBool,
}

impl AppState {
    pub async fn initialize(config: Config) -> anyhow::Result<Self> {
        let database = Database::open(&config.data_root, migrator())
            .await
            .with_context(|| format!("initialize data root {}", config.data_root.display()))?;
        let pool = database.pool().clone();
        let events = EventStore::new(pool.clone());
        let unit_of_work = UnitOfWork::new(pool.clone(), events.clone());
        let secrets = SecretCipher::load(
            &config.data_root,
            config.mode == config::RunMode::Production,
        )?;
        let blobs = BlobStore::new(pool.clone(), &config.data_root)?;
        let operations = OperationInterface::new(pool.clone(), events.clone());
        let workspace = WorkspaceInterface::new(pool.clone(), &config.data_root, blobs.clone());
        let identity = IdentityInterface::new(
            pool.clone(),
            &config.webauthn_rp_id,
            &config.public_origin,
            &config.webauthn_rp_name,
            config.development_auth,
        )?;
        let models = ModelsInterface::new(pool.clone(), secrets.clone(), events.clone())?;
        let projects = ProjectsInterface::new(
            pool.clone(),
            secrets.clone(),
            operations.clone(),
            workspace.clone(),
            events.clone(),
            &config.data_root,
            adapters::git::system_runner(),
        );
        let runtime_logs = janus_runtime::interface::LogStore::new(pool.clone(), &config.data_root);
        let local_executor = Arc::new(adapters::runtime::local::LocalExecutor::new(runtime_logs));
        let runtime = RuntimeInterface::new(
            pool.clone(),
            events.clone(),
            &config.data_root,
            local_executor,
        );
        let sessions = SessionsInterface::new(
            pool.clone(),
            events.clone(),
            workspace.clone(),
            blobs.clone(),
        );
        workspace
            .recover_orphan_session_worktrees()
            .await
            .context("recover orphan session worktrees")?;
        workspace
            .recover_orphan_main_worktrees()
            .await
            .context("recover orphan main worktrees")?;
        let execution = ExecutionInterface::new(ExecutionDependencies {
            pool: pool.clone(),
            events: events.clone(),
            models: models.clone(),
            projects: projects.clone(),
            workspace: workspace.clone(),
            sessions: sessions.clone(),
            blobs: blobs.clone(),
            runtime: runtime.clone(),
        });
        let execution_coordinator = ExecutionCoordinator::new(
            models.clone(),
            projects.clone(),
            sessions.clone(),
            execution.clone(),
            runtime.clone(),
            unit_of_work.clone(),
            operations.clone(),
        );
        let application = Application::new(ApplicationDependencies {
            unit_of_work: unit_of_work.clone(),
            operations: operations.clone(),
            workspace: workspace.clone(),
            models: models.clone(),
            projects: projects.clone(),
            runtime: runtime.clone(),
            sessions: sessions.clone(),
            execution: execution.clone(),
            execution_coordinator,
        });
        workspace
            .recover_uncertain_propagations()
            .await
            .context("recover interrupted workspace propagation")?;
        application::lifecycle::recover_workspace_mutations(&application)
            .await
            .context("recover interrupted workspace mutations")?;
        application::lifecycle::recover_execution_state(&application).await?;
        Ok(Self {
            inner: Arc::new(AppStateInner {
                config,
                database,
                events,
                blobs,
                identity,
                application,
                // main() flips this after the remaining recovery steps
                // (incoming blobs + stale operations) complete, so unit tests
                // that only call initialize() still see ready=true by default.
                recovery_complete: AtomicBool::new(true),
            }),
        })
    }

    /// Mark startup recovery finished. Called from `main` after blob/ops
    /// cleanup so `/health/ready` can flip to 200.
    pub fn mark_recovery_complete(&self) {
        self.inner.recovery_complete.store(true, Ordering::SeqCst);
    }

    /// True once startup recovery has finished. Used by `/health/ready`.
    pub fn recovery_complete(&self) -> bool {
        self.inner.recovery_complete.load(Ordering::SeqCst)
    }

    /// Begin process startup in the "not yet recovered" posture so a freshly
    /// constructed AppState used by `main` keeps `/health/ready` at 503 until
    /// `mark_recovery_complete` runs. Tests leave the default `true`.
    pub fn begin_startup_recovery(&self) {
        self.inner.recovery_complete.store(false, Ordering::SeqCst);
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

    pub fn blobs(&self) -> &BlobStore {
        &self.inner.blobs
    }

    pub fn operations(&self) -> &OperationInterface {
        self.inner.application.operations()
    }

    pub fn workspace(&self) -> &WorkspaceInterface {
        self.inner.application.workspace()
    }

    pub fn identity(&self) -> &IdentityInterface {
        &self.inner.identity
    }

    pub fn models(&self) -> &ModelsInterface {
        self.inner.application.models()
    }

    pub fn projects(&self) -> &ProjectsInterface {
        self.inner.application.projects()
    }

    pub fn runtime(&self) -> &RuntimeInterface {
        self.inner.application.runtime()
    }

    pub fn sessions(&self) -> &SessionsInterface {
        self.inner.application.sessions()
    }

    pub fn application(&self) -> &Application {
        &self.inner.application
    }
}

pub fn router(state: AppState) -> Router {
    transport::http::router(state)
}
