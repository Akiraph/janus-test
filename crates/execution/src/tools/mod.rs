//! Execute registered tools against the Main workspace.

//! The tool surface is split by domain into `display` (outcome display),
//! `file` (file and attachment tools), and `runtime` (bash/process tools);
//! the registry and dispatch (`execute_tool`) stay here.

mod collaboration;
mod display;
mod file;
mod runtime;

use collaboration::{tool_active_sessions, tool_memory, tool_read_session};
pub(crate) use display::attach_tool_display;
pub(crate) use file::{read_attachment_bytes, supported_image_mime};
use file::{
    tool_attachment_list, tool_attachment_read, tool_attachment_save, tool_edit, tool_read,
    tool_remove, tool_write,
};
use runtime::{tool_bash, tool_read_output, tool_stop};

use std::{collections::HashSet, path::Path};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use janus_infrastructure::id::{AttachmentId, ProjectId, SessionId, ToolCallId, TurnId};
use janus_infrastructure::managed_storage::BlobStore;
use janus_infrastructure::shell::{bash_program, decode_process_output};
use janus_projects::interface::ProjectsInterface;
use janus_sessions::interface::{AttachmentResource, SessionsInterface};
use janus_workspace::interface::PathError;
use janus_workspace::interface::{
    DiffLineKind, FileMutation, WorkspaceHandle, WorkspaceInterface, line_hunks,
};

use super::paths::{normalize_workspace_path, resolve_workspace_path};
use super::registry::is_registered;
use super::types::{
    ExecutionError, ToolDisplay, ToolDisplayBody, ToolExecutionDisposition, ToolOutcome,
    ToolResultPart,
};

/// Hard image decode limits (SES-TOOL-READ-03 subset).
const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;
/// Largest text file `read` will return in one call. Above this the caller is
/// told the size and asked for an `offset`/`limit` slice.
const MAX_TEXT_BYTES: usize = 10 * 1024 * 1024;
const MAX_EDGE_PX: u32 = 32_768;
const MAX_PIXELS: u64 = 100_000_000;
const MAX_ATTACHMENT_TEXT_BYTES: usize = 256 * 1024;

pub struct ToolContext<'a> {
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub tool_call_id: ToolCallId,
    pub workspace: &'a WorkspaceInterface,
    pub workspace_root: &'a Path,
    pub workspace_handle: WorkspaceHandle,
    pub sessions: &'a SessionsInterface,
    pub projects: &'a ProjectsInterface,
    pub blobs: &'a BlobStore,
    pub runtime: &'a janus_runtime::interface::RuntimeInterface,
    /// Optional GitHub PAT for project automation. It is only passed into
    /// secret environment slots and must never be included in tool output.
    pub git_token: Option<&'a str>,
    pub read_paths: &'a HashSet<String>,
    pub actor: Value,
}

pub async fn execute_tool(
    ctx: &ToolContext<'_>,
    name: &str,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    if !is_registered(name) {
        let mut outcome = ToolOutcome {
            disposition: ToolExecutionDisposition::Failed,
            parts: vec![ToolResultPart::Text {
                text: format!("tool not allowed: {name}"),
            }],
            summary: json!({"error": "TOOL_NOT_ALLOWED", "name": name}),
            error_code: Some("TOOL_NOT_ALLOWED".into()),
            finish_summary: None,
        };
        attach_tool_display(name, input, &mut outcome);
        return Ok(outcome);
    }

    let handle = ctx.workspace_handle.clone();
    let repo = ctx.workspace_root.to_path_buf();

    let mut outcome = match name {
        "read" => tool_read(&repo, input, &handle, ctx).await,
        "write" => tool_write(ctx, &handle, input).await,
        "edit" => tool_edit(ctx, &handle, input).await,
        "delete" => tool_remove(ctx, &handle, input).await,
        "bash" => tool_bash(ctx, input).await,
        "read_output" => tool_read_output(ctx, input).await,
        "stop" => tool_stop(ctx, input).await,
        "active_sessions" => tool_active_sessions(ctx).await,
        "read_session" => tool_read_session(ctx, input).await,
        "memory" => tool_memory(ctx, input).await,
        "todo" => tool_todo(input).await,
        "attachment_list" => tool_attachment_list(ctx).await,
        "attachment_read" => tool_attachment_read(ctx, input).await,
        "attachment_save" => tool_attachment_save(ctx, &handle, input).await,
        other => Ok(ToolOutcome {
            disposition: ToolExecutionDisposition::Failed,
            parts: vec![ToolResultPart::Text {
                text: format!("unknown tool: {other}"),
            }],
            summary: json!({"error": "TOOL_NOT_ALLOWED"}),
            error_code: Some("TOOL_NOT_ALLOWED".into()),
            finish_summary: None,
        }),
    }?;
    attach_tool_display(name, input, &mut outcome);
    Ok(outcome)
}
fn fail_text(msg: &str, code: &str) -> ToolOutcome {
    ToolOutcome {
        disposition: ToolExecutionDisposition::Failed,
        parts: vec![ToolResultPart::Text {
            text: msg.to_owned(),
        }],
        summary: json!({"error": code, "detail": msg}),
        error_code: Some(code.into()),
        finish_summary: None,
    }
}

fn map_path_err(error: PathError) -> ToolOutcome {
    // PathError already names the rule that was broken — traversal, not
    // relative, empty/device name, NUL. Collapsing all four into "invalid path"
    // left the caller guessing which one, so the rejected path was retried
    // unchanged. The stable error code is unchanged.
    fail_text(&error.to_string(), "TOOL_PATH_INVALID")
}

async fn tool_todo(input: &Value) -> Result<ToolOutcome, ExecutionError> {
    use janus_infrastructure::id::TimelineItemId;

    let todos = input.get("todos").cloned().unwrap_or(json!([]));
    let evidence = input.get("evidence").cloned().unwrap_or(json!([]));
    let plan_id = format!("pln_{}", TimelineItemId::new());
    let summary = json!({
        "plan": {
            "id": plan_id,
            "todos": todos,
            "evidence": evidence,
        },
    });
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Json {
            value: summary.clone(),
        }],
        summary,
        error_code: None,
        finish_summary: None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::attach_tool_display;
    use super::runtime::truncate_tool_text;
    use crate::{
        registry::available_tools,
        types::{ToolExecutionDisposition, ToolOutcome, ToolResultPart},
    };

    #[test]
    fn tool_text_truncation_preserves_utf8() {
        let (text, truncated) = truncate_tool_text("你好世界", 5);
        assert!(truncated);
        assert_eq!(text, "你...[truncated]");
    }

    #[test]
    fn every_registered_tool_gets_a_versioned_display() {
        let input = json!({
            "path": "src/main.rs",
            "command": "echo hello",
            "task_id": "task-1",
            "attachment_id": "attachment-1",
        });
        for tool in available_tools(true) {
            let mut outcome = ToolOutcome {
                disposition: ToolExecutionDisposition::Succeeded,
                parts: vec![ToolResultPart::Text { text: "ok".into() }],
                summary: json!({"stdout": "ok", "mode": "sync"}),
                error_code: None,
                finish_summary: None,
            };
            attach_tool_display(tool.name, &input, &mut outcome);
            let display = &outcome.summary["display"];
            assert_eq!(display["version"], 1, "{} display version", tool.name);
            assert!(
                display["title"]
                    .as_str()
                    .is_some_and(|title| !title.is_empty()),
                "{} display title",
                tool.name
            );
            assert!(
                display["body"]["kind"].is_string(),
                "{} display body",
                tool.name
            );
        }
    }
}
