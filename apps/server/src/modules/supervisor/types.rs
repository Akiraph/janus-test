//! Supervisor domain types (M3 Stage 4).

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::modules::models::interface::ModelsError;
use crate::modules::sessions::types::SessionsError;
use crate::modules::workspace_sync::interface::WorkspaceSyncError;
use crate::platform::path::PathError;

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error("turn not found")]
    TurnNotFound,
    #[error("session not found")]
    SessionNotFound,
    #[error("model is not configured")]
    ModelNotConfigured,
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
    Workspace(#[from] WorkspaceSyncError),
    #[error("models error: {0}")]
    Models(#[from] ModelsError),
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
    pub ok: bool,
    pub parts: Vec<ToolResultPart>,
    pub summary: serde_json::Value,
    pub error_code: Option<String>,
    /// When set, the Turn should complete with this summary.
    pub finish_summary: Option<serde_json::Value>,
    /// When set, the Turn should pause in this status after the tool returns
    /// (`waiting_for_job` / `waiting_for_ask`). Stage 5 runtime tools use this.
    pub wait_state: Option<String>,
}
