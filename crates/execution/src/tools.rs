//! Execute registered tools against a Session workspace.

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

pub(super) fn attach_tool_display(name: &str, input: &Value, outcome: &mut ToolOutcome) {
    let display = build_tool_display(name, input, outcome);
    if !outcome.summary.is_object() {
        outcome.summary = json!({"result": outcome.summary.clone()});
    }
    outcome
        .summary
        .as_object_mut()
        .expect("Tool summary normalized to an object")
        .insert(
            "display".into(),
            serde_json::to_value(display).expect("ToolDisplay is serializable"),
        );
}

fn build_tool_display(name: &str, input: &Value, outcome: &ToolOutcome) -> ToolDisplay {
    let summary = outcome.summary.as_object();
    let string = |key: &str| {
        summary
            .and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let input_string = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let status = outcome.disposition.as_str().to_owned();

    // Build the target-specific title first (e.g. "Read src/main.rs"),
    // then append " failed" for failures so the user always sees what was
    // being attempted and the target, not just a generic "Tool failed".
    let (title, body) = match name {
        "bash" => {
            let command = {
                let value = string("command");
                if value.is_empty() {
                    input_string("command")
                } else {
                    value
                }
            };
            let mode = string("mode");
            let title = if mode == "async" {
                format!("Started {}", one_line(&command))
            } else {
                format!("Ran {}", one_line(&command))
            };
            let exit_code = summary
                .and_then(|value| value.get("exit_code"))
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok());
            let truncated = summary
                .and_then(|value| value.get("truncated"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            (
                title.trim().to_owned(),
                ToolDisplayBody::CommandOutput {
                    command,
                    stdout: string("stdout"),
                    stderr: string("stderr"),
                    exit_code,
                    truncated,
                },
            )
        }
        "read" => {
            let path = input_string("path");
            (format!("Read {path}"), ToolDisplayBody::None)
        }
        "write" => {
            let path = input_string("path");
            let created = summary
                .and_then(|value| value.get("created"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let action = if created { "Created" } else { "Wrote" };
            (format!("{action} {path}"), patch_or_summary(outcome))
        }
        "edit" => {
            let path = input_string("path");
            (format!("Edited {path}"), patch_or_summary(outcome))
        }
        "delete" => {
            let path = input_string("path");
            (format!("Removed {path}"), ToolDisplayBody::None)
        }
        "delegate_cli" => {
            let cli = input_string("cli");
            (
                format!("Delegated to {}", display_tool_name(&cli)),
                ToolDisplayBody::Structured {
                    value: outcome.summary.clone(),
                },
            )
        }
        "read_output" => {
            let job_id = input_string("job_id");
            (
                format!("Read output for {job_id}"),
                ToolDisplayBody::Structured {
                    value: outcome.summary.clone(),
                },
            )
        }
        "stop" => {
            let job_id = input_string("job_id");
            (format!("Stopped job {job_id}"), ToolDisplayBody::None)
        }
        "todo" => (
            "Updated plan".into(),
            ToolDisplayBody::Structured {
                value: input.clone(),
            },
        ),
        "ask_user" => {
            let prompt = input_string("prompt");
            (format!("Asked {prompt}"), ToolDisplayBody::None)
        }
        "attachment_list" => (
            "Listed attachments".into(),
            ToolDisplayBody::Structured {
                value: outcome.summary.clone(),
            },
        ),
        "attachment_read" => {
            let target = input_string("attachment_id");
            (format!("Read attachment {target}"), result_body(outcome))
        }
        "attachment_save" => {
            let path = input_string("path");
            (format!("Saved attachment to {path}"), ToolDisplayBody::None)
        }
        _ => (
            format!("Used {}", display_tool_name(name)),
            result_body(outcome),
        ),
    };

    // For failed tools, keep the target-specific title and body so the user
    // sees what was attempted. The status dot (danger) is the only failure
    // signal — the title does NOT append " failed" to avoid redundancy.
    if outcome.disposition == ToolExecutionDisposition::Failed {
        let code = outcome
            .error_code
            .clone()
            .unwrap_or_else(|| "TOOL_EXECUTION_FAILED".into());
        let detail = summary
            .and_then(|value| value.get("detail").or_else(|| value.get("error")))
            .and_then(Value::as_str)
            .unwrap_or("Tool execution failed")
            .to_owned();
        return ToolDisplay {
            version: 1,
            title: if title.trim().is_empty() {
                format!("Used {}", display_tool_name(name))
            } else {
                title
            },
            status,
            body: ToolDisplayBody::Error { code, detail },
        };
    }

    ToolDisplay {
        version: 1,
        title: if title.trim().is_empty() {
            format!("Used {}", display_tool_name(name))
        } else {
            title
        },
        status,
        body,
    }
}

fn result_body(outcome: &ToolOutcome) -> ToolDisplayBody {
    match outcome.parts.first() {
        Some(ToolResultPart::Text { text }) => ToolDisplayBody::Text { text: text.clone() },
        Some(ToolResultPart::Json { value }) => ToolDisplayBody::Structured {
            value: value.clone(),
        },
        _ => ToolDisplayBody::Structured {
            value: outcome.summary.clone(),
        },
    }
}

fn patch_or_summary(outcome: &ToolOutcome) -> ToolDisplayBody {
    outcome
        .summary
        .get("patch")
        .and_then(Value::as_str)
        .map(|patch| ToolDisplayBody::Patch {
            patch: patch.to_owned(),
        })
        .unwrap_or_else(|| ToolDisplayBody::Structured {
            value: outcome.summary.clone(),
        })
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn display_tool_name(value: &str) -> String {
    value
        .split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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

fn map_path_err(e: PathError) -> ToolOutcome {
    let _ = e;
    fail_text("invalid path", "TOOL_PATH_INVALID")
}

async fn tool_read(
    repo: &Path,
    input: &Value,
    handle: &WorkspaceHandle,
    ctx: &ToolContext<'_>,
) -> Result<ToolOutcome, ExecutionError> {
    let raw = input
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or_else(|| ExecutionError::ToolPathInvalid)?;
    let path = match resolve_session_path(repo, raw) {
        Ok(p) => p,
        Err(e) => return Ok(map_path_err(e)),
    };
    if !path.is_file() {
        return Ok(fail_text("file not found", "TOOL_PATH_INVALID"));
    }
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| ExecutionError::Internal(anyhow::anyhow!(e)))?;
    if meta.len() > MAX_IMAGE_BYTES {
        // Still allow large text? Cap text at 10MiB for safety.
        if meta.len() > 10 * 1024 * 1024 {
            return Ok(fail_text("file too large", "IMAGE_TOO_LARGE"));
        }
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| ExecutionError::Internal(anyhow::anyhow!(e)))?;

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
    let (text, offset, limit) = apply_read_range(text, input);
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Text { text: text.clone() }],
        summary: json!({
            "path": raw,
            "kind": "text",
            "bytes": text.len(),
            "offset": offset,
            "limit": limit,
        }),
        error_code: None,
        finish_summary: None,
        wait: None,
    })
}

/// Apply the optional `offset` (1-indexed) / `limit` (line count) read range to
/// the full file text. Returns the slice plus the effective offset/limit used
/// so the caller can echo them back. When neither is given, the full text is
/// returned and offset/limit are null.
fn apply_read_range(text: String, input: &Value) -> (String, Option<i64>, Option<i64>) {
    let offset = input
        .get("offset")
        .and_then(|v| v.as_u64())
        .filter(|v| *v >= 1);
    let limit = input
        .get("limit")
        .and_then(|v| v.as_u64())
        .filter(|v| *v >= 1);
    let Some(offset) = offset else {
        return (text, None, None);
    };
    let start = (offset as usize).saturating_sub(1);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let total = lines.len();
    if start >= total {
        return (String::new(), Some(offset as i64), Some(0));
    }
    let end = match limit {
        Some(l) => (start + l as usize).min(total),
        None => total,
    };
    let slice: String = lines[start..end].concat();
    (slice, Some(offset as i64), Some((end - start) as i64))
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

pub(super) fn supported_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return None;
    }
    let kind = sniff_image(bytes)?;
    if matches!(kind, ImageKind::Gif) && gif_is_animated(bytes) {
        return None;
    }
    let (width, height) = probe_dimensions(bytes, kind).ok()?;
    if width == 0 || height == 0 || width > MAX_EDGE_PX || height > MAX_EDGE_PX {
        return None;
    }
    ((width as u64).saturating_mul(height as u64) <= MAX_PIXELS).then(|| mime_of(kind))
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
fn probe_dimensions(bytes: &[u8], kind: ImageKind) -> Result<(u32, u32), ExecutionError> {
    match kind {
        ImageKind::Png if bytes.len() >= 24 => {
            let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
            let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
            Ok((w, h))
        }
        ImageKind::Png => Err(ExecutionError::UnsupportedImage),
        ImageKind::Gif if bytes.len() >= 10 => {
            let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
            let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
            Ok((w, h))
        }
        ImageKind::Gif => Err(ExecutionError::UnsupportedImage),
        ImageKind::Jpeg => jpeg_dimensions(bytes).ok_or(ExecutionError::UnsupportedImage),
        ImageKind::Webp => webp_dimensions(bytes).ok_or(ExecutionError::UnsupportedImage),
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
) -> Result<ToolOutcome, ExecutionError> {
    let revision = ctx
        .workspace
        .current_revision(handle)
        .await
        .ok()
        .map(|r| r.0);
    image_outcome(path, bytes, kind, revision)
}

fn image_outcome(
    path: &str,
    bytes: &[u8],
    kind: ImageKind,
    revision: Option<String>,
) -> Result<ToolOutcome, ExecutionError> {
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
) -> Result<ToolOutcome, ExecutionError> {
    let path = input
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or(ExecutionError::ToolPathInvalid)?;
    let content = input
        .get("content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| ExecutionError::Internal(anyhow::anyhow!("content required")))?;
    let normalized_path =
        normalize_session_path(path).map_err(|_| ExecutionError::ToolPathInvalid)?;
    // Path validation only (mutation API re-validates).
    let abs = resolve_session_path(&session_repo(ctx.workspace, ctx.session_id)?, path)
        .map_err(|_| ExecutionError::ToolPathInvalid)?;
    // Detect whether this is a create vs an overwrite so the UI can show
    // "Created" vs "Wrote". Best-effort: a metadata error counts as absent.
    let existed = tokio::fs::metadata(&abs).await.is_ok();
    let old_text = if existed {
        tokio::fs::read_to_string(&abs).await.ok()
    } else {
        None
    };

    let mutation = FileMutation::Write {
        path: normalized_path,
        content: content.as_bytes().to_vec(),
    };
    let rev = ctx
        .workspace
        .apply_file_mutation(handle, mutation, None, "tool.write", ctx.actor.clone())
        .await?;
    let patch = compute_patch(old_text.as_deref(), content);
    let summary = match patch {
        Some(ref p) => json!({"path": path, "revision": rev.0, "created": !existed, "patch": p}),
        None => json!({"path": path, "revision": rev.0, "created": !existed}),
    };
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Text {
            text: format!("wrote {path} -> {}", rev.0),
        }],
        summary,
        error_code: None,
        finish_summary: None,
        wait: None,
    })
}

/// Render a compact unified-diff string for the tool summary so the UI can show
/// `+x -y` line counts and an expandable diff. Reuses the workspace diff LCS so
/// the algorithm matches the Session diff view. `old=None` means a newly created
/// file (every new line is an addition).
fn compute_patch(old: Option<&str>, new: &str) -> Option<String> {
    let new_bytes = new.as_bytes();
    let old_bytes = old.map(str::as_bytes).unwrap_or(&[]);
    if old_bytes == new_bytes {
        return None;
    }
    let (hunks, _binary) = line_hunks(old_bytes, new_bytes);
    if _binary {
        return None;
    }
    let mut out = String::new();
    for hunk in &hunks {
        for line in &hunk.lines {
            match line.kind {
                DiffLineKind::Context => {
                    out.push(' ');
                    out.push_str(&line.text);
                }
                DiffLineKind::Add => {
                    out.push('+');
                    out.push_str(&line.text);
                }
                DiffLineKind::Delete => {
                    out.push('-');
                    out.push_str(&line.text);
                }
                DiffLineKind::Skip => {
                    out.push_str(&line.text);
                    out.push('\n');
                    continue;
                }
            }
            if !line.text.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

async fn tool_edit(
    ctx: &ToolContext<'_>,
    handle: &WorkspaceHandle,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    let path = input
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or(ExecutionError::ToolPathInvalid)?;
    let normalized_path =
        normalize_session_path(path).map_err(|_| ExecutionError::ToolPathInvalid)?;
    let abs = resolve_session_path(&session_repo(ctx.workspace, ctx.session_id)?, path)
        .map_err(|_| ExecutionError::ToolPathInvalid)?;
    // edit cannot create files — the model must use `write` for new files.
    if tokio::fs::metadata(&abs).await.is_err() {
        return Ok(fail_text(
            "edit target does not exist; use write to create a file",
            "TOOL_EDIT_TARGET_MISSING",
        ));
    }
    if !ctx.read_paths.contains(path) && !ctx.read_paths.contains(&normalized_path) {
        return Ok(fail_text(
            "edit requires reading the file first; call read on this path before editing",
            "TOOL_EDIT_NOT_READ",
        ));
    }
    let edits = match input.get("edits").and_then(|e| e.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => {
            return Ok(fail_text(
                "edit requires a non-empty edits array",
                "TOOL_EDIT_EMPTY",
            ));
        }
    };

    let original = tokio::fs::read_to_string(&abs)
        .await
        .map_err(|e| ExecutionError::Internal(anyhow::anyhow!(e)))?;
    let mut content = original.clone();
    let mut applied = 0u32;
    for edit in edits {
        let old_text = edit.get("oldText").and_then(|v| v.as_str()).unwrap_or("");
        let new_text = edit.get("newText").and_then(|v| v.as_str()).unwrap_or("");
        if old_text.is_empty() {
            return Ok(fail_text(
                "edit oldText must not be empty",
                "TOOL_EDIT_EMPTY_OLDTEXT",
            ));
        }
        // Each edit matches against the running result of prior edits in this
        // call; edits must be disjoint and oldText unique within the current
        // content (matches the schema contract).
        let occurrences = content.matches(old_text).count();
        match occurrences {
            0 => {
                return Ok(fail_text(
                    "edit oldText was not found in the file",
                    "TOOL_EDIT_NOT_FOUND",
                ));
            }
            1 => {
                content = content.replacen(old_text, new_text, 1);
                applied += 1;
            }
            _ => {
                return Ok(fail_text(
                    "edit oldText is not unique in the file",
                    "TOOL_EDIT_NOT_UNIQUE",
                ));
            }
        }
    }

    let mutation = FileMutation::Write {
        path: normalized_path,
        content: content.as_bytes().to_vec(),
    };
    let rev = ctx
        .workspace
        .apply_file_mutation(handle, mutation, None, "tool.edit", ctx.actor.clone())
        .await?;
    let patch = compute_patch(Some(&original), &content);
    let summary = match patch {
        Some(ref p) => json!({"path": path, "revision": rev.0, "edits": applied, "patch": p}),
        None => json!({"path": path, "revision": rev.0, "edits": applied}),
    };
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Text {
            text: format!("edited {path} -> {} ({applied} edit(s))", rev.0),
        }],
        summary,
        error_code: None,
        finish_summary: None,
        wait: None,
    })
}

async fn tool_remove(
    ctx: &ToolContext<'_>,
    handle: &WorkspaceHandle,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    let path = input
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or(ExecutionError::ToolPathInvalid)?;
    let normalized_path =
        normalize_session_path(path).map_err(|_| ExecutionError::ToolPathInvalid)?;
    let _ = resolve_session_path(&session_repo(ctx.workspace, ctx.session_id)?, path)
        .map_err(|_| ExecutionError::ToolPathInvalid)?;
    let rev = ctx
        .workspace
        .apply_file_mutation(
            handle,
            FileMutation::Delete {
                path: normalized_path,
            },
            None,
            "tool.delete",
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

async fn tool_attachment_list(ctx: &ToolContext<'_>) -> Result<ToolOutcome, ExecutionError> {
    let attachments = ctx.sessions.list_attachments(ctx.session_id).await?;
    let items = attachments
        .iter()
        .map(|attachment| {
            json!({
                "id": attachment.id.to_string(),
                "name": attachment.name,
                "mime": attachment.mime,
                "byte_size": attachment.byte_size,
            })
        })
        .collect::<Vec<_>>();
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Json {
            value: json!({"attachments": items}),
        }],
        summary: json!({"count": attachments.len()}),
        error_code: None,
        finish_summary: None,
        wait: None,
    })
}

async fn tool_attachment_read(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    let Some(attachment) = find_attachment(ctx, input).await? else {
        return Ok(fail_text(
            "attachment not found",
            "TOOL_ATTACHMENT_NOT_FOUND",
        ));
    };
    let Some(bytes) = read_attachment_bytes(ctx.blobs, &attachment).await? else {
        return Ok(fail_text(
            "attachment content is not available",
            "TOOL_ATTACHMENT_UNAVAILABLE",
        ));
    };

    if let Some(kind) = sniff_image(&bytes) {
        let label = format!("attachment:{}/{}", attachment.id, attachment.name);
        return image_outcome(&label, &bytes, kind, None);
    }

    if let Ok(text) = std::str::from_utf8(&bytes)
        && !text.contains('\0')
    {
        let mut end = text.len().min(MAX_ATTACHMENT_TEXT_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let truncated = end < text.len();
        return Ok(ToolOutcome {
            disposition: ToolExecutionDisposition::Succeeded,
            parts: vec![ToolResultPart::Text {
                text: text[..end].to_owned(),
            }],
            summary: json!({
                "attachment_id": attachment.id.to_string(),
                "name": attachment.name,
                "mime": attachment.mime,
                "byte_size": attachment.byte_size,
                "kind": "text",
                "returned_bytes": end,
                "truncated": truncated,
            }),
            error_code: None,
            finish_summary: None,
            wait: None,
        });
    }

    let metadata = json!({
        "attachment_id": attachment.id.to_string(),
        "name": attachment.name,
        "mime": attachment.mime,
        "byte_size": attachment.byte_size,
        "kind": "binary",
        "next_action": "Use attachment.save with this attachment_id and a Session-relative path.",
    });
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Json {
            value: metadata.clone(),
        }],
        summary: metadata,
        error_code: None,
        finish_summary: None,
        wait: None,
    })
}

async fn tool_attachment_save(
    ctx: &ToolContext<'_>,
    handle: &WorkspaceHandle,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    let Some(attachment) = find_attachment(ctx, input).await? else {
        return Ok(fail_text(
            "attachment not found",
            "TOOL_ATTACHMENT_NOT_FOUND",
        ));
    };
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .ok_or(ExecutionError::ToolPathInvalid)?;
    let normalized_path =
        normalize_session_path(path).map_err(|_| ExecutionError::ToolPathInvalid)?;
    let _ = resolve_session_path(&session_repo(ctx.workspace, ctx.session_id)?, path)
        .map_err(|_| ExecutionError::ToolPathInvalid)?;
    let Some(bytes) = read_attachment_bytes(ctx.blobs, &attachment).await? else {
        return Ok(fail_text(
            "attachment content is not available",
            "TOOL_ATTACHMENT_UNAVAILABLE",
        ));
    };
    let revision = ctx
        .workspace
        .apply_file_mutation(
            handle,
            FileMutation::Write {
                path: normalized_path,
                content: bytes,
            },
            None,
            "tool.attachment.save",
            ctx.actor.clone(),
        )
        .await?;
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Text {
            text: format!("saved attachment {} to {path}", attachment.id),
        }],
        summary: json!({
            "attachment_id": attachment.id.to_string(),
            "path": path,
            "revision": revision.0,
        }),
        error_code: None,
        finish_summary: None,
        wait: None,
    })
}

async fn find_attachment(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<Option<AttachmentResource>, ExecutionError> {
    let Some(id) = input
        .get("attachment_id")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<AttachmentId>().ok())
    else {
        return Ok(None);
    };
    match ctx.sessions.get_attachment(ctx.session_id, id).await {
        Ok(attachment) => Ok(Some(attachment)),
        Err(janus_sessions::interface::SessionsError::NotFound) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) async fn read_attachment_bytes(
    blobs: &BlobStore,
    attachment: &AttachmentResource,
) -> Result<Option<Vec<u8>>, ExecutionError> {
    let Some(blob_sha) = attachment.blob_sha.as_deref() else {
        return Ok(None);
    };
    let bytes = blobs.read(blob_sha).await?;
    if bytes.len() as u64 != attachment.byte_size {
        return Err(ExecutionError::Internal(anyhow::anyhow!(
            "attachment {} byte length does not match its stored metadata",
            attachment.id
        )));
    }
    Ok(Some(bytes))
}

// ---------------------------------------------------------------------------
// Runtime-backed tools
// ---------------------------------------------------------------------------

fn default_limits(timeout_ms: u64) -> janus_runtime::interface::ResourceLimits {
    janus_runtime::interface::ResourceLimits {
        timeout_ms,
        memory_bytes: 256 * 1024 * 1024,
        cpu_millis: 1_000,
        pids: 64,
        temporary_disk_bytes: 128 * 1024 * 1024,
        open_files: 128,
    }
}

async fn ensure_session_runtime(
    ctx: &ToolContext<'_>,
) -> Result<janus_runtime::interface::RuntimeProjection, ExecutionError> {
    use janus_infrastructure::id::RuntimeId;
    use janus_runtime::interface::{ExecutorKind, NetworkPolicy, RuntimeScope, RuntimeSpec};

    let existing = ctx
        .runtime
        .current_runtime(RuntimeScope::session(ctx.session_id))
        .await
        .map_err(ExecutionError::Runtime)?;
    let workspace_root = session_repo(ctx.workspace, ctx.session_id)?;
    let abs = workspace_root.canonicalize().unwrap_or(workspace_root);
    let spec = RuntimeSpec::new(
        existing.map_or_else(RuntimeId::new, |runtime| runtime.id),
        RuntimeScope::session(ctx.session_id),
        ExecutorKind::Local,
        abs,
        default_limits(30_000),
        NetworkPolicy::DenyAll,
    )
    .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("runtime spec: {e}")))?;
    ctx.runtime
        .ensure_runtime(&spec)
        .await
        .map_err(ExecutionError::Runtime)
}

fn working_directory(
    input: &Value,
) -> Result<janus_runtime::interface::RelativeWorkingDirectory, ExecutionError> {
    use janus_runtime::interface::RelativeWorkingDirectory;
    let raw = input
        .get("working_directory")
        .and_then(|v| v.as_str())
        .unwrap_or(".");
    RelativeWorkingDirectory::new(raw)
        .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("working_directory: {e}")))
}

fn timeout_ms(input: &Value, default: u64) -> u64 {
    input
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

async fn tool_bash(ctx: &ToolContext<'_>, input: &Value) -> Result<ToolOutcome, ExecutionError> {
    use janus_runtime::interface::{
        ExecutionEnvironment, ExecutionSpec, NetworkPolicy, ValidatedCommand,
    };
    use std::collections::BTreeMap;

    let display_command = input
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExecutionError::Internal(anyhow::anyhow!("command required")))?;
    if display_command.trim().is_empty() {
        return Ok(fail_text("command is empty", "VALIDATION_FAILED"));
    }
    if let Err(detail) = validate_workspace_command(display_command) {
        return Ok(fail_text(&detail, "BASH_PATH_ESCAPE"));
    }
    let mode = input.get("mode").and_then(|v| v.as_str()).unwrap_or("sync");
    if mode == "async" {
        return tool_bash_async(ctx, display_command, input).await;
    }
    if mode != "sync" {
        return Ok(fail_text(
            &format!("unknown bash mode: {mode}"),
            "VALIDATION_FAILED",
        ));
    }

    let timeout = timeout_ms(input, 30_000).min(120_000);
    let repo = session_repo(ctx.workspace, ctx.session_id)?;
    let cwd = working_directory(input)?;
    let fallback_cwd = local_working_directory(&repo, &cwd)?;
    let command = normalize_workspace_command(
        display_command,
        &workspace_command_prefix(&repo, &fallback_cwd),
    );

    let runtime_proj = match ensure_session_runtime(ctx).await {
        Ok(runtime) => runtime,
        Err(ExecutionError::Runtime(error)) if error.retryable() => {
            tracing::warn!(%error, "preferred Runtime unavailable; using system Bash fallback");
            return run_local_sync(
                ctx,
                &command,
                display_command,
                &repo,
                &fallback_cwd,
                timeout,
            )
            .await;
        }
        Err(error) => return Err(error),
    };
    let spec = ExecutionSpec::new(
        runtime_proj.id,
        cwd,
        ValidatedCommand::shell(&command)
            .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("command: {e}")))?,
        ExecutionEnvironment::new(BTreeMap::new(), vec![])
            .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("env: {e}")))?,
        default_limits(timeout),
        NetworkPolicy::DenyAll,
    )
    .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("execution: {e}")))?;

    let result = match ctx.runtime.execute_sync(spec).await {
        Ok(result) => result,
        Err(error) if error.retryable() => {
            tracing::warn!(
                %error,
                "Runtime sync execution unavailable; using local Git Bash fallback"
            );
            return run_local_sync(
                ctx,
                &command,
                display_command,
                &repo,
                &fallback_cwd,
                timeout,
            )
            .await;
        }
        Err(error) => return Err(ExecutionError::Runtime(error)),
    };

    bash_outcome(
        display_command,
        result.exit.exit_code,
        result.timed_out,
        result.duration_ms,
        result.truncated,
        &result.stdout,
        &result.stderr,
        &repo,
    )
}

/// async bash mode: a finite background Job. Requires a bound Runtime because
/// the background-job machinery (log stream, Turn waiting) lives there.
async fn tool_bash_async(
    ctx: &ToolContext<'_>,
    display_command: &str,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    use janus_infrastructure::id::JobId;
    use janus_runtime::interface::{
        ExecutionEnvironment, ExecutionSpec, JobSpec, NetworkPolicy, ValidatedCommand,
    };
    use std::collections::BTreeMap;

    let repo = session_repo(ctx.workspace, ctx.session_id)?;
    let cwd = working_directory(input)?;
    let fallback_cwd = local_working_directory(&repo, &cwd)?;
    let command = normalize_workspace_command(
        display_command,
        &workspace_command_prefix(&repo, &fallback_cwd),
    );
    let runtime_proj = ensure_session_runtime(ctx).await?;
    let timeout = timeout_ms(input, 300_000).min(3_600_000);
    let execution = ExecutionSpec::new(
        runtime_proj.id,
        cwd,
        ValidatedCommand::shell(command)
            .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("command: {e}")))?,
        ExecutionEnvironment::new(BTreeMap::new(), vec![])
            .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("env: {e}")))?,
        default_limits(timeout),
        NetworkPolicy::DenyAll,
    )
    .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("execution: {e}")))?;
    let job_id = JobId::new();
    let spec = JobSpec::new(
        job_id,
        ctx.session_id,
        ctx.turn_id,
        ctx.tool_call_id,
        execution,
    )
    .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("job spec: {e}")))?;

    let job = ctx
        .runtime
        .start_job(spec)
        .await
        .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("start_job: {e}")))?;

    let summary = json!({
        "job_id": job.id.to_string(),
        "status": format!("{:?}", job.status).to_ascii_lowercase(),
        "log_stream_id": job.log_stream_id.to_string(),
        "command_summary": job.command_summary,
        "mode": "async",
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

/// Local fallback for sync bash when no Runtime is bound. Windows uses Git
/// Bash (`bash -c`), other platforms use `/bin/sh -c`.
async fn run_local_sync(
    _ctx: &ToolContext<'_>,
    command: &str,
    display_command: &str,
    workspace_root: &Path,
    cwd: &Path,
    timeout_ms: u64,
) -> Result<ToolOutcome, ExecutionError> {
    use std::time::Duration;
    let Some(program) = bash_program() else {
        let detail = if cfg!(windows) {
            "Git Bash is not installed or could not be located"
        } else {
            "/bin/bash is not available"
        };
        return Ok(fail_text(detail, "BASH_UNAVAILABLE"));
    };
    let mut cmd = tokio::process::Command::new(program);
    // The fallback is still a degraded local executor, but it must not load
    // the host user's shell profile or inherit its complete environment.
    cmd.args(["--noprofile", "--norc", "-c", command]);
    apply_fallback_environment(&mut cmd, workspace_root);
    cmd.current_dir(cwd);
    let started = std::time::Instant::now();
    let output = tokio::time::timeout(Duration::from_millis(timeout_ms), cmd.output()).await;
    let (timed_out, exit_code, stdout, stderr) = match output {
        Ok(Ok(out)) => {
            let stdout = decode_process_output(&out.stdout, 1024 * 1024);
            let stderr = decode_process_output(&out.stderr, 1024 * 1024);
            (false, out.status.code(), stdout.text, stderr.text)
        }
        Ok(Err(e)) => {
            return Ok(fail_text(
                &format!("failed to run command: {e}"),
                "COMMAND_FAILED",
            ));
        }
        Err(_) => (true, None, String::new(), String::new()),
    };
    let duration_ms = started.elapsed().as_millis() as u64;
    bash_outcome(
        display_command,
        exit_code,
        timed_out,
        duration_ms,
        false,
        &stdout,
        &stderr,
        workspace_root,
    )
}

fn local_working_directory(
    repo: &Path,
    cwd: &janus_runtime::interface::RelativeWorkingDirectory,
) -> Result<std::path::PathBuf, ExecutionError> {
    let candidate =
        resolve_session_path(repo, cwd.as_str()).map_err(|_| ExecutionError::ToolPathInvalid)?;
    let canonical_root = repo
        .canonicalize()
        .map_err(|error| ExecutionError::Internal(anyhow::anyhow!("workspace: {error}")))?;
    let canonical_cwd = candidate
        .canonicalize()
        .map_err(|_| ExecutionError::ToolPathInvalid)?;
    if !canonical_cwd.starts_with(&canonical_root) {
        return Err(ExecutionError::ToolPathInvalid);
    }
    Ok(canonical_cwd)
}

fn workspace_command_prefix(workspace_root: &Path, cwd: &Path) -> String {
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_owned());
    let relative = cwd.strip_prefix(&canonical_root).unwrap_or(cwd);
    let depth = relative
        .components()
        .filter(|component| matches!(component, std::path::Component::Normal(_)))
        .count();
    if depth == 0 {
        return ".".into();
    }
    std::iter::repeat_n("..", depth)
        .collect::<Vec<_>>()
        .join("/")
}

fn validate_workspace_command(command: &str) -> Result<(), String> {
    if command.contains('\0')
        || command.contains("$(")
        || command.contains('`')
        || command.contains("${")
    {
        return Err(
            "Bash blocked: command substitution or environment expansion is not allowed; use an explicit `/workspace/...` or workspace-relative path."
                .into(),
        );
    }

    // Keep quoted text intact: a sentence such as "compare vba / gradio" is
    // not a path expression and must not turn its slash into a fake root token.
    for token in shell_command_tokens(command) {
        if let Some(detail) = workspace_path_violation(&token) {
            return Err(detail);
        }
    }
    Ok(())
}

fn workspace_path_violation(value: &str) -> Option<String> {
    let value = value.trim_matches(['"', '\'']);
    if value == "/dev/null" {
        return None;
    }
    if value == ".." || (value.starts_with('\\') && !is_shell_escape_literal(value)) {
        return Some(format_parent_traversal(value));
    }
    if value.starts_with('/') {
        let Some(relative) = value.strip_prefix("/workspace") else {
            return Some(format_absolute_path(value));
        };
        if relative.is_empty() {
            return None;
        }
        if !relative.starts_with('/') {
            return Some(format_absolute_path(value));
        }
        if relative
            .split(['/', '\\'])
            .any(|part| part.trim_matches(['"', '\'']) == "..")
        {
            return Some(format_parent_traversal(value));
        }
        return None;
    }
    if value.len() >= 3
        && value.as_bytes()[0].is_ascii_alphabetic()
        && value.as_bytes()[1] == b':'
        && matches!(value.as_bytes()[2], b'/' | b'\\')
    {
        return Some(format_absolute_path(value));
    }
    if value
        .split(['/', '\\'])
        .any(|part| part.trim_matches(['"', '\'']) == "..")
    {
        return Some(format_parent_traversal(value));
    }
    None
}

fn is_shell_escape_literal(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 2
        && bytes[0] == b'\\'
        && matches!(
            bytes[1],
            b'a' | b'b' | b'e' | b'f' | b'n' | b'r' | b't' | b'v' | b'0'
        )
}

fn format_absolute_path(value: &str) -> String {
    format!(
        "Bash blocked: absolute path `{value}` is outside the Session workspace; use `/workspace/...` or a workspace-relative path."
    )
}

fn format_parent_traversal(value: &str) -> String {
    format!(
        "Bash blocked: path `{value}` traverses outside the Session workspace; remove `..` and use a workspace-relative path."
    )
}

fn normalize_workspace_command(command: &str, workspace_prefix: &str) -> String {
    const ALIAS: &str = "/workspace";
    let mut normalized = String::with_capacity(command.len());
    let mut offset = 0;
    while offset < command.len() {
        let remaining = &command[offset..];
        if remaining.starts_with(ALIAS) {
            let before = command[..offset].chars().next_back();
            let after = command[offset + ALIAS.len()..].chars().next();
            let before_is_boundary = before
                .is_none_or(|value| value.is_whitespace() || "\"'(){}[];|&<>=$".contains(value));
            let after_is_boundary = after
                .is_none_or(|value| value.is_whitespace() || "/\\\"'(){}[];|&<>=$".contains(value));
            if before_is_boundary && after_is_boundary {
                normalized.push_str(workspace_prefix);
                offset += ALIAS.len();
                continue;
            }
        }
        let character = remaining
            .chars()
            .next()
            .expect("offset is always on a character boundary");
        normalized.push(character);
        offset += character.len_utf8();
    }
    normalized
}

fn shell_command_tokens(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut single_quote = false;
    let mut double_quote = false;
    let mut escaped = false;
    for character in command.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && !single_quote {
            current.push(character);
            escaped = true;
            continue;
        }
        match character {
            '\'' if !double_quote => single_quote = !single_quote,
            '"' if !single_quote => double_quote = !double_quote,
            character
                if !single_quote
                    && !double_quote
                    && (character.is_whitespace() || ";&|<>".contains(character)) =>
            {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                if !character.is_whitespace() {
                    tokens.push(character.to_string());
                }
            }
            _ => current.push(character),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn apply_fallback_environment(command: &mut tokio::process::Command, workspace_root: &Path) {
    command.env_clear();
    if let Some(path) = bash_search_path() {
        command.env("PATH", path);
    }
    command.env("HOME", workspace_root);
    command.env("GIT_OPTIONAL_LOCKS", "0");
    command.env("LANG", "C.UTF-8");
    let temp = workspace_root.join(".janus-tmp");
    let _ = std::fs::create_dir_all(&temp);
    command.env("TMPDIR", &temp);
    command.env("TEMP", &temp);
    command.env("TMP", &temp);
    command.env("USER", "Janus");
    command.env("USERNAME", "Janus");
    command.env("LOGNAME", "Janus");
    command.env("USERDOMAIN", "Janus");
    command.env("HOSTNAME", "Janus");
    command.env("COMPUTERNAME", "Janus");
    #[cfg(windows)]
    for key in ["COMSPEC", "PATHEXT", "SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
}

/// Build a ToolOutcome from a completed (sync) bash run. Shared by the
/// Runtime path and the local fallback.
fn bash_outcome(
    command: &str,
    exit_code: Option<i32>,
    timed_out: bool,
    duration_ms: u64,
    truncated: bool,
    stdout: &str,
    stderr: &str,
    workspace_root: &Path,
) -> Result<ToolOutcome, ExecutionError> {
    let command = sanitize_workspace_text(command, workspace_root);
    let stdout = sanitize_workspace_text(stdout, workspace_root);
    let stderr = sanitize_workspace_text(stderr, workspace_root);
    let (stdout_out, stdout_truncated) = truncate_tool_text(&stdout, 8_000);
    let (stderr_out, stderr_truncated) = truncate_tool_text(&stderr, 4_000);
    let ok = !timed_out && exit_code == Some(0);
    let summary = json!({
        "command": command,
        "exit_code": exit_code,
        "timed_out": timed_out,
        "duration_ms": duration_ms,
        "truncated": truncated || stdout_truncated || stderr_truncated,
        "stdout": stdout_out,
        "stderr": stderr_out,
        "stdout_bytes": stdout.len(),
        "stderr_bytes": stderr.len(),
        "mode": "sync",
    });
    let text = format!(
        "exit={:?} timed_out={} duration_ms={}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        exit_code, timed_out, duration_ms, stdout_out, stderr_out
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
        } else if timed_out {
            Some("COMMAND_TIMEOUT".into())
        } else {
            Some("COMMAND_FAILED".into())
        },
        finish_summary: None,
        wait: None,
    })
}

fn sanitize_workspace_text(text: &str, workspace_root: &Path) -> String {
    let mut sanitized = text.to_owned();
    let mut variants = vec![workspace_root.to_string_lossy().into_owned()];
    if let Ok(canonical) = workspace_root.canonicalize() {
        variants.push(canonical.to_string_lossy().into_owned());
    }
    let unix_variants = variants
        .iter()
        .map(|value| value.replace('\\', "/"))
        .collect::<Vec<_>>();
    variants.extend(unix_variants);
    if cfg!(windows) {
        let drive_variants = variants
            .iter()
            .filter_map(|value| {
                let bytes = value.as_bytes();
                (bytes.len() >= 3 && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\')).then(
                    || {
                        let drive = value[..1].to_ascii_lowercase();
                        let rest = value[2..].replace('\\', "/");
                        format!("/{drive}{rest}")
                    },
                )
            })
            .collect::<Vec<_>>();
        variants.extend(drive_variants);
    }
    variants.retain(|value| !value.is_empty());
    variants.sort_by(|left, right| right.len().cmp(&left.len()));
    variants.dedup();
    for variant in variants {
        sanitized = sanitized.replace(&variant, "/workspace");
    }
    sanitized = sanitized
        .replace("CodexSandboxOffline", "Janus")
        .replace("codexsandboxoffline", "Janus")
        .replace("CODEXSANDBOXOFFLINE", "Janus")
        .replace("CodexOfflineSandbox", "Janus")
        .replace("codexofflinesandbox", "Janus")
        .replace("CODEXOFFLINESANDBOX", "Janus");
    if cfg!(windows) {
        sanitized = sanitized.replace('\\', "/");
    }

    let mut output = String::with_capacity(sanitized.len());
    for line in sanitized.split_inclusive('\n') {
        let (content, ending) = line
            .strip_suffix('\n')
            .map_or((line, ""), |content| (content, "\n"));
        if content.contains(".janus-runtime-") || content.contains(".janus-tmp") {
            output.push_str("[runtime artifact hidden]");
        } else if is_git_bash_host_identity_line(content) {
            output.push_str("[host detail hidden]");
        } else if let Some(redacted) = redact_host_path_fragments(content) {
            output.push_str(&redacted);
        } else if is_runtime_identity_line(content) {
            let leading = &content[..content.len() - content.trim_start().len()];
            output.push_str(leading);
            output.push_str("Janus");
        } else if let Some(key) = sensitive_environment_key(content) {
            let leading = &content[..content.len() - content.trim_start().len()];
            output.push_str(leading);
            output.push_str(key);
            if is_runtime_identity_key(key) {
                output.push_str("=Janus");
            } else {
                output.push_str("=[redacted]");
            }
        } else {
            output.push_str(content);
        }
        output.push_str(ending);
    }
    output
}

fn is_git_bash_host_identity_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("MINGW_NT-")
        || trimmed.starts_with("MINGW64")
        || trimmed.starts_with("MSYS_NT-")
}

fn redact_host_path_fragments(line: &str) -> Option<String> {
    let mut result = String::with_capacity(line.len());
    let mut cursor = 0;
    let mut found = false;
    for (index, _) in line.char_indices() {
        let boundary = line[..index]
            .chars()
            .next_back()
            .is_none_or(|character| character.is_whitespace() || ":=([{<\"'".contains(character));
        if index < cursor || !boundary || !is_host_path_start(&line[index..]) {
            continue;
        }
        if !found {
            result.push_str(&line[..index]);
            found = true;
        } else {
            result.push_str(&line[cursor..index]);
        }
        let end = line[index..]
            .char_indices()
            .find_map(|(offset, character)| character.is_whitespace().then_some(index + offset))
            .unwrap_or(line.len());
        result.push_str("[host path hidden]");
        cursor = end;
    }
    if !found {
        return None;
    }
    result.push_str(&line[cursor..]);
    Some(result)
}

fn is_host_path_start(value: &str) -> bool {
    if value.starts_with("/workspace") {
        return false;
    }
    let bytes = value.as_bytes();
    let unix_drive_path =
        bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b'/';
    let windows_drive_path =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/';
    unix_drive_path
        || windows_drive_path
        || value.starts_with("//")
        || value.starts_with("/mingw")
        || value.starts_with("/usr/")
        || value.starts_with("/bin/")
        || value.starts_with("/etc/")
}

fn sensitive_environment_key(line: &str) -> Option<&str> {
    let trimmed = line.trim_start().trim_end_matches('\r');
    let key = trimmed
        .split_once('=')
        .map(|(key, _)| key)
        .or_else(|| trimmed.split_once(':').map(|(key, _)| key))?;
    const SENSITIVE_KEYS: &[&str] = &[
        "ACLOCAL_PATH",
        "ALLUSERSPROFILE",
        "APPDATA",
        "COMPUTERNAME",
        "COMSPEC",
        "CONFIG_SITE",
        "DISPLAY",
        "EXEPATH",
        "HOMEDRIVE",
        "HOMEPATH",
        "HOSTNAME",
        "HOME",
        "INFOPATH",
        "LOCALAPPDATA",
        "MANPATH",
        "MINGW_CHOST",
        "MINGW_PACKAGE_PREFIX",
        "MINGW_PREFIX",
        "MSYSTEM",
        "MSYSTEM_CARCH",
        "MSYSTEM_CHOST",
        "MSYSTEM_PREFIX",
        "ORIGINAL_PATH",
        "ORIGINAL_TEMP",
        "ORIGINAL_TMP",
        "LOGNAME",
        "OLDPWD",
        "PATH",
        "PATHEXT",
        "PKG_CONFIG_PATH",
        "PKG_CONFIG_SYSTEM_INCLUDE_PATH",
        "PKG_CONFIG_SYSTEM_LIBRARY_PATH",
        "PWD",
        "PS1",
        "PLINK_PROTOCOL",
        "SHLVL",
        "SHELL",
        "SystemDrive",
        "SystemRoot",
        "TEMP",
        "TMP",
        "TMPDIR",
        "USER",
        "USERDOMAIN",
        "USERNAME",
        "USERPROFILE",
        "WINDIR",
        "JANUS_RUNTIME_PID_FILE",
        "_",
    ];
    SENSITIVE_KEYS
        .iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(key))
        .map(|_| key)
}

fn is_runtime_identity_key(key: &str) -> bool {
    ["LOGNAME", "USER", "USERNAME", "USERDOMAIN"]
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(key))
}

fn is_runtime_identity_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.contains('=') || trimmed.chars().any(char::is_whitespace) {
        return false;
    }
    let separator = match (trimmed.rfind('\\'), trimmed.rfind('/')) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(index), None) | (None, Some(index)) => Some(index),
        (None, None) => None,
    };
    let Some(separator) = separator else {
        return false;
    };
    let user = &trimmed[separator + 1..];
    user.eq_ignore_ascii_case("codexsandboxoffline")
        || user.eq_ignore_ascii_case("codexofflinesandbox")
        || user.eq_ignore_ascii_case("janus")
}

fn truncate_tool_text(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}...[truncated]", &text[..end]), true)
}

async fn tool_delegate_cli(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    use janus_infrastructure::id::{CliSessionId, JobId};
    use janus_runtime::interface::{
        DelegatedCliKind, DeploymentCapabilityProbe, ExecutionEnvironment, ExecutionSpec, JobSpec,
        NetworkPolicy, RuntimeCapabilityId, ValidatedCommand,
    };
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
        .ok_or_else(|| ExecutionError::Internal(anyhow::anyhow!("instruction required")))?;
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

    let cli_session_id = match input.get("cli_session_id").and_then(|v| v.as_str()) {
        Some(raw) if !raw.is_empty() => Some(
            CliSessionId::from_str(raw)
                .map_err(|_| ExecutionError::Internal(anyhow::anyhow!("invalid cli_session_id")))?,
        ),
        _ => None,
    };

    let runtime_proj = ensure_session_runtime(ctx).await?;
    let timeout = timeout_ms(input, 600_000).min(3_600_000);
    let execution = ExecutionSpec::new(
        runtime_proj.id,
        working_directory(input)?,
        ValidatedCommand::delegated_cli(cli, instruction, cli_session_id)
            .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("command: {e}")))?,
        ExecutionEnvironment::new(BTreeMap::new(), vec![])
            .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("env: {e}")))?,
        default_limits(timeout),
        NetworkPolicy::DenyAll,
    )
    .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("execution: {e}")))?;

    let job_id = JobId::new();
    let spec = JobSpec::new(
        job_id,
        ctx.session_id,
        ctx.turn_id,
        ctx.tool_call_id,
        execution,
    )
    .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("job spec: {e}")))?;

    let job = ctx
        .runtime
        .start_job(spec)
        .await
        .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("start_job: {e}")))?;

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

/// Read the accumulated output of a background job (bash async or delegate_cli)
/// by its job_id. The job keeps running; this only reads what it has produced.
async fn tool_read_output(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    use janus_infrastructure::id::JobId;
    use janus_runtime::interface::{LogChannel, LogCursor};
    use std::str::FromStr;

    let raw_id = input
        .get("job_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExecutionError::Internal(anyhow::anyhow!("job_id required")))?;
    let job_id = JobId::from_str(raw_id)
        .map_err(|_| ExecutionError::Internal(anyhow::anyhow!("invalid job_id")))?;
    let job = ctx
        .runtime
        .job(job_id)
        .await
        .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("read_output job: {e}")))?;
    let range = ctx
        .runtime
        .log_range(job.log_stream_id, LogCursor::ZERO, 256 * 1024)
        .await
        .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("read_output log: {e}")))?;

    let mut stdout = String::new();
    let mut stderr = String::new();
    for chunk in &range.chunks {
        match chunk.channel {
            LogChannel::Stdout | LogChannel::System => stdout.push_str(&chunk.text),
            LogChannel::Stderr => stderr.push_str(&chunk.text),
        }
    }
    let workspace_root = session_repo(ctx.workspace, ctx.session_id)?;
    let stdout = sanitize_workspace_text(&stdout, &workspace_root);
    let stderr = sanitize_workspace_text(&stderr, &workspace_root);
    let status = format!("{:?}", job.status).to_ascii_lowercase();
    let summary = json!({
        "job_id": raw_id,
        "status": status,
        "stdout_bytes": stdout.len(),
        "stderr_bytes": stderr.len(),
        "done": job.status.is_terminal(),
    });
    let text = format!(
        "job {} (status={})\n--- stdout ---\n{}\n--- stderr ---\n{}",
        raw_id, status, stdout, stderr
    );
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Text { text }],
        summary,
        error_code: None,
        finish_summary: None,
        wait: None,
    })
}

/// Terminate a background job (bash async or delegate_cli) by its job_id.
async fn tool_stop(ctx: &ToolContext<'_>, input: &Value) -> Result<ToolOutcome, ExecutionError> {
    use janus_infrastructure::id::JobId;
    use std::str::FromStr;

    let raw_id = input
        .get("job_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExecutionError::Internal(anyhow::anyhow!("job_id required")))?;
    let job_id = JobId::from_str(raw_id)
        .map_err(|_| ExecutionError::Internal(anyhow::anyhow!("invalid job_id")))?;
    let job = ctx
        .runtime
        .cancel_job(job_id)
        .await
        .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("stop: {e}")))?;
    let status = format!("{:?}", job.status).to_ascii_lowercase();
    let summary = json!({"job_id": raw_id, "status": status});
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Text {
            text: format!("stopped job {raw_id} (status={status})"),
        }],
        summary,
        error_code: None,
        finish_summary: None,
        wait: None,
    })
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

    use super::{
        attach_tool_display, normalize_workspace_command, sanitize_workspace_text,
        truncate_tool_text, validate_workspace_command,
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
            validate_workspace_command("cat /etc/passwd").unwrap_err(),
            "Bash blocked: absolute path `/etc/passwd` is outside the Session workspace; use `/workspace/...` or a workspace-relative path.",
        );
        assert_eq!(
            validate_workspace_command("cat /workspace/../outside").unwrap_err(),
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
