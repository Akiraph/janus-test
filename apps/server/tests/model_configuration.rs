use std::{net::SocketAddr, path::PathBuf, time::Duration};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use futures_util::TryStreamExt;
use janus_server::{
    AppState,
    config::{Config, RunMode},
    router,
};
use mongodb::bson::{Bson, Document, doc};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

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

async fn json_request(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
) -> anyhow::Result<(StatusCode, String)> {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))?;
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await?;
    Ok((status, String::from_utf8(bytes.to_vec())?))
}

#[tokio::test]
async fn provider_embeds_models_and_masks_key_without_leaking() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let app = router(state.clone());
    let secret = "test-provider-key-that-must-not-leak";

    // Create a provider with two embedded models in a single upsert.
    let (status, response) = json_request(
        &app,
        "POST",
        "/api/v1/model-providers",
        json!({
            "kind": "openai_chat",
            "display_name": "Fixture",
            "base_url": "http://127.0.0.1:8999/v1",
            "api_key": secret,
            "models": [
                {
                    "display_name": "Small",
                    "upstream_model_id": "fixture-small",
                    "supports_1m": false,
                    "supports_images": false,
                    "enabled": true
                },
                {
                    "display_name": "Large",
                    "upstream_model_id": "fixture-large",
                    "supports_1m": true,
                    "supports_images": true,
                    "enabled": true
                }
            ],
            "enabled": true
        }),
    )
    .await?;
    assert_eq!(status, StatusCode::CREATED, "{response}");
    assert!(!response.contains(secret));
    let created: Value = serde_json::from_str(&response)?;
    assert_eq!(created["data"]["kind"], "openai_chat");
    assert_eq!(created["data"]["base_url"], "http://127.0.0.1:8999/v1");
    assert_eq!(created["data"]["api_key_is_set"], true);
    let preview = created["data"]["api_key_preview"]
        .as_str()
        .expect("api_key_preview");
    assert!(preview.contains('*'));
    assert!(!preview.contains(secret));
    let models = created["data"]["models"].as_array().expect("models array");
    assert_eq!(models.len(), 2);
    assert_eq!(models[1]["supports_1m"], true);
    assert_eq!(models[1]["supports_images"], true);
    let provider_id = created["data"]["id"].as_str().expect("provider id");

    // The ciphertext is stored and does not contain the plaintext secret.
    let stored = state
        .pool()
        .collection::<Document>("model_providers")
        .find_one(doc! {"_id": provider_id})
        .await?
        .expect("provider row")
        .get("api_key_ciphertext")
        .and_then(|value| match value {
            Bson::Binary(binary) => Some(binary),
            _ => None,
        })
        .expect("api_key_ciphertext stored as BSON binary")
        .bytes
        .clone();
    assert!(!String::from_utf8_lossy(&stored).contains(secret));

    // The masked preview is persisted alongside the ciphertext.
    let stored_preview = state
        .pool()
        .collection::<Document>("model_providers")
        .find_one(doc! {"_id": provider_id})
        .await?
        .expect("provider row")
        .get("api_key_preview")
        .and_then(Bson::as_str)
        .map(str::to_owned);
    assert_eq!(stored_preview.as_deref(), Some(preview));

    // The change is recorded as an event and the secret never leaks into it.
    let mut event_cursor = state
        .pool()
        .collection::<Document>("public_events")
        .find(doc! {"event_type": "model_config.changed"})
        .await?;
    let mut event_payloads: Vec<(String,)> = Vec::new();
    while let Some(document) = event_cursor.try_next().await? {
        event_payloads.push((document.get_str("payload_json")?.to_owned(),));
    }
    assert!(!event_payloads.is_empty());
    let events = serde_json::to_string(&event_payloads)?;
    assert!(!events.contains(secret));

    // Updating the provider without supplying an api_key keeps the existing key.
    let (status, response) = json_request(
        &app,
        "PATCH",
        &format!("/api/v1/model-providers/{provider_id}"),
        json!({
            "kind": "openai_chat",
            "display_name": "Fixture",
            "base_url": "http://127.0.0.1:8999/v1/",
            "models": [
                {
                    "display_name": "Small",
                    "upstream_model_id": "fixture-small",
                    "supports_1m": false,
                    "supports_images": false,
                    "enabled": true
                }
            ],
            "enabled": true
        }),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{response}");
    let updated: Value = serde_json::from_str(&response)?;
    assert_eq!(updated["data"]["api_key_is_set"], true);
    assert_eq!(updated["data"]["api_key_preview"], preview);
    assert_eq!(
        updated["data"]["models"]
            .as_array()
            .expect("test value")
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn enabled_model_requires_enabled_provider() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let app = router(state.clone());

    // A disabled provider cannot host an enabled model.
    let (status, response) = json_request(
        &app,
        "POST",
        "/api/v1/model-providers",
        json!({
            "kind": "anthropic",
            "display_name": "Disabled",
            "base_url": "https://api.anthropic.com/v1/",
            "api_key": "sk-test-only-key",
            "models": [
                {
                    "display_name": "Large model",
                    "upstream_model_id": "fixture-large",
                    "supports_1m": true,
                    "supports_images": false,
                    "enabled": true
                }
            ],
            "enabled": false
        }),
    )
    .await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    let problem: Value = serde_json::from_str(&response)?;
    assert_eq!(problem["code"], "VALIDATION_FAILED");
    Ok(())
}

#[tokio::test]
async fn provider_names_are_unique_for_the_single_supervisor_client() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let app = router(state.clone());

    let base = json!({
        "kind": "openai_responses",
        "display_name": "Shared",
        "base_url": "http://127.0.0.1:8999/v1/",
        "api_key": "sk-test-only-key",
        "models": [
            {
                "display_name": "Same",
                "upstream_model_id": "fixture-a",
                "supports_1m": false,
                "supports_images": false,
                "enabled": true
            }
        ],
        "enabled": true
    });

    // A supervisor provider named "Shared".
    let (status, response) =
        json_request(&app, "POST", "/api/v1/model-providers", base.clone()).await?;
    assert_eq!(status, StatusCode::CREATED, "{response}");

    let (status, response) = json_request(&app, "POST", "/api/v1/model-providers", base).await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    Ok(())
}

#[tokio::test]
async fn model_display_names_must_be_unique() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let app = router(state.clone());

    let (status, response) = json_request(
        &app,
        "POST",
        "/api/v1/model-providers",
        json!({
            "kind": "openai_responses",
            "display_name": "Dup",
            "base_url": "http://127.0.0.1:8999/v1/",
            "api_key": "sk-test-only-key",
            "models": [
                {
                    "display_name": "Same",
                    "upstream_model_id": "fixture-a",
                    "supports_1m": false,
                    "supports_images": false,
                    "enabled": true
                },
                {
                    "display_name": "Same",
                    "upstream_model_id": "fixture-b",
                    "supports_1m": false,
                    "supports_images": false,
                    "enabled": true
                }
            ],
            "enabled": true
        }),
    )
    .await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    let problem: Value = serde_json::from_str(&response)?;
    assert_eq!(problem["code"], "VALIDATION_FAILED");
    Ok(())
}
