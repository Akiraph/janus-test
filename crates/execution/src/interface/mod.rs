//! Turn execution loop and durable Round/Tool Call coordination.

mod persistence;
mod scheduling;
mod turn;

use std::{collections::HashSet, future::Future};

use janus_infrastructure::clock::now_utc_str;
use janus_infrastructure::{
    events::{EventStore, EventType, NewEvent},
    id::{CorrelationId, RoundId, SessionId, ToolCallId, TurnId},
    managed_storage::BlobStore,
    state_broadcaster::StateBroadcaster,
    unit_of_work::UnitOfWork,
};
use janus_models::interface::{
    AttemptType, ChatMessage, ChatRole, CompletedToolCall, ContentPart, ModelRequest,
    ModelStreamEvent, ModelsInterface, StreamChannel, ToolSpec,
};
use janus_projects::interface::ProjectsInterface;
use janus_sessions::interface::{
    ActiveTurnOutcome, AppendAssistantMessage, AppendToolResultInput, ExecutionTurn,
    ModelAttemptStatus, SessionsInterface, TurnModelAttempt, TurnStatus, TurnTokenExchange,
};
use janus_workspace::interface::WorkspaceInterface;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{SqliteConnection, SqlitePool};

use super::context::SYSTEM_PROMPT;
pub use super::context::{
    AUTO_COMPACT_THRESHOLD_PERCENT, DEFAULT_CONTEXT_LIMIT, ScheduleCompactInput,
    context_usage_near_limit, latest_compact_summary, record_context_version,
    record_context_version_in_tx, schedule_compact, schedule_compact_in_tx,
};
use super::registry::{SCHEMA_VERSION, available_tools};
use super::retry::{FaultClass, classify};
use super::tools::{
    ToolContext, attach_tool_display, execute_tool, read_attachment_bytes, supported_image_mime,
};
pub use super::types::{
    CompletionSummary, ExecutionError, ToolCallStatus, ToolExecutionDisposition, ToolOutcome,
    ToolResultPart, TurnExecutionOutcome,
};

#[derive(Debug, Clone, Serialize)]
pub struct ContextUsageView {
    pub estimated_input_tokens: i64,
    pub context_limit: i64,
    pub compact_status: String,
    pub created_at: String,
}

fn estimated_system_prompt_tokens() -> i64 {
    i64::try_from(SYSTEM_PROMPT.len().saturating_add(3) / 4).unwrap_or(i64::MAX)
}

/// Aggregate the durable model ledger into the one value exposed for a Turn.
/// Ledger input is already provider-cache-free; only Janus's system prefix is
/// removed here, once per model attempt. Failed attempts with reported usage
/// remain part of the Turn exchange because the Owner paid for that request.
fn aggregate_turn_token_exchange(
    rows: &[(i64, i64)],
    system_prompt_tokens: i64,
) -> TurnTokenExchange {
    let upload_tokens = rows.iter().fold(0_i64, |total, (input, _)| {
        total.saturating_add(input.saturating_sub(system_prompt_tokens).max(0))
    });
    let download_tokens = rows
        .iter()
        .fold(0_i64, |total, (_, output)| total.saturating_add(*output));
    TurnTokenExchange {
        upload_tokens,
        download_tokens,
    }
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
    /// Raw provider reasoning to echo back verbatim on the next request; the
    /// display-formatted `reasoning` must not be used for echo-back.
    reasoning_content: Option<&'a str>,
    reasoning_duration_ms: Option<u64>,
    tool_calls: &'a [CompletedToolCall],
    actor: &'a Value,
}

struct ExecutedToolCall {
    outcome: ToolOutcome,
    message: ChatMessage,
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
            reasoning_content: None,
        },
        Value::Array(durable_parts),
    )
}
