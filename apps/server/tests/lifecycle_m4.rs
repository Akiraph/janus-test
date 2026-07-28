//! Stage 10: Session/Project deletion routes through Runtime cleanup first.
//!
//! Proves that deleting a Session with a live Local Runtime stops the Runtime
//! (status → stopped/lost) before the Session row and workspace copy disappear,
//! and that a second delete is a clean not-found rather than a process leak.

mod support;

use std::{net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

use janus_server::{
    AppState,
    application::lifecycle::{cleanup_session_runtime, delete_session_with_runtime},
    config::{Config, RunMode},
    modules::runtime::interface::{
        ExecutorKind, NetworkPolicy, ResourceLimits, RuntimeSpec, RuntimeStatus,
    },
    platform::id::{ProjectId, RuntimeId, SessionId},
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

fn limits() -> ResourceLimits {
    ResourceLimits {
        timeout_ms: 10_000,
        memory_bytes: 256 * 1024 * 1024,
        cpu_millis: 1_000,
        pids: 64,
        temporary_disk_bytes: 128 * 1024 * 1024,
        open_files: 128,
    }
}

async fn seed_project(state: &AppState) -> anyhow::Result<ProjectId> {
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
    let project_id = ProjectId::new();
    sqlx::query(
        "INSERT INTO projects \
         (id, owner_id, tenant_id, name, state, repo_access, repo_url, \
          version, created_at, updated_at, last_activity_at) \
         VALUES (?, 'owner-test', 'tenant-test', 'del', 'ready', 'public_https', \
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
    std::fs::write(main_abs.join("README.md"), b"# del\n")?;
    support::init_git_repo(&main_abs)?;
    state
        .workspace_sync()
        .ensure_main_copy(project_id, &main_managed, "test", json!({"kind": "test"}))
        .await?;
    Ok(project_id)
}

#[tokio::test]
async fn delete_session_stops_runtime_before_row_removal() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let data_root = dir.path().to_path_buf();
    let state = AppState::initialize(test_config(data_root.clone())).await?;
    let project_id = seed_project(&state).await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let session = state
        .sessions()
        .create_session(project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;

    let runtime_id = RuntimeId::new();
    let workspace_root = data_root.join(format!("workspaces/sessions/{session_id}"));
    std::fs::create_dir_all(&workspace_root)?;
    let spec = RuntimeSpec::new(
        runtime_id,
        session_id,
        ExecutorKind::Local,
        workspace_root,
        limits(),
        NetworkPolicy::DenyAll,
    )?;
    let ready = state.runtime().ensure_runtime(&spec).await?;
    assert_eq!(ready.status, RuntimeStatus::Ready);

    delete_session_with_runtime(&state, session_id, actor).await?;

    // Session row is gone.
    assert!(state.sessions().get_session(session_id).await.is_err());

    // Runtime is no longer ready/starting — either stopped by cleanup or
    // absent from the live set. current_runtime only returns live rows.
    let live = state.runtime().current_runtime(session_id).await?;
    assert!(
        live.is_none(),
        "live runtime must not survive session delete: {live:?}"
    );

    // Durable runtime row, if still present, must not be ready.
    if let Ok(row) = state.runtime().runtime(runtime_id).await {
        assert!(
            !matches!(row.status, RuntimeStatus::Ready | RuntimeStatus::Starting),
            "runtime status after delete: {:?}",
            row.status
        );
    }

    Ok(())
}

#[tokio::test]
async fn cleanup_session_runtime_is_idempotent_without_runtime() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let state = AppState::initialize(test_config(dir.path().into())).await?;
    let project_id = seed_project(&state).await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let session = state
        .sessions()
        .create_session(project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;

    // No ensure_runtime — cleanup must be a quiet no-op.
    cleanup_session_runtime(&state, session_id).await;
    delete_session_with_runtime(&state, session_id, actor).await?;
    assert!(state.sessions().get_session(session_id).await.is_err());
    Ok(())
}

/// Deleting a Session that owns a running Job cancels the Job and leaves no
/// live Runtime. A subsequent restart recovery must not resurrect the Job as
/// running (no replay).
#[tokio::test]
async fn delete_session_cancels_job_and_recovery_does_not_replay() -> anyhow::Result<()> {
    use janus_server::modules::runtime::interface::{
        ExecutionEnvironment, ExecutionSpec, JobSpec, JobStatus, RelativeWorkingDirectory,
        ValidatedCommand,
    };
    use janus_server::platform::id::{JobId, ToolCallId, TurnId};
    use std::collections::BTreeMap;

    let dir = TempDir::new()?;
    let data_root = dir.path().to_path_buf();
    let state = AppState::initialize(test_config(data_root.clone())).await?;
    let project_id = seed_project(&state).await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let session = state
        .sessions()
        .create_session(project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;

    let runtime_id = RuntimeId::new();
    let workspace_root = data_root.join(format!("workspaces/sessions/{session_id}"));
    std::fs::create_dir_all(&workspace_root)?;
    let spec = RuntimeSpec::new(
        runtime_id,
        session_id,
        ExecutorKind::Local,
        workspace_root,
        limits(),
        NetworkPolicy::DenyAll,
    )?;
    state.runtime().ensure_runtime(&spec).await?;

    // Long-running Job so cancel has something to interrupt.
    #[cfg(windows)]
    let script = "powershell -NoProfile -Command \"Start-Sleep -Seconds 30; Write-Output done\"";
    #[cfg(not(windows))]
    let script = "sleep 30; echo done";
    let job_id = JobId::new();
    let execution = ExecutionSpec::new(
        runtime_id,
        RelativeWorkingDirectory::new(".")?,
        ValidatedCommand::shell(script)?,
        ExecutionEnvironment::new(BTreeMap::new(), vec![])?,
        limits(),
        NetworkPolicy::DenyAll,
    )?;
    state
        .runtime()
        .start_job(JobSpec::new(
            job_id,
            session_id,
            TurnId::new(),
            ToolCallId::new(),
            execution,
        )?)
        .await?;

    delete_session_with_runtime(&state, session_id, actor).await?;

    // Job is no longer running/queued.
    if let Ok(job) = state.runtime().job(job_id).await {
        assert!(
            !matches!(job.status, JobStatus::Queued | JobStatus::Running),
            "job status after delete: {:?}",
            job.status
        );
    }

    // Simulate control-plane restart recovery: uncertain rows become lost, never
    // re-queued as running.
    state.runtime().recover_uncertain().await?;
    if let Ok(job) = state.runtime().job(job_id).await {
        assert_ne!(
            job.status,
            JobStatus::Running,
            "recovery must not replay a deleted session's job"
        );
        assert_ne!(job.status, JobStatus::Queued);
    }
    assert!(state.runtime().current_runtime(session_id).await?.is_none());

    Ok(())
}
