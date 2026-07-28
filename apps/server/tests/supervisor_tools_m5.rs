//! Stage 5: supervisor runtime tools (bash / plan / ask / finish-defer).

mod support;

use std::{net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

use janus_server::modules::supervisor::tools::{ToolContext, execute_tool};
use janus_server::platform::id::{JobId, LogStreamId, RuntimeId, SessionId, ToolCallId, TurnId};
use janus_server::{
    AppState,
    config::{Config, RunMode},
};
use serde_json::json;
use tempfile::TempDir;

fn test_config(data_root: PathBuf) -> Config {
    Config {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        data_root,
        mode: RunMode::Development,
        development_auth: true,
        webauthn_rp_name: "Janus Test".into(),
        webauthn_rp_id: "localhost".into(),
        public_origin: url::Url::parse("http://localhost").expect("url"),
        event_heartbeat: Duration::from_millis(50),
    }
}

async fn seed(state: &AppState) -> anyhow::Result<janus_server::platform::id::ProjectId> {
    let pool = state.database().pool();
    let now = "2026-01-01T00:00:00.000Z";
    sqlx::query("INSERT INTO tenants (id, created_at) VALUES ('tenant-test', ?)")
        .bind(now)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO owners (id, tenant_id, display_name, created_at) \
         VALUES ('owner-test', 'tenant-test', 'Owner', ?)",
    )
    .bind(now)
    .execute(pool)
    .await?;
    let project_id = janus_server::platform::id::ProjectId::new();
    sqlx::query(
        "INSERT INTO projects \
         (id, owner_id, tenant_id, name, state, repo_access, repo_url, \
          version, created_at, updated_at, last_activity_at) \
         VALUES (?, 'owner-test', 'tenant-test', 'p', 'ready', 'public_https', \
                 'https://example.com/r.git', 'v1', ?, ?, ?)",
    )
    .bind(project_id.to_string())
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    let main_managed = format!("workspaces/main/{project_id}/repo");
    let main_abs = state.config().data_root.join(&main_managed);
    std::fs::create_dir_all(main_abs.join("src"))?;
    std::fs::write(main_abs.join("README.md"), b"# main\n")?;
    support::init_git_repo(&main_abs)?;
    state
        .workspace_sync()
        .ensure_main_copy(project_id, &main_managed, "test", json!({"kind": "test"}))
        .await?;
    Ok(project_id)
}

#[tokio::test]
async fn bash_tool_runs_sync_command() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let state = AppState::initialize(test_config(dir.path().into())).await?;
    let project_id = seed(&state).await?;
    let actor = json!({"kind": "test"});
    let session = state
        .sessions()
        .create_session(project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;
    let started = state
        .sessions()
        .post_message(session_id, "run bash", &session.version, actor.clone())
        .await?;
    let turn_id = TurnId::from_str(&started.turn_id)?;

    let ctx = ToolContext {
        session_id,
        turn_id,
        tool_call_id: ToolCallId::new(),
        workspace: state.workspace_sync(),
        runtime: Some(state.runtime()),
        pool: state.database().pool(),
        actor,
    };
    let out = execute_tool(
        &ctx,
        "bash",
        &json!({"command": "echo hello-stage5", "timeout_ms": 10_000}),
    )
    .await?;
    assert!(out.ok, "{:?}", out.summary);
    assert!(out.wait.is_none());
    let text = match &out.parts[0] {
        janus_server::modules::supervisor::types::ToolResultPart::Text { text } => text.clone(),
        _ => String::new(),
    };
    assert!(text.contains("hello-stage5"), "{text}");
    Ok(())
}

#[tokio::test]
async fn update_plan_and_ask_user_tools() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let state = AppState::initialize(test_config(dir.path().into())).await?;
    let project_id = seed(&state).await?;
    let actor = json!({"kind": "test"});
    let session = state
        .sessions()
        .create_session(project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;
    let started = state
        .sessions()
        .post_message(session_id, "plan", &session.version, actor.clone())
        .await?;
    let turn_id = TurnId::from_str(&started.turn_id)?;
    let ctx = ToolContext {
        session_id,
        turn_id,
        tool_call_id: ToolCallId::new(),
        workspace: state.workspace_sync(),
        runtime: Some(state.runtime()),
        pool: state.database().pool(),
        actor,
    };

    let plan = execute_tool(
        &ctx,
        "update_plan",
        &json!({"plan": {"steps": ["a", "b"]}, "evidence": ["e1"]}),
    )
    .await?;
    assert!(plan.ok, "{:?}", plan.summary);
    assert!(plan.summary.get("plan_version_id").is_some());

    let ask = execute_tool(
        &ctx,
        "ask_user",
        &json!({"prompt": "continue?", "mode": "blocking", "choices": ["yes", "no"]}),
    )
    .await?;
    assert!(ask.ok, "{:?}", ask.summary);
    assert_eq!(ask.wait.map(|wait| wait.status()), Some("waiting_for_ask"));
    assert!(ask.summary.get("ask_id").is_some());
    Ok(())
}

#[tokio::test]
async fn finish_defers_when_unfinished_jobs_exist() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let state = AppState::initialize(test_config(dir.path().into())).await?;
    let project_id = seed(&state).await?;
    let actor = json!({"kind": "test"});
    let session = state
        .sessions()
        .create_session(project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;
    let started = state
        .sessions()
        .post_message(session_id, "finish later", &session.version, actor.clone())
        .await?;
    let turn_id = TurnId::from_str(&started.turn_id)?;

    let pool = state.database().pool();
    let now = "2026-01-01T00:00:00.000Z";
    let runtime_id = RuntimeId::new();
    let log_id = LogStreamId::new();
    let job_id = JobId::new();
    let tool_call_id = ToolCallId::new();
    sqlx::query(
        "INSERT INTO log_streams \
         (id, owner_kind, owner_id, relative_path, first_cursor, next_cursor, \
          retained_bytes, total_bytes, truncated, closed, created_at, updated_at) \
         VALUES (?, 'job', ?, 'logs/j', 0, 0, 0, 0, 0, 0, ?, ?)",
    )
    .bind(log_id.to_string())
    .bind(job_id.to_string())
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO runtimes \
         (id, session_id, executor_kind, executor_identity, executor_nonce, \
          limits_json, capability_snapshot_json, status, version, created_at, updated_at) \
         VALUES (?, ?, 'local', 't', 'n', '{}', '[]', 'ready', 'v1', ?, ?)",
    )
    .bind(runtime_id.to_string())
    .bind(session_id.to_string())
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO jobs \
         (id, runtime_id, session_id, initiated_by_tool_call_id, controlling_turn_id, \
          command_summary, executor_nonce, log_stream_id, status, version, created_at) \
         VALUES (?, ?, ?, ?, ?, 'sleep', 'n', ?, 'running', 'v1', ?)",
    )
    .bind(job_id.to_string())
    .bind(runtime_id.to_string())
    .bind(session_id.to_string())
    .bind(tool_call_id.to_string())
    .bind(turn_id.to_string())
    .bind(log_id.to_string())
    .bind(now)
    .execute(pool)
    .await?;

    let ctx = ToolContext {
        session_id,
        turn_id,
        tool_call_id,
        workspace: state.workspace_sync(),
        runtime: Some(state.runtime()),
        pool,
        actor,
    };
    let out = execute_tool(&ctx, "finish", &json!({"summary": "done"})).await?;
    assert!(out.ok);
    assert_eq!(out.wait.map(|wait| wait.status()), Some("waiting_for_job"));
    assert!(out.finish_summary.is_none());
    Ok(())
}

#[tokio::test]
async fn service_tool_starts_and_is_nonblocking() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let state = AppState::initialize(test_config(dir.path().into())).await?;
    let project_id = seed(&state).await?;
    let actor = json!({"kind": "test"});
    let session = state
        .sessions()
        .create_session(project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;
    let started = state
        .sessions()
        .post_message(session_id, "run service", &session.version, actor.clone())
        .await?;
    let turn_id = TurnId::from_str(&started.turn_id)?;

    let cmd = if cfg!(windows) {
        "while ($true) { Write-Output 'tick'; Start-Sleep -Milliseconds 100 }"
    } else {
        "while true; do printf tick; sleep 0.1; done"
    };
    let ctx = ToolContext {
        session_id,
        turn_id,
        tool_call_id: ToolCallId::new(),
        workspace: state.workspace_sync(),
        runtime: Some(state.runtime()),
        pool: state.database().pool(),
        actor,
    };
    let out = execute_tool(
        &ctx,
        "service",
        &json!({"command": cmd, "impact": "read_only"}),
    )
    .await?;
    assert!(out.ok, "{:?}", out.summary);
    assert!(out.wait.is_none(), "service must not block the Turn");
    let service_id = out.summary["service_id"].as_str().expect("service_id");
    // Stop so the test process does not leak a long-lived shell.
    let sid: janus_server::platform::id::ServiceId = service_id.parse()?;
    let stopped = state.runtime().stop_service(sid).await?;
    assert!(matches!(
        stopped.status,
        janus_server::modules::runtime::interface::ServiceStatus::Stopped
            | janus_server::modules::runtime::interface::ServiceStatus::Failed
            | janus_server::modules::runtime::interface::ServiceStatus::StoppedAfterRestart
    ));
    Ok(())
}

#[tokio::test]
async fn delegate_cli_rejects_missing_binary() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let state = AppState::initialize(test_config(dir.path().into())).await?;
    let project_id = seed(&state).await?;
    let actor = json!({"kind": "test"});
    let session = state
        .sessions()
        .create_session(project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;
    let started = state
        .sessions()
        .post_message(session_id, "delegate", &session.version, actor.clone())
        .await?;
    let turn_id = TurnId::from_str(&started.turn_id)?;
    let ctx = ToolContext {
        session_id,
        turn_id,
        tool_call_id: ToolCallId::new(),
        workspace: state.workspace_sync(),
        runtime: Some(state.runtime()),
        pool: state.database().pool(),
        actor,
    };
    // On a typical Windows CI host neither claude nor codex is installed.
    // Prefer the missing binary path; if both exist, just assert validation.
    let out = execute_tool(
        &ctx,
        "delegate_cli",
        &json!({"cli": "claude_code", "instruction": "say hi"}),
    )
    .await?;
    if out.ok {
        // Binary present: tool starts a Job and parks the Turn.
        assert_eq!(out.wait.map(|wait| wait.status()), Some("waiting_for_job"));
        assert!(out.summary.get("job_id").is_some());
        if let Some(job_id) = out.summary["job_id"].as_str() {
            let jid: JobId = job_id.parse()?;
            let _ = state.runtime().cancel_job(jid).await;
        }
    } else {
        assert_eq!(out.error_code.as_deref(), Some("CAPABILITY_UNAVAILABLE"));
    }
    Ok(())
}

#[tokio::test]
async fn delegate_cli_rejects_bad_cli_kind() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let state = AppState::initialize(test_config(dir.path().into())).await?;
    let project_id = seed(&state).await?;
    let actor = json!({"kind": "test"});
    let session = state
        .sessions()
        .create_session(project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;
    let started = state
        .sessions()
        .post_message(session_id, "delegate bad", &session.version, actor.clone())
        .await?;
    let turn_id = TurnId::from_str(&started.turn_id)?;
    let ctx = ToolContext {
        session_id,
        turn_id,
        tool_call_id: ToolCallId::new(),
        workspace: state.workspace_sync(),
        runtime: Some(state.runtime()),
        pool: state.database().pool(),
        actor,
    };
    let out = execute_tool(
        &ctx,
        "delegate_cli",
        &json!({"cli": "not-a-cli", "instruction": "x"}),
    )
    .await?;
    assert!(!out.ok);
    assert_eq!(out.error_code.as_deref(), Some("VALIDATION_FAILED"));
    Ok(())
}
