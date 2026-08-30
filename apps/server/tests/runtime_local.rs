use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::Context as _;
use janus_infrastructure::id::{AsyncTaskId, ProjectId, RuntimeId, SessionId, ToolCallId, TurnId};
use janus_runtime::interface::*;
use janus_server::{
    AppState,
    config::{Config, RunMode},
};
use mongodb::bson::{Document, doc};
use tempfile::TempDir;

static TEST_DB_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn test_config(data_root: PathBuf) -> Config {
    Config {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        data_root,
        web_dist: None,
        mode: RunMode::Development,
        development_auth: true,
        auth_mode: janus_identity::AuthMode::Passkey,
        webauthn_rp_name: "Janus Test".into(),
        webauthn_rp_id: "localhost".into(),
        public_origin: url::Url::parse("http://localhost").expect("static URL"),
        event_heartbeat: Duration::from_millis(50),
        automation_webhook_enabled: false,
        automation_webhook_secret: None,
        automation_github_token: None,
        mongodb_uri: std::env::var("JANUS_MONGODB_URI")
            .unwrap_or_else(|_| "mongodb://localhost:27017/?replicaSet=rs0".into()),
        mongodb_database: format!(
            "janus_test_{}_{}",
            std::process::id(),
            TEST_DB_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ),
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
    )?)
}

async fn wait_for_async_task(state: &AppState, id: AsyncTaskId) -> anyhow::Result<AsyncTaskStatus> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = state.runtime().async_task(id).await?.status;
            if matches!(
                status,
                AsyncTaskStatus::Succeeded
                    | AsyncTaskStatus::Failed
                    | AsyncTaskStatus::Canceled
                    | AsyncTaskStatus::Lost
            ) {
                return Ok::<_, anyhow::Error>(status);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?
}

#[tokio::test]
async fn local_runtime_persists_sync_async_tasks_events_and_recovery() -> anyhow::Result<()> {
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
        janus_runtime::interface::RuntimeScope::project(ProjectId::new()),
        workspace,
        limits(10_000),
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

    let stdin_id = AsyncTaskId::new();
    let stdin_async_task = AsyncTaskSpec::new(
        stdin_id,
        session_id,
        TurnId::new(),
        ToolCallId::new(),
        execution(runtime_id, stdin_script(), 10_000)?,
    )?;
    state
        .runtime()
        .start_async_task(stdin_async_task)
        .await
        .context("start stdin async_task")?;
    state
        .runtime()
        .write_async_task_stdin(stdin_id, b"hello\n".to_vec())
        .await
        .context("write stdin async_task input")?;
    assert_eq!(
        wait_for_async_task(&state, stdin_id).await?,
        AsyncTaskStatus::Succeeded
    );
    let stdin_projection = state.runtime().async_task(stdin_id).await?;
    let stdin_log = state
        .runtime()
        .log_range(
            stdin_projection.log_stream_id,
            janus_runtime::interface::LogCursor::new(0),
            4096,
        )
        .await
        .context("read stdin async_task log")?;
    assert!(
        stdin_log
            .chunks
            .iter()
            .any(|chunk| chunk.text.contains("hello"))
    );

    let mut parallel = Vec::new();
    for _ in 0..2 {
        let id = AsyncTaskId::new();
        state
            .runtime()
            .start_async_task(AsyncTaskSpec::new(
                id,
                session_id,
                TurnId::new(),
                ToolCallId::new(),
                execution(runtime_id, short_async_task_script(), 10_000)?,
            )?)
            .await
            .context("start parallel async_task")?;
        parallel.push(id);
    }
    for id in parallel {
        assert_eq!(
            wait_for_async_task(&state, id).await?,
            AsyncTaskStatus::Succeeded
        );
    }

    let cancel_id = AsyncTaskId::new();
    state
        .runtime()
        .start_async_task(AsyncTaskSpec::new(
            cancel_id,
            session_id,
            TurnId::new(),
            ToolCallId::new(),
            execution(runtime_id, long_async_task_script(), 30_000)?,
        )?)
        .await
        .context("start cancelable async_task")?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let canceled = state
        .runtime()
        .cancel_async_task(cancel_id)
        .await
        .context("cancel async_task")?;
    assert_eq!(canceled.status, AsyncTaskStatus::Canceled);

    let events = state.system().events_after(0, 100).await?;
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "async_task.changed")
    );

    state
        .pool()
        .collection::<Document>("runtimes")
        .update_one(
            doc! {"_id": runtime_id.to_string()},
            doc! {"$set": {"status": "ready"}},
        )
        .await?;
    state
        .pool()
        .collection::<Document>("async_tasks")
        .update_one(
            doc! {"_id": stdin_id.to_string()},
            doc! {"$set": {"status": "running", "ended_at": null}},
        )
        .await?;
    state
        .runtime()
        .recover_uncertain()
        .await
        .context("recover uncertain runtime resources")?;
    assert_eq!(
        state.runtime().async_task(stdin_id).await?.status,
        AsyncTaskStatus::Lost
    );
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
        event.event_type == "async_task.changed"
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
fn short_async_task_script() -> &'static str {
    "sleep 0.1; printf 'done\\n'"
}
fn long_async_task_script() -> &'static str {
    "read -r forever"
}
