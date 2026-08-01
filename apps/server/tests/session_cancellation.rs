use std::{net::SocketAddr, path::PathBuf, time::Duration};

use janus_server::{
    AppState,
    config::{Config, RunMode},
    platform::id::{ProjectId, SessionId, TurnId},
    router,
};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};

const NOW: &str = "2026-07-31T00:00:00.000Z";

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

struct LiveServer {
    base_url: String,
    task: JoinHandle<()>,
}

impl Drop for LiveServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn(state: AppState) -> anyhow::Result<LiveServer> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router(state)).await {
            panic!("test server failed: {error}");
        }
    });
    Ok(LiveServer {
        base_url: format!("http://{address}"),
        task,
    })
}

struct SeededSession {
    session_id: SessionId,
    active_turn_id: TurnId,
    queued_turn_id: Option<TurnId>,
}

async fn seed_session(
    state: &AppState,
    include_queued_turn: bool,
) -> anyhow::Result<SeededSession> {
    let pool = state.database().pool();
    let project_id = ProjectId::new();
    let session_id = SessionId::new();
    let active_turn_id = TurnId::new();
    let queued_turn_id = include_queued_turn.then(TurnId::new);

    sqlx::query("INSERT INTO tenants (id, created_at) VALUES ('tenant-cancel-test', ?)")
        .bind(NOW)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO owners (id, tenant_id, display_name, created_at) \
         VALUES ('owner-cancel-test', 'tenant-cancel-test', 'Owner', ?)",
    )
    .bind(NOW)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO projects \
         (id, owner_id, tenant_id, name, state, repo_access, repo_url, version, \
          created_at, updated_at, last_activity_at) \
         VALUES (?, 'owner-cancel-test', 'tenant-cancel-test', 'Project', 'ready', \
                 'public_https', 'https://example.com/repo.git', 'v_project', ?, ?, ?)",
    )
    .bind(project_id.to_string())
    .bind(NOW)
    .bind(NOW)
    .bind(NOW)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO sessions \
         (id, project_id, kind, state, workspace_handle, active_turn_id, \
          source_main_revision_id, version, created_at, updated_at, last_activity_at) \
         VALUES (?, ?, 'regular', 'active', 'workspace', ?, 'revision', \
                 'v_session', ?, ?, ?)",
    )
    .bind(session_id.to_string())
    .bind(project_id.to_string())
    .bind(active_turn_id.to_string())
    .bind(NOW)
    .bind(NOW)
    .bind(NOW)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO turns \
         (id, session_id, sequence, status, model_snapshot_json, version, created_at, updated_at) \
         VALUES (?, ?, 1, 'running', '{}', 'v_turn_1', ?, ?)",
    )
    .bind(active_turn_id.to_string())
    .bind(session_id.to_string())
    .bind(NOW)
    .bind(NOW)
    .execute(pool)
    .await?;
    if let Some(turn_id) = queued_turn_id {
        sqlx::query(
            "INSERT INTO turns \
             (id, session_id, sequence, status, model_snapshot_json, version, created_at, updated_at) \
             VALUES (?, ?, 2, 'queued', '{}', 'v_turn_2', ?, ?)",
        )
        .bind(turn_id.to_string())
        .bind(session_id.to_string())
        .bind(NOW)
        .bind(NOW)
        .execute(pool)
        .await?;
    }

    Ok(SeededSession {
        session_id,
        active_turn_id,
        queued_turn_id,
    })
}

fn cancel_url(server: &LiveServer, session_id: SessionId, turn_id: TurnId) -> String {
    format!(
        "{}/api/v1/sessions/{session_id}/turns/{turn_id}/cancel",
        server.base_url
    )
}

#[tokio::test]
async fn cancel_endpoint_validates_version_and_preserves_active_turn_when_canceling_queue()
-> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let seeded = seed_session(&state, true).await?;
    let queued_turn_id = seeded.queued_turn_id.expect("queued Turn fixture");
    let server = spawn(state.clone()).await?;
    let client = Client::new();
    let url = cancel_url(&server, seeded.session_id, queued_turn_id);

    let missing_version = client
        .post(&url)
        .json(&json!({ "reason": "user_cancel" }))
        .send()
        .await?;
    assert_eq!(missing_version.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        missing_version
            .text()
            .await?
            .contains("expected_session_version")
    );

    let stale_version = client
        .post(&url)
        .json(&json!({
            "expected_session_version": "v_stale",
            "reason": "user_cancel"
        }))
        .send()
        .await?;
    assert_eq!(stale_version.status(), StatusCode::PRECONDITION_FAILED);
    let problem: Value = stale_version.json().await?;
    assert_eq!(problem["code"], "RESOURCE_VERSION_MISMATCH");

    let accepted = client
        .post(&url)
        .json(&json!({
            "expected_session_version": "v_session",
            "reason": "user_cancel"
        }))
        .send()
        .await?;
    assert_eq!(accepted.status(), StatusCode::OK);
    let response: Value = accepted.json().await?;
    assert_eq!(response["data"]["turn_id"], queued_turn_id.to_string());
    assert_eq!(response["data"]["from_status"], "queued");
    assert_eq!(response["data"]["to_status"], "canceled");
    let next_version = response["data"]["session_version"]
        .as_str()
        .expect("Session version in cancellation response");
    assert_ne!(next_version, "v_session");

    let pool = state.database().pool();
    let queued_status: String = sqlx::query_scalar("SELECT status FROM turns WHERE id = ?")
        .bind(queued_turn_id.to_string())
        .fetch_one(pool)
        .await?;
    assert_eq!(queued_status, "canceled");
    let session: (String, Option<String>, String) =
        sqlx::query_as("SELECT state, active_turn_id, version FROM sessions WHERE id = ?")
            .bind(seeded.session_id.to_string())
            .fetch_one(pool)
            .await?;
    assert_eq!(session.0, "active");
    assert_eq!(session.1, Some(seeded.active_turn_id.to_string()));
    assert_eq!(session.2, next_version);

    Ok(())
}

#[tokio::test]
async fn cancel_endpoint_settles_active_turn_and_releases_session() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let seeded = seed_session(&state, false).await?;
    let server = spawn(state.clone()).await?;
    let client = Client::new();

    let accepted = client
        .post(cancel_url(
            &server,
            seeded.session_id,
            seeded.active_turn_id,
        ))
        .json(&json!({
            "expected_session_version": "v_session",
            "reason": "user_cancel"
        }))
        .send()
        .await?;
    assert_eq!(accepted.status(), StatusCode::OK);
    let response: Value = accepted.json().await?;
    assert_eq!(
        response["data"]["turn_id"],
        seeded.active_turn_id.to_string()
    );
    assert_eq!(response["data"]["from_status"], "running");
    assert_eq!(response["data"]["to_status"], "canceled");
    let next_version = response["data"]["session_version"]
        .as_str()
        .expect("Session version in cancellation response");
    assert_ne!(next_version, "v_session");

    let pool = state.database().pool();
    let turn: (String, Option<String>) =
        sqlx::query_as("SELECT status, cancellation_reason FROM turns WHERE id = ?")
            .bind(seeded.active_turn_id.to_string())
            .fetch_one(pool)
            .await?;
    assert_eq!(turn, ("canceled".into(), Some("user_cancel".into())));
    let session: (String, Option<String>, String) =
        sqlx::query_as("SELECT state, active_turn_id, version FROM sessions WHERE id = ?")
            .bind(seeded.session_id.to_string())
            .fetch_one(pool)
            .await?;
    assert_eq!(session, ("ready".into(), None, next_version.into()));

    Ok(())
}
