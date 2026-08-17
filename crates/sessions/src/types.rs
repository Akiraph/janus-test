//! Session projection types and durable state-machine values.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use janus_infrastructure::id::{AttachmentId, ProjectId, RoundId, SessionId, TurnId, UploadId};
use janus_workspace::interface::WorkspaceError;

pub const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;
pub const MAX_MESSAGE_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_ATTACHMENTS: u16 = 20;

#[derive(Debug, thiserror::Error)]
pub enum SessionsError {
    #[error("session not found")]
    NotFound,
    #[error("session is deleting")]
    SessionDeleting,
    #[error("active turn already exists")]
    ActiveTurnExists,
    #[error("turn is not in an interactive state")]
    TurnNotInteractive,
    #[error("turn is terminal")]
    TurnTerminal,
    #[error("no queued turn to promote")]
    NothingQueued,
    #[error("revision mismatch: expected {expected}, current {current}")]
    VersionMismatch { expected: String, current: String },
    #[error("timeline cursor is invalid")]
    TimelineCursorInvalid,
    #[error("model is not configured")]
    ModelNotConfigured,
    #[error("the selected model or reasoning effort is not available")]
    InvalidModelPreference,
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceError),
    #[error("storage error: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Queued,
    Running,
    Canceling,
    Completed,
    Failed,
    Canceled,
    Interrupted,
}

impl TurnStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Canceling => "canceling",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Interrupted => "interrupted",
        }
    }

    pub const fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Canceling)
    }

    pub const fn is_interactive(self) -> bool {
        matches!(self, Self::Running)
    }

    pub const fn advances_queue(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Canceled | Self::Interrupted
        )
    }

    pub const fn can_transition_to(self, target: Self) -> bool {
        match self {
            Self::Queued => matches!(target, Self::Running | Self::Canceled),
            Self::Running => matches!(
                target,
                Self::Canceling | Self::Completed | Self::Failed | Self::Interrupted
            ),
            Self::Canceling => matches!(target, Self::Canceled | Self::Interrupted),
            Self::Completed | Self::Failed | Self::Canceled | Self::Interrupted => false,
        }
    }
}

impl std::str::FromStr for TurnStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "canceling" => Ok(Self::Canceling),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            "interrupted" => Ok(Self::Interrupted),
            other => Err(format!("unknown Turn status {other}")),
        }
    }
}

/// Explicit route/source recorded on every user-driven message instead of
/// inferring intent from message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageRoute {
    /// Idle Session — a new Turn was promoted to running.
    Started,
    /// An active Turn already exists, so the message is appended to the queue.
    Queued,
}

impl MessageRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Queued => "queued",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionSummary {
    pub id: String,
    pub project_id: String,
    pub title: Option<String>,
    pub state: String,
    pub active_turn_id: Option<String>,
    pub model_preference: Option<SessionModelPreference>,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_activity_at: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    #[default]
    None,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ReasoningEffort {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SessionModelPreference {
    pub provider_id: String,
    pub upstream_model_id: String,
    #[serde(default)]
    pub reasoning_effort: ReasoningEffort,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AttachmentView {
    pub id: String,
    pub session_id: String,
    pub name: String,
    pub mime: String,
    pub byte_size: u64,
    pub lifecycle: String,
    pub version: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct AttachmentResource {
    pub id: AttachmentId,
    pub name: String,
    pub mime: String,
    pub byte_size: u64,
    pub blob_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TurnModelSnapshot {
    pub model_id: String,
    pub provider_id: String,
    pub display_name: String,
    pub upstream_model_id: String,
    pub context_limit: u32,
    pub supports_images: bool,
    pub supports_tools: bool,
    pub parameters: serde_json::Value,
    #[serde(default)]
    pub failover: Vec<TurnModelCandidateSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TurnModelCandidateSnapshot {
    pub model_id: String,
    pub provider_id: String,
    pub display_name: String,
    pub upstream_model_id: String,
    pub context_limit: u32,
    pub supports_images: bool,
    pub supports_tools: bool,
    pub parameters: serde_json::Value,
}

impl TurnModelSnapshot {
    pub(crate) fn parse(raw: &str) -> Result<Option<Self>, serde_json::Error> {
        let value: serde_json::Value = serde_json::from_str(raw)?;
        if value.is_null()
            || value
                .get("provider_id")
                .is_none_or(serde_json::Value::is_null)
        {
            return Ok(None);
        }
        serde_json::from_value(value).map(Some)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelAttemptStatus {
    Running,
    Succeeded,
    Failed,
    Canceled,
    Interrupted,
}

/// The most recent model attempt for this Turn's active Round, projected onto
/// `TurnSummary` so the UI can render the live retry counter
/// ("Reconnecting (X): reason") even when the SSE event that announced it has
/// already been consumed. Absent when the Turn has had no attempts yet.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TurnModelAttempt {
    /// 1-based retry index surfaced to the UI (0 = the initial attempt, not a
    /// retry). Matches the `attempt` field of `model.attempt_retrying` events.
    pub attempt: i64,
    pub status: ModelAttemptStatus,
    /// Normalized failure detail for a `failed` attempt; null otherwise.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct TurnTokenExchange {
    /// Non-cached model input tokens for this Turn, excluding the system and
    /// developer prompt prefix.
    pub upload_tokens: i64,
    /// Model output tokens for this Turn.
    pub download_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TurnSummary {
    pub id: String,
    pub session_id: String,
    pub sequence: i64,
    pub status: String,
    pub input_message_id: Option<String>,
    pub goal_mode: bool,
    pub model_snapshot: Option<TurnModelSnapshot>,
    pub predecessor_turn_id: Option<String>,
    pub cancellation_reason: Option<String>,
    pub completion_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model_attempt: Option<TurnModelAttempt>,
    /// The durable whole-Turn model exchange. Live direction remains a
    /// stream concern and is intentionally not stored here.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub token_exchange: Option<TurnTokenExchange>,
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
}

#[derive(Debug, Clone)]
pub struct ExecutionTurn {
    pub id: TurnId,
    pub session_id: SessionId,
    pub project_id: ProjectId,
    pub status: TurnStatus,
    pub sequence: i64,
    pub active: bool,
    pub goal_mode: bool,
    pub model_snapshot: Option<TurnModelSnapshot>,
}

#[derive(Debug, Clone)]
pub struct ContextMessage {
    pub turn_id: Option<String>,
    pub kind: String,
    pub body: serde_json::Value,
    pub timeline_sequence: i64,
}

#[derive(Debug, Clone)]
pub struct TurnTransition {
    pub from_status: TurnStatus,
    pub to_status: TurnStatus,
    pub session_version: String,
}

pub enum ActiveTurnOutcome<'a> {
    Completed {
        summary: &'a serde_json::Value,
        input_tokens: i64,
        output_tokens: i64,
    },
    Failed {
        reason: &'a str,
        summary: &'a serde_json::Value,
    },
    Canceled {
        reason: &'a str,
    },
    Interrupted {
        reason: &'a str,
    },
}

#[derive(Debug, Clone)]
pub struct RecoveredTurn {
    pub turn_id: TurnId,
    pub session_id: SessionId,
    pub from_status: TurnStatus,
    pub turn_version: String,
    pub session_version: Option<String>,
    pub wake_required: bool,
}

#[derive(Debug, Clone)]
pub struct TerminalSettlement {
    pub transition: Option<TurnTransition>,
    pub promoted_turn: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionCommandState {
    pub project_id: String,
    pub state: String,
    pub next_model_ref: Option<String>,
    pub active_turn_id: Option<String>,
    pub session_version: String,
}

#[derive(Debug, Clone)]
pub struct QueuedTurnCandidate {
    pub turn_id: TurnId,
    pub session_id: SessionId,
    pub model_snapshot: Option<TurnModelSnapshot>,
}

/// A queued Turn shown in the conversation's QueuedMessagesBar: enough to
/// render the message text and delete (cancel) the Turn out of order.
#[derive(Debug, Clone, sqlx::FromRow, ToSchema, Serialize)]
pub struct QueuedTurnItem {
    pub turn_id: String,
    pub sequence: i64,
    pub version: String,
    /// Best-effort message text extracted from the queued user message body.
    /// Empty when there is no associated message or the body is non-textual.
    pub message_text: String,
}

#[derive(Debug, Clone)]
pub struct CreatedTurnInput {
    pub turn_id: String,
    pub message_id: String,
    pub timeline_item_id: String,
    pub sequence: i64,
    pub display_order: i64,
    pub checkpoint_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RecordedTurnInput {
    pub message_id: String,
    pub timeline_item_id: String,
    pub display_order: i64,
}

/// All values needed to store and persist an uploaded attachment.
/// Keeping the owner, bytes, and session together prevents callers from
/// accidentally attaching bytes to a different session than the upload.
pub struct UploadAttachmentInput<'a> {
    pub owner_id: &'a str,
    pub session_id: SessionId,
    pub upload_id: UploadId,
    pub attachment_id: AttachmentId,
    pub name: &'a str,
    pub mime: &'a str,
    pub byte_size: u64,
    pub bytes: &'a [u8],
}

/// Values for the user message and Turn rows created under one transaction.
pub struct CreateTurnInput<'a> {
    pub session_id: SessionId,
    pub content: &'a str,
    pub actor: &'a serde_json::Value,
    pub message_kind: &'a str,
    pub timeline_kind: &'a str,
    pub metadata: Option<&'a serde_json::Value>,
    pub goal_mode: bool,
    pub predecessor_turn_id: Option<&'a str>,
    pub attachment_ids: &'a [AttachmentId],
    pub model_snapshot: Option<&'a TurnModelSnapshot>,
    pub checkpoint_revision: Option<&'a str>,
    pub now: &'a str,
}

/// Values for a steer message bound to an already active Turn.
pub struct AppendSteerInput<'a> {
    pub session_id: SessionId,
    pub expected_turn_id: Option<TurnId>,
    pub content: &'a str,
    pub expected_version: &'a str,
    pub actor: &'a serde_json::Value,
    pub now: &'a str,
}

/// Values for the durable tool-call projection and protocol message.
pub struct AppendToolResultInput<'a> {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub tool_call_id: &'a str,
    pub provider_call_id: &'a str,
    pub tool_name: &'a str,
    pub status: &'a str,
    pub summary: &'a serde_json::Value,
    pub model_parts: &'a serde_json::Value,
    pub actor: &'a serde_json::Value,
    pub now: &'a str,
}

/// Values used when a completed Runtime result replaces its requested tool
/// projection and protocol message.
pub struct ReplaceToolResultInput<'a> {
    pub session_id: SessionId,
    pub source_turn_id: TurnId,
    pub tool_call_id: &'a str,
    pub provider_call_id: &'a str,
    pub tool_name: &'a str,
    pub status: &'a str,
    pub summary: &'a serde_json::Value,
    pub model_parts: &'a serde_json::Value,
    pub now: &'a str,
}

pub struct AppendAssistantMessage<'a> {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub round_id: RoundId,
    pub text: &'a str,
    pub reasoning: &'a str,
    /// Raw provider reasoning, echoed back verbatim on the next request. The
    /// display-formatted `reasoning` must not be used for echo-back.
    pub reasoning_content: Option<&'a str>,
    /// Wall-clock ms the round spent producing this assistant message (from
    /// round creation to acceptance). `None` only when the start stamp could not
    /// be parsed. Surfaced in the timeline item projection so the UI can render
    /// "Thought for {duration}".
    pub duration_ms: Option<i64>,
    pub tool_calls: &'a serde_json::Value,
    pub actor: &'a serde_json::Value,
    pub now: &'a str,
}

/// Result of cancelling a Turn (`running ->
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TimelineTurnStatus {
    pub id: String,
    pub status: String,
    pub cancellation_reason: Option<String>,
    pub completion_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
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
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub turn_status: Option<TimelineTurnStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TimelinePage {
    pub items: Vec<TimelineItemView>,
    pub oldest_cursor: Option<String>,
    pub newest_cursor: Option<String>,
    pub has_older: bool,
    pub has_newer: bool,
}

#[cfg(test)]
mod tests {
    use super::TurnStatus;

    #[test]
    fn every_terminal_turn_status_advances_the_queue() {
        for status in [
            TurnStatus::Completed,
            TurnStatus::Failed,
            TurnStatus::Canceled,
            TurnStatus::Interrupted,
        ] {
            assert!(status.advances_queue(), "{status:?} must advance the queue");
        }
    }

    #[test]
    fn active_turn_statuses_do_not_advance_the_queue() {
        for status in [
            TurnStatus::Queued,
            TurnStatus::Running,
            TurnStatus::Canceling,
        ] {
            assert!(
                !status.advances_queue(),
                "{status:?} must remain in control"
            );
        }
    }
}
