//! Stage 4: Session Control State Machine — start/queue/Steer/Cancel/promote.
//!
//! Covers the in-`sessions` portion of the M4 state machine that does not yet
//! require the application Handoff coordinator or supervisor Ask suspend/resume
//! (those land in `runtime_sessions_m4.rs`). Asserts documented routes, the
//! single-active-Turn invariant preserved by the widened
//! `turns_one_active_per_session` partial index, ordered queue projection,
//! Steer binding, Cancel transition, and queue progression/pause.

mod support;

use std::str::FromStr;

use janus_server::modules::sessions::interface::SessionsInterface;
use janus_server::modules::sessions::types::SessionsError;
use janus_server::modules::workspace_sync::interface::WorkspaceSyncInterface;
use janus_server::platform::{
    database::Database,
    events::EventStore,
    id::{ProjectId, SessionId, TurnId},
    managed_storage::BlobStore,
};
use serde_json::json;
use sqlx::Acquire;
use sqlx::SqlitePool;
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    _database: Database,
    _pool: SqlitePool,
    _sync: WorkspaceSyncInterface,
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
        std::fs::write(main_abs.join("src/lib.rs"), b"fn main() {}\n")?;
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
            _pool: pool,
            _sync: sync,
            sessions,
            project_id,
        })
    }
}

async fn seed_project(pool: &SqlitePool, project_id: ProjectId) -> anyhow::Result<()> {
    let now = "2026-01-01T00:00:00.000Z";
    sqlx::query("INSERT INTO tenants (id, created_at) VALUES ('tenant-test', ?)")
        .bind(now)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO owners (id, tenant_id, display_name, created_at) \
         VALUES ('owner-test', 'tenant-test', 'Test Owner', ?)",
    )
    .bind(now)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO projects \
         (id, owner_id, tenant_id, name, state, repo_access, repo_url, \
          version, created_at, updated_at, last_activity_at) \
         VALUES (?, 'owner-test', 'tenant-test', 'fixture', 'ready', 'public_https', \
                 'https://example.com/r.git', 'v1', ?, ?, ?)",
    )
    .bind(project_id.to_string())
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
async fn idle_message_starts_and_second_queues_in_order() -> anyhow::Result<()> {
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

    let second = fx
        .sessions
        .post_message(session_id, "second", &first.session_version, actor.clone())
        .await?;
    assert_eq!(second.route, "queued");

    let third = fx
        .sessions
        .post_message(session_id, "third", &second.session_version, actor.clone())
        .await?;
    assert_eq!(third.route, "queued");

    let queued = fx.sessions.list_queued_turns(session_id).await?;
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[0].turn_id, second.turn_id);
    assert_eq!(queued[1].turn_id, third.turn_id);
    assert_eq!(queued[0].source, "message");
    Ok(())
}

#[tokio::test]
async fn completing_predecessor_promotes_oldest_queued() -> anyhow::Result<()> {
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
    let second = fx
        .sessions
        .post_message(session_id, "second", &first.session_version, actor.clone())
        .await?;

    let first_turn = TurnId::from_str(&first.turn_id)?;
    let promoted = fx
        .sessions
        .settle_terminal_turn(
            session_id,
            first_turn,
            "completed",
            Some("finish"),
            actor.clone(),
        )
        .await?;
    assert_eq!(
        promoted.map(|t| t.to_string()),
        Some(second.turn_id.clone())
    );

    let session = fx.sessions.get_session(session_id).await?;
    assert_eq!(
        session.active_turn_id.as_deref(),
        Some(second.turn_id.as_str())
    );

    let turn = fx
        .sessions
        .get_turn(session_id, TurnId::from_str(&second.turn_id)?)
        .await?;
    assert_eq!(turn.status, "running");
    Ok(())
}

#[tokio::test]
async fn failed_predecessor_leaves_queue_paused() -> anyhow::Result<()> {
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
    let second = fx
        .sessions
        .post_message(session_id, "second", &first.session_version, actor.clone())
        .await?;

    let first_turn = TurnId::from_str(&first.turn_id)?;
    let promoted = fx
        .sessions
        .settle_terminal_turn(
            session_id,
            first_turn,
            "failed",
            Some("model"),
            actor.clone(),
        )
        .await?;
    assert!(promoted.is_none(), "queue must stay paused on failure");

    // No active turn, queued Turn stays queued.
    let session = fx.sessions.get_session(session_id).await?;
    assert!(session.active_turn_id.is_none());
    let queued = fx.sessions.list_queued_turns(session_id).await?;
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].turn_id, second.turn_id);
    Ok(())
}

#[tokio::test]
async fn cancel_transitions_running_to_canceling_then_canceled() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let summary = fx
        .sessions
        .create_session(fx.project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&summary.id)?;

    let started = fx
        .sessions
        .post_message(session_id, "work", &summary.version, actor.clone())
        .await?;
    let turn_id = TurnId::from_str(&started.turn_id)?;

    let cancel = fx
        .sessions
        .cancel_turn(
            session_id,
            turn_id,
            "user_request",
            &started.session_version,
            actor.clone(),
        )
        .await?;
    assert_eq!(cancel.from_status, "running");
    assert_eq!(cancel.to_status, "canceling");
    let turn = fx.sessions.get_turn(session_id, turn_id).await?;
    assert_eq!(turn.status, "canceling");
    assert_eq!(turn.cancellation_reason.as_deref(), Some("user_request"));

    // Runtime settles canceling -> canceled and frees the slot.
    let promoted = fx
        .sessions
        .settle_terminal_turn(session_id, turn_id, "canceled", None, actor.clone())
        .await?;
    assert!(promoted.is_none());
    let turn = fx.sessions.get_turn(session_id, turn_id).await?;
    assert_eq!(turn.status, "canceled");
    let session = fx.sessions.get_session(session_id).await?;
    assert!(session.active_turn_id.is_none());
    Ok(())
}

#[tokio::test]
async fn cancel_on_terminal_turn_is_rejected() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let summary = fx
        .sessions
        .create_session(fx.project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&summary.id)?;

    let started = fx
        .sessions
        .post_message(session_id, "work", &summary.version, actor.clone())
        .await?;
    let turn_id = TurnId::from_str(&started.turn_id)?;

    // Force-complete first; cancelling a completed Turn must be rejected.
    fx.sessions
        .force_complete_turn_for_test(session_id, turn_id)
        .await?;
    let after = fx.sessions.get_session(session_id).await?;
    let res = fx
        .sessions
        .cancel_turn(session_id, turn_id, "late", &after.version, actor.clone())
        .await;
    assert!(matches!(res, Err(SessionsError::TurnTerminal)), "{res:?}");
    Ok(())
}

#[tokio::test]
async fn steer_binds_to_running_turn_without_stealing_slot() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let summary = fx
        .sessions
        .create_session(fx.project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&summary.id)?;

    let started = fx
        .sessions
        .post_message(session_id, "do something", &summary.version, actor.clone())
        .await?;
    let turn_id = TurnId::from_str(&started.turn_id)?;

    let steer = fx
        .sessions
        .steer(
            session_id,
            "and also check logs",
            &started.session_version,
            actor.clone(),
        )
        .await?;
    assert_eq!(steer.turn_id, started.turn_id);

    // Steer must not take the active slot or change Turn status.
    let session = fx.sessions.get_session(session_id).await?;
    assert_eq!(
        session.active_turn_id.as_deref(),
        Some(turn_id.to_string().as_str())
    );
    let turn = fx.sessions.get_turn(session_id, turn_id).await?;
    assert_eq!(turn.status, "running");
    Ok(())
}

#[tokio::test]
async fn steer_without_running_turn_is_rejected() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let summary = fx
        .sessions
        .create_session(fx.project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&summary.id)?;

    let res = fx
        .sessions
        .steer(
            session_id,
            "no turn to steer",
            &summary.version,
            actor.clone(),
        )
        .await;
    assert!(
        matches!(res, Err(SessionsError::TurnNotInteractive)),
        "{res:?}"
    );
    Ok(())
}

#[tokio::test]
async fn queued_start_after_terminal_progresses_full_queue() -> anyhow::Result<()> {
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
    let second = fx
        .sessions
        .post_message(session_id, "second", &first.session_version, actor.clone())
        .await?;
    let third = fx
        .sessions
        .post_message(session_id, "third", &second.session_version, actor.clone())
        .await?;

    // Settle predecessor -> second promoted; settle second -> third promoted.
    let promoted_first = fx
        .sessions
        .settle_terminal_turn(
            session_id,
            TurnId::from_str(&first.turn_id)?,
            "completed",
            None,
            actor.clone(),
        )
        .await?;
    assert_eq!(
        promoted_first.map(|t| t.to_string()),
        Some(second.turn_id.clone())
    );

    let promoted_second = fx
        .sessions
        .settle_terminal_turn(
            session_id,
            TurnId::from_str(&second.turn_id)?,
            "canceled",
            None,
            actor.clone(),
        )
        .await?;
    assert_eq!(
        promoted_second.map(|t| t.to_string()),
        Some(third.turn_id.clone())
    );

    let session = fx.sessions.get_session(session_id).await?;
    assert_eq!(
        session.active_turn_id.as_deref(),
        Some(third.turn_id.as_str())
    );
    let queued = fx.sessions.list_queued_turns(session_id).await?;
    assert!(queued.is_empty());
    Ok(())
}

#[tokio::test]
async fn pause_and_resume_round_trip_keeps_active_slot() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let summary = fx
        .sessions
        .create_session(fx.project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&summary.id)?;
    let started = fx
        .sessions
        .post_message(session_id, "work", &summary.version, actor.clone())
        .await?;
    let turn_id = TurnId::from_str(&started.turn_id)?;

    // running -> waiting_for_job
    let v1 = fx
        .sessions
        .pause_turn_for(
            session_id,
            turn_id,
            "waiting_for_job",
            json!({"kind": "supervisor"}),
        )
        .await?;
    let turn = fx.sessions.get_turn(session_id, turn_id).await?;
    assert_eq!(turn.status, "waiting_for_job");
    // Slot still held.
    let session = fx.sessions.get_session(session_id).await?;
    assert_eq!(
        session.active_turn_id.as_deref(),
        Some(turn_id.to_string().as_str())
    );

    // waiting_for_job -> running
    let _v2 = fx
        .sessions
        .resume_turn(
            session_id,
            turn_id,
            "waiting_for_job",
            json!({"kind": "supervisor"}),
        )
        .await?;
    let turn = fx.sessions.get_turn(session_id, turn_id).await?;
    assert_eq!(turn.status, "running");
    assert_ne!(v1, summary.version); // version advanced
    Ok(())
}

#[tokio::test]
async fn handoff_tx_primitives_settle_predecessor_and_promote_successor() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let summary = fx
        .sessions
        .create_session(fx.project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&summary.id)?;

    // Predecessor: a started Turn we manually park in waiting_for_job.
    let first = fx
        .sessions
        .post_message(session_id, "first", &summary.version, actor.clone())
        .await?;
    let predecessor = TurnId::from_str(&first.turn_id)?;
    let paused_version = fx
        .sessions
        .pause_turn_for(
            session_id,
            predecessor,
            "waiting_for_job",
            json!({"kind": "supervisor"}),
        )
        .await?;

    // Successor: a second user message while the predecessor waits is queued
    // by post_message (route=queued, awaiting_handoff=true).
    let second = fx
        .sessions
        .post_message(session_id, "second", &paused_version, actor.clone())
        .await?;
    assert_eq!(second.route, "queued");
    assert!(
        second.awaiting_handoff,
        "queued successor should report awaiting_handoff"
    );
    let successor = TurnId::from_str(&second.turn_id)?;

    // Drive the Handoff in one transaction, exactly as session_flow would.
    let pool = fx.sessions.pool().clone();
    let mut tx = pool.begin().await?;
    {
        let mut conn = tx.acquire().await?;
        fx.sessions
            .attach_predecessor_in_tx(&mut conn, successor, predecessor)
            .await?;
        fx.sessions
            .record_handoff_links_in_tx(&mut conn, predecessor, successor)
            .await?;
        let _ = fx
            .sessions
            .mark_predecessor_handed_off_in_tx(&mut conn, session_id, predecessor, Some("handoff"))
            .await?;
        let promoted = fx
            .sessions
            .promote_successor_in_tx(&mut conn, session_id, successor)
            .await?;
        assert!(promoted.is_some(), "successor must be promoted to running");
    }
    tx.commit().await?;

    // Predecessor is terminal handed_off; successor owns the active slot.
    let predecessor_turn = fx.sessions.get_turn(session_id, predecessor).await?;
    assert_eq!(predecessor_turn.status, "handed_off");
    assert_eq!(
        predecessor_turn.handoff_to_turn_id.as_deref(),
        Some(successor.to_string().as_str())
    );
    let successor_turn = fx.sessions.get_turn(session_id, successor).await?;
    assert_eq!(successor_turn.status, "running");
    assert_eq!(
        successor_turn.handoff_from_turn_id.as_deref(),
        Some(predecessor.to_string().as_str())
    );
    assert_eq!(
        successor_turn.predecessor_turn_id.as_deref(),
        Some(predecessor.to_string().as_str())
    );
    let session = fx.sessions.get_session(session_id).await?;
    assert_eq!(
        session.active_turn_id.as_deref(),
        Some(successor.to_string().as_str())
    );
    Ok(())
}

#[tokio::test]
async fn settle_terminal_canceled_promotes_queue() -> anyhow::Result<()> {
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
    let second = fx
        .sessions
        .post_message(session_id, "second", &first.session_version, actor.clone())
        .await?;
    let first_turn = TurnId::from_str(&first.turn_id)?;

    // canceling intermediate, then settle as canceled -> promote queue.
    // Use the version after the second message (post_message bumps it).
    let _ = fx
        .sessions
        .cancel_turn(
            session_id,
            first_turn,
            "user",
            &second.session_version,
            actor.clone(),
        )
        .await?;
    let promoted = fx
        .sessions
        .settle_terminal_turn(
            session_id,
            first_turn,
            "canceled",
            Some("user"),
            actor.clone(),
        )
        .await?;
    assert_eq!(
        promoted.map(|t| t.to_string()),
        Some(second.turn_id.clone())
    );
    let session = fx.sessions.get_session(session_id).await?;
    assert_eq!(
        session.active_turn_id.as_deref(),
        Some(second.turn_id.as_str())
    );
    Ok(())
}

#[tokio::test]
async fn settle_terminal_interrupted_pauses_queue() -> anyhow::Result<()> {
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
    let second = fx
        .sessions
        .post_message(session_id, "second", &first.session_version, actor.clone())
        .await?;
    let first_turn = TurnId::from_str(&first.turn_id)?;

    let promoted = fx
        .sessions
        .settle_terminal_turn(
            session_id,
            first_turn,
            "interrupted",
            Some("lost_job"),
            actor.clone(),
        )
        .await?;
    assert!(promoted.is_none(), "interrupted must pause the queue");
    let session = fx.sessions.get_session(session_id).await?;
    assert!(session.active_turn_id.is_none());
    let queued = fx.sessions.list_queued_turns(session_id).await?;
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].turn_id, second.turn_id);
    Ok(())
}

#[tokio::test]
async fn pause_for_model_keeps_active_slot() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let actor = json!({"kind": "user", "id": "owner-test"});
    let summary = fx
        .sessions
        .create_session(fx.project_id, None, actor.clone())
        .await?;
    let session_id = SessionId::from_str(&summary.id)?;
    let started = fx
        .sessions
        .post_message(session_id, "work", &summary.version, actor.clone())
        .await?;
    let turn_id = TurnId::from_str(&started.turn_id)?;

    fx.sessions
        .pause_turn_for(
            session_id,
            turn_id,
            "waiting_for_model",
            json!({"kind": "supervisor"}),
        )
        .await?;
    let turn = fx.sessions.get_turn(session_id, turn_id).await?;
    assert_eq!(turn.status, "waiting_for_model");
    // Steer must be rejected while waiting for model.
    let session = fx.sessions.get_session(session_id).await?;
    let res = fx
        .sessions
        .steer(session_id, "late steer", &session.version, actor.clone())
        .await;
    assert!(
        matches!(res, Err(SessionsError::SteerBlockedByModel)),
        "{res:?}"
    );
    Ok(())
}
