//! Turn execution state: lifecycle transitions (state), runnability/queue scheduling (scheduling), and turn content read/write (persistence).

mod persistence;
mod scheduling;
mod state;

use std::collections::HashSet;

use janus_infrastructure::clock::now_utc_str;
use serde_json::json;
use sqlx::{Row, SqliteConnection};

use janus_infrastructure::id::{
    AttachmentId, CheckpointId, MessageId, ProjectId, SessionId, TimelineItemId, TurnId,
};
use janus_workspace::interface::WorkspaceHandle;

use super::interface::SessionsInterface;
use super::types::{
    ActiveTurnOutcome, AppendAssistantMessage, AppendSteerInput, AppendToolResultInput,
    ContextMessage, CreateTurnInput, CreatedTurnInput, ExecutionTurn, MAX_ATTACHMENTS,
    MAX_MESSAGE_BYTES, QueuedTurnCandidate, RecoveredTurn, ReplaceToolResultInput,
    SessionCommandState, SessionModelPreference, SessionsError, TurnBlockerOutcome, TurnBlockers,
    TurnModelSnapshot, TurnStatus, TurnTransition,
};

pub(crate) struct ActiveTurnTransition<'a> {
    session_id: SessionId,
    turn_id: TurnId,
    from_status: TurnStatus,
    to_status: TurnStatus,
    reason: Option<&'a str>,
    now: &'a str,
}

impl SessionsInterface {
    pub fn now(&self) -> String {
        now_utc_str()
    }
}
