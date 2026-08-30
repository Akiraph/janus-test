//! Session project isolation for cross-session coordination surfaces.
//!
//! `active_sessions` and the `read_session` tool are documented as
//! project-scoped ("in this project"), but both previously read across every
//! project in the deployment: the SQL had no project filter and the tool never
//! compared the target session's project. This leaks other projects'
//! conversation content into the current Turn's context. These tests pin the
//! project-scoped behavior at the sessions interface layer; the tool-layer
//! guard is unit-tested in `janus-execution`.

use janus_infrastructure::id::{ProjectId, SessionId, TurnId};
use janus_server::{
    AppState,
    config::{Config, RunMode},
};
use mongodb::bson::{Bson, Document, doc};
use std::{net::SocketAddr, path::PathBuf, time::Duration};
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
        auth_mode: janus_identity::AuthMode::Passkey,
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

/// Insert one owner, one project, and one active session with a running turn.
async fn seed_project_with_active_session(
    state: &AppState,
    label: &str,
) -> anyhow::Result<(ProjectId, SessionId)> {
    let pool = state.pool();
    let project_id = ProjectId::new();
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let owner_id = format!("owner-{label}");
    pool.collection::<Document>("owners")
        .insert_one(doc! {
            "_id": &owner_id,
            "display_name": "Owner",
            "created_at": NOW,
        })
        .await?;
    pool.collection::<Document>("projects")
        .insert_one(doc! {
            "_id": project_id.to_string(),
            "owner_id": &owner_id,
            "name": label,
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
            "title": Bson::Null,
            "state": "active",
            "next_model_ref": Bson::Null,
            "active_turn_id": turn_id.to_string(),
            "version": "v_session",
            "created_at": NOW,
            "updated_at": NOW,
            "last_activity_at": NOW,
        })
        .await?;
    pool.collection::<Document>("turns")
        .insert_one(doc! {
            "_id": turn_id.to_string(),
            "session_id": session_id.to_string(),
            "sequence": 1i64,
            "status": "running",
            "goal_mode": 0i64,
            "model_snapshot_json": "{}",
            "version": "v_turn",
            "created_at": NOW,
            "updated_at": NOW,
        })
        .await?;
    Ok((project_id, session_id))
}

#[tokio::test]
async fn active_sessions_is_scoped_to_the_requesting_project() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let state = AppState::initialize(test_config(temp.path().into())).await?;
    let (project_a, session_a) = seed_project_with_active_session(&state, "alpha").await?;
    let (_project_b, _session_b) = seed_project_with_active_session(&state, "beta").await?;

    let sessions = state.sessions().active_sessions(project_a, 100).await?;
    assert_eq!(
        sessions.len(),
        1,
        "active_sessions must not leak sessions from other projects"
    );
    assert_eq!(sessions[0].project_id, project_a.to_string());
    assert_eq!(sessions[0].id, session_a.to_string());
    Ok(())
}

#[tokio::test]
async fn idle_sessions_stay_excluded_from_active_sessions() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let state = AppState::initialize(test_config(temp.path().into())).await?;
    let (project_a, _session_a) = seed_project_with_active_session(&state, "alpha").await?;

    // A same-project session without an active turn must not appear.
    let idle = SessionId::new();
    state
        .pool()
        .collection::<Document>("sessions")
        .insert_one(doc! {
            "_id": idle.to_string(),
            "project_id": project_a.to_string(),
            "title": Bson::Null,
            "state": "active",
            "next_model_ref": Bson::Null,
            "active_turn_id": Bson::Null,
            "version": "v_idle",
            "created_at": NOW,
            "updated_at": NOW,
            "last_activity_at": NOW,
        })
        .await?;

    let sessions = state.sessions().active_sessions(project_a, 100).await?;
    assert_eq!(sessions.len(), 1);
    assert_ne!(sessions[0].id, idle.to_string());
    Ok(())
}

#[tokio::test]
async fn read_session_tool_fails_closed_across_projects() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let state = AppState::initialize(test_config(temp.path().into())).await?;
    let (project_a, session_a) = seed_project_with_active_session(&state, "alpha").await?;
    let (_project_b, session_b) = seed_project_with_active_session(&state, "beta").await?;

    let blobs =
        janus_infrastructure::managed_storage::BlobStore::new(state.pool().clone(), temp.path())?;
    let workspace_root = temp.path().join("workspace-root");
    std::fs::create_dir_all(&workspace_root)?;
    let ctx = janus_execution::ToolContext {
        project_id: project_a,
        session_id: session_a,
        turn_id: TurnId::new(),
        tool_call_id: janus_infrastructure::id::ToolCallId::new(),
        workspace: state.workspace(),
        workspace_root: &workspace_root,
        workspace_handle: janus_workspace::interface::WorkspaceHandle::main(project_a),
        sessions: state.sessions(),
        projects: state.projects(),
        blobs: &blobs,
        runtime: state.runtime(),
        git_token: None,
        read_paths: &std::collections::HashSet::new(),
        actor: serde_json::json!({"kind": "test"}),
    };

    // Cross-project target: must fail closed, never return the other
    // project's session or timeline.
    let outcome = janus_execution::execute_tool(
        &ctx,
        "read_session",
        &serde_json::json!({"session_id": session_b.to_string()}),
    )
    .await?;
    assert_eq!(
        outcome.error_code.as_deref(),
        Some("NOT_FOUND"),
        "cross-project read_session must fail closed, got: {}",
        outcome.summary
    );
    assert_eq!(
        outcome.summary.get("session"),
        None,
        "cross-project read must not expose the session body"
    );

    // Same-project target still works.
    let outcome = janus_execution::execute_tool(
        &ctx,
        "read_session",
        &serde_json::json!({"session_id": session_a.to_string()}),
    )
    .await?;
    assert_eq!(outcome.error_code, None);
    assert_eq!(
        outcome.summary.get("session").and_then(|s| s.get("id")),
        Some(&serde_json::json!(session_a.to_string()))
    );
    Ok(())
}
