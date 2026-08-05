use anyhow::Context;
use janus_infrastructure::{events::NewEvent, id::CorrelationId, operations::OperationStatus};
use janus_server::{AppState, application::workers, config::Config, router};
use serde_json::json;
use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "janus=info".into()))
        .json()
        .init();

    let config = match Config::from_env().context("invalid Janus configuration") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration error: {:#}", e);
            return Err(e);
        }
    };
    let bind = config.bind;
    let state = match AppState::initialize(config).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("initialization error: {:#}", e);
            return Err(e);
        }
    };
    // Hold ready=503 until the recovery steps below finish. initialize() leaves
    // the flag true for unit tests; main flips it off for the real process.
    state.begin_startup_recovery();

    // Finish process-level recovery after capability and execution recovery:
    // remove crash leftovers and make interrupted Operations explicit so a
    // client can retry instead of the control plane guessing their outcome.
    // Runtime and execution recovery already ran during AppState::initialize.
    state
        .blobs()
        .clean_incoming()
        .await
        .context("clean incoming objects on startup")?;
    for op_id in state
        .operations()
        .stale_running()
        .await
        .context("list stale operations on startup")?
    {
        warn!(%op_id, "marking stale running operation as needs_attention");
        state
            .operations()
            .finish(
                &op_id,
                OperationStatus::NeedsAttention,
                None,
                Some(json!({"code": "OPERATION_INTERRUPTED", "detail": "process restarted while operation was running"})),
                CorrelationId::new(),
            )
            .await
            .with_context(|| format!("mark operation {op_id} as needs_attention"))?;
    }
    state.mark_recovery_complete();

    state
        .events()
        .append(NewEvent {
            event_type: "system.started".into(),
            actor: json!({ "kind": "system", "display_name": "Janus" }),
            resource: None,
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({ "version": env!("CARGO_PKG_VERSION") }),
        })
        .await
        .context("record system startup event")?;

    workers::spawn(state.application().clone());
    workers::spawn_job_wake(state.application().clone());
    workers::spawn_ask_expiry(state.application().clone());

    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind Janus listener at {bind}"))?;
    info!(address = %listener.local_addr()?, "janus control plane ready");

    // Capture state for the post-serve graceful shutdown path. axum stops
    // accepting once `shutdown_signal` fires; we then bound-stop live Runtimes
    // before dropping the process so Local process groups do not leak.
    let shutdown_state = state.clone();
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve Janus")?;
    janus_server::application::lifecycle::graceful_shutdown(
        shutdown_state.application(),
        std::time::Duration::from_secs(10),
    )
    .await;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install Ctrl-C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => tracing::error!(%error, "failed to install SIGTERM handler"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
