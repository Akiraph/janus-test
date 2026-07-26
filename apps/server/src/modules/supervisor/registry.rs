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
                    "main_changes": {"type": "string"},
                    "risks": {"type": "string"}
                },
                "required": ["summary"]
            }),
        },
    ]
}

pub fn is_registered(name: &str) -> bool {
    registry().iter().any(|t| t.name == name)
}

/// Hard deny list — architecture assertion for tests.
pub fn is_forbidden_tool(name: &str) -> bool {
    matches!(
        name,
        "git.write"
            | "git.commit"
            | "git.push"
            | "git.stage"
            | "bash"
            | "shell"
            | "apply"
            | "sync"
            | "main.write"
    )
}
