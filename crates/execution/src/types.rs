//! Execution domain types.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use janus_infrastructure::id::{AskId, ToolCallId, TurnId};
use janus_models::interface::ModelsError;
use janus_projects::interface::ProjectsError;
use janus_runtime::interface::RuntimeError;
use janus_sessions::interface::SessionsError;
use janus_workspace::interface::PathError;
use janus_workspace::interface::WorkspaceError;

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("turn not found")]
    TurnNotFound,
    #[error("session not found")]
    SessionNotFound,
    #[error("model is not configured")]
    ModelNotConfigured,
    #[error("ask not found")]
    AskNotFound,
    #[error("tool not allowed: {0}")]
    ToolNotAllowed(String),
    #[error("tool path invalid")]
    ToolPathInvalid,
    #[error("image too large")]
    ImageTooLarge,
    #[error("unsupported image")]
    UnsupportedImage,
    #[error("provider stream failed: {0}")]
    ProviderStream(String),
    #[error("sessions error: {0}")]
    Sessions(#[from] SessionsError),
    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceError),
    #[error("models error: {0}")]
    Models(#[from] ModelsError),
    #[error("projects error: {0}")]
    Projects(#[from] ProjectsError),
    #[error("runtime error: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("path error: {0}")]
    Path(#[from] PathError),
    #[error("storage error: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpecEntry {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultPart {
    Text {
        text: String,
    },
    /// Ephemeral image for the next model request only — not persisted as Base64.
    Image {
        mime: String,
        #[serde(skip)]
        bytes: Vec<u8>,
        width: u32,
        height: u32,
        path: String,
        content_revision: Option<String>,
        derived: bool,
        content_hash: String,
    },
    Json {
        value: serde_json::Value,
    },
}

#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub disposition: ToolExecutionDisposition,
    pub parts: Vec<ToolResultPart>,
    pub summary: serde_json::Value,
    pub error_code: Option<String>,
    /// When set, the Turn should complete with this summary.
    pub finish_summary: Option<CompletionSummary>,
    pub wait: Option<TurnWait>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDisplay {
    pub version: u16,
    pub title: String,
    pub status: String,
    pub body: ToolDisplayBody,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolDisplayBody {
    None,
    Text {
        text: String,
    },
    Structured {
        value: serde_json::Value,
    },
    Patch {
        patch: String,
    },
    CommandOutput {
        command: String,
        stdout: String,
        stderr: String,
        exit_code: Option<i32>,
        truncated: bool,
    },
    Error {
        code: String,
        detail: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionDisposition {
    Succeeded,
    Failed,
    Waiting,
}

impl ToolExecutionDisposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Waiting => "waiting",
        }
    }

    pub const fn is_waiting(self) -> bool {
        matches!(self, Self::Waiting)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallStatus {
    Requested,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Canceled,
    Lost,
}

impl ToolCallStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Lost => "lost",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolCallSettlement {
    pub tool_call_id: String,
    pub source_turn_id: String,
    pub provider_call_id: String,
    pub tool_name: String,
    pub status: ToolCallStatus,
    pub summary: serde_json::Value,
    pub model_parts: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionSummary {
    pub summary: String,
    pub main_changes: Vec<String>,
    pub validation_performed: Vec<String>,
    pub validation_not_performed: Vec<String>,
    pub remaining_risks: Vec<String>,
}

impl CompletionSummary {
    pub fn from_tool_input(input: &serde_json::Value) -> Self {
        Self {
            summary: input
                .get("summary")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("done")
                .to_owned(),
            main_changes: string_items(input.get("main_changes")),
            validation_performed: string_items(input.get("validation_performed")),
            validation_not_performed: string_items(input.get("validation_not_performed")),
            remaining_risks: string_items(
                input.get("remaining_risks").or_else(|| input.get("risks")),
            ),
        }
    }

    pub fn from_text(text: &str) -> Self {
        Self {
            summary: if text.is_empty() {
                "The model stopped without a structured completion summary.".to_owned()
            } else {
                text.to_owned()
            },
            main_changes: Vec::new(),
            validation_performed: Vec::new(),
            validation_not_performed: vec![
                "No validation was reported in the text-only completion.".to_owned(),
            ],
            remaining_risks: vec![
                "Structured completion details were not provided by the model.".to_owned(),
            ],
        }
    }
}

fn string_items(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {
            vec![value.to_owned()]
        }
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskMode {
    Blocking,
    NonBlocking,
}

impl AskMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocking => "blocking",
            Self::NonBlocking => "non_blocking",
        }
    }

    /// Keep the deployed database value while exposing the clearer public
    /// `non_blocking` name to the model and HTTP projections.
    pub const fn storage_str(self) -> &'static str {
        match self {
            Self::Blocking => "blocking",
            Self::NonBlocking => "best_effort",
        }
    }

    pub const fn is_blocking(self) -> bool {
        matches!(self, Self::Blocking)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskStatus {
    Open,
    Answered,
    Expired,
    ClosedByHandoff,
    Canceled,
}

impl AskStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Answered => "answered",
            Self::Expired => "expired",
            Self::ClosedByHandoff => "closed_by_handoff",
            Self::Canceled => "canceled",
        }
    }
}

impl TryFrom<&str> for AskStatus {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "open" => Ok(Self::Open),
            "answered" => Ok(Self::Answered),
            "expired" => Ok(Self::Expired),
            "closed_by_handoff" => Ok(Self::ClosedByHandoff),
            "canceled" => Ok(Self::Canceled),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskClosure {
    Handoff,
    UserCancel,
    ControlPlaneRestart,
}

impl AskClosure {
    pub const fn status(self) -> AskStatus {
        match self {
            Self::Handoff => AskStatus::ClosedByHandoff,
            Self::UserCancel | Self::ControlPlaneRestart => AskStatus::Canceled,
        }
    }

    pub const fn reason(self) -> &'static str {
        match self {
            Self::Handoff => "handoff",
            Self::UserCancel => "user_cancel",
            Self::ControlPlaneRestart => "control_plane_restart",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AskRequest {
    pub id: AskId,
    pub turn_id: TurnId,
    pub tool_call_id: ToolCallId,
    pub mode: AskMode,
    pub prompt: serde_json::Value,
    pub choices: serde_json::Value,
    pub multiple: bool,
    pub default: Option<serde_json::Value>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskAnswerDisposition {
    Accepted,
    Duplicate,
    Late,
}

#[derive(Debug, Clone)]
pub struct AskAnswer {
    pub ask_id: AskId,
    pub turn_id: TurnId,
    pub disposition: AskAnswerDisposition,
    pub tool_call: Option<ToolCallSettlement>,
}

#[derive(Debug, Clone)]
pub struct ExpiredAsk {
    pub ask_id: AskId,
    pub turn_id: TurnId,
    pub default: Option<serde_json::Value>,
    pub tool_call: ToolCallSettlement,
}

#[derive(Debug, Clone, Default)]
pub struct TurnWait {
    waits_for_job: bool,
    asks: Vec<AskRequest>,
}

impl TurnWait {
    pub fn job() -> Self {
        Self {
            waits_for_job: true,
            asks: Vec::new(),
        }
    }

    pub fn ask(request: AskRequest) -> Self {
        Self {
            waits_for_job: false,
            asks: vec![request],
        }
    }

    pub fn status(&self) -> &'static str {
        if self.has_ask() {
            "waiting_for_ask"
        } else if self.waits_for_job {
            "waiting_for_job"
        } else {
            "running"
        }
    }

    pub fn combine(mut self, mut other: Self) -> Self {
        self.waits_for_job |= other.waits_for_job;
        self.asks.append(&mut other.asks);
        self
    }

    pub fn waits_for_job(&self) -> bool {
        self.waits_for_job
    }

    pub fn has_ask(&self) -> bool {
        !self.asks.is_empty()
    }

    pub fn asks(&self) -> &[AskRequest] {
        &self.asks
    }
}

#[derive(Debug, Clone, Default)]
pub struct TurnExecutionOutcome {
    pub coordination: Option<TurnWait>,
}

impl TurnExecutionOutcome {
    pub fn coordinate(wait: TurnWait) -> Self {
        Self {
            coordination: Some(wait),
        }
    }
}
