//! Execution tool registry.
//!
//! Tools are grouped into capability sets so the model only sees tools it can
//! actually use for the current turn:
//! - core: always available (read/write/edit/delete/bash + control tools).
//! - runtime: job/service/delegate_cli tools backed by Runtime.
//! - attachment: only when the session has at least one attached file.
//!
//! `fs.list` and `git.inspect` were removed: the model uses `bash` for both
//! `ls` and read-only `git status/diff/log`. `finish` was removed: a Turn ends
//! when the model replies without tool calls (execution already handles
//! that path).

use serde_json::json;

use super::types::ToolSpecEntry;

pub const SCHEMA_VERSION: i64 = 1;

/// Capability buckets. A tool belongs to exactly one bucket; `available_tools`
/// unions the buckets that apply to a given turn.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToolSet {
    /// Available on every Turn regardless of context.
    Core,
    /// Requires a bound Runtime interface (bash/job/service/delegate_cli).
    Runtime,
    /// Requires at least one attached file in the session.
    Attachment,
}

/// Full registry, each entry tagged with its capability bucket.
fn full_registry() -> Vec<(ToolSpecEntry, ToolSet)> {
    vec![
        // ── core fs + control ──────────────────────────────────────────────
        (
            ToolSpecEntry {
                name: "read",
                description: "Read a Session file as text or supported image (PNG/JPEG/WebP/non-animated GIF). Supports line offset and limit.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "offset": {"type": "integer", "minimum": 1, "description": "1-indexed line to start reading from"},
                        "limit": {"type": "integer", "minimum": 1, "description": "Maximum number of lines to read"}
                    },
                    "required": ["path"]
                }),
            },
            ToolSet::Core,
        ),
        (
            ToolSpecEntry {
                name: "write",
                description: "Write UTF-8 text to a Session-relative path. Creates the file if it does not exist (overwrites if it does) and creates parent directories.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "content": {"type": "string"}
                    },
                    "required": ["path", "content"]
                }),
            },
            ToolSet::Core,
        ),
        (
            ToolSpecEntry {
                name: "edit",
                description: "Apply one or more exact text replacements to an existing Session file. Each oldText must be unique in the file and edits must not overlap. Use write for new files.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "edits": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "oldText": {"type": "string"},
                                    "newText": {"type": "string"}
                                },
                                "required": ["oldText", "newText"]
                            }
                        }
                    },
                    "required": ["path", "edits"]
                }),
            },
            ToolSet::Core,
        ),
        (
            ToolSpecEntry {
                name: "delete",
                description: "Delete a Session file or empty directory.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    },
                    "required": ["path"]
                }),
            },
            ToolSet::Core,
        ),
        (
            ToolSpecEntry {
                name: "bash",
                description: "Run a shell command in the Session workspace. Use for ls, grep, find, read-only git, builds, and tests. mode=sync (default) waits for the result; mode=async runs it in the background (the Turn can keep working and wait on it later, like waiting on a delegated CLI).",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"},
                        "mode": {"type": "string", "enum": ["sync", "async"], "description": "sync (default) blocks until exit; async runs in the background and the Turn may wait on it"},
                        "working_directory": {"type": "string", "description": "Workspace-relative path; default \".\""},
                        "timeout_ms": {"type": "integer", "minimum": 1}
                    },
                    "required": ["command"]
                }),
            },
            ToolSet::Core,
        ),
        (
            ToolSpecEntry {
                name: "delegate_cli",
                description: "Launch or follow up a constrained Claude Code / Codex Job.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "cli": {"type": "string", "enum": ["claude_code", "codex"]},
                        "instruction": {"type": "string"},
                        "working_directory": {"type": "string"},
                        "cli_session_id": {"type": "string", "description": "Reuse an existing CLI session for follow-up"}
                    },
                    "required": ["cli", "instruction"]
                }),
            },
            ToolSet::Runtime,
        ),
        (
            ToolSpecEntry {
                name: "read_output",
                description: "Read the current accumulated output (stdout/stderr) of a background job started by bash (mode=async) or delegate_cli. The job keeps running; this only reads what it has produced so far. Pass the job_id returned when the job was started.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "job_id": {"type": "string"}
                    },
                    "required": ["job_id"]
                }),
            },
            ToolSet::Runtime,
        ),
        (
            ToolSpecEntry {
                name: "stop",
                description: "Terminate a background job started by bash (mode=async) or delegate_cli by its job_id.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "job_id": {"type": "string"}
                    },
                    "required": ["job_id"]
                }),
            },
            ToolSet::Runtime,
        ),
        // ── control tools (always available) ──────────────────────────────
        (
            ToolSpecEntry {
                name: "todo",
                description: "Append an immutable todo list version for this Turn.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "todos": {"type": "array", "items": {"type": "object"}},
                        "evidence": {"type": "array", "items": {"type": "string"}}
                    },
                    "required": ["todos"]
                }),
            },
            ToolSet::Core,
        ),
        (
            ToolSpecEntry {
                name: "ask_user",
                description: "Ask the user one concise-sentence question. Provide concise choices; each choice may be a string or an object with a short one-sentence annotation. Set multiple true when more than one choice may be selected. The UI also offers a manual answer and decline option. Use blocking when the turn must wait, or non_blocking when it may continue after the answer window closes. Non-blocking asks require expires_in_ms and do not use a preselected answer.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "prompt": {"type": "string"},
                        "choices": {
                            "type": "array",
                            "items": {
                                "oneOf": [
                                    {"type": "string"},
                                    {
                                        "type": "object",
                                        "properties": {
                                            "label": {"type": "string"},
                                            "annotation": {"type": "string"}
                                        },
                                        "required": ["label"],
                                        "additionalProperties": false
                                    }
                                ]
                            },
                            "minItems": 2
                        },
                        "multiple": {"type": "boolean", "default": false},
                        "mode": {"type": "string", "enum": ["blocking", "non_blocking"]},
                        "expires_in_ms": {"type": "integer", "minimum": 1, "description": "Required for non-blocking asks."}
                    },
                    "required": ["prompt", "mode"]
                }),
            },
            ToolSet::Core,
        ),
        // ── attachment tools (only when the session has attachments) ──────
        (
            ToolSpecEntry {
                name: "attachment_list",
                description: "List files the user has made available to this Session, including reusable IDs and metadata.",
                parameters: json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
            ToolSet::Attachment,
        ),
        (
            ToolSpecEntry {
                name: "attachment_read",
                description: "Read a Session attachment by ID. Returns bounded UTF-8 text, a supported image, or binary metadata.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "attachment_id": {"type": "string"}
                    },
                    "required": ["attachment_id"]
                }),
            },
            ToolSet::Attachment,
        ),
        (
            ToolSpecEntry {
                name: "attachment_save",
                description: "Save a Session attachment by ID to a Session-relative workspace path so it can be used by the project.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "attachment_id": {"type": "string"},
                        "path": {"type": "string"}
                    },
                    "required": ["attachment_id", "path"]
                }),
            },
            ToolSet::Attachment,
        ),
    ]
}

/// Tools exposed to the model for a given turn. Runtime tools are always
/// available; attachments are included only when the Session has them.
pub fn available_tools(has_attachments: bool) -> Vec<ToolSpecEntry> {
    full_registry()
        .into_iter()
        .filter(|(_, set)| match set {
            ToolSet::Core => true,
            ToolSet::Runtime => true,
            ToolSet::Attachment => has_attachments,
        })
        .map(|(t, _)| t)
        .collect()
}

pub fn is_registered(name: &str) -> bool {
    full_registry().iter().any(|(t, _)| t.name == name)
}

/// Hard deny list — architecture assertion for tests. The old shell/main
/// aliases are still blocked even though they are no longer registered.
pub fn is_forbidden_tool(name: &str) -> bool {
    matches!(
        name,
        "git.write"
            | "git.commit"
            | "git.push"
            | "git.stage"
            | "shell"
            | "apply"
            | "sync"
            | "main.write"
    )
}
