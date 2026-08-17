//! Tool outcome display formatting and text truncation.
use super::*;

pub(crate) fn attach_tool_display(name: &str, input: &Value, outcome: &mut ToolOutcome) {
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
        "read_output" => {
            let async_task_id = input_string("task_id");
            (
                format!("Read output for {async_task_id}"),
                ToolDisplayBody::Structured {
                    value: outcome.summary.clone(),
                },
            )
        }
        "stop" => {
            let async_task_id = input_string("task_id");
            (
                format!("Stopped async_task {async_task_id}"),
                ToolDisplayBody::None,
            )
        }
        "todo" => (
            "Updated plan".into(),
            ToolDisplayBody::Structured {
                value: input.clone(),
            },
        ),
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
