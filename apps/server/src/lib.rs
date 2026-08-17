//! Server composition root: wire infrastructure, capabilities, adapters, and
//! public transports without owning capability business state.

pub mod adapters;
pub mod application;
pub mod config;
pub mod system;
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
    operations::OperationInterface, secrets::SecretCipher, state_broadcaster::StateBroadcaster,
    unit_of_work::UnitOfWork,
};
use janus_models::interface::ModelsInterface;
use janus_notifications::interface::NotificationsInterface;
use janus_projects::interface::ProjectsInterface;
use janus_runtime::interface::RuntimeInterface;
use janus_sessions::interface::SessionsInterface;
use janus_source_control::SourceControlInterface;
use janus_workspace::interface::WorkspaceInterface;
use system::SystemRead;
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
    /// Raw connection pool for test assertions and admin tooling. Transport
    /// handlers must not reach SQL directly; they use the capability
    /// interfaces or `system()`.
    pool: sqlx::SqlitePool,
    identity: IdentityInterface,
    application: Application,
    system: SystemRead,
    state_broadcaster: StateBroadcaster,
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
        let state_broadcaster = StateBroadcaster::new();
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
        let notifications =
            NotificationsInterface::new(pool.clone(), secrets.clone(), events.clone())?;
        let projects = ProjectsInterface::new(
            pool.clone(),
            secrets.clone(),
            operations.clone(),
            workspace.clone(),
            events.clone(),
            &config.data_root,
        );
        let source_control = SourceControlInterface::new(
            pool.clone(),
            secrets.clone(),
            operations.clone(),
            workspace.clone(),
            events.clone(),
            &config.data_root,
            adapters::git::system_runner(),
        );
        let runtime_logs = janus_runtime::interface::LogStore::new(pool.clone(), &config.data_root);
        let sessions = SessionsInterface::new(pool.clone(), events.clone(), blobs.clone());
        let local_executor = Arc::new(adapters::runtime::local::LocalExecutor::new(runtime_logs));
        let runtime = RuntimeInterface::new(
            pool.clone(),
            events.clone(),
            &config.data_root,
            local_executor,
        );
        workspace
            .recover_orphan_main_worktrees()
            .await
            .context("recover orphan main worktrees")?;
        let execution = ExecutionInterface::new(ExecutionDependencies {
            pool: pool.clone(),
            events: events.clone(),
            state_broadcaster: state_broadcaster.clone(),
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
            workspace.clone(),
            execution.clone(),
            unit_of_work.clone(),
            operations.clone(),
        );
        let application = Application::new(ApplicationDependencies {
            unit_of_work: unit_of_work.clone(),
            operations: operations.clone(),
            workspace: workspace.clone(),
            models: models.clone(),
            projects: projects.clone(),
            source_control: source_control.clone(),
            runtime: runtime.clone(),
            sessions: sessions.clone(),
            execution: execution.clone(),
            execution_coordinator,
            state_broadcaster: state_broadcaster.clone(),
            events: events.clone(),
            notifications: notifications.clone(),
        });
        application::lifecycle::recover_workspace_mutations(&application)
            .await
            .context("recover interrupted workspace mutations")?;
        application::lifecycle::recover_execution_state(&application).await?;
        Ok(Self {
            inner: Arc::new(AppStateInner {
                config,
                pool,
                identity,
                system: SystemRead::new(
                    database,
                    events,
                    blobs,
                    state_broadcaster.clone(),
                    application.clone(),
                ),
                application,
                state_broadcaster,
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

    /// Raw connection pool, reserved for test assertions. Transport handlers
    /// must read through capability interfaces or `system()` instead.
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.inner.pool
    }

    /// Narrow read-only facade over the database/event-store/application
    /// plumbing, so health and SSE transports never hold the raw services.
    pub fn system(&self) -> &SystemRead {
        &self.inner.system
    }

    pub fn operations(&self) -> &OperationInterface {
        self.inner.application.operations()
    }

    pub fn workspace(&self) -> &WorkspaceInterface {
        self.inner.application.workspace()
    }

    pub fn state_broadcaster(&self) -> &StateBroadcaster {
        &self.inner.state_broadcaster
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

    pub fn source_control(&self) -> &SourceControlInterface {
        self.inner.application.source_control()
    }

    pub fn runtime(&self) -> &RuntimeInterface {
        self.inner.application.runtime()
    }

    pub fn sessions(&self) -> &SessionsInterface {
        self.inner.application.sessions()
    }

    /// Cross-capability read of a session's current context usage (sessions +
    /// execution), kept behind AppState so the handler never reaches the whole
    /// `Application`.
    pub async fn session_context_usage(
        &self,
        session_id: janus_infrastructure::id::SessionId,
    ) -> Result<
        Option<janus_execution::interface::ContextUsageView>,
        janus_sessions::interface::SessionsError,
    > {
        self.inner
            .application
            .session_context_usage(session_id)
            .await
    }

    pub fn application(&self) -> &Application {
        &self.inner.application
    }

    pub fn notifications(&self) -> &NotificationsInterface {
        self.inner.application.notifications()
    }
}

pub fn router(state: AppState) -> Router {
    transport::http::router(state)
}
