mod auth;
mod conditions;
pub mod dto;
mod git;
mod handlers;
mod jobs;
mod models;
mod notifications;
mod operations;
mod problem;
mod projects;
mod request_id;
mod sessions;
mod sse;
mod terminal;

use axum::{
    Router, middleware,
    routing::{delete, get, patch, post, put},
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
        sessions::delete_session, sessions::post_message, sessions::upload_attachment,
        sessions::delete_attachment, sessions::session_context, sessions::timeline,
        sessions::queued_turns, sessions::get_turn, sessions::session_diff, sessions::sync, sessions::apply, sessions::steer, sessions::cancel_turn,
        sessions::answer_ask, sessions::retry_model,
        terminal::create_terminal, terminal::list_terminals, terminal::issue_terminal_ticket,
        terminal::resize_terminal, terminal::signal_terminal, terminal::close_terminal,
        terminal::terminal_scrollback, terminal::connect_terminal,
        jobs::list_jobs, jobs::job_log, jobs::cancel_job
        , notifications::list_channels, notifications::create_channel,
        notifications::update_channel, notifications::delete_channel, notifications::test_channel
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
        janus_identity::CeremonyOptions,
        janus_identity::OwnerView,
        janus_identity::AuthenticationMode,
        janus_identity::PasskeyView,
        janus_models::interface::ProviderInput,
        janus_models::interface::ProviderView,
        janus_models::interface::ProviderKind,
        janus_models::interface::ModelClient,
        janus_models::interface::ProbeResult,
        janus_models::interface::ProbeStatus,
        janus_models::interface::EmbeddedModelInput,
        janus_models::interface::EmbeddedModelView,
        problem::Problem,
        janus_infrastructure::events::EventEnvelope,
        janus_projects::interface::CreateProjectInput,
        janus_projects::interface::RepositoryInput,
        janus_projects::interface::ProjectView,
        janus_projects::interface::RepositoryView,
        janus_projects::interface::RepoAccess,
        janus_projects::interface::CreateGithubCredentialInput,
        janus_projects::interface::UpdateGithubCredentialInput,
        janus_projects::interface::GithubCredentialView,
        janus_projects::interface::CredentialProbeResult,
        janus_workspace::interface::SaveTextInput,
        janus_workspace::interface::FileMetaView,
        janus_workspace::interface::FileTreeView,
        janus_workspace::interface::MoveFileInput,
        janus_workspace::interface::DeleteFileInput,
        janus_projects::interface::RetryProjectInput,
        janus_source_control::interface::GitUpdateConflictView,
        janus_source_control::interface::GitUpdateConflictPathView,
        janus_source_control::interface::ResolveGitUpdateConflictInput,
        janus_source_control::interface::ResolveGitUpdateConflictPath,
        janus_infrastructure::operations::OperationView,
        janus_infrastructure::operations::OperationStatus,
        janus_workspace::interface::RevisionRef,
        janus_workspace::interface::DiffSummary,
        janus_workspace::interface::PropagationConflict,
        janus_workspace::interface::PropagationConflictPath,
        janus_workspace::interface::PropagationDirection,
        janus_workspace::interface::PropagationResult,
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
        janus_sessions::interface::SessionSummary,
        janus_sessions::interface::TurnSummary,
        janus_sessions::interface::MessageRouteResult,
        janus_sessions::interface::TimelinePage,
        janus_sessions::interface::TimelineItemView,
        janus_sessions::interface::CancelResult,
        janus_sessions::interface::SteerResult,
        janus_sessions::interface::SessionModelPreference,
        janus_sessions::interface::ReasoningEffort,
        janus_sessions::interface::AttachmentView,
        janus_sessions::interface::QueuedTurnItem,
        sessions::ContextUsageView,
        sessions::CreateSessionRequest,
        sessions::PostMessageRequest,
        sessions::SteerRequest,
        sessions::CancelTurnRequest,
        sessions::AnswerAskRequest,
        sessions::AnswerAskResult,
        janus_runtime::interface::TerminalProjection,
        janus_runtime::interface::TerminalStatus,
        janus_runtime::interface::TerminalSize,
        janus_runtime::interface::TerminalSignal,
        janus_runtime::interface::TerminalTicket,
        janus_runtime::interface::LogRange,
        janus_runtime::interface::JobProjection,
        janus_runtime::interface::DelegatedCliKind,
        janus_notifications::interface::NotificationChannelKind,
        janus_notifications::interface::NotificationEventKind,
        janus_notifications::interface::NotificationTarget,
        janus_notifications::interface::NotificationChannelInput,
        janus_notifications::interface::NotificationChannelView,
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
        .route("/api/v1/sessions/{id}/jobs", get(jobs::list_jobs))
        .route("/api/v1/jobs/{id}/log", get(jobs::job_log))
        .route("/api/v1/jobs/{id}/cancel", post(jobs::cancel_job))
        .route(
            "/api/v1/notification-channels",
            get(notifications::list_channels).post(notifications::create_channel),
        )
        .route(
            "/api/v1/notification-channels/{id}",
            patch(notifications::update_channel).delete(notifications::delete_channel),
        )
        .route(
            "/api/v1/notification-channels/{id}/test",
            post(notifications::test_channel),
        )
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
        .route(
            "/api/v1/sessions/{id}/attachments",
            post(sessions::upload_attachment),
        )
        .route(
            "/api/v1/sessions/{id}/attachments/{attachment_id}",
            delete(sessions::delete_attachment),
        )
        .route("/api/v1/sessions/{id}/steer", post(sessions::steer))
        .route(
            "/api/v1/sessions/{id}/context",
            get(sessions::session_context),
        )
        .route("/api/v1/sessions/{id}/timeline", get(sessions::timeline))
        .route(
            "/api/v1/sessions/{id}/queued-turns",
            get(sessions::queued_turns),
        )
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
        .route("/api/v1/sessions/{id}/sync", post(sessions::sync))
        .route("/api/v1/sessions/{id}/apply", post(sessions::apply))
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
