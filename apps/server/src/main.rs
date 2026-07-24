use anyhow::Context;
use janus_server::{
    AppState,
    application::workers,
    config::Config,
    platform::{events::NewEvent, id::CorrelationId, operations::OperationStatus},
    router,
};
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

    let config = Config::from_env().context("invalid Janus configuration")?;
    let bind = config.bind;
    let state = AppState::initialize(config).await?;

    // Startup recovery (`DAT-RECOVER-01` subset): clean leftover incoming temp
    // objects from a crashed write, and mark operations left `running` by a
    // prior process as `needs_attention` so a client can re-issue them rather
    // than the control plane guessing whether a half-done clone happened.
    if let Err(error) = state.blobs().clean_incoming().await {
        warn!(%error, "clean incoming objects on startup");
    }
    for op_id in state
        .operations()
        .stale_running()
        .await
        .unwrap_or_default()
    {
        warn!(%op_id, "marking stale running operation as needs_attention");
        let _ = state
            .operations()
            .finish(
                &op_id,
                OperationStatus::NeedsAttention,
                None,
                Some(json!({"code": "OPERATION_INTERRUPTED", "detail": "process restarted while operation was running"})),
                CorrelationId::new(),
            )
            .await;
    }

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

    workers::spawn(state.clone());

    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind Janus listener at {bind}"))?;
    info!(address = %listener.local_addr()?, "janus control plane ready");

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve Janus")?;
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
