use std::{net::SocketAddr, path::PathBuf, time::Duration};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use janus_server::{
    AppState,
    config::{Config, RunMode},
    router,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

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
async fn provider_secret_is_encrypted_and_one_million_context_requires_capability()
-> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let app = router(state.clone());
    let secret = "test-provider-key-that-must-not-leak";

    let (status, response) = json_request(
        &app,
        "POST",
        "/api/v1/model-providers",
        json!({
            "kind": "openai_compatible",
            "display_name": "Fixture",
            "base_url": "http://127.0.0.1:8999/v1/",
            "api_key": secret,
            "supports_1m": false,
            "enabled": true
        }),
    )
    .await?;
    assert_eq!(status, StatusCode::CREATED, "{response}");
    assert!(!response.contains(secret));
    let created: Value = serde_json::from_str(&response)?;
    assert_eq!(created["data"]["api_key_is_set"], true);
    let provider_id = created["data"]["id"].as_str().expect("provider id");

    let stored = sqlx::query_as::<_, (Vec<u8>,)>(
        "SELECT api_key_ciphertext FROM model_providers WHERE id = ?",
    )
    .bind(provider_id)
    .fetch_one(state.database().pool())
    .await?
    .0;
    assert!(!String::from_utf8_lossy(&stored).contains(secret));
    let event_payloads: Vec<(String,)> = sqlx::query_as(
        "SELECT payload_json FROM public_events WHERE event_type = 'model_config.changed'",
    )
    .fetch_all(state.database().pool())
    .await?;
    assert!(!event_payloads.is_empty());
    let events = serde_json::to_string(&event_payloads)?;
    assert!(!events.contains(secret));

    let (status, response) = json_request(
        &app,
        "POST",
        "/api/v1/models",
        json!({
            "provider_id": provider_id,
            "display_name": "Large model",
            "upstream_model_id": "fixture-large",
            "context_window": "1m",
            "supports_images": true,
            "supports_tools": true,
            "max_output_tokens": 4096,
            "enabled": true
        }),
    )
    .await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let problem: Value = serde_json::from_str(&response)?;
    assert_eq!(problem["code"], "VALIDATION_FAILED");
    Ok(())
}
