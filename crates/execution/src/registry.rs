//! Execution tool registry.
//!
//! Tools are grouped into capability sets so the model only sees tools it can
//! actually use for the current turn:
//! - core: always available (read/write/edit/delete/bash + control tools).
//! - runtime: global async task process-control tools backed by Runtime.
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
    /// Requires a bound Runtime interface (global async-task control).
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
                description: "Read a file in the Main workspace as text or a supported image. Supports line offset and limit.",
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
                description: "Write UTF-8 text to a Main workspace path. Creates the file if it does not exist and creates parent directories.",
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
                description: "Apply one or more exact text replacements to an existing Main workspace file. Each oldText must be unique in the file and edits must not overlap. Use write for new files.",
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
                description: "Delete a Main workspace file or empty directory.",
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
                description: "Run a shell command in the Main workspace. Use for inspection, git, builds, tests, and other repository work. mode=sync (default) returns the result; mode=async starts a global background task and its completion is delivered to this session as a new system Turn.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"},
                        "mode": {"type": "string", "enum": ["sync", "async"], "description": "sync (default) returns when the command exits; async starts a global task and returns immediately"},
                        "working_directory": {"type": "string", "description": "Path passed directly to Git Bash; default is the Main workspace"},
                        "timeout_ms": {"type": "integer", "minimum": 1}
                    },
                    "required": ["command"]
                }),
            },
            ToolSet::Core,
        ),
        (
            ToolSpecEntry {
                name: "read_output",
                description: "Read the current accumulated output of a global async bash task. Pass the task_id returned when it was started.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "task_id": {"type": "string"}
                    },
                    "required": ["task_id"]
                }),
            },
            ToolSet::Runtime,
        ),
        (
            ToolSpecEntry {
                name: "stop",
                description: "Terminate a global async bash task by its task_id.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "task_id": {"type": "string"}
                    },
                    "required": ["task_id"]
                }),
            },
            ToolSet::Runtime,
        ),
        (
            ToolSpecEntry {
                name: "active_sessions",
                description: "List every active session in this project so parallel sessions can coordinate their work.",
                parameters: json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
            ToolSet::Core,
        ),
        (
            ToolSpecEntry {
                name: "read_session",
                description: "Read the recent timeline of another session in this project.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "session_id": {"type": "string"},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 100}
                    },
                    "required": ["session_id"]
                }),
            },
            ToolSet::Core,
        ),
        (
            ToolSpecEntry {
                name: "memory",
                description: "List, set, update, or delete persistent project memory. Memory is injected into every new Turn and remains after context compaction.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "enum": ["list", "set", "delete"]},
                        "key": {"type": "string"},
                        "content": {"type": "string"}
                    },
                    "required": ["action"]
                }),
            },
            ToolSet::Core,
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
                description: "Save a Session attachment by ID to a Main workspace path so it can be used by the project.",
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
