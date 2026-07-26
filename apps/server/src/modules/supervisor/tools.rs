//! Execute registered Supervisor tools against a Session workspace.

use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::adapters::git::{GitRunner, SystemGit};
use crate::modules::workspace_sync::interface::{
    FileMutation, WorkspaceHandle, WorkspaceSyncInterface,
};
use crate::platform::id::SessionId;
use crate::platform::path::PathError;

use super::paths::resolve_session_path;
use super::registry::{is_forbidden_tool, is_registered};
use super::types::{SupervisorError, ToolOutcome, ToolResultPart};

/// Hard image decode limits (SES-TOOL-READ-03 subset).
const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_EDGE_PX: u32 = 32_768;
const MAX_PIXELS: u64 = 100_000_000;

pub struct ToolContext<'a> {
    pub session_id: SessionId,
    pub workspace: &'a WorkspaceSyncInterface,
    pub actor: Value,
}

pub async fn execute_tool(
    ctx: &ToolContext<'_>,
    name: &str,
    input: &Value,
) -> Result<ToolOutcome, SupervisorError> {
    if is_forbidden_tool(name) || !is_registered(name) {
        return Ok(ToolOutcome {
            ok: false,
            parts: vec![ToolResultPart::Text {
                text: format!("tool not allowed: {name}"),
            }],
            summary: json!({"error": "TOOL_NOT_ALLOWED", "name": name}),
            error_code: Some("TOOL_NOT_ALLOWED".into()),
            finish_summary: None,
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
        "finish" => tool_finish(input),
        other => Ok(ToolOutcome {
            ok: false,
            parts: vec![ToolResultPart::Text {
                text: format!("unknown tool: {other}"),
            }],
            summary: json!({"error": "TOOL_NOT_ALLOWED"}),
            error_code: Some("TOOL_NOT_ALLOWED".into()),
            finish_summary: None,
        }),
    }
}

fn session_repo(
    workspace: &WorkspaceSyncInterface,
    session_id: SessionId,
) -> Result<std::path::PathBuf, SupervisorError> {
    Ok(
        crate::modules::workspace_sync::session_copy::session_repo_abs(
            workspace.data_root(),
            session_id,
        ),
    )
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
        ok: true,
        parts: vec![ToolResultPart::Json {
            value: json!({"entries": entries}),
        }],
        summary: json!({"count": entries.len()}),
        error_code: None,
        finish_summary: None,
    })
}

fn path_invalid() -> Result<ToolOutcome, SupervisorError> {
    Ok(fail_text("invalid path", "TOOL_PATH_INVALID"))
}

fn fail_text(msg: &str, code: &str) -> ToolOutcome {
    ToolOutcome {
        ok: false,
        parts: vec![ToolResultPart::Text {
            text: msg.to_owned(),
        }],
        summary: json!({"error": code, "detail": msg}),
        error_code: Some(code.into()),
        finish_summary: None,
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
        ok: true,
        parts: vec![ToolResultPart::Text { text: text.clone() }],
        summary: json!({"path": raw, "kind": "text", "bytes": text.len()}),
        error_code: None,
        finish_summary: None,
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
        ok: true,
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
        ok: true,
        parts: vec![ToolResultPart::Text {
            text: format!("wrote {path} -> {}", rev.0),
        }],
        summary: json!({"path": path, "revision": rev.0}),
        error_code: None,
        finish_summary: None,
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
        ok: true,
        parts: vec![ToolResultPart::Text {
            text: format!("removed {path} -> {}", rev.0),
        }],
        summary: json!({"path": path, "revision": rev.0}),
        error_code: None,
        finish_summary: None,
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
                ok: true,
                parts: vec![ToolResultPart::Json {
                    value: summary.clone(),
                }],
                summary,
                error_code: None,
                finish_summary: None,
            })
        }
        Err(e) => Ok(fail_text(
            &format!("git status failed: {e}"),
            "TOOL_PATH_INVALID",
        )),
    }
}

fn tool_finish(input: &Value) -> Result<ToolOutcome, SupervisorError> {
    let summary_text = input
        .get("summary")
        .and_then(|s| s.as_str())
        .unwrap_or("done");
    let finish = json!({
        "summary": summary_text,
        "main_changes": input.get("main_changes").and_then(|v| v.as_str()).unwrap_or(""),
        "risks": input.get("risks").and_then(|v| v.as_str()).unwrap_or(""),
    });
    Ok(ToolOutcome {
        ok: true,
        parts: vec![ToolResultPart::Text {
            text: format!("finished: {summary_text}"),
        }],
        summary: finish.clone(),
        error_code: None,
        finish_summary: Some(finish),
    })
}
