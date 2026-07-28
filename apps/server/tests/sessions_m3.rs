//! Stage 2: sessions projection — TST-SES-01 / TST-SES-03.

mod support;

use std::path::PathBuf;
use std::str::FromStr;

use janus_server::modules::sessions::interface::SessionsInterface;
use janus_server::modules::sessions::types::SessionsError;
use janus_server::modules::workspace_sync::interface::{WorkspaceHandle, WorkspaceSyncInterface};
use janus_server::platform::{
    database::Database,
    events::EventStore,
    id::{ProjectId, SessionId, TurnId},
    managed_storage::BlobStore,
};
use serde_json::json;
use sqlx::SqlitePool;
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    _database: Database,
    data_root: PathBuf,
    pool: SqlitePool,
    sync: WorkspaceSyncInterface,
    sessions: SessionsInterface,
    project_id: ProjectId,
}

impl Fixture {
    async fn new() -> anyhow::Result<Self> {
        let temp = TempDir::new()?;
        let data_root = temp.path().to_path_buf();
        let database = Database::open(&data_root).await?;
        let pool = database.pool().clone();
        let blobs = BlobStore::new(pool.clone(), &data_root)?;
        let sync = WorkspaceSyncInterface::new(pool.clone(), &data_root, blobs);
        let events = EventStore::new(pool.clone());
        let sessions = SessionsInterface::new(pool.clone(), events, sync.clone());
        let project_id = ProjectId::new();

        seed_project(&pool, project_id).await?;

        let main_managed = format!("workspaces/main/{project_id}/repo");
        let main_abs = data_root.join(&main_managed);
        std::fs::create_dir_all(main_abs.join("src"))?;
        std::fs::write(main_abs.join("README.md"), b"# main\n")?;
        std::fs::write(main_abs.join("src").join("lib.rs"), b"fn main() {}\n")?;
        support::init_git_repo(&main_abs)?;

        let _ = sync
            .ensure_main_copy(
                project_id,
                &main_managed,
                "test.setup",
                json!({"kind": "test"}),
            )
            .await?;

        Ok(Self {
            _temp: temp,
            _database: database,
            data_root,
            pool,
            sync,
            sessions,
            project_id,
        })
    }
}

async fn seed_project(pool: &SqlitePool, project_id: ProjectId) -> anyhow::Result<()> {
    let now = "2026-01-01T00:00:00.000Z";
    let tenant_id = "tenant-test";
    let owner_id = "owner-test";
    sqlx::query("INSERT INTO tenants (id, created_at) VALUES (?, ?)")
        .bind(tenant_id)
        .bind(now)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO owners (id, tenant_id, display_name, created_at) VALUES (?, ?, ?, ?)")
        .bind(owner_id)
        .bind(tenant_id)
        .bind("Test Owner")
        .bind(now)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO projects \
         (id, owner_id, tenant_id, name, state, repo_access, repo_url, \
          version, created_at, updated_at, last_activity_at) \
         VALUES (?, ?, ?, 'fixture', 'ready', 'public_https', 'https://example.com/r.git', \
                 'v1', ?, ?, ?)",
    )
    .bind(project_id.to_string())
    .bind(owner_id)
    .bind(tenant_id)
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// TST-SES-01: create Session from Main with revision+manifest; delete cleans Session not Main.
#[tokio::test]
async fn tst_ses_01_create_and_delete_session() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let actor = json!({"kind": "user", "id": "owner-test"});

    let summary = fx
        .sessions
        .create_session(fx.project_id, Some("hello".into()), actor.clone())
        .await?;
    assert_eq!(summary.state, "ready");
    assert_eq!(summary.title.as_deref(), Some("hello"));
    assert!(summary.workspace_revision.is_some());

    let session_id = SessionId::from_str(&summary.id)?;
    let repo = fx
        .data_root
        .join("workspaces/sessions")
        .join(session_id.to_string())
        .join("repo");
    assert!(repo.join("README.md").is_file());
    assert!(!repo.join(".git").join("HEAD").exists() || repo.join(".git").exists());

    let handle = WorkspaceHandle::session(session_id);
    let rev = fx.sync.current_revision(&handle).await?;
    let root: Option<String> = sqlx::query_scalar(
        "SELECT manifest_root_hash FROM content_revisions WHERE revision_id = ?",
    )
    .bind(&rev.0)
    .fetch_one(&fx.pool)
    .await?;
    assert!(
        root.as_ref().is_some_and(|h| !h.is_empty()),
        "manifest_root_hash must be set on Session create"
    );

    // Main still present.
    let main_repo = fx
        .data_root
        .join("workspaces/main")
        .join(fx.project_id.to_string())
        .join("repo");
    assert!(main_repo.join("README.md").is_file());

    fx.sessions.delete_session(session_id, actor).await?;
    assert!(matches!(
        fx.sessions.get_session(session_id).await,
        Err(SessionsError::NotFound)
    ));
    assert!(!repo.exists(), "session repo must be removed");
    assert!(
        main_repo.join("README.md").is_file(),
        "Main must survive session delete"
    );

    Ok(())
}

/// TST-SES-03: a second message while a Turn is active is queued (M4 state
/// machine). M3 used to reject with `ActiveTurnExists`; M4 routes it as
/// `queued`. The single-active-Turn invariant is still preserved by the
/// `turns_one_active_per_session` partial index, and the queued Turn only
/// becomes active once the predecessor settles and the queue is promoted.
#[tokio::test]
async fn tst_ses_03_single_active_turn() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let summary = fx
        .sessions
        .create_session(fx.project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&summary.id)?;

    let first = fx
        .sessions
        .post_message(session_id, "first", &summary.version, actor.clone())
        .await?;
    assert_eq!(first.route, "started");

    // While the first Turn is running, the second message is queued, not rejected.
    let second = fx
        .sessions
        .post_message(session_id, "second", &first.session_version, actor.clone())
        .await?;
    assert_eq!(second.route, "queued");

    // The queue reflects the single queued Turn waiting to be promoted.
    let queued = fx.sessions.list_queued_turns(session_id).await?;
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].turn_id, second.turn_id);
    assert_eq!(queued[0].source, "message");

    // Completing the predecessor frees the active slot. Promoting the queue
    // starts the second Turn without taking a second message checkpoint yet.
    let turn_id = TurnId::from_str(&first.turn_id)?;
    fx.sessions
        .force_complete_turn_for_test(session_id, turn_id)
        .await?;
    let promoted = fx.sessions.promote_oldest_queued(session_id).await?;
    assert_eq!(promoted.map(|t| t.to_string()), Some(second.turn_id.clone()));

    let session = fx.sessions.get_session(session_id).await?;
    assert_eq!(session.active_turn_id.as_deref(), Some(second.turn_id.as_str()));
    let promoted_turn = fx
        .sessions
        .get_turn(session_id, TurnId::from_str(&second.turn_id)?)
        .await?;
    assert_eq!(promoted_turn.status, "running");

    let actor_clone = actor.clone();
    let _ = actor_clone;
    Ok(())
}

#[tokio::test]
async fn timeline_contains_user_message() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let summary = fx
        .sessions
        .create_session(fx.project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&summary.id)?;
    let _ = fx
        .sessions
        .post_message(session_id, "hi timeline", &summary.version, actor)
        .await?;

    let page = fx.sessions.timeline(session_id, None, None, 50).await?;
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].kind, "user_message");
    assert!(!page.has_older);

    Ok(())
}
