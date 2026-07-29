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
    // The router is the single source of public routes. We mirror its route
    // set here (kept in OpenAPI-path form, parameters as `{name}`) and assert
    // the generated OpenAPI document exposes exactly that set, no more, no
    // less. Drift in either direction fails loudly: a router route missing
    // from the document means the frontend type surface is incomplete; an
    // orphan document path means a route was retired without updating OpenAPI.
    let expected: std::collections::BTreeSet<String> = [
        "/health/live",
        "/health/ready",
        "/api/v1/bootstrap",
        "/api/v1/system/info",
        "/api/v1/events",
        "/api/v1/auth/initialize/options",
        "/api/v1/auth/initialize/complete",
        "/api/v1/auth/logout",
        "/api/v1/me",
        "/api/v1/me/passkeys",
        "/api/v1/me/passkeys/options",
        "/api/v1/me/passkeys/complete",
        "/api/v1/me/passkeys/{id}",
        "/api/v1/me/recovery-codes/regenerate",
        "/api/v1/auth/passkey/options",
        "/api/v1/auth/passkey/complete",
        "/api/v1/auth/recovery/exchange",
        "/api/v1/auth/recovery/passkey/options",
        "/api/v1/auth/recovery/passkey/complete",
        "/api/v1/model-providers",
        "/api/v1/model-providers/{id}",
        "/api/v1/model-providers/{id}/probe",
        "/api/v1/operations/{id}",
        "/api/v1/projects",
        "/api/v1/projects/{id}",
        "/api/v1/projects/{id}/retry",
        "/api/v1/projects/{id}/files",
        "/api/v1/projects/{id}/files/tree",
        "/api/v1/projects/{id}/files/meta",
        "/api/v1/projects/{id}/files/content",
        "/api/v1/projects/{id}/files/text",
        "/api/v1/projects/{id}/files/move",
        "/api/v1/github-credentials",
        "/api/v1/github-credentials/{id}",
        "/api/v1/github-credentials/{id}/probe",
        "/api/v1/projects/{id}/git/status",
        "/api/v1/projects/{id}/git/diff",
        "/api/v1/projects/{id}/git/log",
        "/api/v1/projects/{id}/git/branches",
        "/api/v1/projects/{id}/git/remotes",
        "/api/v1/projects/{id}/git/commands/fetch",
        "/api/v1/projects/{id}/git/commands/stage",
        "/api/v1/projects/{id}/git/commands/unstage",
        "/api/v1/projects/{id}/git/commands/commit",
        "/api/v1/projects/{id}/git/commands/push",
        "/api/v1/projects/{id}/git/commands/update",
        "/api/v1/projects/{id}/git/update-conflicts",
        "/api/v1/projects/{id}/git/update-conflicts/{conflict_id}",
        "/api/v1/projects/{id}/git/update-conflicts/{conflict_id}/resolve",
        "/api/v1/projects/{project_id}/sessions",
        "/api/v1/sessions/{id}",
        "/api/v1/sessions/{id}/context",
        "/api/v1/sessions/{id}/messages",
        "/api/v1/sessions/{id}/attachments",
        "/api/v1/sessions/{id}/attachments/{attachment_id}",
        "/api/v1/sessions/{id}/timeline",
        "/api/v1/sessions/{id}/turns/{turn_id}",
        "/api/v1/sessions/{id}/diff",
        "/api/v1/terminals",
        "/api/v1/terminals/{id}/scrollback",
        "/api/v1/terminals/{id}/tickets",
        "/api/v1/terminals/{id}/resize",
        "/api/v1/terminals/{id}/signal",
        "/api/v1/terminals/{id}/close",
        "/api/v1/terminals/{id}/connect",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    let document = janus_server::transport::http::openapi()
        .to_json()
        .expect("OpenAPI serializes");
    let value: serde_json::Value = serde_json::from_str(&document).expect("OpenAPI parses");
    let paths = value
        .get("paths")
        .and_then(|node| node.as_object())
        .expect("OpenAPI has a paths object");
    let actual: std::collections::BTreeSet<String> = paths.keys().cloned().collect();

    assert_eq!(
        actual,
        expected,
        "router/OpenAPI route set diverged — missing from OpenAPI: {:?}, orphan in OpenAPI: {:?}",
        expected.difference(&actual).collect::<Vec<_>>(),
        actual.difference(&expected).collect::<Vec<_>>(),
    );

    // Every advertised path must carry at least one operation. An empty
    // path object would quietly break the frontend type surface.
    for (route, node) in paths {
        let has_operation = ["get", "post", "put", "patch", "delete", "head", "options"]
            .into_iter()
            .any(|method| node.get(method).is_some());
        assert!(has_operation, "OpenAPI path {route} declares no operation");
    }
}
