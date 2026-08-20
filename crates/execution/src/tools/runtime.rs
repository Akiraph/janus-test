//! Bash/runtime and process-control tools (bash, read_output, stop).
use super::*;

/// Bytes of an async task's log stream one `read_output` call pulls.
const READ_OUTPUT_WINDOW_BYTES: usize = 256 * 1024;

fn default_limits(timeout_ms: u64) -> janus_runtime::interface::ResourceLimits {
    janus_runtime::interface::ResourceLimits {
        timeout_ms,
        memory_bytes: u64::MAX,
        cpu_millis: u64::MAX,
        pids: u32::MAX,
        temporary_disk_bytes: u64::MAX,
        open_files: u32::MAX,
    }
}

async fn ensure_project_runtime(
    ctx: &ToolContext<'_>,
) -> Result<janus_runtime::interface::RuntimeProjection, ExecutionError> {
    use janus_infrastructure::id::RuntimeId;
    use janus_runtime::interface::{RuntimeScope, RuntimeSpec};

    let existing = ctx
        .runtime
        .current_runtime(RuntimeScope::project(ctx.project_id))
        .await
        .map_err(ExecutionError::Runtime)?;
    let workspace_root = ctx.workspace_root.to_path_buf();
    let abs = workspace_root.canonicalize().unwrap_or(workspace_root);
    let spec = RuntimeSpec::new(
        existing.map_or_else(RuntimeId::new, |runtime| runtime.id),
        RuntimeScope::project(ctx.project_id),
        abs,
        default_limits(30_000),
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

pub(super) async fn tool_bash(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    use janus_runtime::interface::{ExecutionSpec, ValidatedCommand};

    let display_command = input
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExecutionError::Internal(anyhow::anyhow!("command required")))?;
    if display_command.trim().is_empty() {
        return Ok(fail_text("command is empty", "VALIDATION_FAILED"));
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

    let timeout = timeout_ms(input, u64::MAX);
    let repo = ctx.workspace_root.to_path_buf();
    let cwd = working_directory(input)?;
    let fallback_cwd = local_working_directory(&repo, &cwd)?;
    let command = display_command.to_owned();

    let runtime_proj = match ensure_project_runtime(ctx).await {
        Ok(runtime) => runtime,
        Err(ExecutionError::Runtime(error)) if error.retryable() => {
            tracing::warn!(%error, "preferred Runtime unavailable; using system Bash fallback");
            return run_local_sync(ctx, &command, display_command, &fallback_cwd).await;
        }
        Err(error) => return Err(error),
    };
    let environment = execution_environment(ctx)?;
    let spec = ExecutionSpec::new(
        runtime_proj.id,
        cwd,
        ValidatedCommand::shell(&command)
            .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("command: {e}")))?,
        environment,
        default_limits(timeout),
    )
    .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("execution: {e}")))?;

    let result = match ctx.runtime.execute_sync(spec).await {
        Ok(result) => result,
        Err(error) if error.retryable() => {
            tracing::warn!(
                %error,
                "Runtime sync execution unavailable; using local Git Bash fallback"
            );
            return run_local_sync(ctx, &command, display_command, &fallback_cwd).await;
        }
        Err(error) => return Err(ExecutionError::Runtime(error)),
    };

    bash_outcome(BashOutcomeInput {
        command: display_command,
        exit_code: result.exit.exit_code,
        timed_out: result.timed_out,
        duration_ms: result.duration_ms,
        truncated: result.truncated,
        stdout: &result.stdout,
        stderr: &result.stderr,
        secret_value: ctx.git_token,
    })
}

/// async bash mode: a finite background AsyncTask. Requires a bound Runtime because
/// the background-async_task machinery (log stream and global task delivery) lives there.
async fn tool_bash_async(
    ctx: &ToolContext<'_>,
    display_command: &str,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    use janus_infrastructure::id::AsyncTaskId;
    use janus_runtime::interface::{AsyncTaskSpec, ExecutionSpec, ValidatedCommand};

    let cwd = working_directory(input)?;
    let command = display_command.to_owned();
    let runtime_proj = ensure_project_runtime(ctx).await?;
    let timeout = timeout_ms(input, u64::MAX);
    let execution = ExecutionSpec::new(
        runtime_proj.id,
        cwd,
        ValidatedCommand::shell(command)
            .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("command: {e}")))?,
        execution_environment(ctx)?,
        default_limits(timeout),
    )
    .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("execution: {e}")))?;
    let async_task_id = AsyncTaskId::new();
    let spec = AsyncTaskSpec::new(
        async_task_id,
        ctx.session_id,
        ctx.turn_id,
        ctx.tool_call_id,
        execution,
    )
    .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("async_task spec: {e}")))?;

    let async_task = ctx
        .runtime
        .start_async_task(spec)
        .await
        .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("start_async_task: {e}")))?;

    let summary = json!({
        "task_id": async_task.id.to_string(),
        "status": format!("{:?}", async_task.status).to_ascii_lowercase(),
        "log_stream_id": async_task.log_stream_id.to_string(),
        "command_summary": async_task.command_summary,
        "mode": "async",
    });
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Text {
            text: format!(
                "async task {} started ({})",
                async_task.id, async_task.command_summary
            ),
        }],
        summary,
        error_code: None,
        finish_summary: None,
    })
}

/// Local fallback for sync bash when no Runtime is bound. Windows uses Git
/// Bash (`bash -c`), other platforms use `/bin/sh -c`.
async fn run_local_sync(
    ctx: &ToolContext<'_>,
    command: &str,
    display_command: &str,
    cwd: &Path,
) -> Result<ToolOutcome, ExecutionError> {
    let Some(program) = bash_program() else {
        let detail = if cfg!(windows) {
            "Git Bash is not installed or could not be located"
        } else {
            "/bin/bash is not available"
        };
        return Ok(fail_text(detail, "BASH_UNAVAILABLE"));
    };
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(["-c", command]);
    if let Some(token) = ctx.git_token {
        cmd.env("GH_TOKEN", token);
        cmd.env("GITHUB_TOKEN", token);
    }
    cmd.current_dir(cwd);
    let started = std::time::Instant::now();
    let output = cmd.output().await;
    let (timed_out, exit_code, stdout, stderr) = match output {
        Ok(out) => {
            let stdout = decode_process_output(&out.stdout, 1024 * 1024);
            let stderr = decode_process_output(&out.stderr, 1024 * 1024);
            (false, out.status.code(), stdout.text, stderr.text)
        }
        Err(e) => {
            return Ok(fail_text(
                &format!("failed to run command: {e}"),
                "COMMAND_FAILED",
            ));
        }
    };
    let duration_ms = started.elapsed().as_millis() as u64;
    bash_outcome(BashOutcomeInput {
        command: display_command,
        exit_code,
        timed_out,
        duration_ms,
        truncated: false,
        stdout: &stdout,
        stderr: &stderr,
        secret_value: ctx.git_token,
    })
}

fn execution_environment(
    ctx: &ToolContext<'_>,
) -> Result<janus_runtime::interface::ExecutionEnvironment, ExecutionError> {
    use janus_infrastructure::secrets::Secret;
    use janus_runtime::interface::SecretEnvironmentVariable;
    let ordinary = std::collections::BTreeMap::new();
    let mut secrets = Vec::new();
    if let Some(token) = ctx.git_token {
        secrets.push(
            SecretEnvironmentVariable::new("GH_TOKEN", Secret::new(token.to_owned()))
                .map_err(|error| ExecutionError::Internal(anyhow::anyhow!(error)))?,
        );
        secrets.push(
            SecretEnvironmentVariable::new("GITHUB_TOKEN", Secret::new(token.to_owned()))
                .map_err(|error| ExecutionError::Internal(anyhow::anyhow!(error)))?,
        );
    }
    janus_runtime::interface::ExecutionEnvironment::new(ordinary, secrets)
        .map_err(|error| ExecutionError::Internal(anyhow::anyhow!("env: {error}")))
}

fn local_working_directory(
    repo: &Path,
    cwd: &janus_runtime::interface::RelativeWorkingDirectory,
) -> Result<std::path::PathBuf, ExecutionError> {
    let candidate = repo.join(cwd.as_str());
    let canonical_cwd = candidate
        .canonicalize()
        .map_err(|error| ExecutionError::Internal(anyhow::anyhow!("working directory: {error}")))?;
    Ok(canonical_cwd)
}

/// Build a ToolOutcome from a completed (sync) bash run. Shared by the
/// Runtime path and the local fallback.
struct BashOutcomeInput<'a> {
    command: &'a str,
    exit_code: Option<i32>,
    timed_out: bool,
    duration_ms: u64,
    truncated: bool,
    stdout: &'a str,
    stderr: &'a str,
    secret_value: Option<&'a str>,
}

fn bash_outcome(input: BashOutcomeInput<'_>) -> Result<ToolOutcome, ExecutionError> {
    let BashOutcomeInput {
        command,
        exit_code,
        timed_out,
        duration_ms,
        truncated,
        stdout,
        stderr,
        secret_value,
    } = input;
    let command = sanitize_secret(command, secret_value);
    let stdout = sanitize_secret(stdout, secret_value);
    let stderr = sanitize_secret(stderr, secret_value);
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
    })
}

fn sanitize_secret(value: &str, secret: Option<&str>) -> String {
    secret.filter(|secret| !secret.is_empty()).map_or_else(
        || value.to_owned(),
        |secret| value.replace(secret, "[secret redacted]"),
    )
}

pub(super) fn truncate_tool_text(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}...[truncated]", &text[..end]), true)
}

/// Read the accumulated output of a background bash async_task
/// by its task_id. The task keeps running; this only reads what it has produced.
pub(super) async fn tool_read_output(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    use janus_infrastructure::id::AsyncTaskId;
    use janus_runtime::interface::{LogChannel, LogCursor};
    use std::str::FromStr;

    let raw_id = input
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExecutionError::Internal(anyhow::anyhow!("task_id required")))?;
    let async_task_id = AsyncTaskId::from_str(raw_id)
        .map_err(|_| ExecutionError::Internal(anyhow::anyhow!("invalid task_id")))?;
    let async_task =
        ctx.runtime.async_task(async_task_id).await.map_err(|e| {
            ExecutionError::Internal(anyhow::anyhow!("read_output async_task: {e}"))
        })?;
    let range = ctx
        .runtime
        .log_range(
            async_task.log_stream_id,
            LogCursor::ZERO,
            READ_OUTPUT_WINDOW_BYTES,
        )
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
    let stdout = sanitize_secret(&stdout, ctx.git_token);
    let stderr = sanitize_secret(&stderr, ctx.git_token);
    // The read window and the per-channel display caps both drop output. Say so
    // in the summary: without it the caller cannot tell a task that printed
    // nothing more from one whose remaining output was never fetched.
    let window_reached_end = range
        .chunks
        .last()
        .is_none_or(|chunk| chunk.end_cursor >= range.stream.next_cursor);
    let (stdout_out, stdout_truncated) = truncate_tool_text(&stdout, 8_000);
    let (stderr_out, stderr_truncated) = truncate_tool_text(&stderr, 4_000);
    let truncated =
        !window_reached_end || range.stream.truncated || stdout_truncated || stderr_truncated;
    let status = format!("{:?}", async_task.status).to_ascii_lowercase();
    let exit_code = async_task.exit.as_ref().and_then(|exit| exit.exit_code);
    let summary = json!({
        "task_id": raw_id,
        "command": async_task.command_summary,
        "status": status,
        "exit_code": exit_code,
        "stdout": stdout_out,
        "stderr": stderr_out,
        "stdout_bytes": stdout.len(),
        "stderr_bytes": stderr.len(),
        "truncated": truncated,
        "next_cursor": range.stream.next_cursor.value(),
        "done": async_task.status.is_terminal(),
    });
    let text = format!(
        "task {} (status={}, exit={:?}, truncated={})\n--- stdout ---\n{}\n--- stderr ---\n{}",
        raw_id, status, exit_code, truncated, stdout_out, stderr_out
    );
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Text { text }],
        summary,
        error_code: None,
        finish_summary: None,
    })
}

/// Terminate a global async bash task by its task_id.
pub(super) async fn tool_stop(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    use janus_infrastructure::id::AsyncTaskId;
    use std::str::FromStr;

    let raw_id = input
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ExecutionError::Internal(anyhow::anyhow!("task_id required")))?;
    let async_task_id = AsyncTaskId::from_str(raw_id)
        .map_err(|_| ExecutionError::Internal(anyhow::anyhow!("invalid task_id")))?;
    let async_task = ctx
        .runtime
        .cancel_async_task(async_task_id)
        .await
        .map_err(|e| ExecutionError::Internal(anyhow::anyhow!("stop: {e}")))?;
    let status = format!("{:?}", async_task.status).to_ascii_lowercase();
    let summary = json!({"task_id": raw_id, "status": status});
    Ok(ToolOutcome {
        disposition: ToolExecutionDisposition::Succeeded,
        parts: vec![ToolResultPart::Text {
            text: format!("stopped task {raw_id} (status={status})"),
        }],
        summary,
        error_code: None,
        finish_summary: None,
    })
}
