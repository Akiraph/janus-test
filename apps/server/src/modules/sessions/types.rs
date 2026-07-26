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
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MessageRouteResult {
    /// M3 always starts immediately (no queue/handoff).
    pub route: String,
    pub message_id: String,
    pub turn_id: String,
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
