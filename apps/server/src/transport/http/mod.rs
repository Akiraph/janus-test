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
mod sessions;
mod sse;
mod terminal;

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
        projects::get_credential, projects::update_credential, projects::delete_credential, projects::probe_credential,
        projects::file_meta, projects::file_content, projects::save_text,
        projects::file_tree, projects::move_file, projects::delete_file,
        git::git_status, git::git_diff, git::git_log, git::git_branches, git::git_remotes,
        git::git_fetch, git::git_stage, git::git_unstage, git::git_commit, git::git_push,
        git::git_update, git::list_update_conflicts, git::get_update_conflict, git::resolve_update_conflict,
        operations::get_operation,
        sessions::list_sessions, sessions::create_session, sessions::get_session,
        sessions::delete_session, sessions::post_message, sessions::timeline,
        sessions::get_turn, sessions::session_diff, sessions::steer, sessions::cancel_turn,
        sessions::answer_ask, sessions::retry_model,
        terminal::create_terminal, terminal::list_terminals, terminal::issue_terminal_ticket,
        terminal::resize_terminal, terminal::signal_terminal, terminal::close_terminal,
        terminal::terminal_scrollback, terminal::connect_terminal
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
        dto::RuntimeCapabilityId,
        dto::CapabilityScope,
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
        crate::modules::projects::interface::UpdateGithubCredentialInput,
        crate::modules::projects::interface::GithubCredentialView,
        crate::modules::projects::interface::CredentialProbeResult,
        crate::modules::projects::interface::SaveTextInput,
        crate::modules::projects::interface::FileMetaView,
        crate::modules::projects::interface::FileTreeView,
        crate::modules::projects::interface::MoveFileInput,
        crate::modules::projects::interface::DeleteFileInput,
        crate::modules::projects::interface::RetryProjectInput,
        crate::modules::projects::interface::GitUpdateConflictView,
        crate::modules::projects::interface::GitUpdateConflictPathView,
        crate::modules::projects::interface::ResolveGitUpdateConflictInput,
        crate::modules::projects::interface::ResolveGitUpdateConflictPath,
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
        git::GitUpdateRequest,
        git::ResolveConflictRequest,
        git::ResolveConflictPathRequest,
        crate::modules::sessions::interface::SessionSummary,
        crate::modules::sessions::interface::TurnSummary,
        crate::modules::sessions::interface::MessageRouteResult,
        crate::modules::sessions::interface::TimelinePage,
        crate::modules::sessions::interface::TimelineItemView,
        crate::modules::sessions::interface::CancelResult,
        crate::modules::sessions::interface::SteerResult,
        sessions::CreateSessionRequest,
        sessions::PostMessageRequest,
        sessions::SteerRequest,
        sessions::CancelTurnRequest,
        sessions::AnswerAskRequest,
        crate::modules::runtime::interface::TerminalProjection,
        crate::modules::runtime::interface::TerminalStatus,
        crate::modules::runtime::interface::TerminalSize,
        crate::modules::runtime::interface::TerminalSignal,
        crate::modules::runtime::interface::TerminalTicket,
        crate::modules::runtime::interface::LogRange,
        terminal::CreateTerminalRequest,
        terminal::TerminalSizeInput,
        terminal::EnvironmentInput,
        terminal::ResizeTerminalRequest,
        terminal::SignalTerminalRequest
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
        .route("/api/v1/projects/{id}/retry", post(projects::retry_project))
        .route("/api/v1/projects/{id}/files/meta", get(projects::file_meta))
        .route(
            "/api/v1/projects/{id}/files/content",
            get(projects::file_content),
        )
        .route("/api/v1/projects/{id}/files/text", put(projects::save_text))
        .route("/api/v1/projects/{id}/files/tree", get(projects::file_tree))
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
            get(projects::get_credential)
                .patch(projects::update_credential)
                .delete(projects::delete_credential),
        )
        .route(
            "/api/v1/github-credentials/{id}/probe",
            post(projects::probe_credential),
        )
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
        .route(
            "/api/v1/projects/{id}/git/update-conflicts",
            get(git::list_update_conflicts),
        )
        .route(
            "/api/v1/projects/{id}/git/update-conflicts/{conflict_id}",
            get(git::get_update_conflict),
        )
        .route(
            "/api/v1/projects/{id}/git/update-conflicts/{conflict_id}/resolve",
            post(git::resolve_update_conflict),
        )
        .route(
            "/api/v1/projects/{project_id}/sessions",
            get(sessions::list_sessions).post(sessions::create_session),
        )
        .route(
            "/api/v1/sessions/{id}",
            get(sessions::get_session).delete(sessions::delete_session),
        )
        .route(
            "/api/v1/sessions/{id}/messages",
            post(sessions::post_message),
        )
        .route("/api/v1/sessions/{id}/steer", post(sessions::steer))
        .route("/api/v1/sessions/{id}/timeline", get(sessions::timeline))
        .route(
            "/api/v1/sessions/{id}/turns/{turn_id}",
            get(sessions::get_turn),
        )
        .route(
            "/api/v1/sessions/{id}/turns/{turn_id}/cancel",
            post(sessions::cancel_turn),
        )
        .route(
            "/api/v1/sessions/{id}/turns/{turn_id}/retry-model",
            post(sessions::retry_model),
        )
        .route("/api/v1/asks/{ask_id}/answer", post(sessions::answer_ask))
        .route("/api/v1/sessions/{id}/diff", get(sessions::session_diff))
        .route(
            "/api/v1/terminals",
            get(terminal::list_terminals).post(terminal::create_terminal),
        )
        .route(
            "/api/v1/terminals/{id}/scrollback",
            get(terminal::terminal_scrollback),
        )
        .route(
            "/api/v1/terminals/{id}/tickets",
            post(terminal::issue_terminal_ticket),
        )
        .route(
            "/api/v1/terminals/{id}/resize",
            post(terminal::resize_terminal),
        )
        .route(
            "/api/v1/terminals/{id}/signal",
            post(terminal::signal_terminal),
        )
        .route(
            "/api/v1/terminals/{id}/close",
            post(terminal::close_terminal),
        )
        .route(
            "/api/v1/terminals/{id}/connect",
            get(terminal::connect_terminal),
        )
        .layer(middleware::from_fn(request_id::middleware))
        .with_state(state)
}

pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}
