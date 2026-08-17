//! Turn execution state: lifecycle transitions (state), runnability/queue scheduling (scheduling), and turn content read/write (persistence).

mod persistence;
mod scheduling;
mod state;

use std::collections::HashSet;

use janus_infrastructure::clock::now_utc_str;
use serde_json::json;
use sqlx::{Row, SqliteConnection};

use super::interface::SessionsInterface;
use super::types::{
    ActiveTurnOutcome, AppendAssistantMessage, AppendSteerInput, AppendToolResultInput,
    ContextMessage, CreateTurnInput, CreatedTurnInput, ExecutionTurn, MAX_ATTACHMENTS,
    MAX_MESSAGE_BYTES, QueuedTurnCandidate, RecoveredTurn, ReplaceToolResultInput,
    SessionCommandState, SessionModelPreference, SessionsError, TurnModelSnapshot, TurnStatus,
    TurnTransition,
};
use janus_infrastructure::id::{
    AttachmentId, CheckpointId, MessageId, ProjectId, SessionId, TimelineItemId, TurnId,
};

impl SessionsInterface {
    pub fn now(&self) -> String {
        now_utc_str()
    }
}
