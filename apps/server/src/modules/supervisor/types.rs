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
    pub finish_summary: Option<CompletionSummary>,
    pub wait: Option<TurnWait>,
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
pub enum TurnWait {
    Job,
    Ask,
}

impl TurnWait {
    pub const fn status(self) -> &'static str {
        match self {
            Self::Job => "waiting_for_job",
            Self::Ask => "waiting_for_ask",
        }
    }

    pub const fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Ask, _) | (_, Self::Ask) => Self::Ask,
            _ => Self::Job,
        }
    }
}
