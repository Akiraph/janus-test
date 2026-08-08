//! Execute registered tools against a Session workspace.

//! The tool surface is split by domain into `display` (outcome display),
//! `file` (file and attachment tools), and `runtime` (bash/process tools);
//! the registry and dispatch (`execute_tool`) stay here.

mod display;
mod file;
mod runtime;

pub(crate) use display::attach_tool_display;
use file::{
    tool_attachment_list, tool_attachment_read, tool_attachment_save, tool_edit, tool_read,
    tool_remove, tool_write,
};
pub(crate) use file::{read_attachment_bytes, supported_image_mime};
use runtime::{tool_bash, tool_delegate_cli, tool_read_output, tool_stop};

use std::{collections::HashSet, path::Path};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use janus_infrastructure::id::{AskId, AttachmentId, SessionId, ToolCallId, TurnId};
use janus_infrastructure::managed_storage::BlobStore;
use janus_infrastructure::shell::{bash_program, bash_search_path, decode_process_output};
use janus_sessions::interface::{AttachmentResource, SessionsInterface};
use janus_workspace::interface::PathError;
use janus_workspace::interface::{
    DiffLineKind, FileMutation, WorkspaceHandle, WorkspaceInterface, line_hunks,
};

use super::paths::{normalize_session_path, resolve_session_path};
use super::registry::{is_forbidden_tool, is_registered};
use super::types::{
    AskMode, AskRequest, ExecutionError, ToolDisplay, ToolDisplayBody, ToolExecutionDisposition,
    ToolOutcome, ToolResultPart, TurnWait,
};

/// Hard image decode limits (SES-TOOL-READ-03 subset).
const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_EDGE_PX: u32 = 32_768;
const MAX_PIXELS: u64 = 100_000_000;
const MAX_ATTACHMENT_TEXT_BYTES: usize = 256 * 1024;

pub struct ToolContext<'a> {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub tool_call_id: ToolCallId,
    pub workspace: &'a WorkspaceInterface,
    pub sessions: &'a SessionsInterface,
    pub blobs: &'a BlobStore,
    pub runtime: &'a janus_runtime::interface::RuntimeInterface,
    pub read_paths: &'a HashSet<String>,
    pub actor: Value,
}

pub async fn execute_tool(
    ctx: &ToolContext<'_>,
    name: &str,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    if is_forbidden_tool(name) || !is_registered(name) {
        let mut outcome = ToolOutcome {
            disposition: ToolExecutionDisposition::Failed,
            parts: vec![ToolResultPart::Text {
                text: format!("tool not allowed: {name}"),
            }],
            summary: json!({"error": "TOOL_NOT_ALLOWED", "name": name}),
            error_code: Some("TOOL_NOT_ALLOWED".into()),
            finish_summary: None,
            wait: None,
        };
        attach_tool_display(name, input, &mut outcome);
        return Ok(outcome);
    }

    let handle = WorkspaceHandle::session(ctx.session_id);
    let repo = session_repo(ctx.workspace, ctx.session_id)?;

    let mut outcome = match name {
        "read" => tool_read(&repo, input, &handle, ctx).await,
        "write" => tool_write(ctx, &handle, input).await,
        "edit" => tool_edit(ctx, &handle, input).await,
        "delete" => tool_remove(ctx, &handle, input).await,
        "bash" => tool_bash(ctx, input).await,
        "delegate_cli" => tool_delegate_cli(ctx, input).await,
        "read_output" => tool_read_output(ctx, input).await,
        "stop" => tool_stop(ctx, input).await,
        "todo" => tool_todo(input).await,
        "ask_user" => tool_ask_user(ctx, input).await,
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
            wait: None,
        }),
    }?;
    attach_tool_display(name, input, &mut outcome);
    Ok(outcome)
}
fn session_repo(
    workspace: &WorkspaceInterface,
    session_id: SessionId,
) -> Result<std::path::PathBuf, ExecutionError> {
    Ok(workspace.session_repo_path(session_id))
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
        wait: None,
    }
}

fn map_path_err(_error: PathError) -> ToolOutcome {
    fail_text("invalid path", "TOOL_PATH_INVALID")
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
        wait: None,
    })
}

async fn tool_ask_user(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    use janus_infrastructure::clock::{format_utc, now_utc};

    let prompt = input
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    if prompt.trim().is_empty() {
        return Ok(fail_text("prompt is required", "VALIDATION_FAILED"));
    }
    let requested_mode = input.get("mode").and_then(Value::as_str);
    let mode = match requested_mode.unwrap_or("blocking") {
        "blocking" => AskMode::Blocking,
        "non_blocking" | "best_effort" => AskMode::NonBlocking,
        _ => {
            return Ok(fail_text(
                "mode must be blocking|non_blocking",
                "VALIDATION_FAILED",
            ));
        }
    };
    let choices = input.get("choices").cloned().unwrap_or(json!([]));
    let multiple = input
        .get("multiple")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // `default` is read only for the legacy mode so new non-blocking asks
    // cannot preselect the answer that the model is supposed to judge later.
    let default = (requested_mode == Some("best_effort"))
        .then(|| input.get("default").cloned())
        .flatten();
    let expires_in_ms = match input.get("expires_in_ms") {
        None => None,
        Some(value) => match value.as_u64().filter(|value| *value > 0) {
            Some(value) => Some(value),
            None => {
                return Ok(fail_text(
                    "expires_in_ms must be a positive integer",
                    "VALIDATION_FAILED",
                ));
            }
        },
    };
    if mode == AskMode::NonBlocking && expires_in_ms.is_none() {
        return Ok(fail_text(
            "non_blocking requires expires_in_ms",
            "VALIDATION_FAILED",
        ));
    }
    let expires_at = match expires_in_ms {
        Some(milliseconds) => {
            let milliseconds = match i64::try_from(milliseconds) {
                Ok(value) => value,
                Err(_) => {
                    return Ok(fail_text("expires_in_ms is too large", "VALIDATION_FAILED"));
                }
            };
            Some(format_utc(
                now_utc() + chrono::Duration::milliseconds(milliseconds),
            ))
        }
        None => None,
    };
    let ask_id = AskId::new();
    let summary = json!({
        "ask_id": ask_id.to_string(),
        "mode": mode.as_str(),
        "prompt": prompt,
        "choices": choices.clone(),
        "multiple": multiple,
        "expires_at": expires_at,
    });
    let request = AskRequest {
        id: ask_id,
        turn_id: ctx.turn_id,
        tool_call_id: ctx.tool_call_id,
        mode,
        prompt: json!({"text": prompt}),
        choices,
        multiple,
        default,
        expires_at,
    };
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Waiting,
        parts: vec![ToolResultPart::Text {
            text: format!("ask_user ({}): {prompt}", mode.as_str()),
        }],
        summary,
        error_code: None,
        finish_summary: None,
        wait: Some(TurnWait::ask(request)),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::attach_tool_display;
    use super::runtime::{
        normalize_workspace_command, sanitize_workspace_text, truncate_tool_text,
        validate_workspace_command,
    };
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
            "cli": "codex",
            "job_id": "job-1",
            "prompt": "Continue?",
            "attachment_id": "attachment-1",
        });
        for tool in available_tools(true) {
            let mut outcome = ToolOutcome {
                disposition: ToolExecutionDisposition::Succeeded,
                parts: vec![ToolResultPart::Text { text: "ok".into() }],
                summary: json!({"stdout": "ok", "mode": "sync"}),
                error_code: None,
                finish_summary: None,
                wait: None,
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

    #[test]
    fn workspace_command_guard_allows_read_only_compounds() {
        assert!(validate_workspace_command("pwd && echo --- && ls -la").is_ok());
        assert!(validate_workspace_command("git status && git diff -- README.md").is_ok());
        assert!(validate_workspace_command("cat /workspace/src/main.rs").is_ok());
        assert!(validate_workspace_command("pwd 2>/dev/null").is_ok());
        assert!(
            validate_workspace_command(
                r#"ls -- '%SystemDrive%/ProgramData/Microsoft/Windows/Caches/cversions.2.db'"#
            )
            .is_ok()
        );
        assert!(validate_workspace_command(
            r#"cd /workspace && grep -rn -i -e "gradio" --include="*.md" . 2>/dev/null | head -40"#
        )
        .is_ok());
        assert!(
            validate_workspace_command(
                r#"echo "references to vba / zxxk / gradio" && grep -rn . 2>/dev/null"#
            )
            .is_ok()
        );
        assert!(
            validate_workspace_command(r#"echo "$ORIGINAL_PATH" | tr ':' '\n' | grep -i janus"#)
                .is_ok()
        );
        assert_eq!(
            normalize_workspace_command("cat /workspace/src/main.rs", "."),
            "cat ./src/main.rs"
        );
        assert_eq!(
            normalize_workspace_command("cat /workspace/src/main.rs", "../.."),
            "cat ../../src/main.rs"
        );
    }

    #[test]
    fn workspace_command_guard_rejects_escape_vectors() {
        assert_eq!(
            validate_workspace_command("cat /etc/passwd")
                .expect_err("absolute host path must be rejected"),
            "Bash blocked: absolute path `/etc/passwd` is outside the Session workspace; use `/workspace/...` or a workspace-relative path.",
        );
        assert_eq!(
            validate_workspace_command("cat /workspace/../outside")
                .expect_err("workspace traversal must be rejected"),
            "Bash blocked: path `/workspace/../outside` traverses outside the Session workspace; remove `..` and use a workspace-relative path.",
        );
        for command in [
            "cd ..",
            "cat ../secrets.txt",
            r#"type C:\Users\Administrator\secret.txt"#,
            "cat /c/Windows/System32/drivers/etc/hosts",
        ] {
            assert!(
                validate_workspace_command(command).is_err(),
                "expected rejection for {command:?}"
            );
        }
    }

    #[test]
    fn workspace_output_redacts_paths_and_environment_lines() {
        let root = std::path::Path::new(r"C:\workspace\session");
        let output = sanitize_workspace_text(
            "PWD=C:\\workspace\\session\\src\nPATH=C:\\Windows\\System32\nC:\\workspace\\session\\src\\main.rs\n",
            root,
        );
        assert!(!output.contains(r"C:\workspace\session"));
        assert!(output.contains("PWD=[redacted]"));
        assert!(output.contains("PATH=[redacted]"));
        assert!(output.contains("/workspace/src/main.rs"));
    }

    #[test]
    fn workspace_output_hides_runtime_host_details() {
        let root = std::path::Path::new(r"C:\\workspace\\session");
        let output = sanitize_workspace_text(
            "ORIGINAL_PATH=C:\\host\\bin\nUSERNAME=CodexSandboxOffline\nwin-host\\codexsandboxoffline\n",
            root,
        );
        assert!(output.contains("ORIGINAL_PATH=[redacted]"));
        assert!(output.contains("USERNAME=Janus"));
        assert!(output.contains("Janus\n"));
        assert!(!output.contains("Codex"));
    }

    #[test]
    fn workspace_output_hides_git_bash_host_details() {
        let root = std::path::Path::new(r"C:\\workspace\\session");
        let output = sanitize_workspace_text(
            "MINGW64_NT-10.0-22631 WIN-20260424EMY 3.6.7-fb42d713.x86_64 2026-03-29 11:44 UTC x86_64 Msys\n/c/Users/Administrator/AppData/Local/Microsoft/WindowsApps/python3\n/c/Python314/python\nwhich: /d/Tools/python.exe\n/c/Program Files/nodejs/node\n/mingw64/bin/git\nsrc/c/project.py\n/workspace/src/main.rs\n",
            root,
        );
        assert!(!output.contains("MINGW64_NT-10.0-22631"));
        assert!(!output.contains("/c/Users/Administrator"));
        assert!(!output.contains("/c/Python314"));
        assert!(!output.contains("/d/Tools/python.exe"));
        assert!(!output.contains("/mingw64/bin/git"));
        assert!(output.contains("src/c/project.py"));
        assert!(output.contains("/workspace/src/main.rs"));
    }

    #[test]
    fn workspace_output_hides_janus_runtime_artifacts() {
        let root = std::path::Path::new(r"C:\workspace\session");
        let output = sanitize_workspace_text(
            ".janus-runtime-nRQ23w-V9JsiFp-N.pid\n.janus-tmp\n/workspace/.janus-tmp/output.log\n",
            root,
        );

        assert!(!output.contains(".janus-runtime-"));
        assert!(!output.contains(".janus-tmp"));
    }
}
