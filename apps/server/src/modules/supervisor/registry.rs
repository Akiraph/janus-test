//! M3 tool registry: fs.* / git.inspect / finish.

use serde_json::json;

use super::types::ToolSpecEntry;

pub const SCHEMA_VERSION: i64 = 1;

pub fn registry() -> Vec<ToolSpecEntry> {
    vec![
        ToolSpecEntry {
            name: "fs.list",
            description: "List a directory under the Session workspace (relative path).",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Relative directory path; empty or \".\" for root"}
                },
                "required": []
            }),
        },
        ToolSpecEntry {
            name: "fs.read",
            description: "Read a Session file as text or supported image (PNG/JPEG/WebP/non-animated GIF).",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        },
        ToolSpecEntry {
            name: "fs.write",
            description: "Write UTF-8 text to a Session-relative path (creates parents).",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        },
        ToolSpecEntry {
            name: "fs.patch",
            description: "Replace an existing Session file with new UTF-8 content after a patch is applied.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        },
        ToolSpecEntry {
            name: "fs.remove",
            description: "Delete a Session file or empty directory.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        },
        ToolSpecEntry {
            name: "git.inspect",
            description: "Read-only git status for the Session workspace.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["status"]}
                },
                "required": []
            }),
        },
        ToolSpecEntry {
            name: "finish",
            description: "Complete the Turn with a structured summary.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "summary": {"type": "string"},
                    "main_changes": {"type": "array", "items": {"type": "string"}},
                    "validation_performed": {"type": "array", "items": {"type": "string"}},
                    "validation_not_performed": {"type": "array", "items": {"type": "string"}},
                    "remaining_risks": {"type": "array", "items": {"type": "string"}}
                },
                "required": [
                    "summary",
                    "main_changes",
                    "validation_performed",
                    "validation_not_performed",
                    "remaining_risks"
                ]
            }),
        },
        // M4 Stage 5 runtime tools. Execution lands in tools.rs; registry
        // entries make them visible to the model schema immediately.
        ToolSpecEntry {
            name: "bash",
            description: "Run a short synchronous shell command in the Session workspace.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "working_directory": {"type": "string", "description": "Workspace-relative path; default \".\""},
                    "timeout_ms": {"type": "integer", "minimum": 1}
                },
                "required": ["command"]
            }),
        },
        ToolSpecEntry {
            name: "job",
            description: "Start a finite background Job in the Session Runtime.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "working_directory": {"type": "string"},
                    "timeout_ms": {"type": "integer", "minimum": 1}
                },
                "required": ["command"]
            }),
        },
        ToolSpecEntry {
            name: "service",
            description: "Start a long-lived Service in the Session Runtime.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "working_directory": {"type": "string"},
                    "impact": {
                        "type": "string",
                        "enum": ["read_only", "ignored_output", "source_writing"]
                    }
                },
                "required": ["command", "impact"]
            }),
        },
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
        ToolSpecEntry {
            name: "update_plan",
            description: "Append an immutable plan version for this Turn.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "plan": {"type": "object"},
                    "evidence": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["plan"]
            }),
        },
        ToolSpecEntry {
            name: "ask_user",
            description: "Ask the user a blocking or best-effort question.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "prompt": {"type": "string"},
                    "choices": {"type": "array", "items": {"type": "string"}},
                    "mode": {"type": "string", "enum": ["blocking", "best_effort"]},
                    "default": {"type": "string"},
                    "expires_in_ms": {"type": "integer", "minimum": 1}
                },
                "required": ["prompt", "mode"]
            }),
        },
    ]
}

pub fn is_registered(name: &str) -> bool {
    registry().iter().any(|t| t.name == name)
}

/// Hard deny list — architecture assertion for tests.
pub fn is_forbidden_tool(name: &str) -> bool {
    // `bash` is a registered Stage-5 tool (routed through Runtime). The
    // forbidden list still blocks unconstrained shell aliases and Main writes.
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
