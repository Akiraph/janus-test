use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::Context as _;
use janus_infrastructure::id::{JobId, RuntimeId, ServiceId, SessionId, ToolCallId, TurnId};
use janus_runtime::interface::*;
use janus_server::{
    AppState,
    config::{Config, RunMode},
};
use tempfile::TempDir;

fn test_config(data_root: PathBuf) -> Config {
    Config {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        data_root,
        mode: RunMode::Development,
        development_auth: true,
        webauthn_rp_name: "Janus Test".into(),
        webauthn_rp_id: "localhost".into(),
        public_origin: url::Url::parse("http://localhost").expect("static URL"),
        event_heartbeat: Duration::from_millis(50),
    }
}

fn limits(timeout_ms: u64) -> ResourceLimits {
    ResourceLimits {
        timeout_ms,
        memory_bytes: 256 * 1024 * 1024,
        cpu_millis: 1_000,
        pids: 64,
        temporary_disk_bytes: 128 * 1024 * 1024,
        open_files: 128,
    }
}

fn execution(
    runtime_id: RuntimeId,
    script: &str,
    timeout_ms: u64,
) -> anyhow::Result<ExecutionSpec> {
    Ok(ExecutionSpec::new(
        runtime_id,
        RelativeWorkingDirectory::new(".")?,
        ValidatedCommand::shell(script)?,
        ExecutionEnvironment::new(BTreeMap::new(), vec![])?,
        limits(timeout_ms),
        NetworkPolicy::DenyAll,
    )?)
}

async fn wait_for_job(state: &AppState, id: JobId) -> anyhow::Result<JobStatus> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = state.runtime().job(id).await?.status;
            if matches!(
                status,
                JobStatus::Succeeded | JobStatus::Failed | JobStatus::Canceled | JobStatus::Lost
            ) {
                return Ok::<_, anyhow::Error>(status);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?
}

#[tokio::test]
async fn local_runtime_persists_sync_jobs_services_events_and_recovery() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    tokio::fs::create_dir_all(&workspace).await?;
    let state = AppState::initialize(test_config(temp.path().join("data")))
        .await
        .context("initialize app state")?;
    let session_id = SessionId::new();
    let runtime_id = RuntimeId::new();
    let runtime = RuntimeSpec::new(
        runtime_id,
        janus_runtime::interface::RuntimeScope::session(session_id),
        ExecutorKind::Local,
        workspace,
        limits(10_000),
        NetworkPolicy::DenyAll,
    )?;
    let ready = state
        .runtime()
        .ensure_runtime(&runtime)
        .await
        .context("ensure runtime")?;
    assert_eq!(ready.status, janus_runtime::interface::RuntimeStatus::Ready);

    let sync = state
        .runtime()
        .execute_sync(execution(runtime_id, success_script(), 5_000)?)
        .await
        .context("execute sync command")?;
    assert_eq!(sync.exit.exit_code, Some(0));
    assert!(sync.stdout.contains("sync-ok"));

    let stdin_id = JobId::new();
    let stdin_job = JobSpec::new(
        stdin_id,
        session_id,
        TurnId::new(),
        ToolCallId::new(),
        execution(runtime_id, stdin_script(), 10_000)?,
    )?;
    state
        .runtime()
        .start_job(stdin_job)
        .await
        .context("start stdin job")?;
    state
        .runtime()
        .write_job_stdin(stdin_id, b"hello\n".to_vec())
        .await
        .context("write stdin job input")?;
    assert_eq!(wait_for_job(&state, stdin_id).await?, JobStatus::Succeeded);
    let stdin_projection = state.runtime().job(stdin_id).await?;
    let stdin_log = state
        .runtime()
        .log_range(
            stdin_projection.log_stream_id,
            janus_runtime::interface::LogCursor::new(0),
            4096,
        )
        .await
        .context("read stdin job log")?;
    assert!(
        stdin_log
            .chunks
            .iter()
            .any(|chunk| chunk.text.contains("hello"))
    );

    let mut parallel = Vec::new();
    for _ in 0..2 {
        let id = JobId::new();
        state
            .runtime()
            .start_job(JobSpec::new(
                id,
                session_id,
                TurnId::new(),
                ToolCallId::new(),
                execution(runtime_id, short_job_script(), 10_000)?,
            )?)
            .await
            .context("start parallel job")?;
        parallel.push(id);
    }
    for id in parallel {
        assert_eq!(wait_for_job(&state, id).await?, JobStatus::Succeeded);
    }

    let cancel_id = JobId::new();
    state
        .runtime()
        .start_job(JobSpec::new(
            cancel_id,
            session_id,
            TurnId::new(),
            ToolCallId::new(),
            execution(runtime_id, long_job_script(), 30_000)?,
        )?)
        .await
        .context("start cancelable job")?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let canceled = state
        .runtime()
        .cancel_job(cancel_id)
        .await
        .context("cancel job")?;
    assert_eq!(canceled.status, JobStatus::Canceled);

    let service_id = ServiceId::new();
    let service = state
        .runtime()
        .start_service(ServiceSpec::new(
            service_id,
            session_id,
            ToolCallId::new(),
            ServiceImpact::ReadOnly,
            execution(runtime_id, service_script(), 30_000)?,
        )?)
        .await
        .context("start service")?;
    assert_eq!(service.status, ServiceStatus::Running);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let stopped = state
        .runtime()
        .stop_service(service_id)
        .await
        .context("stop service")?;
    assert_eq!(stopped.status, ServiceStatus::Stopped);

    let events = state.system().events_after(0, 100).await?;
    assert!(events.iter().any(|event| event.event_type == "job.changed"));
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "service.changed")
    );

    sqlx::query("UPDATE runtimes SET status = 'ready' WHERE id = ?")
        .bind(runtime_id.to_string())
        .execute(state.pool())
        .await?;
    sqlx::query("UPDATE jobs SET status = 'running', ended_at = NULL WHERE id = ?")
        .bind(stdin_id.to_string())
        .execute(state.pool())
        .await?;
    state
        .runtime()
        .recover_uncertain()
        .await
        .context("recover uncertain runtime resources")?;
    assert_eq!(state.runtime().job(stdin_id).await?.status, JobStatus::Lost);
    assert_eq!(
        state.runtime().runtime(runtime_id).await?.status,
        janus_runtime::interface::RuntimeStatus::Lost
    );
    let recovery_events = state.system().events_after(0, 200).await?;
    assert!(recovery_events.iter().any(|event| {
        event.event_type == "runtime.changed"
            && event
                .resource
                .as_ref()
                .is_some_and(|resource| resource["id"] == runtime_id.to_string())
            && event.payload["status"] == "lost"
    }));
    assert!(recovery_events.iter().any(|event| {
        event.event_type == "job.changed"
            && event
                .resource
                .as_ref()
                .is_some_and(|resource| resource["id"] == stdin_id.to_string())
            && event.payload["status"] == "lost"
    }));
    Ok(())
}

fn success_script() -> &'static str {
    "printf 'sync-ok\\n'"
}
fn stdin_script() -> &'static str {
    "read line; printf 'got:%s\\n' \"$line\""
}
fn short_job_script() -> &'static str {
    "sleep 0.1; printf 'done\\n'"
}
fn long_job_script() -> &'static str {
    "read -r forever"
}
fn service_script() -> &'static str {
    "while true; do printf 'tick\\n'; read -r -t 1 _ || true; done"
}
