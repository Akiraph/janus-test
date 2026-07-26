//! Stage 5 HTTP smoke: create session + list + timeline via public routes.

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

#[tokio::test]
async fn sessions_http_create_list_get_diff() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let state = AppState::initialize(test_config(directory.path().into())).await?;
    let app = router(state.clone());

    // Create a ready project via DB (skip clone operation for smoke).
    let pool = state.database().pool();
    // Ensure development owner exists via authenticate path on first request.
    let _ = request(&app, "GET", "/api/v1/bootstrap", None).await?;
    // Touch auth to ensure development owner.
    let _ = request(&app, "GET", "/api/v1/me", None).await?;
    let owner: String = sqlx::query_scalar("SELECT id FROM owners LIMIT 1")
        .fetch_one(pool)
        .await?;
    let project_id = uuid::Uuid::now_v7().to_string();
    let now = "2026-01-01T00:00:00.000Z";
    sqlx::query(
        "INSERT INTO projects \
         (id, owner_id, tenant_id, name, state, repo_access, repo_url, \
          version, created_at, updated_at, last_activity_at) \
         VALUES (?, ?, (SELECT tenant_id FROM owners WHERE id = ?), 'http-session', 'ready', \
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
    let main_abs = directory.path().join(&main_managed);
    std::fs::create_dir_all(&main_abs)?;
    std::fs::write(main_abs.join("README.md"), b"# http\n")?;
    state
        .workspace_sync()
        .ensure_main_copy(
            project_id.parse()?,
            &main_managed,
            "test",
            json!({"kind": "test"}),
        )
        .await?;

    let (status, created) = request(
        &app,
        "POST",
        &format!("/api/v1/projects/{project_id}/sessions"),
        Some(json!({"title": "from-http"})),
    )
    .await?;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let session_id = created["data"]["id"]
        .as_str()
        .expect("session id")
        .to_owned();
    assert_eq!(created["data"]["title"], "from-http");

    let (status, listed) = request(
        &app,
        "GET",
        &format!("/api/v1/projects/{project_id}/sessions"),
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["data"].as_array().expect("sessions list").len(), 1);

    let (status, got) =
        request(&app, "GET", &format!("/api/v1/sessions/{session_id}"), None).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["data"]["id"], session_id);

    let (status, diff) = request(
        &app,
        "GET",
        &format!("/api/v1/sessions/{session_id}/diff"),
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{diff}");
    assert_eq!(diff["data"]["apply_enabled"], false);

    let (status, _) = request(
        &app,
        "DELETE",
        &format!("/api/v1/sessions/{session_id}"),
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::NO_CONTENT);

    Ok(())
}
