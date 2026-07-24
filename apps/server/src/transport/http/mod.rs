mod auth;
mod conditions;
pub mod dto;
mod git;
mod handlers;
mod models;
mod operations;
mod problem;
mod projects;
mod request_id;
mod sse;

use axum::{
    Router, middleware,
    routing::{get, patch, post, put},
};
use utoipa::OpenApi;

use crate::AppState;

pub use problem::Problem;

#[derive(OpenApi)]
#[openapi(
    paths(
        handlers::live,
        handlers::ready,
        handlers::bootstrap,
        handlers::system_info,
        sse::events
        , auth::initialize_options, auth::initialize_complete, auth::login_options,
        auth::login_complete, auth::me, auth::logout, auth::passkeys, auth::passkey_options,
        auth::passkey_complete, auth::rename_passkey, auth::revoke_passkey,
        auth::regenerate_recovery_codes, auth::recovery_exchange,
        auth::recovery_passkey_options, auth::recovery_passkey_complete,
        models::providers, models::create_provider, models::update_provider,
        models::delete_provider, models::probe_provider,
        projects::list_projects, projects::create_project, projects::get_project,
        projects::update_project, projects::delete_project, projects::retry_project,
        projects::list_credentials, projects::create_credential,
        projects::get_credential, projects::delete_credential, projects::probe_credential,
        projects::file_meta, projects::file_content, projects::save_text,
        projects::file_tree, projects::move_file, projects::delete_file,
        git::git_status, git::git_diff, git::git_log, git::git_branches, git::git_remotes,
        git::git_fetch, git::git_stage, git::git_unstage, git::git_commit, git::git_push,
        git::git_update,
        operations::get_operation
    ),
    components(schemas(
        dto::LiveResponse,
        dto::ReadyResponse,
        dto::BootstrapResponse,
        dto::BootstrapData,
        dto::BootstrapState,
        dto::PublicLimits,
        dto::SystemInfoResponse,
        dto::SystemInfo,
        dto::DatabaseInfo,
        dto::EventInfo,
        dto::RuntimeCapability,
        dto::CapabilityState,
        dto::CapabilityReason,
        dto::InitializeOptionsRequest,
        dto::CeremonyCompleteRequest,
        dto::PasskeyOptionsRequest,
        dto::RenamePasskeyRequest,
        dto::RecoveryExchangeRequest,
        crate::modules::identity::interface::CeremonyOptions,
        crate::modules::identity::interface::OwnerView,
        crate::modules::identity::interface::AuthenticationMode,
        crate::modules::identity::interface::PasskeyView,
        crate::modules::models::interface::ProviderInput,
        crate::modules::models::interface::ProviderView,
        crate::modules::models::interface::ProviderKind,
        crate::modules::models::interface::ProbeResult,
        crate::modules::models::interface::ProbeStatus,
        crate::modules::models::interface::EmbeddedModelInput,
        crate::modules::models::interface::EmbeddedModelView,
        problem::Problem,
        crate::platform::events::EventEnvelope,
        crate::modules::projects::interface::CreateProjectInput,
        crate::modules::projects::interface::RepositoryInput,
        crate::modules::projects::interface::ProjectView,
        crate::modules::projects::interface::RepositoryView,
        crate::modules::projects::interface::RepoAccess,
        crate::modules::projects::interface::CreateGithubCredentialInput,
        crate::modules::projects::interface::GithubCredentialView,
        crate::modules::projects::interface::CredentialProbeResult,
        crate::modules::projects::interface::SaveTextInput,
        crate::modules::projects::interface::FileMetaView,
        crate::modules::projects::interface::FileTreeView,
        crate::modules::projects::interface::MoveFileInput,
        crate::modules::projects::interface::DeleteFileInput,
        crate::modules::projects::interface::RetryProjectInput,
        crate::platform::operations::OperationView,
        crate::platform::operations::OperationStatus,
        crate::modules::workspace_sync::interface::RevisionRef,
        projects::UpdateProjectRequest,
        git::GitStatusView,
        git::GitLogEntryView,
        git::GitLogResponse,
        git::DiffViewParam,
        git::GitFetchRequest,
        git::GitStageRequest,
        git::GitCommitRequest,
        git::GitPushRequest,
        git::GitUpdateRequest
    )),
    tags((name = "system", description = "Janus system probes"))
)]
pub struct ApiDoc;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get(handlers::live))
        .route("/health/ready", get(handlers::ready))
        .route("/api/v1/bootstrap", get(handlers::bootstrap))
        .route("/api/v1/system/info", get(handlers::system_info))
        .route("/api/v1/events", get(sse::events))
        .route(
            "/api/v1/auth/initialize/options",
            post(auth::initialize_options),
        )
        .route(
            "/api/v1/auth/initialize/complete",
            post(auth::initialize_complete),
        )
        .route("/api/v1/auth/passkey/options", post(auth::login_options))
        .route("/api/v1/auth/passkey/complete", post(auth::login_complete))
        .route("/api/v1/auth/logout", post(auth::logout))
        .route("/api/v1/me", get(auth::me))
        .route("/api/v1/me/passkeys", get(auth::passkeys))
        .route("/api/v1/me/passkeys/options", post(auth::passkey_options))
        .route("/api/v1/me/passkeys/complete", post(auth::passkey_complete))
        .route(
            "/api/v1/me/passkeys/{id}",
            patch(auth::rename_passkey).delete(auth::revoke_passkey),
        )
        .route(
            "/api/v1/me/recovery-codes/regenerate",
            post(auth::regenerate_recovery_codes),
        )
        .route(
            "/api/v1/auth/recovery/exchange",
            post(auth::recovery_exchange),
        )
        .route(
            "/api/v1/auth/recovery/passkey/options",
            post(auth::recovery_passkey_options),
        )
        .route(
            "/api/v1/auth/recovery/passkey/complete",
            post(auth::recovery_passkey_complete),
        )
        .route(
            "/api/v1/model-providers",
            get(models::providers).post(models::create_provider),
        )
        .route(
            "/api/v1/model-providers/{id}",
            patch(models::update_provider).delete(models::delete_provider),
        )
        .route(
            "/api/v1/model-providers/{id}/probe",
            post(models::probe_provider),
        )
        .route(
            "/api/v1/projects",
            get(projects::list_projects).post(projects::create_project),
        )
        .route(
            "/api/v1/projects/{id}",
            get(projects::get_project)
                .patch(projects::update_project)
                .delete(projects::delete_project),
        )
        .route(
            "/api/v1/projects/{id}/retry",
            post(projects::retry_project),
        )
        .route("/api/v1/projects/{id}/files/meta", get(projects::file_meta))
        .route(
            "/api/v1/projects/{id}/files/content",
            get(projects::file_content),
        )
        .route("/api/v1/projects/{id}/files/text", put(projects::save_text))
        .route(
            "/api/v1/projects/{id}/files/tree",
            get(projects::file_tree),
        )
        .route(
            "/api/v1/projects/{id}/files/move",
            post(projects::move_file),
        )
        .route(
            "/api/v1/projects/{id}/files",
            axum::routing::delete(projects::delete_file),
        )
        .route(
            "/api/v1/github-credentials",
            get(projects::list_credentials).post(projects::create_credential),
        )
        .route(
            "/api/v1/github-credentials/{id}",
            get(projects::get_credential).delete(projects::delete_credential),
        )
        .route(
            "/api/v1/github-credentials/{id}/probe",
            post(projects::probe_credential),
        )
        // TODO(M2 follow-up): PATCH /api/v1/github-credentials/{id} for PAT
        // rotation. M2 exposes list+create only; PAT replacement requires an
        // update_credential path on the Module that re-encrypts the PAT.
        .route("/api/v1/operations/{id}", get(operations::get_operation))
        .route("/api/v1/projects/{id}/git/status", get(git::git_status))
        .route("/api/v1/projects/{id}/git/diff", get(git::git_diff))
        .route("/api/v1/projects/{id}/git/log", get(git::git_log))
        .route("/api/v1/projects/{id}/git/branches", get(git::git_branches))
        .route("/api/v1/projects/{id}/git/remotes", get(git::git_remotes))
        .route(
            "/api/v1/projects/{id}/git/commands/fetch",
            post(git::git_fetch),
        )
        .route(
            "/api/v1/projects/{id}/git/commands/stage",
            post(git::git_stage),
        )
        .route(
            "/api/v1/projects/{id}/git/commands/unstage",
            post(git::git_unstage),
        )
        .route(
            "/api/v1/projects/{id}/git/commands/commit",
            post(git::git_commit),
        )
        .route(
            "/api/v1/projects/{id}/git/commands/push",
            post(git::git_push),
        )
        .route(
            "/api/v1/projects/{id}/git/commands/update",
            post(git::git_update),
        )
        .layer(middleware::from_fn(request_id::middleware))
        .with_state(state)
}

pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}
