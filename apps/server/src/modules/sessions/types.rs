//! Session projection types (M3 Stage 2).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::modules::workspace_sync::interface::WorkspaceSyncError;
use crate::platform::id::{AskId, AttachmentId, ProjectId, RoundId, SessionId, TurnId};

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
    #[error("the selected model or reasoning effort is not available")]
    InvalidModelPreference,
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceSyncError),
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
    WaitingForJob,
    WaitingForAsk,
    WaitingForModel,
    Canceling,
    Completed,
    Failed,
    Canceled,
    Interrupted,
    HandedOff,
}

impl TurnStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingForJob => "waiting_for_job",
            Self::WaitingForAsk => "waiting_for_ask",
            Self::WaitingForModel => "waiting_for_model",
            Self::Canceling => "canceling",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Interrupted => "interrupted",
            Self::HandedOff => "handed_off",
        }
    }

    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Running
                | Self::WaitingForJob
                | Self::WaitingForAsk
                | Self::WaitingForModel
                | Self::Canceling
        )
    }

    pub const fn is_interactive(self) -> bool {
        matches!(
            self,
            Self::Running | Self::WaitingForJob | Self::WaitingForAsk | Self::WaitingForModel
        )
    }

    pub const fn advances_queue(self) -> bool {
        matches!(self, Self::Completed | Self::Canceled)
    }

    pub const fn can_transition_to(self, target: Self) -> bool {
        match self {
            Self::Queued => matches!(target, Self::Running),
            Self::Running => matches!(
                target,
                Self::WaitingForJob
                    | Self::WaitingForAsk
                    | Self::WaitingForModel
                    | Self::Canceling
                    | Self::Completed
                    | Self::Failed
                    | Self::Interrupted
            ),
            Self::WaitingForJob => matches!(
                target,
                Self::Running
                    | Self::WaitingForAsk
                    | Self::Canceling
                    | Self::Interrupted
                    | Self::HandedOff
            ),
            Self::WaitingForAsk => matches!(
                target,
                Self::Running | Self::WaitingForJob | Self::Canceling | Self::Interrupted
            ),
            Self::WaitingForModel => {
                matches!(target, Self::Running | Self::Canceling | Self::Interrupted)
            }
            Self::Canceling => matches!(target, Self::Canceled | Self::Interrupted),
            Self::Completed
            | Self::Failed
            | Self::Canceled
            | Self::Interrupted
            | Self::HandedOff => false,
        }
    }
}

impl std::str::FromStr for TurnStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "waiting_for_job" => Ok(Self::WaitingForJob),
            "waiting_for_ask" => Ok(Self::WaitingForAsk),
            "waiting_for_model" => Ok(Self::WaitingForModel),
            "canceling" => Ok(Self::Canceling),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            "interrupted" => Ok(Self::Interrupted),
            "handed_off" => Ok(Self::HandedOff),
            other => Err(format!("unknown Turn status {other}")),
        }
    }
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
    HandedOff,
    /// Late answer to a finished blocking Ask was re-routed as a Steer.
    AskAnswerSteer,
}

impl MessageRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Queued => "queued",
            Self::HandedOff => "handed_off",
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
/// ("Reconnecting (X/5): reason") even when the SSE event that announced it has
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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TurnSummary {
    pub id: String,
    pub session_id: String,
    pub sequence: i64,
    pub status: String,
    pub input_message_id: Option<String>,
    pub model_snapshot: Option<TurnModelSnapshot>,
    pub predecessor_turn_id: Option<String>,
    pub handoff_from_turn_id: Option<String>,
    pub handoff_to_turn_id: Option<String>,
    pub cancellation_reason: Option<String>,
    pub completion_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model_attempt: Option<TurnModelAttempt>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_from_turn_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExecutionTurn {
    pub id: TurnId,
    pub session_id: SessionId,
    pub project_id: ProjectId,
    pub status: TurnStatus,
    pub sequence: i64,
    pub active: bool,
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

#[derive(Debug, Clone, Copy, Default)]
pub struct TurnBlockers {
    pub open_ask: bool,
    pub unfinished_job: bool,
}

impl TurnBlockers {
    pub const fn status(self) -> TurnStatus {
        if self.open_ask {
            TurnStatus::WaitingForAsk
        } else if self.unfinished_job {
            TurnStatus::WaitingForJob
        } else {
            TurnStatus::Running
        }
    }
}

#[derive(Debug, Clone)]
pub struct TurnBlockerOutcome {
    pub session_id: SessionId,
    pub status: TurnStatus,
    pub active: bool,
    pub session_version: String,
    pub transition: Option<TurnTransition>,
}

#[derive(Debug, Clone)]
pub struct RecoveredTurn {
    pub turn_id: TurnId,
    pub session_id: SessionId,
    pub from_status: TurnStatus,
    pub turn_version: String,
    pub session_version: Option<String>,
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
    pub workspace_handle: String,
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

pub struct RecordAskAnswer<'a> {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub ask_id: AskId,
    pub answer: &'a serde_json::Value,
    pub actor: &'a serde_json::Value,
    pub now: &'a str,
}

pub struct AppendAssistantMessage<'a> {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub round_id: RoundId,
    pub text: &'a str,
    pub reasoning: &'a str,
    /// Wall-clock ms the round spent producing this assistant message (from
    /// round creation to acceptance). `None` only when the start stamp could not
    /// be parsed. Surfaced in the timeline item projection so the UI can render
    /// "Thought for {duration}".
    pub duration_ms: Option<i64>,
    pub tool_calls: &'a serde_json::Value,
    pub actor: &'a serde_json::Value,
    pub now: &'a str,
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
