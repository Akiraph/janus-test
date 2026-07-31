//! Model streaming request/response types (M3 Stage 3).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One content part in a model message. Image bytes are held only for the
/// duration of a Provider request (transport encoding); they are never
/// persisted as Base64 in Janus history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    /// In-memory image payload for the current attempt only.
    Image {
        mime: String,
        /// Raw image bytes (not Base64). Adapter may encode for wire format.
        #[serde(skip)]
        bytes: Vec<u8>,
        width: Option<u32>,
        height: Option<u32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub parts: Vec<ContentPart>,
    /// Optional tool call id when role is Tool.
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<CompletedToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub owner_id: String,
    pub provider_id: String,
    pub upstream_model_id: String,
    pub parameters: serde_json::Value,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    /// Correlation for attempt ledger (optional for pure stream tests).
    pub round_id: Option<String>,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamChannel {
    Text,
    ReasoningSummary,
    ToolCallPreview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallDelta {
    pub index: u32,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments_delta: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedToolCall {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Default)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelStreamEvent {
    /// Provisional delta; never execute tools from this alone.
    Delta {
        attempt_id: String,
        sequence: u64,
        channel: StreamChannel,
        text: String,
        provisional: bool,
    },
    ToolCallDelta {
        attempt_id: String,
        sequence: u64,
        delta: ToolCallDelta,
        provisional: bool,
    },
    Completed {
        attempt_id: String,
        usage: TokenUsage,
        stop_reason: Option<String>,
        tool_calls: Vec<CompletedToolCall>,
        /// Final assistant text assembled from deltas (for Round commit).
        text: String,
        reasoning: String,
    },
    Failed {
        attempt_id: String,
        code: String,
        detail: String,
    },
    /// Emitted just before an in-Round retry. `attempt` is the retry index the
    /// model_attempts ledger is about to record (1-based; `MAX_ATTEMPTS_PER_CANDIDATE`),
    /// and `detail` is the human-facing failure reason for the attempt that just
    /// failed. The model stream publisher forwards it as `model.attempt_retrying`
    /// so the UI can render `Reconnecting ({attempt}/5): {detail}`.
    Retrying {
        attempt_id: String,
        attempt: usize,
        detail: String,
        retry_after_ms: u64,
    },
}
