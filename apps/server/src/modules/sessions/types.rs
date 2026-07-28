//! Session projection types (M3 Stage 2).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::modules::workspace_sync::interface::WorkspaceSyncError;

#[derive(Debug, thiserror::Error)]
pub enum SessionsError {
    #[error("session not found")]
    NotFound,
    #[error("project not found")]
    ProjectNotFound,
    #[error("project is not ready")]
    ProjectNotReady,
    #[error("session is deleting")]
    SessionDeleting,
    #[error("active turn already exists")]
    ActiveTurnExists,
    #[error("turn is not in an interactive state")]
    TurnNotInteractive,
    #[error("turn is terminal")]
    TurnTerminal,
    #[error("steer rejected while turn is waiting for model")]
    SteerBlockedByModel,
    #[error("ask not found")]
    AskNotFound,
    #[error("ask is not open")]
    AskNotOpen,
    #[error("no queued turn to promote")]
    NothingQueued,
    #[error("handoff is not applicable")]
    HandoffNotApplicable,
    #[error("revision mismatch: expected {expected}, current {current}")]
    VersionMismatch { expected: String, current: String },
    #[error("timeline cursor is invalid")]
    TimelineCursorInvalid,
    #[error("model is not configured")]
    ModelNotConfigured,
    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceSyncError),
    #[error("storage error: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

/// Explicit route/source recorded on every user-driven message instead of
/// inferring intent from text (M4 design: "Message Routing").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageRoute {
    /// Idle Session — a new Turn was promoted to running.
    Started,
    /// Active Turn not waiting for finite work — appended to the queue.
    Queued,
    /// Terminal predecessor handed off, this successor now holds the active Turn.
    Handoff,
    /// Late answer to a finished blocking Ask was re-routed as a Steer.
    AskAnswerSteer,
}

impl MessageRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Queued => "queued",
            Self::Handoff => "handoff",
            Self::AskAnswerSteer => "ask_answer_steer",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionSummary {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    pub title: Option<String>,
    pub state: String,
    pub workspace_handle: String,
    pub workspace_revision: Option<String>,
    pub source_main_revision_id: String,
    pub active_turn_id: Option<String>,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_activity_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TurnSummary {
    pub id: String,
    pub session_id: String,
    pub sequence: i64,
    pub status: String,
    pub input_message_id: Option<String>,
    pub predecessor_turn_id: Option<String>,
    pub handoff_from_turn_id: Option<String>,
    pub handoff_to_turn_id: Option<String>,
    pub cancellation_reason: Option<String>,
    pub completion_reason: Option<String>,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MessageRouteResult {
    /// Current routing result for the accepted message.
    pub route: String,
    pub message_id: String,
    pub turn_id: String,
    pub session_version: String,
    /// True when the active Turn is `waiting_for_job` and the just-queued
    /// message is intended to take over via an atomic Handoff. The HTTP layer
    /// hands this Turn to `application::session_flow::handoff_message`, which
    /// promotes it to the successor and transfers the predecessor's finite
    /// Jobs/Asks transactionally. `route` stays `queued` until the coordinator
    /// promotes it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub awaiting_handoff: bool,
}

/// A queued Turn awaiting promotion (projection used by Session UI queue view).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QueuedTurnSummary {
    pub turn_id: String,
    pub session_id: String,
    pub sequence: i64,
    pub message_id: Option<String>,
    pub source: String,
    pub created_at: String,
}

/// Result of cancelling a Turn (Stage 4 state machine: `running ->
/// canceling -> canceled|interrupted`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CancelResult {
    pub turn_id: String,
    pub from_status: String,
    pub to_status: String,
    pub session_version: String,
}

/// Result of a Steer: bound to the running Turn, visible at the next safe Round.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SteerResult {
    pub turn_id: String,
    pub message_id: String,
    pub session_version: String,
}

/// Blocking vs best-effort Ask (M4 Ask flow). Supervisor creates the Ask row;
/// Sessions owns the answer/expiry write that resumes the Turn.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AskSummary {
    pub id: String,
    pub turn_id: String,
    pub mode: String,
    pub status: String,
    pub expires_at: Option<String>,
    pub answered_at: Option<String>,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AskAnswerResult {
    pub ask_id: String,
    pub turn_id: String,
    pub turn_status_after: String,
    pub session_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TimelineItemView {
    pub id: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub kind: String,
    pub source_resource_id: Option<String>,
    pub display_order: i64,
    pub projection: serde_json::Value,
    pub status: String,
    pub version: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TimelinePage {
    pub items: Vec<TimelineItemView>,
    pub oldest_cursor: Option<String>,
    pub newest_cursor: Option<String>,
    pub has_older: bool,
    pub has_newer: bool,
}
