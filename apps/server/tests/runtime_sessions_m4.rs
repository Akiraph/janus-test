//! Stage 4: Handoff + Job wake-up via application coordinators.

mod support;

use std::{net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

use janus_server::{
    AppState,
    config::{Config, RunMode},
};
use janus_server::platform::id::{JobId, LogStreamId, RuntimeId, SessionId, ToolCallId, TurnId};
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

async fn seed_project(state: &AppState) -> anyhow::Result<(String, String)> {
    let pool = state.database().pool();
    let now = "2026-01-01T00:00:00.000Z";
    sqlx::query("INSERT INTO tenants (id, created_at) VALUES ('tenant-test', ?)")
        .bind(now)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO owners (id, tenant_id, display_name, created_at) \
         VALUES ('owner-test', 'tenant-test', 'Test Owner', ?)",
    )
    .bind(now)
    .execute(pool)
    .await?;
    let project_id = uuid::Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO projects \
         (id, owner_id, tenant_id, name, state, repo_access, repo_url, \
          version, created_at, updated_at, last_activity_at) \
         VALUES (?, 'owner-test', 'tenant-test', 'm4', 'ready', 'public_https', \
                 'https://example.com/r.git', 'v1', ?, ?, ?)",
    )
    .bind(&project_id)
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    let main_managed = format!("workspaces/main/{project_id}/repo");
    let main_abs = state.config().data_root.join(&main_managed);
    std::fs::create_dir_all(main_abs.join("src"))?;
    std::fs::write(main_abs.join("README.md"), b"# m4\n")?;
    support::init_git_repo(&main_abs)?;
    let _ = state
        .workspace_sync()
        .ensure_main_copy(
            project_id.parse()?,
            &main_managed,
            "test",
            json!({"kind": "test"}),
        )
        .await?;
    Ok((project_id, "owner-test".into()))
}

#[tokio::test]
async fn handoff_transfers_jobs_and_promotes_successor() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let state = AppState::initialize(test_config(dir.path().into())).await?;
    let (project_id, owner_id) = seed_project(&state).await?;
    let actor = json!({"kind": "user", "id": owner_id});
    let project_id = project_id.parse()?;

    let summary = state
        .sessions()
        .create_session(project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&summary.id)?;
    let first = state
        .sessions()
        .post_message(session_id, "first", &summary.version, actor.clone())
        .await?;
    let predecessor = TurnId::from_str(&first.turn_id)?;
    let paused = state
        .sessions()
        .pause_turn_for(
            session_id,
            predecessor,
            "waiting_for_job",
            json!({"kind": "supervisor"}),
        )
        .await?;

    // Seed a runtime + unfinished job controlled by the predecessor.
    let pool = state.database().pool();
    let runtime_id = RuntimeId::new();
    let log_id = LogStreamId::new();
    let job_id = JobId::new();
    let tool_call_id = ToolCallId::new();
    let now = "2026-01-01T00:00:00.000Z";
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
    .bind(predecessor.to_string())
    .bind(log_id.to_string())
    .bind(now)
    .execute(pool)
    .await?;

    let second = state
        .sessions()
        .post_message(session_id, "take over", &paused, actor.clone())
        .await?;
    assert!(second.awaiting_handoff);
    let handled = state
        .handle_message(session_id, second.clone(), "take over", &owner_id)
        .await?;
    let successor = handled.run_turn.expect("successor must run");
    assert_eq!(successor.to_string(), second.turn_id);

    let pred = state.sessions().get_turn(session_id, predecessor).await?;
    assert_eq!(pred.status, "handed_off");
    let succ = state.sessions().get_turn(session_id, successor).await?;
    assert_eq!(succ.status, "running");

    let controlling: String =
        sqlx::query_scalar("SELECT controlling_turn_id FROM jobs WHERE id = ?")
            .bind(job_id.to_string())
            .fetch_one(pool)
            .await?;
    assert_eq!(controlling, successor.to_string());
    Ok(())
}

#[tokio::test]
async fn job_settle_resumes_waiting_for_job_turn() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let state = AppState::initialize(test_config(dir.path().into())).await?;
    let (project_id, owner_id) = seed_project(&state).await?;
    let actor = json!({"kind": "user", "id": owner_id});
    let project_id = project_id.parse()?;

    let summary = state
        .sessions()
        .create_session(project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&summary.id)?;
    let first = state
        .sessions()
        .post_message(session_id, "work", &summary.version, actor.clone())
        .await?;
    let turn_id = TurnId::from_str(&first.turn_id)?;
    state
        .sessions()
        .pause_turn_for(
            session_id,
            turn_id,
            "waiting_for_job",
            json!({"kind": "supervisor"}),
        )
        .await?;

    let pool = state.database().pool();
    let runtime_id = RuntimeId::new();
    let log_id = LogStreamId::new();
    let job_id = JobId::new();
    let tool_call_id = ToolCallId::new();
    let now = "2026-01-01T00:00:00.000Z";
    sqlx::query(
        "INSERT INTO log_streams \
         (id, owner_kind, owner_id, relative_path, first_cursor, next_cursor, \
          retained_bytes, total_bytes, truncated, closed, created_at, updated_at) \
         VALUES (?, 'job', ?, 'logs/j2', 0, 0, 0, 0, 0, 0, ?, ?)",
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
    // Job already terminal: wake path should resume the Turn immediately.
    sqlx::query(
        "INSERT INTO jobs \
         (id, runtime_id, session_id, initiated_by_tool_call_id, controlling_turn_id, \
          command_summary, executor_nonce, log_stream_id, status, version, created_at, ended_at) \
         VALUES (?, ?, ?, ?, ?, 'done', 'n', ?, 'succeeded', 'v1', ?, ?)",
    )
    .bind(job_id.to_string())
    .bind(runtime_id.to_string())
    .bind(session_id.to_string())
    .bind(tool_call_id.to_string())
    .bind(turn_id.to_string())
    .bind(log_id.to_string())
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;

    let resumed = state.on_job_settled(job_id).await?;
    assert_eq!(resumed.map(|t| t.to_string()), Some(turn_id.to_string()));
    let turn = state.sessions().get_turn(session_id, turn_id).await?;
    assert_eq!(turn.status, "running");
    Ok(())
}
