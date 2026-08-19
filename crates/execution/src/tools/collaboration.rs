//! Cross-session coordination and persistent project memory tools.

use super::*;
use janus_infrastructure::id::SessionId;
use serde_json::Value;
use std::str::FromStr;

pub(super) async fn tool_active_sessions(
    ctx: &ToolContext<'_>,
) -> Result<ToolOutcome, ExecutionError> {
    let sessions = ctx.sessions.active_sessions(ctx.project_id, 100).await?;
    let summary = json!({
        "sessions": sessions,
        "async_tasks": ctx.runtime.async_tasks(200).await?,
        "current_session_id": ctx.session_id.to_string(),
    });
    Ok(json_outcome(summary))
}

pub(super) async fn tool_read_session(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    let raw_id = input
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ExecutionError::Internal(anyhow::anyhow!("session_id required")))?;
    let session_id = SessionId::from_str(raw_id).map_err(|error| {
        ExecutionError::Internal(anyhow::anyhow!("invalid session_id: {error}"))
    })?;
    let session = ctx.sessions.get_session(session_id).await?;
    // Sessions are project-scoped. Reading another project's session (or its
    // timeline) would leak cross-project user content into this Turn's
    // context, so treat it as not found unless it belongs to ctx.project_id.
    if session.project_id != ctx.project_id.to_string() {
        return Ok(super::fail_text(
            "session not found in this project",
            "NOT_FOUND",
        ));
    }
    let limit = input
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(50)
        .clamp(1, 100);
    let timeline = ctx.sessions.timeline(session_id, None, None, limit).await?;
    let summary = json!({
        "session": session,
        "timeline": timeline,
    });
    Ok(json_outcome(summary))
}

pub(super) async fn tool_memory(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("list");
    let summary =
        match action {
            "list" => json!({
                "action": action,
                "memories": ctx.projects.list_memories(ctx.project_id).await?,
            }),
            "set" => {
                let key = input.get("key").and_then(Value::as_str).ok_or_else(|| {
                    ExecutionError::Internal(anyhow::anyhow!("memory key required"))
                })?;
                let content = input
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        ExecutionError::Internal(anyhow::anyhow!("memory content required"))
                    })?;
                json!({
                    "action": action,
                    "memory": ctx.projects.set_memory(ctx.project_id, key, content).await?,
                })
            }
            "delete" => {
                let key = input.get("key").and_then(Value::as_str).ok_or_else(|| {
                    ExecutionError::Internal(anyhow::anyhow!("memory key required"))
                })?;
                json!({
                    "action": action,
                    "key": key,
                    "deleted": ctx.projects.delete_memory(ctx.project_id, key).await?,
                })
            }
            other => {
                return Ok(super::fail_text(
                    &format!("unknown memory action: {other}"),
                    "VALIDATION_FAILED",
                ));
            }
        };
    Ok(json_outcome(summary))
}

fn json_outcome(summary: Value) -> ToolOutcome {
    ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Json {
            value: summary.clone(),
        }],
        summary,
        error_code: None,
        finish_summary: None,
    }
}
