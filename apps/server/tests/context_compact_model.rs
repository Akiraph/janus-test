//! Model-driven context compaction.
//!
//! `run_context_compact_operation` must generate the compact summary with a
//! real model attempt (ledger `attempt_type = 'compact'`), persist the model's
//! text plus its real token usage on the `compact_summaries` row, and complete
//! the context version. A second pass covers the degraded path: when no model
//! is configured the operation still completes with the mechanical digest and
//! an explicit `summary_model_status` marker.

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use janus_infrastructure::operations::IdempotencyRequest;
use janus_infrastructure::{
    clock::format_utc,
    id::{CorrelationId, ProjectId, SessionId},
};
use janus_models::interface::{ModelClient, ProviderInput, ProviderKind};
use janus_server::application::context::{CompactContextRequest, run_context_compact_operation};
use janus_server::{AppState, config::{Config, RunMode}};
use serde_json::{Value, json};
use tempfile::TempDir;

const NOW: &str = "2026-08-19T00:00:00.000Z";

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
        automation_webhook_enabled: false,
        automation_webhook_secret: None,
        automation_github_token: None,
    }
}

/// OpenAI-compatible fixture that streams a deterministic summary and usage.
async fn summary_fixture() -> Response {
    let body = concat!(
        "data: {\"id\":\"chatcmpl-compact\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"The user fixed the login bug\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl-compact\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" by editing auth.rs and running cargo test.\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl-compact\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":4200,\"completion_tokens\":64}}\n\n",
        "data: [DONE]\n\n",
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .expect("response")
}

async fn spawn_summary_fixture() -> anyhow::Result<SocketAddr> {
    let app = Router::new().route("/v1/chat/completions", post(summary_fixture));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(addr)
}

/// Owner + ready project + idle session with a short conversation timeline.
async fn seed_session_with_timeline(state: &AppState) -> anyhow::Result<SessionId> {
    let pool = state.pool();
    let project_id = ProjectId::new();
    let session_id = SessionId::new();
    sqlx::query("INSERT INTO owners (id, display_name, created_at) VALUES ('owner', 'Owner', ?)")
        .bind(NOW)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO projects \
         (id, owner_id, name, state, repo_access, repo_url, version, \
          created_at, updated_at, last_activity_at) \
         VALUES (?, 'owner', 'Compact Fixture', 'ready', 'public_https', \
                 'https://example.com/repo.git', 'v1', ?, ?, ?)",
    )
    .bind(project_id.to_string())
    .bind(NOW)
    .bind(NOW)
    .bind(NOW)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO sessions \
         (id, project_id, title, state, active_turn_id, version, created_at, updated_at, \
          last_activity_at) \
         VALUES (?, ?, 'Compact me', 'ready', NULL, 'v_session', ?, ?, ?)",
    )
    .bind(session_id.to_string())
    .bind(project_id.to_string())
    .bind(NOW)
    .bind(NOW)
    .bind(NOW)
    .execute(pool)
    .await?;

    // A minimal user/assistant exchange on the visible timeline.
    sqlx::query(
        "INSERT INTO timeline_items \
         (id, session_id, turn_id, kind, source_resource_id, display_order, projection_json, \
          status, version, created_at, updated_at) \
         VALUES \
           ('tl-user-1', ?, NULL, 'user_message', 'msg-user-1', 1, ?, 'active', 'v1', ?, ?), \
           ('tl-assistant-1', ?, NULL, 'assistant_message', 'msg-assistant-1', 2, ?, 'active', \
            'v1', ?, ?)",
    )
    .bind(session_id.to_string())
    .bind(
        json!({
            "kind": "user_message",
            "text": "Fix the login bug in auth.rs and run the tests.",
        })
        .to_string(),
    )
    .bind(NOW)
    .bind(NOW)
    .bind(session_id.to_string())
    .bind(
        json!({
            "kind": "assistant_message",
            "text": "Fixed auth.rs and ran cargo test: 18 passed.",
        })
        .to_string(),
    )
    .bind(NOW)
    .bind(NOW)
    .execute(pool)
    .await?;
    Ok(session_id)
}

async fn request_compact(state: &AppState, session_id: SessionId) -> anyhow::Result<()> {
    state
        .application()
        .request_context_compact(CompactContextRequest {
            owner_id: "owner".to_owned(),
            session_id,
            actor: json!({"kind": "owner", "id": "owner"}),
            correlation_id: CorrelationId::new(),
            idempotency: IdempotencyRequest {
                key: format!("compact-test:{session_id}"),
                owner_id: "owner".to_owned(),
                method: "POST".to_owned(),
                normalized_route: format!("/internal/sessions/{session_id}/context/compact"),
                digest: "compact-test".to_owned(),
                expires_at: format_utc(
                    janus_infrastructure::clock::now_utc() + chrono::Duration::hours(1),
                ),
            },
            context_limit: None,
        })
        .await?;
    // Claim the queued work item the same way the live worker loop does.
    let claimed = state
        .operations()
        .claim_work("context.compact", 60)
        .await?
        .expect("compact work item should be claimable");
    run_context_compact_operation(
        state.application(),
        &claimed.payload,
        &claimed.id,
        &claimed.nonce,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn compact_generates_a_model_summary_with_real_tokens() -> anyhow::Result<()> {
    let addr = spawn_summary_fixture().await?;
    let temp = TempDir::new()?;
    let state = AppState::initialize(test_config(temp.path().into())).await?;
    let session_id = seed_session_with_timeline(&state).await?;

    // Configure the project's default model against the fixture provider.
    let provider = state
        .models()
        .create_provider(
            "owner",
            ProviderInput {
                client: ModelClient::Supervisor,
                kind: ProviderKind::OpenaiChat,
                display_name: "Compact Fixture".into(),
                base_url: format!("http://{addr}/v1"),
                api_key: Some("sk-test".into()),
                models: vec![janus_models::interface::EmbeddedModelInput {
                    display_name: "Fixture".into(),
                    upstream_model_id: "fixture-model".into(),
                    supports_1m: false,
                    supports_images: false,
                    enabled: true,
                }],
                enabled: true,
            },
            "compact-fixture",
        )
        .await?;
    let model_id: String = sqlx::query_scalar(
        "SELECT id FROM models WHERE provider_id = ? ORDER BY created_at LIMIT 1",
    )
    .bind(&provider.id)
    .fetch_one(state.pool())
    .await?;
    sqlx::query("UPDATE projects SET default_model_id = ? WHERE owner_id = 'owner'")
        .bind(&model_id)
        .execute(state.pool())
        .await?;

    request_compact(&state, session_id).await?;

    // The compact summary row now carries the model's text and real usage.
    let (summary_json, input_tokens, output_tokens, attempt_id): (String, i64, i64, String) =
        sqlx::query_as(
            "SELECT summary_json, input_tokens, output_tokens, model_attempt_id \
             FROM compact_summaries WHERE session_id = ?",
        )
        .bind(session_id.to_string())
        .fetch_one(state.pool())
        .await?;
    let summary: Value = serde_json::from_str(&summary_json)?;
    assert_eq!(
        summary["text"].as_str().expect("model summary text"),
        "The user fixed the login bug by editing auth.rs and running cargo test."
    );
    assert_eq!(input_tokens, 4200);
    assert_eq!(output_tokens, 64);
    assert!(!attempt_id.is_empty());
    assert!(summary.get("timeline_digest").is_none());

    // The attempt ledger row is tagged as a compact attempt.
    let attempt_type: String = sqlx::query_scalar(
        "SELECT attempt_type FROM model_attempts WHERE id = ?",
    )
    .bind(&attempt_id)
    .fetch_one(state.pool())
    .await?;
    assert_eq!(attempt_type, "compact");

    // The context version completed with the real token count.
    let (status, estimated): (String, i64) = sqlx::query_as(
        "SELECT compact_status, estimated_input_tokens FROM context_versions \
         WHERE session_id = ? ORDER BY sequence DESC LIMIT 1",
    )
    .bind(session_id.to_string())
    .fetch_one(state.pool())
    .await?;
    assert_eq!(status, "succeeded");
    assert_eq!(estimated, 4200);

    // The visible timeline item exposes the generated summary.
    let projection: String = sqlx::query_scalar(
        "SELECT projection_json FROM timeline_items \
         WHERE session_id = ? AND kind = 'context_compacted'",
    )
    .bind(session_id.to_string())
    .fetch_one(state.pool())
    .await?;
    let projection: Value = serde_json::from_str(&projection)?;
    assert_eq!(
        projection["summary"]["text"].as_str().expect("timeline summary"),
        "The user fixed the login bug by editing auth.rs and running cargo test."
    );
    Ok(())
}

#[tokio::test]
async fn compact_without_a_model_degrades_to_the_digest() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let state = AppState::initialize(test_config(temp.path().into())).await?;
    let session_id = seed_session_with_timeline(&state).await?;

    // No provider configured: the compact must still complete.
    request_compact(&state, session_id).await?;

    let (summary_json, status): (String, String) = sqlx::query_as(
        "SELECT summary_json, compact_status FROM compact_summaries \
         JOIN context_versions ON context_versions.compact_summary_id = compact_summaries.id \
         WHERE compact_summaries.session_id = ?",
    )
    .bind(session_id.to_string())
    .fetch_one(state.pool())
    .await?;
    let summary: Value = serde_json::from_str(&summary_json)?;
    assert_eq!(summary["summary_model_status"], "no_model_configured");
    assert_eq!(status, "succeeded");
    assert!(summary["timeline_digest"].as_str().is_some_and(|d| !d.is_empty()));
    Ok(())
}
