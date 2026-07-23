use std::{net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::Context;
use futures_util::StreamExt;
use janus_server::{
    AppState,
    config::{Config, RunMode},
    platform::{
        database::Database,
        events::{EventEnvelope, NewEvent},
    },
    router,
};
use reqwest::Client;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};

fn test_config(data_root: PathBuf) -> Config {
    Config {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        data_root,
        mode: RunMode::Development,
        development_auth: true,
        webauthn_rp_name: "Janus Test".into(),
        webauthn_rp_id: "localhost".into(),
        public_origin: url::Url::parse("http://localhost").expect("static test URL"),
        event_heartbeat: Duration::from_millis(50),
    }
}

async fn spawn(state: AppState) -> anyhow::Result<(String, JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router(state)).await {
            panic!("test server failed: {error}");
        }
    });
    Ok((format!("http://{address}"), task))
}

#[tokio::test]
async fn probes_expose_request_and_snapshot_cursors() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let (base, task) = spawn(state).await?;
    let client = Client::new();

    let live = client
        .get(format!("{base}/health/live"))
        .header("X-Request-Id", "test-request")
        .send()
        .await?;
    assert!(live.status().is_success());
    assert_eq!(
        live.headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        Some("test-request")
    );

    let bootstrap = client
        .get(format!("{base}/api/v1/bootstrap"))
        .send()
        .await?;
    assert!(bootstrap.status().is_success());
    assert_eq!(
        bootstrap
            .headers()
            .get("x-janus-event-cursor")
            .and_then(|value| value.to_str().ok()),
        Some("0")
    );
    let body: Value = bootstrap.json().await?;
    assert_eq!(body["data"]["state"], "uninitialized");
    assert_eq!(body["data"]["development_auth"], true);

    let info = client
        .get(format!("{base}/api/v1/system/info"))
        .send()
        .await?;
    assert!(info.status().is_success());
    let body: Value = info.json().await?;
    assert_eq!(body["data"]["database"]["journal_mode"], "wal");
    assert!(
        body["data"]["capabilities"]
            .as_array()
            .is_some_and(|values| values.len() == 7)
    );

    task.abort();
    Ok(())
}

#[tokio::test]
async fn event_stream_replays_committed_rows_and_validates_cursors() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let appended: EventEnvelope = state
        .events()
        .append(NewEvent {
            event_type: "system.started".into(),
            actor: json!({"kind": "system", "display_name": "Janus"}),
            resource: None,
            correlation_id: "test-correlation".into(),
            causation_id: None,
            payload: json!({"ready": true}),
        })
        .await?;
    let (base, task) = spawn(state).await?;
    let client = Client::new();

    let response = client
        .get(format!("{base}/api/v1/events?after=0"))
        .send()
        .await?;
    assert!(response.status().is_success());
    let mut stream = response.bytes_stream();
    let next = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .context("wait for replayed event")?;
    let first = next.context("event stream ended")??;
    let text = String::from_utf8_lossy(&first);
    assert!(text.contains("event: janus"));
    assert!(text.contains(&appended.event_id));

    let mismatch = client
        .get(format!("{base}/api/v1/events?after=0"))
        .header("Last-Event-ID", "1")
        .send()
        .await?;
    assert_eq!(mismatch.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        mismatch
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/problem+json")
    );
    let problem: Value = mismatch.json().await?;
    assert_eq!(problem["code"], "CURSOR_MISMATCH");

    let ahead = client
        .get(format!("{base}/api/v1/events?after=2"))
        .send()
        .await?;
    assert_eq!(ahead.status(), reqwest::StatusCode::BAD_REQUEST);
    let problem: Value = ahead.json().await?;
    assert_eq!(problem["code"], "EVENT_CURSOR_AHEAD");

    task.abort();
    Ok(())
}

#[tokio::test]
async fn data_root_lock_is_exclusive_and_reusable_after_close() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let first = Database::open(directory.path()).await?;
    assert!(Database::open(directory.path()).await.is_err());
    drop(first);
    let reopened = Database::open(directory.path()).await?;
    assert!(reopened.ready().await);
    Ok(())
}

#[test]
fn openapi_contains_every_public_route() {
    let document = janus_server::transport::http::openapi()
        .to_json()
        .expect("OpenAPI serializes");
    for route in [
        "/health/live",
        "/health/ready",
        "/api/v1/bootstrap",
        "/api/v1/system/info",
        "/api/v1/events",
    ] {
        assert!(document.contains(route), "missing OpenAPI route {route}");
    }
}
