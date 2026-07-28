//! Stage 6: supervisor control surface for `waiting_for_model` — retry-model
//! resumes a parked Turn and re-enters the execution loop with a fresh stream.
//!
//! Cancel mid-loop while parked is covered by `sessions_m4::cancel`; here we
//! exercise the retry-model entry only.

mod support;

use std::net::SocketAddr;
use std::str::FromStr;
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
use janus_server::platform::id::{ProjectId, SessionId, TurnId};
use janus_server::platform::sleeper::FakeSleeper;
use janus_server::platform::{events::EventStore, secret::SecretCipher};
use janus_server::platform::{database::Database, managed_storage::BlobStore};
use janus_server::modules::workspace_sync::interface::WorkspaceSyncInterface;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;

const FINISH_STREAM: &str = "\
data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_f\",\"type\":\"function\",\"function\":{\"name\":\"finish\",\"arguments\":\"\"}}]}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"summary\\\":\\\"recovered\\\"}\"}}]}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":4}}\n\n\
data: [DONE]\n\n";

async fn finish_ok(Json(_b): Json<Value>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(FINISH_STREAM))
        .unwrap()
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
        .with_retry_sleeper(
            sleeper.clone() as std::sync::Arc<dyn janus_server::platform::sleeper::Sleeper>,
        );

        Ok(Self {
            _temp: temp,
            _db: database,
            pool,
            sessions,
            supervisor,
            project_id,
        })
    }
}

#[tokio::test]
async fn retry_model_resumes_parked_turn() -> anyhow::Result<()> {
    let addr = spawn(post(finish_ok)).await?;
    let sleeper = Arc::new(FakeSleeper::default());
    let fx = Fx::new(&format!("http://{addr}/v1"), sleeper).await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let session = fx
        .sessions
        .create_session(fx.project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;
    let routed = fx
        .sessions
        .post_message(session_id, "go", &session.version, actor)
        .await?;
    let turn_id = TurnId::from_str(&routed.turn_id)?;

    // Park manually to simulate an exhausted retry (avoids a second fixture).
    fx.supervisor
        .enter_waiting_for_model(session_id, turn_id, "fixture: parked for test")
        .await?;
    let parked = fx.sessions.get_turn(session_id, turn_id).await?;
    assert_eq!(parked.status, "waiting_for_model");

    // retry_model resumes and the stream now completes.
    fx.supervisor.retry_model(turn_id).await?;
    let turn = fx.sessions.get_turn(session_id, turn_id).await?;
    assert_eq!(turn.status, "completed");
    Ok(())
}

#[tokio::test]
async fn retry_model_idempotent_on_running_turn() -> anyhow::Result<()> {
    let addr = spawn(post(finish_ok)).await?;
    let sleeper = Arc::new(FakeSleeper::default());
    let fx = Fx::new(&format!("http://{addr}/v1"), sleeper).await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let session = fx
        .sessions
        .create_session(fx.project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;
    let routed = fx
        .sessions
        .post_message(session_id, "go", &session.version, actor)
        .await?;
    let turn_id = TurnId::from_str(&routed.turn_id)?;
    // Turn is already running and stream completes -> retry_model is a no-op re-execution.
    fx.supervisor.retry_model(turn_id).await?;
    let turn = fx.sessions.get_turn(session_id, turn_id).await?;
    assert_eq!(turn.status, "completed");
    Ok(())
}
