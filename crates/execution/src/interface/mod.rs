//! Turn execution loop and durable Round/Tool Call coordination.

mod persistence;
mod scheduling;
mod turn;

use std::{collections::HashSet, future::Future};

use janus_infrastructure::clock::now_utc_str;
use janus_infrastructure::{
    events::{EventStore, EventType, NewEvent},
    id::{AskId, CorrelationId, JobId, RoundId, SessionId, ToolCallId, TurnId},
    managed_storage::BlobStore,
    state_broadcaster::StateBroadcaster,
    unit_of_work::UnitOfWork,
};
use janus_models::interface::{
    ChatMessage, ChatRole, CompletedToolCall, ContentPart, ModelRequest, ModelStreamEvent,
    ModelsInterface, StreamChannel, ToolSpec,
};
use janus_projects::interface::ProjectsInterface;
use janus_runtime::interface::{JobProjection, JobStatus};
use janus_sessions::interface::{
    ActiveTurnOutcome, AppendAssistantMessage, AppendToolResultInput, ExecutionTurn,
    ModelAttemptStatus, SessionsInterface, TurnModelAttempt, TurnStatus,
};
use janus_workspace::interface::WorkspaceInterface;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{SqliteConnection, SqlitePool};

use super::context::SYSTEM_PROMPT;
pub use super::context::{latest_compact_summary, record_context_version, schedule_compact};
use super::registry::{SCHEMA_VERSION, available_tools};
use super::retry::{FaultClass, MAX_ATTEMPTS_PER_CANDIDATE, classify, classify_fault};
use super::tools::{
    ToolContext, attach_tool_display, execute_tool, read_attachment_bytes, supported_image_mime,
};
pub use super::types::{
    AskAnswer, AskAnswerDisposition, AskClosure, AskMode, AskRequest, AskStatus, CompletionSummary,
    ExecutionError, ExpiredAsk, ToolCallSettlement, ToolCallStatus, ToolExecutionDisposition,
    ToolOutcome, ToolResultPart, TurnExecutionOutcome, TurnWait,
};

#[derive(Debug, Clone, Serialize)]
pub struct ContextUsageView {
    pub estimated_input_tokens: i64,
    pub context_limit: i64,
    pub compact_status: String,
    pub created_at: String,
}

#[derive(Clone)]
struct AcceptedToolCall {
    id: ToolCallId,
    ordinal: i64,
    request: CompletedToolCall,
}

struct AcceptedRoundResponse<'a> {
    session_id: SessionId,
    turn_id: TurnId,
    round_id: &'a RoundId,
    attempt_id: &'a str,
    input_tokens: i64,
    output_tokens: i64,
    stop_reason: Option<&'a str>,
    text: &'a str,
    reasoning: &'a str,
    reasoning_duration_ms: Option<u64>,
    tool_calls: &'a [CompletedToolCall],
    actor: &'a Value,
}

struct ExecutedToolCall {
    outcome: ToolOutcome,
    message: ChatMessage,
}

type SettledAskToolCallRow = (
    String,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
);

enum CompleteTurnOutcome {
    Completed,
    WaitingForJob,
}

/// Dependencies assembled by the server composition root for one execution
/// coordinator. Keeping this as a named bundle makes the capability boundary
/// explicit without turning the constructor into an unreviewable argument list.
pub struct ExecutionDependencies {
    pub pool: SqlitePool,
    pub events: EventStore,
    pub state_broadcaster: StateBroadcaster,
    pub models: ModelsInterface,
    pub projects: ProjectsInterface,
    pub workspace: WorkspaceInterface,
    pub sessions: SessionsInterface,
    pub blobs: BlobStore,
    pub runtime: janus_runtime::interface::RuntimeInterface,
}

#[derive(Clone)]
pub struct ExecutionInterface {
    pool: SqlitePool,
    events: EventStore,
    state_broadcaster: StateBroadcaster,
    unit_of_work: UnitOfWork,
    models: ModelsInterface,
    projects: ProjectsInterface,
    workspace: WorkspaceInterface,
    sessions: SessionsInterface,
    blobs: BlobStore,
    runtime: janus_runtime::interface::RuntimeInterface,
}

impl ExecutionInterface {
    pub fn new(dependencies: ExecutionDependencies) -> Self {
        let ExecutionDependencies {
            pool,
            events,
            state_broadcaster,
            models,
            projects,
            workspace,
            sessions,
            blobs,
            runtime,
        } = dependencies;
        let unit_of_work = UnitOfWork::new(pool.clone(), events.clone());
        Self {
            pool,
            events,
            state_broadcaster,
            unit_of_work,
            models,
            projects,
            workspace,
            sessions,
            blobs,
            runtime,
        }
    }

}

/// Inspect the streamed events and return the fault class only when the stream
/// ended in `Failed` (no `Completed`). Used by the Round-level posture: a
/// `Transient` final failure (retries exhausted) parks the Turn on
/// `waiting_for_model` so the UI can surface the reason; `Config` fails it.
fn classify_failed(events: &[ModelStreamEvent]) -> Option<FaultClass> {
    let failed = events.iter().rev().find_map(|e| match e {
        ModelStreamEvent::Failed {
            code,
            detail,
            attempt_id: _,
        } => Some((code.clone(), detail.clone())),
        _ => None,
    })?;
    Some(classify_fault(&failed.0, &failed.1))
}

fn tool_result_message(outcome: &ToolOutcome, provider_call_id: &str) -> (ChatMessage, Value) {
    let mut model_parts = Vec::new();
    let mut durable_parts = Vec::new();
    for part in &outcome.parts {
        match part {
            ToolResultPart::Text { text } => {
                model_parts.push(ContentPart::Text { text: text.clone() });
                durable_parts.push(json!({"type": "text", "text": text}));
            }
            ToolResultPart::Json { value } => {
                let text = value.to_string();
                model_parts.push(ContentPart::Text { text: text.clone() });
                durable_parts.push(json!({"type": "text", "text": text}));
            }
            ToolResultPart::Image {
                mime,
                bytes,
                width,
                height,
                path,
                content_revision,
                derived,
                content_hash,
            } => {
                model_parts.push(ContentPart::Image {
                    mime: mime.clone(),
                    bytes: bytes.clone(),
                    width: Some(*width),
                    height: Some(*height),
                });
                durable_parts.push(json!({
                    "type": "image_reference",
                    "mime": mime,
                    "path": path,
                    "content_revision": content_revision,
                    "derived": derived,
                    "content_hash": content_hash,
                    "width": width,
                    "height": height,
                }));
                durable_parts.push(json!({
                    "type": "text",
                    "text": format!(
                        "[image result: {path}, {mime}, {width}x{height}, hash={content_hash}]"
                    ),
                }));
            }
        }
    }
    if model_parts.is_empty() {
        let text = outcome.summary.to_string();
        model_parts.push(ContentPart::Text { text: text.clone() });
        durable_parts.push(json!({"type": "text", "text": text}));
    }
    (
        ChatMessage {
            role: ChatRole::Tool,
            parts: model_parts,
            tool_call_id: Some(provider_call_id.to_owned()),
            tool_calls: Vec::new(),
        },
        Value::Array(durable_parts),
    )
}

fn format_ask_answer(answer: &Value) -> String {
    match answer {
        Value::Null => "No answer was provided.".to_owned(),
        Value::String(value) => value.clone(),
        Value::Array(values) => {
            let answer = values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            if answer.is_empty() {
                "No answer was provided.".to_owned()
            } else {
                answer
            }
        }
        value => value.to_string(),
    }
}
