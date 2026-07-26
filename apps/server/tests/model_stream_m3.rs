//! TST-MODEL-M3: OpenAI-compatible + Anthropic stream against a local fixture HTTP server.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use janus_server::config::RunMode;
use janus_server::modules::models::interface::{ModelsInterface, ProviderInput, ProviderKind};
use janus_server::modules::models::stream_types::{
    ChatMessage, ChatRole, ContentPart, ModelRequest, ModelStreamEvent,
};
use janus_server::platform::{database::Database, secret::SecretCipher};
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::Mutex;

struct FixtureState {
    last_auth: Mutex<Option<String>>,
}

async fn openai_chat_fixture(
    state: axum::extract::State<Arc<FixtureState>>,
    headers: HeaderMap,
    Json(_body): Json<Value>,
) -> Response {
    if let Some(auth) = headers.get("authorization") {
        *state.last_auth.lock().await = Some(auth.to_str().unwrap_or("").to_owned());
    }
    let body = concat!(
        "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .expect("response")
}

async fn anthropic_messages_fixture(
    state: axum::extract::State<Arc<FixtureState>>,
    headers: HeaderMap,
    Json(_body): Json<Value>,
) -> Response {
    if let Some(key) = headers.get("x-api-key") {
        *state.last_auth.lock().await = Some(key.to_str().unwrap_or("").to_owned());
    }
    let body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"!\"}}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .expect("response")
}

async fn openai_fail_fixture() -> Response {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"error":{"message":"bad key","type":"auth"}}"#,
        ))
        .expect("response")
}

async fn spawn_fixture(mode: &str) -> anyhow::Result<(SocketAddr, Arc<FixtureState>)> {
    let state = Arc::new(FixtureState {
        last_auth: Mutex::new(None),
    });
    let app = match mode {
        "openai" => Router::new()
            .route("/v1/chat/completions", post(openai_chat_fixture))
            .with_state(state.clone()),
        "anthropic" => Router::new()
            .route("/v1/messages", post(anthropic_messages_fixture))
            .with_state(state.clone()),
        "openai_fail" => Router::new()
            .route("/v1/chat/completions", post(openai_fail_fixture))
            .with_state(state.clone()),
        _ => anyhow::bail!("unknown fixture mode"),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((addr, state))
}

async fn models_with_root(temp: &TempDir) -> anyhow::Result<(Database, ModelsInterface, String)> {
    let database = Database::open(temp.path()).await?;
    let pool = database.pool().clone();
    let now = "2026-01-01T00:00:00.000Z";
    let tenant_id = "tenant-test";
    let owner_id = "owner-test";
    sqlx::query("INSERT INTO tenants (id, created_at) VALUES (?, ?)")
        .bind(tenant_id)
        .bind(now)
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO owners (id, tenant_id, display_name, created_at) VALUES (?, ?, ?, ?)")
        .bind(owner_id)
        .bind(tenant_id)
        .bind("Test Owner")
        .bind(now)
        .execute(&pool)
        .await?;
    let cipher = SecretCipher::load(temp.path(), RunMode::Development)?;
    let models = ModelsInterface::new(pool, cipher)?;
    Ok((database, models, owner_id.into()))
}

fn user_msg(text: &str) -> ChatMessage {
    ChatMessage {
        role: ChatRole::User,
        parts: vec![ContentPart::Text {
            text: text.to_owned(),
        }],
        tool_call_id: None,
    }
}

#[tokio::test]
async fn tst_model_m3_openai_chat_stream() -> anyhow::Result<()> {
    let (addr, _state) = spawn_fixture("openai").await?;
    let temp = TempDir::new()?;
    let (_db, models, owner) = models_with_root(&temp).await?;
    let provider = models
        .create_provider(
            &owner,
            ProviderInput {
                kind: ProviderKind::OpenaiChat,
                display_name: "Local OpenAI".into(),
                base_url: format!("http://{addr}/v1"),
                api_key: Some("sk-test-openai".into()),
                models: vec![
                    janus_server::modules::models::interface::EmbeddedModelInput {
                        display_name: "Fixture".into(),
                        upstream_model_id: "fixture-model".into(),
                        supports_1m: false,
                        supports_images: false,
                        enabled: true,
                    },
                ],
                enabled: true,
            },
        )
        .await?;

    let events = models
        .stream_completion(ModelRequest {
            owner_id: owner.clone(),
            provider_id: provider.id.clone(),
            upstream_model_id: "fixture-model".into(),
            messages: vec![user_msg("hi")],
            tools: vec![],
            round_id: Some("round-1".into()),
            project_id: Some("proj-1".into()),
            session_id: Some("sess-1".into()),
            turn_id: Some("turn-1".into()),
        })
        .await?;

    let text_deltas: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            ModelStreamEvent::Delta {
                text, provisional, ..
            } => {
                assert!(*provisional);
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(text_deltas, ["Hel", "lo"]);

    match events.last() {
        Some(ModelStreamEvent::Completed {
            text,
            usage,
            tool_calls,
            ..
        }) => {
            assert_eq!(text, "Hello");
            assert_eq!(usage.input_tokens, 3);
            assert_eq!(usage.output_tokens, 2);
            assert!(tool_calls.is_empty());
        }
        other => panic!("expected Completed, got {other:?}"),
    }

    // Attempt + ledger rows.
    let attempts: i64 =
        sqlx::query_scalar("SELECT COUNT(1) FROM model_attempts WHERE status = 'succeeded'")
            .fetch_one(_db.pool())
            .await?;
    assert_eq!(attempts, 1);
    let ledger: i64 = sqlx::query_scalar("SELECT COUNT(1) FROM model_usage_ledger")
        .fetch_one(_db.pool())
        .await?;
    assert_eq!(ledger, 1);

    Ok(())
}

#[tokio::test]
async fn tst_model_m3_anthropic_stream() -> anyhow::Result<()> {
    let (addr, _state) = spawn_fixture("anthropic").await?;
    let temp = TempDir::new()?;
    let (_db, models, owner) = models_with_root(&temp).await?;
    let provider = models
        .create_provider(
            &owner,
            ProviderInput {
                kind: ProviderKind::Anthropic,
                display_name: "Local Anthropic".into(),
                base_url: format!("http://{addr}/v1"),
                api_key: Some("sk-ant-test".into()),
                models: vec![
                    janus_server::modules::models::interface::EmbeddedModelInput {
                        display_name: "Claude Fixture".into(),
                        upstream_model_id: "claude-fixture".into(),
                        supports_1m: false,
                        supports_images: true,
                        enabled: true,
                    },
                ],
                enabled: true,
            },
        )
        .await?;

    let events = models
        .stream_completion(ModelRequest {
            owner_id: owner.clone(),
            provider_id: provider.id,
            upstream_model_id: "claude-fixture".into(),
            messages: vec![user_msg("yo")],
            tools: vec![],
            round_id: Some("round-a".into()),
            project_id: Some("p".into()),
            session_id: Some("s".into()),
            turn_id: Some("t".into()),
        })
        .await?;

    match events.last() {
        Some(ModelStreamEvent::Completed { text, usage, .. }) => {
            assert_eq!(text, "Hi!");
            assert_eq!(usage.input_tokens, 5);
            assert_eq!(usage.output_tokens, 2);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn failed_attempt_has_no_completed_output() -> anyhow::Result<()> {
    let (addr, _) = spawn_fixture("openai_fail").await?;
    let temp = TempDir::new()?;
    let (_db, models, owner) = models_with_root(&temp).await?;
    let provider = models
        .create_provider(
            &owner,
            ProviderInput {
                kind: ProviderKind::OpenaiChat,
                display_name: "Bad OpenAI".into(),
                base_url: format!("http://{addr}/v1"),
                api_key: Some("sk-bad".into()),
                models: vec![
                    janus_server::modules::models::interface::EmbeddedModelInput {
                        display_name: "X".into(),
                        upstream_model_id: "x".into(),
                        supports_1m: false,
                        supports_images: false,
                        enabled: true,
                    },
                ],
                enabled: true,
            },
        )
        .await?;

    let events = models
        .stream_completion(ModelRequest {
            owner_id: owner.clone(),
            provider_id: provider.id,
            upstream_model_id: "x".into(),
            messages: vec![user_msg("nope")],
            tools: vec![],
            round_id: Some("r".into()),
            project_id: Some("p".into()),
            session_id: Some("s".into()),
            turn_id: Some("t".into()),
        })
        .await?;

    assert!(
        events
            .iter()
            .all(|e| !matches!(e, ModelStreamEvent::Completed { .. })),
        "failed stream must not emit Completed"
    );
    assert!(matches!(
        events.last(),
        Some(ModelStreamEvent::Failed {
            code,
            ..
        }) if code == "PROVIDER_AUTH_FAILED"
    ));

    let failed: i64 =
        sqlx::query_scalar("SELECT COUNT(1) FROM model_attempts WHERE status = 'failed'")
            .fetch_one(_db.pool())
            .await?;
    assert_eq!(failed, 1);
    // No usage reported → no ledger row.
    let ledger: i64 = sqlx::query_scalar("SELECT COUNT(1) FROM model_usage_ledger")
        .fetch_one(_db.pool())
        .await?;
    assert_eq!(ledger, 0);

    // Secret must not appear in error detail strings we store.
    let err: Option<String> =
        sqlx::query_scalar("SELECT normalized_error_json FROM model_attempts LIMIT 1")
            .fetch_one(_db.pool())
            .await?;
    let err = err.unwrap_or_default();
    assert!(!err.contains("sk-bad"));
    assert!(!err.contains("bad key")); // raw upstream body not stored

    Ok(())
}

#[tokio::test]
async fn openai_assembler_unit_parse() {
    use janus_server::modules::models::openai_chat::OpenaiChatAssembler;
    let mut a = OpenaiChatAssembler::default();
    let e1 = a
        .ingest_data(
            "att-1",
            r#"{"choices":[{"delta":{"content":"A"},"index":0}]}"#,
        )
        .expect("test value");
    assert_eq!(e1.len(), 1);
    let e2 = a
        .ingest_data(
            "att-1",
            r#"{"choices":[{"delta":{"content":"B"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        )
        .expect("test value");
    assert_eq!(e2.len(), 1);
    match a.finish("att-1") {
        ModelStreamEvent::Completed { text, usage, .. } => {
            assert_eq!(text, "AB");
            assert_eq!(usage.input_tokens, 1);
        }
        other => panic!("{other:?}"),
    }
}
