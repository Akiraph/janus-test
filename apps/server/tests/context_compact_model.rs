//! Model-driven context compaction.
//!
//! `run_context_compact_operation` must generate the compact summary with a
//! real model attempt (ledger `attempt_type = 'compact'`), persist the model's
//! text plus its real token usage on the `compact_summaries` row, and complete
//! the context version. A second pass covers the degraded path: when no model
//! is configured the operation still completes with the mechanical digest and
//! an explicit `summary_model_status` marker.

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use axum::Router;
use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::post;
use janus_infrastructure::operations::IdempotencyRequest;
use janus_infrastructure::{
    clock::format_utc,
    id::{CorrelationId, ProjectId, SessionId},
};
use janus_models::interface::{ModelClient, ProviderInput, ProviderKind};
use janus_server::application::context::{CompactContextRequest, run_context_compact_operation};
use janus_server::{
    AppState,
    config::{Config, RunMode},
};
use mongodb::bson::{doc, Document};
use serde_json::{Value, json};
use tempfile::TempDir;

const NOW: &str = "2026-08-19T00:00:00.000Z";

static TEST_DB_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn test_config(data_root: PathBuf) -> Config {
    Config {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        data_root,
        web_dist: None,
        mode: RunMode::Development,
        development_auth: true,
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
    pool.collection::<Document>("owners")
        .insert_one(doc! {
            "_id": "owner",
            "display_name": "Owner",
            "created_at": NOW,
        })
        .await?;
    pool.collection::<Document>("projects")
        .insert_one(doc! {
            "_id": project_id.to_string(),
            "owner_id": "owner",
            "name": "Compact Fixture",
            "state": "ready",
            "repo_access": "public_https",
            "repo_url": "https://example.com/repo.git",
            "repo_branch": null,
            "github_credential_id": null,
            "default_model_id": null,
            "main_workspace_handle": null,
            "clone_error": null,
            "version": "v1",
            "created_at": NOW,
            "updated_at": NOW,
            "last_activity_at": NOW,
        })
        .await?;
    pool.collection::<Document>("sessions")
        .insert_one(doc! {
            "_id": session_id.to_string(),
            "project_id": project_id.to_string(),
            "title": "Compact me",
            "state": "ready",
            "next_model_ref": null,
            "active_turn_id": null,
            "version": "v_session",
            "created_at": NOW,
            "updated_at": NOW,
            "last_activity_at": NOW,
        })
        .await?;

    // A minimal user/assistant exchange on the visible timeline.
    pool.collection::<Document>("timeline_items")
        .insert_one(doc! {
            "_id": "tl-user-1",
            "session_id": session_id.to_string(),
            "turn_id": null,
            "kind": "user_message",
            "source_resource_id": "msg-user-1",
            "display_order": 1i64,
            "projection_json": json!({
                "kind": "user_message",
                "text": "Fix the login bug in auth.rs and run the tests.",
            })
            .to_string(),
            "status": "active",
            "version": "v1",
            "created_at": NOW,
            "updated_at": NOW,
        })
        .await?;
    pool.collection::<Document>("timeline_items")
        .insert_one(doc! {
            "_id": "tl-assistant-1",
            "session_id": session_id.to_string(),
            "turn_id": null,
            "kind": "assistant_message",
            "source_resource_id": "msg-assistant-1",
            "display_order": 2i64,
            "projection_json": json!({
                "kind": "assistant_message",
                "text": "Fixed auth.rs and ran cargo test: 18 passed.",
            })
            .to_string(),
            "status": "active",
            "version": "v1",
            "created_at": NOW,
            "updated_at": NOW,
        })
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
    let model_id: String = {
        let model = state
            .pool()
            .collection::<Document>("models")
            .find_one(doc! {"provider_id": &provider.id})
            .sort(doc! {"created_at": 1})
            .await?
            .expect("fixture model should be persisted");
        model.get_str("_id")?.to_owned()
    };
    state
        .pool()
        .collection::<Document>("projects")
        .update_one(doc! {"owner_id": "owner"}, doc! {"$set": {"default_model_id": &model_id}})
        .await?;

    request_compact(&state, session_id).await?;

    // The compact summary row now carries the model's text and real usage.
    let summary_doc = state
        .pool()
        .collection::<Document>("compact_summaries")
        .find_one(doc! {"session_id": session_id.to_string()})
        .await?
        .expect("compact summary should be persisted");
    let summary_json = summary_doc.get_str("summary_json")?.to_owned();
    let input_tokens = summary_doc.get_i64("input_tokens")?;
    let output_tokens = summary_doc.get_i64("output_tokens")?;
    let attempt_id = summary_doc.get_str("model_attempt_id")?.to_owned();
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
    let attempt_type: String = state
        .pool()
        .collection::<Document>("model_attempts")
        .find_one(doc! {"_id": &attempt_id})
        .await?
        .expect("compact model attempt should be persisted")
        .get_str("attempt_type")?
        .to_owned();
    assert_eq!(attempt_type, "compact");

    // The context version completed with the real token count.
    let version_doc = state
        .pool()
        .collection::<Document>("context_versions")
        .find_one(doc! {"session_id": session_id.to_string()})
        .sort(doc! {"sequence": -1})
        .await?
        .expect("context version should be persisted");
    let status = version_doc.get_str("compact_status")?.to_owned();
    let estimated = version_doc.get_i64("estimated_input_tokens")?;
    assert_eq!(status, "succeeded");
    assert_eq!(estimated, 4200);

    // The visible timeline item exposes the generated summary.
    let projection: String = state
        .pool()
        .collection::<Document>("timeline_items")
        .find_one(doc! {"session_id": session_id.to_string(), "kind": "context_compacted"})
        .await?
        .expect("context_compacted timeline item should be persisted")
        .get_str("projection_json")?
        .to_owned();
    let projection: Value = serde_json::from_str(&projection)?;
    assert_eq!(
        projection["summary"]["text"]
            .as_str()
            .expect("timeline summary"),
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

    let summary_doc = state
        .pool()
        .collection::<Document>("compact_summaries")
        .find_one(doc! {"session_id": session_id.to_string()})
        .await?
        .expect("compact summary should be persisted");
    let summary_json = summary_doc.get_str("summary_json")?.to_owned();
    let compact_summary_id = summary_doc.get_str("_id")?.to_owned();
    let version_doc = state
        .pool()
        .collection::<Document>("context_versions")
        .find_one(doc! {
            "session_id": session_id.to_string(),
            "compact_summary_id": &compact_summary_id,
        })
        .await?
        .expect("context version should be persisted");
    let status = version_doc.get_str("compact_status")?.to_owned();
    let summary: Value = serde_json::from_str(&summary_json)?;
    assert_eq!(summary["summary_model_status"], "no_model_configured");
    assert_eq!(status, "succeeded");
    assert!(
        summary["timeline_digest"]
            .as_str()
            .is_some_and(|d| !d.is_empty())
    );
    Ok(())
}
