use std::{net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::Context;
use futures_util::StreamExt;
use janus_infrastructure::{
    database::Database,
    events::{EventEnvelope, EventType, NewEvent},
};
use janus_projects::interface::{CreateGithubCredentialInput, UpdateGithubCredentialInput};
use janus_server::{
    AppState,
    config::{Config, RunMode},
    router,
};
use mongodb::bson::{Document, doc};
use reqwest::Client;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};

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
        public_origin: url::Url::parse("http://localhost").expect("static test URL"),
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
    assert_eq!(body["data"]["database"]["engine"], "mongodb");
    assert_eq!(body["data"]["database"]["journaling"], "on");

    task.abort();
    Ok(())
}

#[tokio::test]
async fn optional_webhook_requires_enablement_and_secret() -> anyhow::Result<()> {
    let disabled_directory = TempDir::new()?;
    let disabled_state =
        AppState::initialize(test_config(disabled_directory.path().into())).await?;
    let (disabled_base, disabled_task) = spawn(disabled_state).await?;
    let client = Client::new();
    let body = r#"{
      "event": "fork_sync_conflict",
      "timestamp": "2026-08-19T06:00:00.000Z",
      "summary": {"scanned": 1, "conflicts": 1},
      "conflicts": [
        {"fullName": "acme/widget", "htmlUrl": "https://github.com/acme/widget",
         "parentFullName": "acme/upstream", "defaultBranch": "main",
         "parentDefaultBranch": "main", "prNumber": 42,
         "prUrl": "https://github.com/acme/widget/pull/42", "message": "conflict"}
      ]
    }"#;

    let disabled = client
        .post(format!("{disabled_base}/api/v1/automation/webhook"))
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await?;
    assert_eq!(disabled.status(), reqwest::StatusCode::NOT_FOUND);
    disabled_task.abort();

    let enabled_directory = TempDir::new()?;
    let mut config = test_config(enabled_directory.path().into());
    config.automation_webhook_enabled = true;
    config.automation_webhook_secret = Some("test-secret".into());
    // The intake refuses to enqueue without an Automation-enabled PAT; a
    // deployment supplies one through JANUS_AUTOMATION_GITHUB_TOKEN.
    config.automation_github_token = Some("ghp-test-automation".into());
    let enabled_state = AppState::initialize(config).await?;
    enabled_state.identity().authenticate(None).await?;
    let (enabled_base, enabled_task) = spawn(enabled_state).await?;

    let webhook_config = client
        .get(format!("{enabled_base}/api/v1/automation/webhook/config"))
        .send()
        .await?;
    assert_eq!(webhook_config.status(), reqwest::StatusCode::OK);
    let config_body: Value = webhook_config.json().await?;
    assert_eq!(config_body["data"]["enabled"], true);
    assert_eq!(config_body["data"]["secret"], Value::Null);
    let revealed = client
        .get(format!(
            "{enabled_base}/api/v1/automation/webhook/config?reveal=true"
        ))
        .send()
        .await?;
    assert_eq!(revealed.status(), reqwest::StatusCode::OK);
    let revealed_body: Value = revealed.json().await?;
    assert_eq!(revealed_body["data"]["secret"], "test-secret");

    let automation_settings = client
        .get(format!("{enabled_base}/api/v1/automation/settings"))
        .send()
        .await?;
    assert_eq!(automation_settings.status(), reqwest::StatusCode::OK);
    let settings_body: Value = automation_settings.json().await?;
    assert_eq!(settings_body["data"]["model_provider_id"], Value::Null);

    let unauthorized = client
        .post(format!("{enabled_base}/api/v1/automation/webhook"))
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await?;
    assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

    // The contract is JSON-only: an authorized request with any other media
    // type is a protocol error, not a parse attempt.
    let wrong_media_type = client
        .post(format!("{enabled_base}/api/v1/automation/webhook"))
        .header("content-type", "text/html")
        .header("x-janus-webhook-secret", "test-secret")
        .body(body)
        .send()
        .await?;
    assert_eq!(
        wrong_media_type.status(),
        reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
    );

    let accepted = client
        .post(format!("{enabled_base}/api/v1/automation/webhook"))
        .header("content-type", "application/json")
        .header("x-janus-webhook-secret", "test-secret")
        .header("idempotency-key", "webhook-test-1")
        .body(body)
        .send()
        .await?;
    assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);
    let accepted_body: Value = accepted.json().await?;
    assert_eq!(accepted_body["data"]["kind"], "automation.fork_sync_batch");
    assert_eq!(accepted_body["data"]["target_kind"], "fork_sync_batch");
    assert_eq!(
        accepted_body["data"]["target_id"],
        "fork-sync:webhook-test-1"
    );

    enabled_task.abort();
    Ok(())
}

#[tokio::test]
async fn generated_webhook_secret_authorizes_the_webhook() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let mut config = test_config(directory.path().into());
    config.automation_webhook_enabled = true;
    config.automation_webhook_secret = Some("env-secret".into());
    config.automation_github_token = Some("ghp-test-automation".into());
    let state = AppState::initialize(config).await?;
    state.identity().authenticate(None).await?;
    let (base, task) = spawn(state).await?;
    let client = Client::new();
    let body = r#"{
      "event": "fork_sync_conflict",
      "timestamp": "2026-08-19T06:00:00.000Z",
      "summary": {"scanned": 1, "conflicts": 1},
      "conflicts": [
        {"fullName": "acme/widget", "htmlUrl": "https://github.com/acme/widget",
         "parentFullName": "acme/upstream", "defaultBranch": "main",
         "parentDefaultBranch": "main", "prNumber": 42,
         "prUrl": "https://github.com/acme/widget/pull/42", "message": "conflict"}
      ]
    }"#;

    let before = client
        .get(format!(
            "{base}/api/v1/automation/webhook/config?reveal=true"
        ))
        .send()
        .await?;
    assert_eq!(before.status(), reqwest::StatusCode::OK);
    let before_body: Value = before.json().await?;
    assert_eq!(before_body["data"]["secret"], "env-secret");
    assert_eq!(before_body["data"]["secret_source"], "env");

    let generated = client
        .post(format!("{base}/api/v1/automation/webhook/secret"))
        .send()
        .await?;
    assert_eq!(generated.status(), reqwest::StatusCode::OK);
    let generated_body: Value = generated.json().await?;
    let new_secret = generated_body["data"]["secret"]
        .as_str()
        .expect("generated secret is revealed once")
        .to_owned();
    assert!(!new_secret.is_empty());
    assert_eq!(generated_body["data"]["secret_configured"], true);
    assert_eq!(generated_body["data"]["secret_source"], "generated");

    let after = client
        .get(format!(
            "{base}/api/v1/automation/webhook/config?reveal=true"
        ))
        .send()
        .await?;
    let after_body: Value = after.json().await?;
    assert_eq!(after_body["data"]["secret"], new_secret);
    assert_eq!(after_body["data"]["secret_source"], "generated");

    let authorized = client
        .post(format!("{base}/api/v1/automation/webhook"))
        .header("content-type", "application/json")
        .header("x-janus-webhook-secret", &new_secret)
        .header("idempotency-key", "webhook-test-generated")
        .body(body)
        .send()
        .await?;
    assert_eq!(authorized.status(), reqwest::StatusCode::ACCEPTED);

    // A generated secret replaces the process-start env value entirely.
    let stale_env_secret = client
        .post(format!("{base}/api/v1/automation/webhook"))
        .header("content-type", "application/json")
        .header("x-janus-webhook-secret", "env-secret")
        .header("idempotency-key", "webhook-test-stale-env")
        .body(body)
        .send()
        .await?;
    assert_eq!(stale_env_secret.status(), reqwest::StatusCode::UNAUTHORIZED);

    task.abort();
    Ok(())
}

#[tokio::test]
async fn github_credentials_require_explicit_automation_opt_in() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let owner_id = "owner-automation-scope";
    state
        .pool()
        .collection::<Document>("owners")
        .insert_one(doc! {
            "_id": owner_id,
            "display_name": "Automation scope test",
            "created_at": "2026-01-01T00:00:00Z",
        })
        .await?;

    let created = state
        .projects()
        .create_credential(
            owner_id,
            CreateGithubCredentialInput {
                name: "private-repo-pat".into(),
                github_host: "github.com".into(),
                pat: Some("ghp-test-project-only".into()),
                automation_enabled: false,
            },
            "credential-create",
        )
        .await?;
    assert!(!created.automation_enabled);
    assert!(
        state
            .projects()
            .list_automation_credentials(owner_id)
            .await?
            .is_empty()
    );

    let updated = state
        .projects()
        .update_credential(
            owner_id,
            &created.id,
            &created.version,
            UpdateGithubCredentialInput {
                name: None,
                github_host: None,
                pat: None,
                automation_enabled: Some(true),
            },
            "credential-scope-update",
        )
        .await?;
    assert!(updated.automation_enabled);
    assert_eq!(
        state
            .projects()
            .list_automation_credentials(owner_id)
            .await?
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn event_stream_replays_committed_rows_and_validates_cursors() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let appended: EventEnvelope = state
        .system()
        .append(NewEvent {
            event_type: EventType::ModelConfigChanged,
            actor: json!({"kind": "user", "id": "owner-test", "display_name": "Test"}),
            resource: None,
            correlation_id: "test-correlation".into(),
            causation_id: None,
            payload: json!({"config_name": "test-model-config"}),
        })
        .await?;
    let (base, task) = spawn(state).await?;
    let client = Client::new();

    // A fresh client replays every committed event as a projection frame. The
    // first `event: state` frame is the projected providers list carrying the
    // cursor of the event that produced it.
    let response = client
        .get(format!("{base}/api/v1/events?after=0"))
        .send()
        .await?;
    assert!(response.status().is_success());
    let mut stream = response.bytes_stream();
    let mut text = String::new();
    loop {
        let next = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .context("wait for replayed projection")?
            .context("event stream ended")??;
        text.push_str(&String::from_utf8_lossy(&next));
        if text.contains("event: state") {
            break;
        }
    }
    let line = text
        .lines()
        .find(|line| line.starts_with("data: ") && line.contains("\"kind\""))
        .context("replayed frame has no state data")?;
    let replayed: Value = serde_json::from_str(line.strip_prefix("data: ").context("data line")?)?;
    assert_eq!(replayed["kind"], "providers");
    assert_eq!(replayed["cursor"].as_str(), Some(appended.cursor.as_str()));

    // Conflicting resume cursors are rejected.
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

    // A cursor ahead of the committed log is rejected.
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
    let uri = std::env::var("JANUS_MONGODB_URI")
        .unwrap_or_else(|_| "mongodb://localhost:27017/?replicaSet=rs0".into());
    let db_name = format!(
        "janus_test_{}_{}",
        std::process::id(),
        TEST_DB_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let first = Database::open(directory.path(), &uri, &db_name).await?;
    assert!(
        Database::open(directory.path(), &uri, &db_name)
            .await
            .is_err()
    );
    drop(first);
    let reopened = Database::open(directory.path(), &uri, &db_name).await?;
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
        "/api/v1/automation/webhook",
        "/api/v1/automation/webhook/config",
        "/api/v1/automation/webhook/secret",
        "/api/v1/automations",
        "/api/v1/automation/settings",
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
        "/api/v1/auth/totp/initialize/options",
        "/api/v1/auth/totp/initialize/complete",
        "/api/v1/auth/totp/login",
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
        "/api/v1/sessions/{id}/context/compact",
        "/api/v1/sessions/{id}/messages",
        "/api/v1/sessions/{id}/attachments",
        "/api/v1/sessions/{id}/attachments/{attachment_id}",
        "/api/v1/sessions/{id}/queued-turns",
        "/api/v1/sessions/{id}/steer",
        "/api/v1/sessions/{id}/timeline",
        "/api/v1/sessions/{id}/turns/{turn_id}",
        "/api/v1/sessions/{id}/turns/{turn_id}/cancel",
        "/api/v1/async-tasks",
        "/api/v1/async-tasks/{id}/log",
        "/api/v1/async-tasks/{id}/cancel",
        "/api/v1/notification-channels",
        "/api/v1/notification-channels/{id}",
        "/api/v1/notification-channels/{id}/test",
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
