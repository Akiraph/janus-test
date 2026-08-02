//! Terminal contract for the PTY-less Bash pipe backend.
//!
//! These tests exercise the durable RuntimeInterface surface for Terminals
//! against the Local pipe backend (git bash on Windows, `/bin/bash` elsewhere).
//! They do NOT exercise the WebSocket transport; that lives in `runtime_http`.

use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::Context as _;
use janus_infrastructure::id::{ProjectId, RuntimeId, TerminalId};
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

fn limits() -> ResourceLimits {
    ResourceLimits {
        timeout_ms: 30_000,
        memory_bytes: 256 * 1024 * 1024,
        cpu_millis: 1_000,
        pids: 64,
        temporary_disk_bytes: 128 * 1024 * 1024,
        open_files: 128,
    }
}

async fn boot_state() -> anyhow::Result<(TempDir, AppState, RuntimeId, ProjectId)> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    tokio::fs::create_dir_all(&workspace).await?;
    let state = AppState::initialize(test_config(temp.path().join("data")))
        .await
        .context("initialize app state")?;
    let project_id = ProjectId::new();
    let runtime_id = RuntimeId::new();
    let runtime = RuntimeSpec::new(
        runtime_id,
        RuntimeScope::project(project_id),
        ExecutorKind::Local,
        workspace,
        limits(),
        NetworkPolicy::DenyAll,
    )?;
    let ready = state
        .runtime()
        .ensure_runtime(&runtime)
        .await
        .context("ensure runtime")?;
    assert_eq!(ready.status, RuntimeStatus::Ready);
    Ok((temp, state, runtime_id, project_id))
}

fn terminal_spec(id: TerminalId, runtime_id: RuntimeId, project_id: ProjectId) -> TerminalSpec {
    TerminalSpec {
        id,
        runtime_id,
        project_id,
        working_directory: RelativeWorkingDirectory::new(".").expect("relative cwd"),
        environment: ExecutionEnvironment::new(BTreeMap::new(), Vec::new())
            .expect("empty environment"),
        size: TerminalSize::new(80, 24).expect("default size"),
    }
}

#[tokio::test]
async fn terminal_roundtrip_creates_issues_consumes_writes_and_closes() -> anyhow::Result<()> {
    let (_temp, state, runtime_id, project_id) = boot_state().await?;
    let terminal_id = TerminalId::new();
    let spec = terminal_spec(terminal_id, runtime_id, project_id);
    let terminal = state
        .runtime()
        .create_terminal(spec)
        .await
        .context("create terminal")?;
    assert_eq!(terminal.status, TerminalStatus::Running);
    assert!(terminal.writable);

    // Issue a ticket; raw token is returned, hash is persisted.
    let ticket = state
        .runtime()
        .issue_terminal_ticket(terminal_ticket_request(terminal_id))
        .await
        .context("issue ticket")?;
    assert!(!ticket.token.is_empty());

    // Consuming with wrong actor/origin is rejected; correct triplet consumes.
    assert!(matches!(
        state
            .runtime()
            .consume_terminal_ticket(&ticket.token, "wrong-actor", "http://localhost")
            .await,
        Err(RuntimeError::TerminalTicketInvalid)
    ));
    assert!(matches!(
        state
            .runtime()
            .consume_terminal_ticket(&ticket.token, "owner", "http://evil")
            .await,
        Err(RuntimeError::TerminalTicketInvalid)
    ));
    let granted = state
        .runtime()
        .consume_terminal_ticket(&ticket.token, "owner", "http://localhost")
        .await
        .context("consume ticket")?;
    assert_eq!(granted, terminal_id);
    // Replay fails after consumption.
    assert!(matches!(
        state
            .runtime()
            .consume_terminal_ticket(&ticket.token, "owner", "http://localhost")
            .await,
        Err(RuntimeError::TerminalTicketInvalid)
    ));

    // Write input and observe echo in the scrollback stream.
    state
        .runtime()
        .write_terminal_input(terminal_id, b"printf ready\\n\n".to_vec())
        .await
        .context("write input")?;
    let echoed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let range = state
                .runtime()
                .terminal_scrollback(terminal_id, LogCursor::ZERO, 64 * 1024)
                .await?;
            let text = range
                .chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<String>();
            if text.contains("ready") {
                return Ok::<_, anyhow::Error>(text);
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await??;
    assert!(echoed.contains("ready"));

    // Close finalizes the shell and marks the terminal exited.
    state
        .runtime()
        .write_terminal_input(terminal_id, b"exit 0\n".to_vec())
        .await?;
    let closed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let projection = state.runtime().terminal(terminal_id).await?;
            if matches!(projection.status, TerminalStatus::Exited) {
                return Ok::<_, anyhow::Error>(projection);
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .context("terminal did not exit")??;
    assert_eq!(closed.status, TerminalStatus::Exited);
    assert!(!closed.writable);

    let events = state.events().after(0, 100).await?;
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "terminal.changed")
    );
    Ok(())
}

#[tokio::test]
async fn terminal_resize_signal_and_scrollback_cursor_resume() -> anyhow::Result<()> {
    let (_temp, state, runtime_id, project_id) = boot_state().await?;
    let terminal_id = TerminalId::new();
    let spec = terminal_spec(terminal_id, runtime_id, project_id);
    let _terminal = state
        .runtime()
        .create_terminal(spec)
        .await
        .context("create terminal")?;
    let resized = state
        .runtime()
        .resize_terminal(terminal_id, TerminalSize::new(120, 40).expect("size"))
        .await
        .context("resize terminal")?;
    assert_eq!(resized.size.cols, 120);
    assert_eq!(resized.size.rows, 40);

    // Produce output, then read with a cursor that resumes partway.
    state
        .runtime()
        .write_terminal_input(terminal_id, b"printf abcdef\n".to_vec())
        .await?;
    let first = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let range = state
                .runtime()
                .terminal_scrollback(terminal_id, LogCursor::ZERO, 64 * 1024)
                .await?;
            let text = range
                .chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<String>();
            if text.contains("abcdef") {
                return Ok::<_, anyhow::Error>(range);
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await??;
    let first_text: String = first
        .chunks
        .iter()
        .map(|chunk| chunk.text.as_str())
        .collect();
    // Slice the abc/def halves by byte index, but split on a UTF-8 char
    // boundary so the resume cursor lands between them.
    let abc = first_text.find("abc").expect("abc present");
    let def = first_text.find("def").expect("def present");
    let midpoint = (abc + def) / 2;
    let mut boundary = midpoint;
    while boundary < first_text.len() && !first_text.is_char_boundary(boundary) {
        boundary += 1;
    }
    let after = LogCursor::new(u64::try_from(boundary).unwrap_or(0));
    let resumed = state
        .runtime()
        .terminal_scrollback(terminal_id, after, 64 * 1024)
        .await
        .context("resume scrollback")?;
    let resumed_text: String = resumed
        .chunks
        .iter()
        .map(|chunk| chunk.text.as_str())
        .collect();
    assert!(resumed_text.contains("def"));
    assert!(!resumed_text.starts_with("abc"));

    // An abstract signal is accepted (best-effort Ctrl-C on the pipe backend).
    // Send `exit 0` so the shell leaves on its own; Ctrl-C is best-effort on a
    // non-tty pipe and may not interrupt the running prompt reliably.
    state
        .runtime()
        .write_terminal_input(terminal_id, b"exit 0\n".to_vec())
        .await?;
    state
        .runtime()
        .signal_terminal(terminal_id, TerminalSignal::CtrlC)
        .await
        .context("signal terminal")?;
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let projection = state.runtime().terminal(terminal_id).await?;
            if matches!(projection.status, TerminalStatus::Exited) {
                return Ok::<_, anyhow::Error>(projection);
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .context("terminal did not exit after signal")??;
    Ok(())
}

#[tokio::test]
async fn terminal_recovery_converts_running_to_lost_and_revokes_tickets() -> anyhow::Result<()> {
    let (_temp, state, runtime_id, project_id) = boot_state().await?;
    let terminal_id = TerminalId::new();
    let spec = terminal_spec(terminal_id, runtime_id, project_id);
    let _ = state
        .runtime()
        .create_terminal(spec)
        .await
        .context("create terminal")?;
    // Issue the ticket while the terminal is still running (recovery revokes it).
    let ticket = state
        .runtime()
        .issue_terminal_ticket(terminal_ticket_request(terminal_id))
        .await?;
    // Close the shell so its background finalize task cannot race recovery.
    state
        .runtime()
        .write_terminal_input(terminal_id, b"exit 0\n".to_vec())
        .await?;
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                state.runtime().terminal(terminal_id).await?.status,
                TerminalStatus::Exited
            ) {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await;
    // Simulate a control-plane restart: force terminal back to a live state,
    // then run recovery.
    sqlx::query("UPDATE terminals SET status = 'running' WHERE id = ?")
        .bind(terminal_id.to_string())
        .execute(state.database().pool())
        .await?;
    state.runtime().recover_uncertain().await?;
    let recovered = state.runtime().terminal(terminal_id).await?;
    assert_eq!(recovered.status, TerminalStatus::Lost);
    assert!(!recovered.writable);
    // Outstanding ticket is revoked.
    assert!(matches!(
        state
            .runtime()
            .consume_terminal_ticket(&ticket.token, "owner", "http://localhost")
            .await,
        Err(RuntimeError::TerminalTicketInvalid)
    ));
    Ok(())
}

#[tokio::test]
async fn project_owner_terminal_persists_and_lists() -> anyhow::Result<()> {
    let (_temp, state, runtime_id, project_id) = boot_state().await?;
    let terminal_id = TerminalId::new();
    let spec = terminal_spec(terminal_id, runtime_id, project_id);
    let _ = state.runtime().create_terminal(spec).await?;
    // Close the shell so the live handle is dropped before list.
    state
        .runtime()
        .write_terminal_input(terminal_id, b"exit 0\n".to_vec())
        .await?;
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                state.runtime().terminal(terminal_id).await?.status,
                TerminalStatus::Exited
            ) {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await;
    let listed = state.runtime().list_terminals(project_id).await?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, terminal_id);
    Ok(())
}

fn terminal_ticket_request(id: TerminalId) -> janus_runtime::interface::TerminalTicketRequest {
    use janus_runtime::interface::TerminalTicketRequest;
    TerminalTicketRequest {
        terminal_id: id,
        actor_id: "owner".into(),
        origin: "http://localhost".into(),
    }
}
