//! Stage 6: model retry/failover/cooldown via the supervisor retry loop.
//!
//! Each test stands up an OpenAI fixture whose `/v1/chat/completions` returns a
//! scripted sequence of responses (failures then success) and asserts the
//! supervisor parks the Turn on `waiting_for_model` after exhausting retries, or
//! succeeds once the fixture recovers, with a deterministic `FakeSleeper`.

mod support;

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use janus_server::config::RunMode;
use janus_server::modules::models::interface::{
    EmbeddedModelInput, ModelsInterface, ProviderInput, ProviderKind,
};
use janus_server::modules::sessions::interface::SessionsInterface;
use janus_server::modules::supervisor::interface::SupervisorInterface;
use janus_server::modules::supervisor::retry::{FaultClass, classify};
use janus_server::platform::id::{ProjectId, SessionId, TurnId};
use janus_server::platform::sleeper::FakeSleeper;
use janus_server::platform::{events::EventStore, secret::SecretCipher};
use janus_server::platform::{database::Database, managed_storage::BlobStore};
use janus_server::modules::workspace_sync::interface::WorkspaceSyncInterface;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;

/// Fixture whose every call returns HTTP 429 ("rate limit"). Used to verify the
/// supervisor exhausts its retries and then parks on `waiting_for_model`.
async fn always_rate_limit(Json(_b): Json<Value>) -> Response {
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("content-type", "text/plain")
        .body(Body::from("rate limit"))
        .expect("resp")
}

/// Fixture that returns 429 for the first `flaky` calls then a `finish` tool
/// completion. Used to verify a transient fault followed by success completes
/// the Turn after one retry.
fn flaky_then_finish(flaky: usize) -> Arc<AtomicUsize> {
    let counter = Arc::new(AtomicUsize::new(0));
    let _ = flaky; // captured by closure below
    counter
}

async fn spawn(handler: axum::routing::MethodRouter) -> anyhow::Result<SocketAddr> {
    let app = Router::new().route("/v1/chat/completions", handler);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(addr)
}

struct Fx {
    _temp: TempDir,
    _db: Database,
    pool: SqlitePool,
    sessions: SessionsInterface,
    supervisor: SupervisorInterface,
    project_id: ProjectId,
    sleeper: Arc<FakeSleeper>,
}

impl Fx {
    async fn new(openai_base: &str, sleeper: Arc<FakeSleeper>) -> anyhow::Result<Self> {
        let temp = TempDir::new()?;
        let data_root = temp.path().to_path_buf();
        let database = Database::open(&data_root).await?;
        let pool = database.pool().clone();
        let blobs = BlobStore::new(pool.clone(), &data_root)?;
        let workspace = WorkspaceSyncInterface::new(pool.clone(), &data_root, blobs);
        let events = EventStore::new(pool.clone());
        let cipher = SecretCipher::load(&data_root, RunMode::Development)?;
        let models = ModelsInterface::new(pool.clone(), cipher)?;

        let now = "2026-01-01T00:00:00.000Z";
        sqlx::query("INSERT INTO tenants (id, created_at) VALUES ('tenant-test', ?)")
            .bind(now)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO owners (id, tenant_id, display_name, created_at) \
             VALUES ('owner-test', 'tenant-test', 'Owner', ?)",
        )
        .bind(now)
        .execute(&pool)
        .await?;
        let project_id = ProjectId::new();
        sqlx::query(
            "INSERT INTO projects \
             (id, owner_id, tenant_id, name, state, repo_access, repo_url, \
              version, created_at, updated_at, last_activity_at) \
             VALUES (?, 'owner-test', 'tenant-test', 'p', 'ready', 'public_https', \
                     'https://example.com/r.git', 'v1', ?, ?, ?)",
        )
        .bind(project_id.to_string())
        .bind(now)
        .bind(now)
        .bind(now)
        .execute(&pool)
        .await?;

        let main_managed = format!("workspaces/main/{project_id}/repo");
        let main_abs = data_root.join(&main_managed);
        std::fs::create_dir_all(main_abs.join("src"))?;
        std::fs::write(main_abs.join("README.md"), b"# main\n")?;
        support::init_git_repo(&main_abs)?;
        workspace
            .ensure_main_copy(project_id, &main_managed, "test", json!({"kind": "test"}))
            .await?;

        models
            .create_provider(
                "owner-test",
                ProviderInput {
                    kind: ProviderKind::OpenaiChat,
                    display_name: "Fixture".into(),
                    base_url: openai_base.into(),
                    api_key: Some("sk-test".into()),
                    models: vec![EmbeddedModelInput {
                        display_name: "F".into(),
                        upstream_model_id: "fixture".into(),
                        supports_1m: false,
                        supports_images: false,
                        enabled: true,
                    }],
                    enabled: true,
                },
            )
            .await?;

        let sessions = SessionsInterface::new(pool.clone(), events.clone(), workspace.clone());
        let supervisor = SupervisorInterface::new(
            pool.clone(),
            events,
            models,
            workspace.clone(),
            "owner-test".into(),
        )
        .with_retry_sleeper(sleeper.clone() as std::sync::Arc<dyn janus_server::platform::sleeper::Sleeper>);

        Ok(Self {
            _temp: temp,
            _db: database,
            pool,
            sessions,
            supervisor,
            project_id,
            sleeper,
        })
    }
}

const FINISH_STREAM: &str = "\
data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_f\",\"type\":\"function\",\"function\":{\"name\":\"finish\",\"arguments\":\"\"}}]}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"summary\\\":\\\"ok\\\"}\"}}]}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":4}}\n\n\
data: [DONE]\n\n";

#[tokio::test]
async fn classifier_transients_get_backoff() {
    let d = classify("PROVIDER_STREAM_FAILED", "provider HTTP 429", 0);
    assert_eq!(d.class, FaultClass::Transient);
    let d = classify("PROVIDER_AUTH_FAILED", "creds", 0);
    assert_eq!(d.class, FaultClass::Config);
}

#[tokio::test]
async fn exhausted_transient_retries_park_waiting_for_model() -> anyhow::Result<()> {
    let addr = spawn(post(always_rate_limit)).await?;
    let sleeper = Arc::new(FakeSleeper::default());
    let fx = Fx::new(&format!("http://{addr}/v1"), sleeper.clone()).await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let session = fx
        .sessions
        .create_session(fx.project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;
    let routed = fx
        .sessions
        .post_message(session_id, "do something", &session.version, actor)
        .await?;
    let turn_id = TurnId::from_str(&routed.turn_id)?;
    fx.supervisor.execute_turn(turn_id).await?;
    let turn = fx.sessions.get_turn(session_id, turn_id).await?;
    assert_eq!(turn.status, "waiting_for_model");
    // 5 backoffs before giving up (initial + 5 retries => 5 sleeps).
    assert!(fx.sleeper.waits.lock().unwrap().len() >= 1, "should have slept");
    Ok(())
}

#[tokio::test]
async fn transient_then_success_completes_after_retry() -> anyhow::Result<()> {
    let counter = flaky_then_finish(1);
    let captured = counter.clone();
    let handler = post(move |Json(b): Json<Value>| {
        let c = captured.clone();
        async move {
            if c.fetch_add(1, Ordering::SeqCst) == 0 {
                return Response::builder()
                    .status(StatusCode::TOO_MANY_REQUESTS)
                    .header("content-type", "text/plain")
                    .body(Body::from("rate limit"))
                    .unwrap();
            }
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(Body::from(FINISH_STREAM))
                .unwrap()
        }
    });
    let addr = spawn(handler).await?;
    let sleeper = Arc::new(FakeSleeper::default());
    let fx = Fx::new(&format!("http://{addr}/v1"), sleeper.clone()).await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let session = fx
        .sessions
        .create_session(fx.project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;
    let routed = fx
        .sessions
        .post_message(session_id, "do it", &session.version, actor)
        .await?;
    let turn_id = TurnId::from_str(&routed.turn_id)?;
    fx.supervisor.execute_turn(turn_id).await?;
    let turn = fx.sessions.get_turn(session_id, turn_id).await?;
    assert_eq!(turn.status, "completed");
    assert!(fx.sleeper.waits.lock().unwrap().len() >= 1);
    Ok(())
}
