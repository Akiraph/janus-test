//! Bash/runtime and process-control tools (bash, delegate_cli, read_output, stop).
use super::*;

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

pub(super) async fn tool_bash(ctx: &ToolContext<'_>, input: &Value) -> Result<ToolOutcome, ExecutionError> {
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

    bash_outcome(BashOutcomeInput {
        command: display_command,
        exit_code: result.exit.exit_code,
        timed_out: result.timed_out,
        duration_ms: result.duration_ms,
        truncated: result.truncated,
        stdout: &result.stdout,
        stderr: &result.stderr,
        workspace_root: &repo,
    })
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
    bash_outcome(BashOutcomeInput {
        command: display_command,
        exit_code,
        timed_out,
        duration_ms,
        truncated: false,
        stdout: &stdout,
        stderr: &stderr,
        workspace_root,
    })
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

pub(super) fn validate_workspace_command(command: &str) -> Result<(), String> {
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

pub(super) fn normalize_workspace_command(command: &str, workspace_prefix: &str) -> String {
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
struct BashOutcomeInput<'a> {
    command: &'a str,
    exit_code: Option<i32>,
    timed_out: bool,
    duration_ms: u64,
    truncated: bool,
    stdout: &'a str,
    stderr: &'a str,
    workspace_root: &'a Path,
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
        workspace_root,
    } = input;
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

pub(super) fn sanitize_workspace_text(text: &str, workspace_root: &Path) -> String {
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
    variants.sort_by_key(|right| std::cmp::Reverse(right.len()));
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
        } else if let Some(key) = sensitive_environment_key(content) {
            let leading = &content[..content.len() - content.trim_start().len()];
            output.push_str(leading);
            output.push_str(key);
            if is_runtime_identity_key(key) {
                output.push_str("=Janus");
            } else {
                output.push_str("=[redacted]");
            }
        } else if let Some(redacted) = redact_host_path_fragments(content) {
            output.push_str(&redacted);
        } else if is_runtime_identity_line(content) {
            let leading = &content[..content.len() - content.trim_start().len()];
            output.push_str(leading);
            output.push_str("Janus");
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

pub(super) async fn tool_delegate_cli(
    ctx: &ToolContext<'_>,
    input: &Value,
) -> Result<ToolOutcome, ExecutionError> {
    use janus_infrastructure::id::{CliSessionId, JobId};
    use janus_runtime::interface::{
        DelegatedCliKind, DelegatedCliLaunchOptions, DeploymentCapabilityProbe,
        ExecutionEnvironment, ExecutionSpec, JobSpec, NetworkPolicy, RuntimeCapabilityId,
        ValidatedCommand,
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
    let launch = match DelegatedCliLaunchOptions::from_raw(
        input.get("model").and_then(|value| value.as_str()),
        input.get("effort").and_then(|value| value.as_str()),
        input.get("access").and_then(|value| value.as_str()),
    ) {
        Ok(options) => options,
        Err(error) => return Ok(fail_text(&error.to_string(), "VALIDATION_FAILED")),
    };

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
        ValidatedCommand::delegated_cli_with_options(
            cli,
            instruction,
            cli_session_id,
            Some(launch),
        )
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
pub(super) async fn tool_read_output(
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
pub(super) async fn tool_stop(ctx: &ToolContext<'_>, input: &Value) -> Result<ToolOutcome, ExecutionError> {
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

