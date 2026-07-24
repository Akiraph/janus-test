use std::{net::SocketAddr, path::PathBuf, process::Command, time::Duration};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use janus_server::{
    AppState,
    application::workers,
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
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> anyhow::Result<(StatusCode, String)> {
    let mut builder = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
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
    Ok((status, String::from_utf8(bytes.to_vec())?))
}

fn make_public_repo() -> anyhow::Result<TempDir> {
    let directory = TempDir::new()?;
    let status = Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .current_dir(directory.path())
        .status()?;
    if !status.success() {
        anyhow::bail!("git init --bare failed");
    }

    let work = TempDir::new()?;
    let work_path = work.path();
    let status = Command::new("git")
        .args(["clone", directory.path().to_str().expect("utf8 path"), "."])
        .current_dir(work_path)
        .status()?;
    if !status.success() {
        anyhow::bail!("git clone bare into work failed");
    }
    std::fs::write(work_path.join("README.md"), "# fixture\n")?;
    let config_email = Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(work_path)
        .status()?;
    if !config_email.success() {
        anyhow::bail!("git config user.email failed");
    }
    let config_name = Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(work_path)
        .status()?;
    if !config_name.success() {
        anyhow::bail!("git config user.name failed");
    }
    let add = Command::new("git")
        .args(["add", "README.md"])
        .current_dir(work_path)
        .status()?;
    if !add.success() {
        anyhow::bail!("git add failed");
    }
    let commit = Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(work_path)
        .status()?;
    if !commit.success() {
        anyhow::bail!("git commit failed");
    }
    let push = Command::new("git")
        .args(["push", "origin", "main"])
        .current_dir(work_path)
        .status()?;
    if !push.success() {
        anyhow::bail!("git push failed");
    }
    Ok(directory)
}

#[tokio::test]
async fn public_clone_reaches_ready_and_exposes_files_and_git() -> anyhow::Result<()> {
    let remote = make_public_repo()?;
    let data_root = TempDir::new()?;
    let state = AppState::initialize(test_config(data_root.path().into())).await?;
    workers::spawn(state.clone());
    let app = router(state.clone());

    let remote_url = remote
        .path()
        .to_str()
        .expect("utf8 remote path")
        // git accepts file:// URLs for local bare repos.
        .replace('\\', "/");
    let remote_url = format!("file:///{remote_url}");

    let (status, response) = request(
        &app,
        "POST",
        "/api/v1/projects",
        &[("Idempotency-Key", "test-create-1")],
        Some(json!({
            "name": "Fixture",
            "repository": {
                "access": "public_https",
                "url": remote_url,
                "branch": "main"
            }
        })),
    )
    .await?;
    assert_eq!(status, StatusCode::ACCEPTED, "{response}");
    let created: Value = serde_json::from_str(&response)?;
    let operation_id = created["data"]["id"]
        .as_str()
        .expect("operation id")
        .to_owned();

    // Poll the Operation until the worker finishes the clone.
    let mut project_id = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (status, response) = request(
            &app,
            "GET",
            &format!("/api/v1/operations/{operation_id}"),
            &[],
            None,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{response}");
        let body: Value = serde_json::from_str(&response)?;
        let op_status = body["data"]["status"].as_str().unwrap_or("");
        if op_status == "succeeded" || op_status == "failed" || op_status == "needs_attention" {
            assert_eq!(op_status, "succeeded", "{response}");
            project_id = body["data"]["target_id"].as_str().map(str::to_owned);
            break;
        }
    }
    let project_id = project_id.expect("clone operation never finished");

    let (status, response) = request(
        &app,
        "GET",
        &format!("/api/v1/projects/{project_id}"),
        &[],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{response}");
    let project: Value = serde_json::from_str(&response)?;
    assert_eq!(project["data"]["state"], "ready", "{response}");
    assert!(project["data"]["main_revision"].as_str().is_some());

    let (status, response) = request(
        &app,
        "GET",
        &format!("/api/v1/projects/{project_id}/files/tree?path="),
        &[],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert!(response.contains("README.md"), "{response}");

    let (status, response) = request(
        &app,
        "GET",
        &format!("/api/v1/projects/{project_id}/git/status"),
        &[],
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{response}");
    let status_body: Value = serde_json::from_str(&response)?;
    assert_eq!(status_body["data"]["branch"], "main");

    // Edit a file through the Main Workspace API and commit it.
    let revision = project["data"]["main_revision"]
        .as_str()
        .expect("main revision")
        .to_owned();
    let (status, response) = request(
        &app,
        "PUT",
        &format!("/api/v1/projects/{project_id}/files/text"),
        &[],
        Some(json!({
            "path": "README.md",
            "content": "# fixture\nedited\n",
            "expected_main_revision": revision
        })),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{response}");

    let (status, response) = request(
        &app,
        "POST",
        &format!("/api/v1/projects/{project_id}/git/commands/stage"),
        &[],
        Some(json!({ "paths": ["README.md"] })),
    )
    .await?;
    assert_eq!(status, StatusCode::NO_CONTENT, "{response}");

    let (status, response) = request(
        &app,
        "POST",
        &format!("/api/v1/projects/{project_id}/git/commands/commit"),
        &[],
        Some(json!({ "message": "edit readme" })),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{response}");
    let commit: Value = serde_json::from_str(&response)?;
    assert!(
        commit["data"].as_str().is_some_and(|sha| sha.len() >= 7),
        "{response}"
    );

    Ok(())
}

#[tokio::test]
async fn create_project_requires_idempotency_key() -> anyhow::Result<()> {
    let data_root = TempDir::new()?;
    let state = AppState::initialize(test_config(data_root.path().into())).await?;
    let app = router(state);
    let (status, response) = request(
        &app,
        "POST",
        "/api/v1/projects",
        &[],
        Some(json!({
            "name": "No Key",
            "repository": {
                "access": "public_https",
                "url": "https://example.com/org/repo.git",
                "branch": "main"
            }
        })),
    )
    .await?;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    assert!(
        response.contains("VALIDATION_FAILED") || response.contains("Idempotency"),
        "{response}"
    );
    Ok(())
}
