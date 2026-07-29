//! Execute registered Supervisor tools against a Session workspace.

use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::adapters::git::{GitRunner, SystemGit};
use crate::modules::workspace_sync::interface::{
    FileMutation, WorkspaceHandle, WorkspaceSyncInterface,
};
use crate::platform::id::{AskId, SessionId, ToolCallId, TurnId};
use crate::platform::path::PathError;

use super::paths::resolve_session_path;
use super::registry::{is_forbidden_tool, is_registered};
use super::types::{
    AskMode, AskRequest, CompletionSummary, SupervisorError, ToolExecutionDisposition, ToolOutcome,
    ToolResultPart, TurnWait,
};

/// Hard image decode limits (SES-TOOL-READ-03 subset).
const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_EDGE_PX: u32 = 32_768;
const MAX_PIXELS: u64 = 100_000_000;

pub struct ToolContext<'a> {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub tool_call_id: ToolCallId,
    pub workspace: &'a WorkspaceSyncInterface,
    pub runtime: Option<&'a crate::modules::runtime::interface::RuntimeInterface>,
    pub pool: &'a sqlx::SqlitePool,
    pub actor: Value,
}

pub async fn execute_tool(
    ctx: &ToolContext<'_>,
    name: &str,
    input: &Value,
) -> Result<ToolOutcome, SupervisorError> {
    if is_forbidden_tool(name) || !is_registered(name) {
        return Ok(ToolOutcome {
            disposition: ToolExecutionDisposition::Failed,
            parts: vec![ToolResultPart::Text {
                text: format!("tool not allowed: {name}"),
            }],
            summary: json!({"error": "TOOL_NOT_ALLOWED", "name": name}),
            error_code: Some("TOOL_NOT_ALLOWED".into()),
            finish_summary: None,
            wait: None,
        });
    }

    let handle = WorkspaceHandle::session(ctx.session_id);
    let repo = session_repo(ctx.workspace, ctx.session_id)?;

    match name {
        "fs.list" => tool_list(&repo, input).await,
        "fs.read" => tool_read(&repo, input, &handle, ctx).await,
        "fs.write" => tool_write(ctx, &handle, input, false).await,
        "fs.patch" => tool_write(ctx, &handle, input, true).await,
        "fs.remove" => tool_remove(ctx, &handle, input).await,
        "git.inspect" => tool_git_status(&repo).await,
        "finish" => tool_finish_checked(ctx, input).await,
        "bash" => tool_bash(ctx, input).await,
        "job" => tool_job(ctx, input).await,
        "service" => tool_service(ctx, input).await,
        "delegate_cli" => tool_delegate_cli(ctx, input).await,
        "update_plan" => tool_update_plan(ctx, input).await,
        "ask_user" => tool_ask_user(ctx, input).await,
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
    }
}

fn session_repo(
    workspace: &WorkspaceSyncInterface,
    session_id: SessionId,
) -> Result<std::path::PathBuf, SupervisorError> {
    Ok(workspace.session_repo_path(session_id))
}

async fn tool_list(repo: &Path, input: &Value) -> Result<ToolOutcome, SupervisorError> {
    let raw = input.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let dir = match resolve_session_path(repo, raw) {
        Ok(p) => p,
        Err(_) => return path_invalid(),
    };
    if !dir.exists() {
        return Ok(fail_text("path not found", "TOOL_PATH_INVALID"));
    }
    if !dir.is_dir() {
        return Ok(fail_text("not a directory", "TOOL_PATH_INVALID"));
    }
    let mut entries = Vec::new();
    let mut rd = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| SupervisorError::Internal(anyhow::anyhow!(e)))?;
    while let Some(ent) = rd
        .next_entry()
        .await
        .map_err(|e| SupervisorError::Internal(anyhow::anyhow!(e)))?
    {
        let name = ent.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let meta = ent
            .metadata()
            .await
            .map_err(|e| SupervisorError::Internal(anyhow::anyhow!(e)))?;
        entries.push(json!({
            "name": name,
            "kind": if meta.is_dir() { "dir" } else { "file" },
            "size": meta.len(),
        }));
    }
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Json {
            value: json!({"entries": entries}),
        }],
        summary: json!({"count": entries.len()}),
        error_code: None,
        finish_summary: None,
        wait: None,
    })
}

fn path_invalid() -> Result<ToolOutcome, SupervisorError> {
    Ok(fail_text("invalid path", "TOOL_PATH_INVALID"))
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

fn map_path_err(e: PathError) -> ToolOutcome {
    let _ = e;
    fail_text("invalid path", "TOOL_PATH_INVALID")
}

async fn tool_read(
    repo: &Path,
    input: &Value,
    handle: &WorkspaceHandle,
    ctx: &ToolContext<'_>,
) -> Result<ToolOutcome, SupervisorError> {
    let raw = input
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or_else(|| SupervisorError::ToolPathInvalid)?;
    let path = match resolve_session_path(repo, raw) {
        Ok(p) => p,
        Err(e) => return Ok(map_path_err(e)),
    };
    if !path.is_file() {
        return Ok(fail_text("file not found", "TOOL_PATH_INVALID"));
    }
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| SupervisorError::Internal(anyhow::anyhow!(e)))?;
    if meta.len() > MAX_IMAGE_BYTES {
        // Still allow large text? Cap text at 10MiB for safety.
        if meta.len() > 10 * 1024 * 1024 {
            return Ok(fail_text("file too large", "IMAGE_TOO_LARGE"));
        }
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| SupervisorError::Internal(anyhow::anyhow!(e)))?;

    if let Some(img) = sniff_image(&bytes) {
        return read_image(raw, &bytes, img, handle, ctx).await;
    }

    // Text: require UTF-8 round-trip.
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(_) => {
            return Ok(fail_text(
                "binary file is not a supported image",
                "UNSUPPORTED_IMAGE",
            ));
        }
    };
    if text.contains('\0') {
        return Ok(fail_text("binary content", "UNSUPPORTED_IMAGE"));
    }
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Text { text: text.clone() }],
        summary: json!({"path": raw, "kind": "text", "bytes": text.len()}),
        error_code: None,
        finish_summary: None,
        wait: None,
    })
}

#[derive(Clone, Copy)]
enum ImageKind {
    Png,
    Jpeg,
    Webp,
    Gif,
}

fn sniff_image(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
        return Some(ImageKind::Png);
    }
    if bytes.len() >= 3 && &bytes[..3] == b"\xff\xd8\xff" {
        return Some(ImageKind::Jpeg);
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(ImageKind::Webp);
    }
    if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        return Some(ImageKind::Gif);
    }
    None
}

fn mime_of(kind: ImageKind) -> &'static str {
    match kind {
        ImageKind::Png => "image/png",
        ImageKind::Jpeg => "image/jpeg",
        ImageKind::Webp => "image/webp",
        ImageKind::Gif => "image/gif",
    }
}

/// Minimal dimension probe without full pixel decode where possible.
fn probe_dimensions(bytes: &[u8], kind: ImageKind) -> Result<(u32, u32), SupervisorError> {
    match kind {
        ImageKind::Png if bytes.len() >= 24 => {
            let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
            let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
            Ok((w, h))
        }
        ImageKind::Png => Err(SupervisorError::UnsupportedImage),
        ImageKind::Gif if bytes.len() >= 10 => {
            let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
            let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
            Ok((w, h))
        }
        ImageKind::Gif => Err(SupervisorError::UnsupportedImage),
        ImageKind::Jpeg => jpeg_dimensions(bytes).ok_or(SupervisorError::UnsupportedImage),
        ImageKind::Webp => webp_dimensions(bytes).ok_or(SupervisorError::UnsupportedImage),
    }
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        if marker == 0xD8 || marker == 0xD9 || marker == 0x01 {
            i += 2;
            continue;
        }
        if i + 4 >= bytes.len() {
            break;
        }
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        // SOF0..SOF3
        if (0xC0..=0xC3).contains(&marker) && i + 9 < bytes.len() {
            let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // VP8X extended: bytes 12..16 'VP8X', width/height at 24..30 (24-bit little-endian, minus 1)
    if bytes.len() >= 30 && &bytes[12..16] == b"VP8X" {
        let w = 1 + u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]);
        let h = 1 + u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]);
        return Some((w, h));
    }
    // VP8 lossy: 'VP8 ' at 12
    if bytes.len() >= 30 && &bytes[12..16] == b"VP8 " {
        // frame tag + start code then 16-bit width/height at offset 26
        let w = u16::from_le_bytes([bytes[26], bytes[27]]) as u32 & 0x3FFF;
        let h = u16::from_le_bytes([bytes[28], bytes[29]]) as u32 & 0x3FFF;
        return Some((w, h));
    }
    None
}

fn gif_is_animated(bytes: &[u8]) -> bool {
    // Count image descriptors (0x2C). >1 => animated.
    let mut count = 0u32;
    let mut i = 13usize; // skip header + logical screen
    while i < bytes.len() {
        match bytes[i] {
            0x3B => break, // trailer
            0x21 => {
                // extension
                if i + 2 >= bytes.len() {
                    break;
                }
                i += 2;
                while i < bytes.len() {
                    let sz = bytes[i] as usize;
                    i += 1;
                    if sz == 0 {
                        break;
                    }
                    i += sz;
                }
            }
            0x2C => {
                count += 1;
                if count > 1 {
                    return true;
                }
                if i + 10 >= bytes.len() {
                    break;
                }
                i += 10;
                let packed = bytes[i - 1];
                if packed & 0x80 != 0 {
                    let n = packed & 0x07;
                    i += 3 * (1 << (n + 1));
                }
                if i >= bytes.len() {
                    break;
                }
                i += 1; // LZW min code size
                while i < bytes.len() {
                    let sz = bytes[i] as usize;
                    i += 1;
                    if sz == 0 {
                        break;
                    }
                    i += sz;
                }
            }
            _ => i += 1,
        }
    }
    false
}

async fn read_image(
    path: &str,
    bytes: &[u8],
    kind: ImageKind,
    handle: &WorkspaceHandle,
    ctx: &ToolContext<'_>,
) -> Result<ToolOutcome, SupervisorError> {
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Ok(fail_text("image exceeds 50MiB", "IMAGE_TOO_LARGE"));
    }
    if matches!(kind, ImageKind::Gif) && gif_is_animated(bytes) {
        return Ok(fail_text("animated GIF not supported", "UNSUPPORTED_IMAGE"));
    }
    let (w, h) = match probe_dimensions(bytes, kind) {
        Ok(d) => d,
        Err(_) => return Ok(fail_text("cannot probe image", "UNSUPPORTED_IMAGE")),
    };
    if w == 0 || h == 0 || w > MAX_EDGE_PX || h > MAX_EDGE_PX {
        return Ok(fail_text("image dimensions invalid", "IMAGE_TOO_LARGE"));
    }
    let pixels = (w as u64).saturating_mul(h as u64);
    if pixels > MAX_PIXELS {
        return Ok(fail_text("image exceeds 100MP", "IMAGE_TOO_LARGE"));
    }

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let content_hash = hex::encode(hasher.finalize());
    let revision = ctx
        .workspace
        .current_revision(handle)
        .await
        .ok()
        .map(|r| r.0);

    // Summary never includes Base64 — only references.
    let summary = json!({
        "path": path,
        "kind": "image",
        "mime": mime_of(kind),
        "width": w,
        "height": h,
        "content_hash": content_hash,
        "derived": false,
        "content_revision": revision,
        "bytes": bytes.len(),
    });

    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![
            ToolResultPart::Image {
                mime: mime_of(kind).into(),
                bytes: bytes.to_vec(),
                width: w,
                height: h,
                path: path.into(),
                content_revision: revision,
                derived: false,
                content_hash: content_hash.clone(),
            },
            ToolResultPart::Text {
                text: format!("read image {path} ({w}x{h}, {})", mime_of(kind)),
            },
        ],
        summary,
        error_code: None,
        finish_summary: None,
        wait: None,
    })
}

async fn tool_write(
    ctx: &ToolContext<'_>,
    handle: &WorkspaceHandle,
    input: &Value,
    is_patch: bool,
) -> Result<ToolOutcome, SupervisorError> {
    let path = input
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or(SupervisorError::ToolPathInvalid)?;
    let content = input
        .get("content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| SupervisorError::Internal(anyhow::anyhow!("content required")))?;
    // Path validation only (mutation API re-validates).
    let _ = resolve_session_path(&session_repo(ctx.workspace, ctx.session_id)?, path)
        .map_err(|_| SupervisorError::ToolPathInvalid)?;

    let mutation = if is_patch {
        FileMutation::Patch {
            path: path.to_owned(),
            content: content.as_bytes().to_vec(),
        }
    } else {
        FileMutation::Write {
            path: path.to_owned(),
            content: content.as_bytes().to_vec(),
        }
    };
    let rev = ctx
        .workspace
        .apply_file_mutation(
            handle,
            mutation,
            None,
            if is_patch {
                "tool.fs.patch"
            } else {
                "tool.fs.write"
            },
            ctx.actor.clone(),
        )
        .await?;
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Text {
            text: format!("wrote {path} -> {}", rev.0),
        }],
        summary: json!({"path": path, "revision": rev.0}),
        error_code: None,
        finish_summary: None,
        wait: None,
    })
}

async fn tool_remove(
    ctx: &ToolContext<'_>,
    handle: &WorkspaceHandle,
    input: &Value,
) -> Result<ToolOutcome, SupervisorError> {
    let path = input
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or(SupervisorError::ToolPathInvalid)?;
    let _ = resolve_session_path(&session_repo(ctx.workspace, ctx.session_id)?, path)
        .map_err(|_| SupervisorError::ToolPathInvalid)?;
    let rev = ctx
        .workspace
        .apply_file_mutation(
            handle,
            FileMutation::Delete {
                path: path.to_owned(),
            },
            None,
            "tool.fs.remove",
            ctx.actor.clone(),
        )
        .await?;
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Text {
            text: format!("removed {path} -> {}", rev.0),
        }],
        summary: json!({"path": path, "revision": rev.0}),
        error_code: None,
        finish_summary: None,
        wait: None,
    })
}

async fn tool_git_status(repo: &Path) -> Result<ToolOutcome, SupervisorError> {
    match SystemGit.status(repo).await {
        Ok(st) => {
            let summary = json!({
                "head_sha": st.head_sha,
                "branch": st.branch,
                "ahead": st.ahead,
                "behind": st.behind,
                "working": st.working,
                "index": st.index,
                "untracked": st.untracked,
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
        Err(e) => Ok(fail_text(
            &format!("git status failed: {e}"),
            "TOOL_PATH_INVALID",
        )),
    }
}

fn tool_finish(input: &Value) -> Result<ToolOutcome, SupervisorError> {
    // Async Job-check variant is tool_finish_checked; this keeps the pure
    // summary path for callers that already verified no unfinished Jobs.
    let finish = CompletionSummary::from_tool_input(input);
    let summary_text = finish.summary.clone();
    let summary = serde_json::to_value(&finish)?;
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Text {
            text: format!("finished: {summary_text}"),
        }],
        summary,
        error_code: None,
        finish_summary: Some(finish),
        wait: None,
    })
}

async fn tool_finish_checked(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<ToolOutcome, SupervisorError> {
    let remaining = match ctx.runtime {
        Some(runtime) => runtime.unfinished_job_count(ctx.turn_id).await?,
        None => 0,
    };
    if remaining > 0 {
        let summary = json!({
            "waiting_for_job": true,
            "unfinished_jobs": remaining,
            "note": "finish deferred until finite Jobs settle",
        });
        return Ok(ToolOutcome {
            disposition: ToolExecutionDisposition::Succeeded,
            parts: vec![ToolResultPart::Text {
                text: format!("finish deferred: {remaining} unfinished job(s)"),
            }],
            summary,
            error_code: None,
            finish_summary: None,
            wait: Some(TurnWait::job()),
        });
    }
    tool_finish(input)
}

// ---------------------------------------------------------------------------
// Stage 5 runtime tools
// ---------------------------------------------------------------------------

fn default_limits(timeout_ms: u64) -> crate::modules::runtime::interface::ResourceLimits {
    crate::modules::runtime::interface::ResourceLimits {
        timeout_ms,
        memory_bytes: 256 * 1024 * 1024,
        cpu_millis: 1_000,
        pids: 64,
        temporary_disk_bytes: 128 * 1024 * 1024,
        open_files: 128,
    }
}

fn require_runtime<'a>(
    ctx: &'a ToolContext<'_>,
) -> Result<&'a crate::modules::runtime::interface::RuntimeInterface, SupervisorError> {
    ctx.runtime.ok_or_else(|| {
        SupervisorError::Internal(anyhow::anyhow!("runtime is not bound to this supervisor"))
    })
}

async fn ensure_session_runtime(
    ctx: &ToolContext<'_>,
) -> Result<crate::modules::runtime::interface::RuntimeProjection, SupervisorError> {
    use crate::modules::runtime::interface::{
        ExecutorKind, NetworkPolicy, RuntimeScope, RuntimeSpec,
    };
    use crate::platform::id::RuntimeId;

    let runtime = require_runtime(ctx)?;
    if let Ok(Some(existing)) = runtime
        .current_runtime(RuntimeScope::session(ctx.session_id))
        .await
    {
        return Ok(existing);
    }
    let workspace_root = session_repo(ctx.workspace, ctx.session_id)?;
    let abs = workspace_root.canonicalize().unwrap_or(workspace_root);
    let spec = RuntimeSpec::new(
        RuntimeId::new(),
        RuntimeScope::session(ctx.session_id),
        ExecutorKind::Local,
        abs,
        default_limits(30_000),
        NetworkPolicy::DenyAll,
    )
    .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("runtime spec: {e}")))?;
    runtime
        .ensure_runtime(&spec)
        .await
        .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("ensure_runtime: {e}")))
}

fn working_directory(
    input: &Value,
) -> Result<crate::modules::runtime::interface::RelativeWorkingDirectory, SupervisorError> {
    use crate::modules::runtime::interface::RelativeWorkingDirectory;
    let raw = input
        .get("working_directory")
        .and_then(|v| v.as_str())
        .unwrap_or(".");
    RelativeWorkingDirectory::new(raw)
        .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("working_directory: {e}")))
}

fn timeout_ms(input: &Value, default: u64) -> u64 {
    input
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

async fn tool_bash(ctx: &ToolContext<'_>, input: &Value) -> Result<ToolOutcome, SupervisorError> {
    use crate::modules::runtime::interface::{
        ExecutionEnvironment, ExecutionSpec, NetworkPolicy, ValidatedCommand,
    };
    use std::collections::BTreeMap;

    let command = input
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SupervisorError::Internal(anyhow::anyhow!("command required")))?;
    if command.trim().is_empty() {
        return Ok(fail_text("command is empty", "VALIDATION_FAILED"));
    }
    let runtime_proj = ensure_session_runtime(ctx).await?;
    let runtime = require_runtime(ctx)?;
    let timeout = timeout_ms(input, 30_000).min(120_000);
    let spec = ExecutionSpec::new(
        runtime_proj.id,
        working_directory(input)?,
        ValidatedCommand::shell(command)
            .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("command: {e}")))?,
        ExecutionEnvironment::new(BTreeMap::new(), vec![])
            .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("env: {e}")))?,
        default_limits(timeout),
        NetworkPolicy::DenyAll,
    )
    .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("execution: {e}")))?;

    let result = runtime
        .execute_sync(spec)
        .await
        .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("bash: {e}")))?;

    let stdout = if result.stdout.len() > 8_000 {
        format!("{}...[truncated]", &result.stdout[..8_000])
    } else {
        result.stdout.clone()
    };
    let stderr = if result.stderr.len() > 4_000 {
        format!("{}...[truncated]", &result.stderr[..4_000])
    } else {
        result.stderr.clone()
    };
    let exit_code = result.exit.exit_code;
    let ok = !result.timed_out && exit_code == Some(0);
    let summary = json!({
        "exit_code": exit_code,
        "timed_out": result.timed_out,
        "duration_ms": result.duration_ms,
        "truncated": result.truncated,
        "log_stream_id": result.log_stream_id.to_string(),
        "stdout_bytes": result.stdout.len(),
        "stderr_bytes": result.stderr.len(),
    });
    let text = format!(
        "exit={:?} timed_out={} duration_ms={}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        exit_code, result.timed_out, result.duration_ms, stdout, stderr
    );
    Ok(ToolOutcome {
        disposition: if ok {
            ToolExecutionDisposition::Succeeded
        } else {
            ToolExecutionDisposition::Failed
        },
        parts: vec![ToolResultPart::Text { text }],
        summary,
        error_code: if ok {
            None
        } else if result.timed_out {
            Some("COMMAND_TIMEOUT".into())
        } else {
            Some("COMMAND_FAILED".into())
        },
        finish_summary: None,
        wait: None,
    })
}

async fn tool_job(ctx: &ToolContext<'_>, input: &Value) -> Result<ToolOutcome, SupervisorError> {
    use crate::modules::runtime::interface::{
        ExecutionEnvironment, ExecutionSpec, JobSpec, NetworkPolicy, ValidatedCommand,
    };
    use crate::platform::id::JobId;
    use std::collections::BTreeMap;

    let command = input
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SupervisorError::Internal(anyhow::anyhow!("command required")))?;
    if command.trim().is_empty() {
        return Ok(fail_text("command is empty", "VALIDATION_FAILED"));
    }
    let runtime_proj = ensure_session_runtime(ctx).await?;
    let runtime = require_runtime(ctx)?;
    let timeout = timeout_ms(input, 300_000).min(3_600_000);
    let execution = ExecutionSpec::new(
        runtime_proj.id,
        working_directory(input)?,
        ValidatedCommand::shell(command)
            .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("command: {e}")))?,
        ExecutionEnvironment::new(BTreeMap::new(), vec![])
            .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("env: {e}")))?,
        default_limits(timeout),
        NetworkPolicy::DenyAll,
    )
    .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("execution: {e}")))?;
    let job_id = JobId::new();
    let spec = JobSpec::new(
        job_id,
        ctx.session_id,
        ctx.turn_id,
        ctx.tool_call_id,
        execution,
    )
    .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("job spec: {e}")))?;

    let job = runtime
        .start_job(spec)
        .await
        .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("start_job: {e}")))?;

    let _ = sqlx::query("UPDATE tool_calls SET job_id = ? WHERE id = ?")
        .bind(job.id.to_string())
        .bind(ctx.tool_call_id.to_string())
        .execute(ctx.pool)
        .await;

    let summary = json!({
        "job_id": job.id.to_string(),
        "status": format!("{:?}", job.status).to_ascii_lowercase(),
        "log_stream_id": job.log_stream_id.to_string(),
        "command_summary": job.command_summary,
    });
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Waiting,
        parts: vec![ToolResultPart::Text {
            text: format!("job {} started ({})", job.id, job.command_summary),
        }],
        summary,
        error_code: None,
        finish_summary: None,
        wait: Some(TurnWait::job()),
    })
}

async fn tool_service(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<ToolOutcome, SupervisorError> {
    use crate::modules::runtime::interface::{
        ExecutionEnvironment, ExecutionSpec, NetworkPolicy, ServiceImpact, ServiceSpec,
        ValidatedCommand,
    };
    use crate::platform::id::ServiceId;
    use std::collections::BTreeMap;

    let command = input
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SupervisorError::Internal(anyhow::anyhow!("command required")))?;
    if command.trim().is_empty() {
        return Ok(fail_text("command is empty", "VALIDATION_FAILED"));
    }
    let impact = match input.get("impact").and_then(|v| v.as_str()).unwrap_or("") {
        "read_only" => ServiceImpact::ReadOnly,
        "ignored_output" => ServiceImpact::IgnoredOutput,
        "source_writing" => ServiceImpact::SourceWriting,
        other => {
            return Ok(fail_text(
                &format!("impact must be read_only|ignored_output|source_writing, got {other:?}"),
                "VALIDATION_FAILED",
            ));
        }
    };

    let runtime_proj = ensure_session_runtime(ctx).await?;
    let runtime = require_runtime(ctx)?;
    // Services are long-lived; use a high timeout as a safety ceiling only.
    let timeout = timeout_ms(input, 3_600_000).min(86_400_000);
    let execution = ExecutionSpec::new(
        runtime_proj.id,
        working_directory(input)?,
        ValidatedCommand::shell(command)
            .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("command: {e}")))?,
        ExecutionEnvironment::new(BTreeMap::new(), vec![])
            .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("env: {e}")))?,
        default_limits(timeout),
        NetworkPolicy::DenyAll,
    )
    .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("execution: {e}")))?;
    let service_id = ServiceId::new();
    let spec = ServiceSpec::new(
        service_id,
        ctx.session_id,
        ctx.tool_call_id,
        impact,
        execution,
    )
    .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("service spec: {e}")))?;

    let service = runtime
        .start_service(spec)
        .await
        .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("start_service: {e}")))?;

    let _ = sqlx::query("UPDATE tool_calls SET service_id = ? WHERE id = ?")
        .bind(service.id.to_string())
        .bind(ctx.tool_call_id.to_string())
        .execute(ctx.pool)
        .await;

    let summary = json!({
        "service_id": service.id.to_string(),
        "status": format!("{:?}", service.status).to_ascii_lowercase(),
        "impact": format!("{:?}", impact).to_ascii_lowercase(),
        "log_stream_id": service.log_stream_id.to_string(),
        "command_summary": service.command_summary,
    });
    // Services do not block the Turn: they are Session-owned and outlive a Turn.
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Text {
            text: format!(
                "service {} started ({}, impact={:?})",
                service.id, service.command_summary, impact
            ),
        }],
        summary,
        error_code: None,
        finish_summary: None,
        wait: None,
    })
}

async fn tool_delegate_cli(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<ToolOutcome, SupervisorError> {
    use crate::modules::runtime::interface::{
        DelegatedCliKind, DeploymentCapabilityProbe, ExecutionEnvironment, ExecutionSpec, JobSpec,
        NetworkPolicy, RuntimeCapabilityId, ValidatedCommand,
    };
    use crate::platform::id::{CliSessionId, JobId};
    use std::collections::BTreeMap;
    use std::str::FromStr;

    let cli_raw = input.get("cli").and_then(|v| v.as_str()).unwrap_or("");
    let cli = match cli_raw {
        "claude_code" => DelegatedCliKind::ClaudeCode,
        "codex" => DelegatedCliKind::Codex,
        other => {
            return Ok(fail_text(
                &format!("cli must be claude_code|codex, got {other:?}"),
                "VALIDATION_FAILED",
            ));
        }
    };
    let instruction = input
        .get("instruction")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SupervisorError::Internal(anyhow::anyhow!("instruction required")))?;
    if instruction.trim().is_empty() {
        return Ok(fail_text("instruction is empty", "VALIDATION_FAILED"));
    }

    // Capability probe: refuse when the CLI binary is not on PATH.
    let probe = DeploymentCapabilityProbe::detect();
    let available = match cli {
        DelegatedCliKind::ClaudeCode => probe.claude_code_available,
        DelegatedCliKind::Codex => probe.codex_available,
    };
    if !available {
        let id = match cli {
            DelegatedCliKind::ClaudeCode => RuntimeCapabilityId::DelegatedCliClaudeCode,
            DelegatedCliKind::Codex => RuntimeCapabilityId::DelegatedCliCodex,
        };
        return Ok(fail_text(
            &format!("delegated CLI not available on this host ({id:?})"),
            "CAPABILITY_UNAVAILABLE",
        ));
    }

    let cli_session_id =
        match input.get("cli_session_id").and_then(|v| v.as_str()) {
            Some(raw) if !raw.is_empty() => Some(CliSessionId::from_str(raw).map_err(|_| {
                SupervisorError::Internal(anyhow::anyhow!("invalid cli_session_id"))
            })?),
            _ => None,
        };

    let runtime_proj = ensure_session_runtime(ctx).await?;
    let runtime = require_runtime(ctx)?;
    let timeout = timeout_ms(input, 600_000).min(3_600_000);
    let execution = ExecutionSpec::new(
        runtime_proj.id,
        working_directory(input)?,
        ValidatedCommand::delegated_cli(cli, instruction, cli_session_id)
            .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("command: {e}")))?,
        ExecutionEnvironment::new(BTreeMap::new(), vec![])
            .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("env: {e}")))?,
        default_limits(timeout),
        NetworkPolicy::DenyAll,
    )
    .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("execution: {e}")))?;

    let job_id = JobId::new();
    let spec = JobSpec::new(
        job_id,
        ctx.session_id,
        ctx.turn_id,
        ctx.tool_call_id,
        execution,
    )
    .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("job spec: {e}")))?;

    let job = runtime
        .start_job(spec)
        .await
        .map_err(|e| SupervisorError::Internal(anyhow::anyhow!("start_job: {e}")))?;

    let _ = sqlx::query("UPDATE tool_calls SET job_id = ? WHERE id = ?")
        .bind(job.id.to_string())
        .bind(ctx.tool_call_id.to_string())
        .execute(ctx.pool)
        .await;

    // Persist CLI session identity for follow-up when the adapter reports one.
    // Local adapter does not yet emit a new session id; follow-up uses the
    // caller-supplied cli_session_id when present.
    let summary = json!({
        "job_id": job.id.to_string(),
        "cli": cli_raw,
        "cli_session_id": cli_session_id.map(|id| id.to_string()),
        "status": format!("{:?}", job.status).to_ascii_lowercase(),
        "log_stream_id": job.log_stream_id.to_string(),
        "command_summary": job.command_summary,
        "follow_up": cli_session_id.is_some(),
    });
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Waiting,
        parts: vec![ToolResultPart::Text {
            text: format!(
                "delegate_cli ({cli_raw}) job {} started{}",
                job.id,
                if cli_session_id.is_some() {
                    " (follow-up)"
                } else {
                    ""
                }
            ),
        }],
        summary,
        error_code: None,
        finish_summary: None,
        wait: Some(TurnWait::job()),
    })
}

async fn tool_update_plan(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<ToolOutcome, SupervisorError> {
    use crate::platform::clock::{Clock, SystemClock, format_utc};
    use crate::platform::id::TimelineItemId;

    let plan = input.get("plan").cloned().unwrap_or(json!({}));
    let evidence = input.get("evidence").cloned().unwrap_or(json!([]));
    let now = format_utc(SystemClock.now());
    let plan_id = format!("pln_{}", TimelineItemId::new());
    let next_seq: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM plan_versions WHERE turn_id = ?",
    )
    .bind(ctx.turn_id.to_string())
    .fetch_one(ctx.pool)
    .await
    .unwrap_or(1);
    sqlx::query(
        "INSERT INTO plan_versions (id, turn_id, sequence, plan_json, evidence_json, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&plan_id)
    .bind(ctx.turn_id.to_string())
    .bind(next_seq)
    .bind(plan.to_string())
    .bind(evidence.to_string())
    .bind(&now)
    .execute(ctx.pool)
    .await?;

    let summary = json!({
        "plan_version_id": plan_id,
        "sequence": next_seq,
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
) -> Result<ToolOutcome, SupervisorError> {
    use crate::platform::clock::{Clock, SystemClock, format_utc};

    let prompt = input
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    if prompt.trim().is_empty() {
        return Ok(fail_text("prompt is required", "VALIDATION_FAILED"));
    }
    let mode = match input
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("blocking")
    {
        "blocking" => AskMode::Blocking,
        "best_effort" => AskMode::BestEffort,
        _ => {
            return Ok(fail_text(
                "mode must be blocking|best_effort",
                "VALIDATION_FAILED",
            ));
        }
    };
    let choices = input.get("choices").cloned().unwrap_or(json!([]));
    let default = input.get("default").cloned();
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
    if mode == AskMode::BestEffort && (default.is_none() || expires_in_ms.is_none()) {
        return Ok(fail_text(
            "best_effort requires default and expires_in_ms",
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
                SystemClock.now() + chrono::Duration::milliseconds(milliseconds),
            ))
        }
        None => None,
    };
    let ask_id = AskId::new();
    let summary = json!({
        "ask_id": ask_id.to_string(),
        "mode": mode.as_str(),
        "prompt": prompt,
        "expires_at": expires_at,
    });
    let request = AskRequest {
        id: ask_id,
        turn_id: ctx.turn_id,
        tool_call_id: ctx.tool_call_id,
        mode,
        prompt: json!({"text": prompt}),
        choices,
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
