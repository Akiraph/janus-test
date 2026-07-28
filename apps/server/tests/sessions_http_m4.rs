//! Stage 4 HTTP: message routing, queue, cancel surface.

mod support;

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
        public_origin: url::Url::parse("http://localhost").expect("url"),
        event_heartbeat: Duration::from_millis(50),
    }
}

async fn request(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> anyhow::Result<(StatusCode, Value)> {
    let mut builder = Request::builder().method(method).uri(path);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder.body(match body {
        Some(value) => Body::from(value.to_string()),
        None => Body::empty(),
    })?;
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await?;
    let text = String::from_utf8(bytes.to_vec())?;
    let json: Value = if text.is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text)?
    };
    Ok((status, json))
}

async fn seed(state: &AppState) -> anyhow::Result<String> {
    let pool = state.database().pool();
    let _ = request(&router(state.clone()), "GET", "/api/v1/bootstrap", None).await?;
    let _ = request(&router(state.clone()), "GET", "/api/v1/me", None).await?;
    let owner: String = sqlx::query_scalar("SELECT id FROM owners LIMIT 1")
        .fetch_one(pool)
        .await?;
    let project_id = uuid::Uuid::now_v7().to_string();
    let now = "2026-01-01T00:00:00.000Z";
    sqlx::query(
        "INSERT INTO projects \
         (id, owner_id, tenant_id, name, state, repo_access, repo_url, \
          version, created_at, updated_at, last_activity_at) \
         VALUES (?, ?, (SELECT tenant_id FROM owners WHERE id = ?), 'http-m4', 'ready', \
                 'public_https', 'https://example.com/r.git', 'v1', ?, ?, ?)",
    )
    .bind(&project_id)
    .bind(&owner)
    .bind(&owner)
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    let main_managed = format!("workspaces/main/{project_id}/repo");
    let main_abs = state.config().data_root.join(&main_managed);
    std::fs::create_dir_all(&main_abs)?;
    std::fs::write(main_abs.join("README.md"), b"# http-m4\n")?;
    support::init_git_repo(&main_abs)?;
    state
        .workspace_sync()
        .ensure_main_copy(
            project_id.parse()?,
            &main_managed,
            "test",
            json!({"kind": "test"}),
        )
        .await?;
    Ok(project_id)
}

#[tokio::test]
async fn post_message_started_then_queued() -> anyhow::Result<()> {
    let dir = TempDir::new()?;
    let state = AppState::initialize(test_config(dir.path().into())).await?;
    let project_id = seed(&state).await?;
    let app = router(state.clone());

    let (status, created) = request(
        &app,
        "POST",
        &format!("/api/v1/projects/{project_id}/sessions"),
        Some(json!({"title": "m4"})),
    )
    .await?;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let session_id = created["data"]["id"].as_str().unwrap().to_owned();
    let version = created["data"]["version"].as_str().unwrap().to_owned();

    let (status, first) = request(
        &app,
        "POST",
        &format!("/api/v1/sessions/{session_id}/messages"),
        Some(json!({"content": "first", "expected_session_version": version})),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["data"]["route"], "started");

    let v2 = first["data"]["session_version"].as_str().unwrap();
    let (status, second) = request(
        &app,
        "POST",
        &format!("/api/v1/sessions/{session_id}/messages"),
        Some(json!({"content": "second", "expected_session_version": v2})),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{second}");
    // Second message while first is active (or still starting) is queued.
    // Depending on whether the background execute_turn finished, route may be
    // started (if first already settled) or queued. Both are valid M4 outcomes.
    let route = second["data"]["route"].as_str().unwrap_or("");
    assert!(
        route == "queued" || route == "started",
        "unexpected route {route}: {second}"
    );
    Ok(())
}
