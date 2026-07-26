//! Stage 4: supervisor tools + execute_turn against OpenAI fixture.

use std::net::SocketAddr;
use std::str::FromStr;

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
use janus_server::modules::supervisor::registry::{is_forbidden_tool, is_registered};
use janus_server::modules::supervisor::tools::{ToolContext, execute_tool};
use janus_server::modules::workspace_sync::interface::WorkspaceSyncInterface;
use janus_server::platform::{
    database::Database,
    events::EventStore,
    id::{ProjectId, SessionId, TurnId},
    managed_storage::BlobStore,
    secret::SecretCipher,
};
use serde_json::{Value, json};
use tempfile::TempDir;

async fn openai_finish_fixture(Json(_body): Json<Value>) -> Response {
    // One tool call: finish
    let body = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_finish\",\"type\":\"function\",\"function\":{\"name\":\"finish\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"summary\\\":\\\"all good\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n",
        "data: [DONE]\n\n",
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .expect("response")
}

async fn spawn_openai() -> anyhow::Result<SocketAddr> {
    let app = Router::new().route("/v1/chat/completions", post(openai_finish_fixture));
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
    sessions: SessionsInterface,
    supervisor: SupervisorInterface,
    workspace: WorkspaceSyncInterface,
    project_id: ProjectId,
}

impl Fx {
    async fn new(openai_base: &str) -> anyhow::Result<Self> {
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
        std::fs::write(main_abs.join("src/lib.rs"), b"fn main() {}\n")?;
        let minimal_png = minimal_png_1x1();
        std::fs::write(main_abs.join("dot.png"), &minimal_png)?;

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
                        supports_images: true,
                        enabled: true,
                    }],
                    enabled: true,
                },
            )
            .await?;

        let sessions = SessionsInterface::new(pool.clone(), events.clone(), workspace.clone());
        let supervisor =
            SupervisorInterface::new(pool, events, models, workspace.clone(), "owner-test".into());

        Ok(Self {
            _temp: temp,
            _db: database,
            sessions,
            supervisor,
            workspace,
            project_id,
        })
    }
}

fn minimal_png_1x1() -> Vec<u8> {
    // Precomputed 1x1 red PNG
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x05, 0xFE, 0xD4, 0xEF, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

#[test]
fn registry_forbids_git_write() {
    assert!(is_registered("fs.read"));
    assert!(is_registered("finish"));
    assert!(is_forbidden_tool("git.commit"));
    assert!(is_forbidden_tool("bash"));
    assert!(!is_registered("git.commit"));
}

#[tokio::test]
async fn tools_list_read_write_and_image() -> anyhow::Result<()> {
    let addr = spawn_openai().await?;
    let fx = Fx::new(&format!("http://{addr}/v1")).await?;
    let actor = json!({"kind": "test"});
    let session = fx
        .sessions
        .create_session(fx.project_id, Some("s".into()), actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;
    let ctx = ToolContext {
        session_id,
        workspace: &fx.workspace,
        actor,
    };

    let listed = execute_tool(&ctx, "fs.list", &json!({"path": "."})).await?;
    assert!(listed.ok);

    let read = execute_tool(&ctx, "fs.read", &json!({"path": "README.md"})).await?;
    assert!(read.ok);

    let img = execute_tool(&ctx, "fs.read", &json!({"path": "dot.png"})).await?;
    assert!(img.ok, "{:?}", img.summary);
    // Summary must not contain base64 payloads.
    let s = img.summary.to_string();
    assert!(!s.contains("base64"));
    assert!(s.contains("image/png") || s.contains("\"kind\":\"image\""));

    let wrote = execute_tool(
        &ctx,
        "fs.write",
        &json!({"path": "hello.txt", "content": "hi"}),
    )
    .await?;
    assert!(wrote.ok);

    Ok(())
}

#[tokio::test]
async fn execute_turn_finish_tool_completes() -> anyhow::Result<()> {
    let addr = spawn_openai().await?;
    let fx = Fx::new(&format!("http://{addr}/v1")).await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let session = fx
        .sessions
        .create_session(fx.project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&session.id)?;
    let routed = fx
        .sessions
        .post_message(session_id, "please finish", &session.version, actor)
        .await?;
    let turn_id = TurnId::from_str(&routed.turn_id)?;
    fx.supervisor.execute_turn(turn_id).await?;

    let turn = fx.sessions.get_turn(session_id, turn_id).await?;
    assert_eq!(turn.status, "completed");
    let session = fx.sessions.get_session(session_id).await?;
    assert_eq!(session.state, "ready");
    assert!(session.active_turn_id.is_none());

    Ok(())
}
