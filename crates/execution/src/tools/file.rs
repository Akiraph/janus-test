//! File and attachment tools (read/write/edit/delete, attachment list/read/save).
use super::*;

pub(super) async fn tool_read(
    repo: &Path,
    input: &Value,
    handle: &WorkspaceHandle,
    ctx: &ToolContext<'_>,
) -> Result<ToolOutcome, ExecutionError> {
    let raw = input
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or_else(|| ExecutionError::ToolPathInvalid)?;
    let path = match resolve_workspace_path(repo, raw) {
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

pub(crate) fn supported_image_mime(bytes: &[u8]) -> Option<&'static str> {
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
    })
}

pub(super) async fn tool_write(
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
        normalize_workspace_path(path).map_err(|_| ExecutionError::ToolPathInvalid)?;
    // Path validation only (mutation API re-validates).
    let abs = resolve_workspace_path(ctx.workspace_root, path)
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
    })
}

/// Render a compact unified-diff string for the tool summary so the UI can show
/// `+x -y` line counts and an expandable diff. Reuses the Workspace diff LCS.
/// `old=None` means a newly created file (every new line is an addition).
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

pub(super) async fn tool_edit(
    ctx: &ToolContext<'_>,
    handle: &WorkspaceHandle,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    let path = input
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or(ExecutionError::ToolPathInvalid)?;
    let normalized_path =
        normalize_workspace_path(path).map_err(|_| ExecutionError::ToolPathInvalid)?;
    let abs = resolve_workspace_path(ctx.workspace_root, path)
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
    })
}

pub(super) async fn tool_remove(
    ctx: &ToolContext<'_>,
    handle: &WorkspaceHandle,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    let path = input
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or(ExecutionError::ToolPathInvalid)?;
    let normalized_path =
        normalize_workspace_path(path).map_err(|_| ExecutionError::ToolPathInvalid)?;
    let _ = resolve_workspace_path(ctx.workspace_root, path)
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
    })
}

pub(super) async fn tool_attachment_list(
    ctx: &ToolContext<'_>,
) -> Result<ToolOutcome, ExecutionError> {
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
    })
}

pub(super) async fn tool_attachment_read(
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
        });
    }

    let metadata = json!({
        "attachment_id": attachment.id.to_string(),
        "name": attachment.name,
        "mime": attachment.mime,
        "byte_size": attachment.byte_size,
        "kind": "binary",
        "next_action": "Use attachment.save with this attachment_id and a Main workspace path.",
    });
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Json {
            value: metadata.clone(),
        }],
        summary: metadata,
        error_code: None,
        finish_summary: None,
    })
}

pub(super) async fn tool_attachment_save(
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
        normalize_workspace_path(path).map_err(|_| ExecutionError::ToolPathInvalid)?;
    let _ = resolve_workspace_path(ctx.workspace_root, path)
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

pub(crate) async fn read_attachment_bytes(
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
