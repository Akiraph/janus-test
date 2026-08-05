//! Gate-1 integration tests for Session copy + Merkle + Diff + delete.
//!
//! These exercise the public `WorkspaceInterface` against a real temp
//! data root (SQLite migrations + BlobStore + filesystem), without HTTP.

mod support;

use std::path::PathBuf;

use janus_infrastructure::{
    database::Database,
    id::{ProjectId, SessionId},
    managed_storage::{BlobReference, BlobStore},
};
use janus_workspace::interface::{
    FileMutation, PropagationDirection, PropagationError, WorkspaceHandle, WorkspaceInterface,
};
use serde_json::json;
use sqlx::SqlitePool;
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    /// Holds the data-root lock for the test lifetime.
    _database: Database,
    data_root: PathBuf,
    pool: SqlitePool,
    sync: WorkspaceInterface,
    project_id: ProjectId,
}

impl Fixture {
    async fn new() -> anyhow::Result<Self> {
        let temp = TempDir::new()?;
        let data_root = temp.path().to_path_buf();
        let database = Database::open(&data_root, janus_server::migrator()).await?;
        let pool = database.pool().clone();
        let blobs = BlobStore::new(pool.clone(), &data_root)?;
        let sync = WorkspaceInterface::new(pool.clone(), &data_root, blobs);
        let project_id = ProjectId::new();

        // Minimal identity + project rows so workspace_copies FK to projects works.
        seed_project(&pool, project_id).await?;

        // Build a real Main git clone on disk and register it. Session copies
        // are git worktrees of Main, so Main must be a genuine git repo with a
        // committed baseline (not a bare file tree).
        let main_managed = format!("workspaces/main/{project_id}/repo");
        let main_abs = data_root.join(&main_managed);
        std::fs::create_dir_all(&main_abs)?;
        std::fs::write(main_abs.join("README.md"), b"# main\n")?;
        std::fs::write(main_abs.join(".gitignore"), b"*.ignored\n")?;
        std::fs::create_dir_all(main_abs.join("src"))?;
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
            project_id,
        })
    }
}

#[tokio::test]
async fn startup_recovery_removes_unregistered_session_worktrees() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let orphan = fx
        .data_root
        .join("workspaces")
        .join("sessions")
        .join("orphan-session")
        .join("repo");
    std::fs::create_dir_all(&orphan)?;
    std::fs::write(orphan.join("leftover.txt"), b"crash debris")?;

    assert_eq!(fx.sync.recover_orphan_session_worktrees().await?, 1);
    assert!(!orphan.parent().expect("repo parent").exists());

    let main_orphan = fx
        .data_root
        .join("workspaces")
        .join("main")
        .join("orphan-project")
        .join("repo");
    std::fs::create_dir_all(&main_orphan)?;
    std::fs::write(main_orphan.join("leftover.txt"), b"crash debris")?;
    assert_eq!(fx.sync.recover_orphan_main_worktrees().await?, 1);
    assert!(!main_orphan.parent().expect("repo parent").exists());
    Ok(())
}

#[tokio::test]
async fn blob_sweeper_removes_only_unreferenced_objects() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let blobs = BlobStore::new(fx.pool.clone(), &fx.data_root)?;
    let reference = BlobReference::new("test", "sweep", "object-1", "content");
    let sha = blobs.write(b"sweep me", reference.clone()).await?;
    let object = fx
        .data_root
        .join("objects/sha256")
        .join(&sha.to_string()[..2])
        .join(sha.to_string());
    assert!(object.exists());

    assert!(blobs.drop_reference(&reference).await?);
    assert!(blobs.sweep_unreferenced().await? >= 1);
    assert!(!object.exists());
    let object_row: Option<String> =
        sqlx::query_scalar("SELECT sha256 FROM blob_objects WHERE sha256 = ?")
            .bind(sha.to_string())
            .fetch_optional(&fx.pool)
            .await?;
    assert!(object_row.is_none());
    Ok(())
}

#[tokio::test]
async fn session_copy_includes_main_working_tree_content() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let main = fx
        .data_root
        .join("workspaces")
        .join("main")
        .join(fx.project_id.to_string())
        .join("repo");
    std::fs::write(main.join("README.md"), b"# uncommitted main\n")?;
    std::fs::remove_file(main.join("src").join("lib.rs"))?;
    std::fs::write(main.join("draft.txt"), b"untracked\n")?;
    std::fs::write(main.join("cache.ignored"), b"ignored\n")?;

    let session_id = SessionId::new();
    fx.sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    let session = fx
        .data_root
        .join("workspaces")
        .join("sessions")
        .join(session_id.to_string())
        .join("repo");

    assert_eq!(
        std::fs::read(session.join("README.md"))?,
        b"# uncommitted main\n"
    );
    assert!(!session.join("src").join("lib.rs").exists());
    assert_eq!(std::fs::read(session.join("draft.txt"))?, b"untracked\n");
    assert!(!session.join("cache.ignored").exists());

    let summary = fx.sync.diff_summary(session_id).await?;
    assert_eq!(summary.added, 0);
    assert_eq!(summary.modified, 0);
    assert_eq!(summary.deleted, 0);
    Ok(())
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

#[tokio::test]
async fn ensure_session_copy_from_main() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let session_id = SessionId::new();
    let result = fx
        .sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;

    assert_eq!(result.handle, WorkspaceHandle::session(session_id));
    assert!(!result.manifest_root_hash.is_empty());
    assert!(result.revision.0.starts_with("rev_"));

    let session_abs = fx
        .data_root
        .join("workspaces")
        .join("sessions")
        .join(session_id.to_string())
        .join("repo");
    assert!(session_abs.join("README.md").is_file());
    assert!(session_abs.join("src").join("lib.rs").is_file());
    // Session is a git worktree of Main: a .git file/dir points back at Main's
    // object store, and the Main baseline commit is reachable.
    assert!(session_abs.join(".git").exists());

    // Sequence = 1, root stored.
    let (seq, root): (i64, Option<String>) = sqlx::query_as(
        "SELECT sequence, manifest_root_hash FROM content_revisions WHERE revision_id = ?",
    )
    .bind(&result.revision.0)
    .fetch_one(&fx.pool)
    .await?;
    assert_eq!(seq, 1);
    assert_eq!(root.as_deref(), Some(result.manifest_root_hash.as_str()));

    // propagation_links row present.
    let links: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM propagation_links WHERE session_id = ?")
            .bind(session_id.to_string())
            .fetch_one(&fx.pool)
            .await?;
    assert_eq!(links, 1);

    let baseline_json: String = sqlx::query_scalar(
        "SELECT baseline_manifest_json FROM propagation_links WHERE session_id = ?",
    )
    .bind(session_id.to_string())
    .fetch_one(&fx.pool)
    .await?;
    let baseline: serde_json::Value = serde_json::from_str(&baseline_json)?;
    assert_eq!(baseline["root_hash"], result.manifest_root_hash);
    assert!(baseline["nodes"].is_object());
    assert!(
        !fx.data_root
            .join("workspaces")
            .join("sessions")
            .join(session_id.to_string())
            .join("base")
            .exists()
    );

    // Idempotent re-call returns the same revision.
    let again = fx
        .sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    assert_eq!(again.revision.0, result.revision.0);

    Ok(())
}

#[tokio::test]
async fn write_bumps_revision_and_changes_root() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let session_id = SessionId::new();
    let created = fx
        .sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    let handle = WorkspaceHandle::session(session_id);
    let first_root = created.manifest_root_hash.clone();

    let rev2 = fx
        .sync
        .apply_file_mutation(
            &handle,
            FileMutation::Write {
                path: "src/lib.rs".into(),
                content: b"fn main() { println!(\"hi\"); }\n".to_vec(),
            },
            Some(&created.revision),
            "tool.write",
            json!({"kind": "agent"}),
        )
        .await?;
    assert_ne!(rev2.0, created.revision.0);

    let root2: Option<String> = sqlx::query_scalar(
        "SELECT manifest_root_hash FROM content_revisions WHERE revision_id = ?",
    )
    .bind(&rev2.0)
    .fetch_one(&fx.pool)
    .await?;
    assert!(root2.is_some());
    assert_ne!(root2.as_deref(), Some(first_root.as_str()));

    let current = fx.sync.current_revision(&handle).await?;
    assert_eq!(current.0, rev2.0);
    Ok(())
}

#[tokio::test]
async fn pending_file_mutation_is_replayed_during_recovery() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let session_id = SessionId::new();
    fx.sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    let handle = WorkspaceHandle::session(session_id);
    let lock = fx.sync.acquire_mutation_lock(&handle).await?;
    let prepared = fx
        .sync
        .prepare_file_mutation(
            &lock,
            janus_workspace::interface::FileMutationRequest {
                handle: &handle,
                mutation: FileMutation::Write {
                    path: "recovered.txt".into(),
                    content: b"replayed\n".to_vec(),
                },
                expected: None,
                cause: "test.recovery",
                actor: json!({"kind": "test"}),
                event: None,
            },
        )
        .await?;
    drop(lock);

    assert_eq!(fx.sync.recover_uncertain_file_mutations().await?.len(), 0);
    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_mutation_intents WHERE id = ?")
            .bind(prepared.intent_id())
            .fetch_one(&fx.pool)
            .await?;
    assert_eq!(state, "completed");
    let path = fx
        .data_root
        .join("workspaces/sessions")
        .join(session_id.to_string())
        .join("repo/recovered.txt");
    assert_eq!(std::fs::read(path)?, b"replayed\n");
    Ok(())
}

#[tokio::test]
async fn applied_file_mutation_is_only_finalized_during_recovery() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let session_id = SessionId::new();
    fx.sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    let handle = WorkspaceHandle::session(session_id);
    let lock = fx.sync.acquire_mutation_lock(&handle).await?;
    let prepared = fx
        .sync
        .prepare_file_mutation(
            &lock,
            janus_workspace::interface::FileMutationRequest {
                handle: &handle,
                mutation: FileMutation::Write {
                    path: "applied.txt".into(),
                    content: b"already applied\n".to_vec(),
                },
                expected: None,
                cause: "test.recovery",
                actor: json!({"kind": "test"}),
                event: None,
            },
        )
        .await?;
    fx.sync
        .apply_prepared_file_mutation(&lock, &prepared)
        .await?;
    drop(lock);

    assert_eq!(fx.sync.recover_uncertain_file_mutations().await?.len(), 0);
    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_mutation_intents WHERE id = ?")
            .bind(prepared.intent_id())
            .fetch_one(&fx.pool)
            .await?;
    assert_eq!(state, "completed");
    Ok(())
}

#[tokio::test]
async fn unexpected_file_edit_stops_mutation_recovery() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let session_id = SessionId::new();
    fx.sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    let handle = WorkspaceHandle::session(session_id);
    let lock = fx.sync.acquire_mutation_lock(&handle).await?;
    let prepared = fx
        .sync
        .prepare_file_mutation(
            &lock,
            janus_workspace::interface::FileMutationRequest {
                handle: &handle,
                mutation: FileMutation::Write {
                    path: "conflicted.txt".into(),
                    content: b"janus intended\n".to_vec(),
                },
                expected: None,
                cause: "test.recovery",
                actor: json!({"kind": "test"}),
                event: None,
            },
        )
        .await?;
    drop(lock);
    let path = fx
        .data_root
        .join("workspaces/sessions")
        .join(session_id.to_string())
        .join("repo/conflicted.txt");
    std::fs::write(&path, b"external edit\n")?;

    assert!(fx.sync.recover_uncertain_file_mutations().await.is_err());
    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_mutation_intents WHERE id = ?")
            .bind(prepared.intent_id())
            .fetch_one(&fx.pool)
            .await?;
    assert_eq!(state, "needs_attention");
    assert_eq!(std::fs::read(path)?, b"external edit\n");
    Ok(())
}

#[tokio::test]
async fn move_and_delete_tree_mutations_update_the_workspace() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let session_id = SessionId::new();
    let created = fx
        .sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    let handle = WorkspaceHandle::session(session_id);

    let moved = fx
        .sync
        .apply_file_mutation(
            &handle,
            FileMutation::Move {
                from: "src/lib.rs".into(),
                to: "src/moved.rs".into(),
            },
            Some(&created.revision),
            "tool.move",
            json!({"kind": "agent"}),
        )
        .await?;
    assert!(fx.sync.file_content(&handle, "src/moved.rs").await.is_ok());
    assert!(fx.sync.file_content(&handle, "src/lib.rs").await.is_err());

    let deleted = fx
        .sync
        .apply_file_mutation(
            &handle,
            FileMutation::DeleteTree { path: "src".into() },
            Some(&moved),
            "tool.delete_tree",
            json!({"kind": "agent"}),
        )
        .await?;
    assert_ne!(deleted.0, moved.0);
    assert!(fx.sync.file_content(&handle, "src/moved.rs").await.is_err());
    Ok(())
}

#[tokio::test]
async fn aba_content_still_new_revision_id() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let session_id = SessionId::new();
    let created = fx
        .sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    let handle = WorkspaceHandle::session(session_id);
    let original = b"fn main() {}\n".to_vec();

    let rev_b = fx
        .sync
        .apply_file_mutation(
            &handle,
            FileMutation::Write {
                path: "src/lib.rs".into(),
                content: b"changed\n".to_vec(),
            },
            None,
            "tool.write",
            json!({}),
        )
        .await?;
    let rev_a2 = fx
        .sync
        .apply_file_mutation(
            &handle,
            FileMutation::Write {
                path: "src/lib.rs".into(),
                content: original,
            },
            None,
            "tool.write",
            json!({}),
        )
        .await?;

    // Three distinct revision identities even though content returned to A.
    assert_ne!(created.revision.0, rev_b.0);
    assert_ne!(rev_b.0, rev_a2.0);
    assert_ne!(created.revision.0, rev_a2.0);

    // Root hash of A and A2 may match (content equal) — that is fine; IDs differ.
    let root_a: Option<String> = sqlx::query_scalar(
        "SELECT manifest_root_hash FROM content_revisions WHERE revision_id = ?",
    )
    .bind(&created.revision.0)
    .fetch_one(&fx.pool)
    .await?;
    let root_a2: Option<String> = sqlx::query_scalar(
        "SELECT manifest_root_hash FROM content_revisions WHERE revision_id = ?",
    )
    .bind(&rev_a2.0)
    .fetch_one(&fx.pool)
    .await?;
    assert_eq!(root_a, root_a2);

    let seq: i64 =
        sqlx::query_scalar("SELECT sequence FROM content_revisions WHERE revision_id = ?")
            .bind(&rev_a2.0)
            .fetch_one(&fx.pool)
            .await?;
    assert_eq!(seq, 3);
    Ok(())
}

#[tokio::test]
async fn diff_summary_clean_session_reports_no_modifications() -> anyhow::Result<()> {
    // A fresh Session copy is a clean worktree checkout of Main HEAD with no
    // tool mutations. The Diff summary must report zero path changes — anything
    // else means the Merkle/diff comparison misclassifies identical bytes.
    let fx = Fixture::new().await?;
    let session_id = SessionId::new();
    fx.sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    let summary = fx.sync.diff_summary(session_id).await?;
    assert_eq!(
        summary.modified,
        0,
        "clean session must show 0 modified, got {}: {:?}",
        summary.modified,
        summary.paths.iter().map(|p| &p.path).collect::<Vec<_>>()
    );
    assert_eq!(summary.added, 0);
    assert_eq!(summary.deleted, 0);
    Ok(())
}

#[tokio::test]
async fn diff_summary_reports_paths() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let session_id = SessionId::new();
    fx.sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    let handle = WorkspaceHandle::session(session_id);

    // Modify one, add one, delete one relative to Main.
    fx.sync
        .apply_file_mutation(
            &handle,
            FileMutation::Write {
                path: "README.md".into(),
                content: b"# session\n".to_vec(),
            },
            None,
            "tool.write",
            json!({}),
        )
        .await?;
    fx.sync
        .apply_file_mutation(
            &handle,
            FileMutation::Write {
                path: "new.txt".into(),
                content: b"brand new\n".to_vec(),
            },
            None,
            "tool.write",
            json!({}),
        )
        .await?;
    fx.sync
        .apply_file_mutation(
            &handle,
            FileMutation::Delete {
                path: "src/lib.rs".into(),
            },
            None,
            "tool.delete",
            json!({}),
        )
        .await?;

    let summary = fx.sync.diff_summary(session_id).await?;
    assert!(summary.apply_enabled);
    assert_eq!(summary.added, 1);
    assert_eq!(summary.modified, 1);
    assert_eq!(summary.deleted, 1);
    let kinds: Vec<_> = summary
        .paths
        .iter()
        .map(|p| (p.path.as_str(), format!("{:?}", p.kind)))
        .collect();
    assert!(
        kinds
            .iter()
            .any(|(p, k)| *p == "new.txt" && k.contains("Added")),
        "expected added new.txt, got {kinds:?}"
    );
    assert!(
        kinds
            .iter()
            .any(|(p, k)| *p == "README.md" && k.contains("Modified")),
        "expected modified README.md, got {kinds:?}"
    );
    assert!(
        kinds
            .iter()
            .any(|(p, k)| *p == "src/lib.rs" && k.contains("Deleted")),
        "expected deleted src/lib.rs, got {kinds:?}"
    );
    let stats = summary
        .paths
        .iter()
        .map(|path| (path.path.as_str(), (path.additions, path.deletions)))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(stats.get("README.md"), Some(&(1, 1)));
    assert_eq!(stats.get("new.txt"), Some(&(1, 0)));
    assert_eq!(stats.get("src/lib.rs"), Some(&(0, 1)));
    Ok(())
}

#[tokio::test]
async fn delete_session_copy_cleans_disk_and_db() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let session_id = SessionId::new();
    fx.sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    let session_root = fx
        .data_root
        .join("workspaces")
        .join("sessions")
        .join(session_id.to_string());
    assert!(session_root.exists());

    fx.sync.delete_session_copy(session_id).await?;

    assert!(!session_root.exists());
    let copies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_copies WHERE handle = ?")
        .bind(WorkspaceHandle::session(session_id).as_str())
        .fetch_one(&fx.pool)
        .await?;
    assert_eq!(copies, 0);
    let links: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM propagation_links WHERE session_id = ?")
            .bind(session_id.to_string())
            .fetch_one(&fx.pool)
            .await?;
    assert_eq!(links, 0);

    // Main still present.
    let main: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_copies WHERE handle = ?")
        .bind(WorkspaceHandle::main(fx.project_id).as_str())
        .fetch_one(&fx.pool)
        .await?;
    assert_eq!(main, 1);

    // Idempotent second delete.
    fx.sync.delete_session_copy(session_id).await?;
    Ok(())
}

#[tokio::test]
async fn sync_copies_main_only_changes_and_preserves_session_changes() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let session_id = SessionId::new();
    fx.sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    let main = fx
        .data_root
        .join("workspaces")
        .join("main")
        .join(fx.project_id.to_string())
        .join("repo");
    let session = fx
        .data_root
        .join("workspaces")
        .join("sessions")
        .join(session_id.to_string())
        .join("repo");

    std::fs::write(main.join("main-only.txt"), b"from main\n")?;
    fx.sync
        .apply_file_mutation(
            &WorkspaceHandle::session(session_id),
            FileMutation::Write {
                path: "session-only.txt".into(),
                content: b"from session\n".to_vec(),
            },
            None,
            "tool.write",
            json!({"kind": "test"}),
        )
        .await?;

    let before = fx.sync.diff_summary(session_id).await?;
    assert!(before.sync_enabled);
    assert!(before.apply_enabled);

    let result = fx
        .sync
        .propagate(
            session_id,
            PropagationDirection::Sync,
            json!({"kind": "test"}),
        )
        .await?;
    assert_eq!(result.changed_paths, vec!["main-only.txt"]);
    assert_eq!(
        std::fs::read(session.join("main-only.txt"))?,
        b"from main\n"
    );
    assert_eq!(
        std::fs::read(session.join("session-only.txt"))?,
        b"from session\n"
    );
    assert!(!fx.sync.diff_summary(session_id).await?.sync_enabled);
    assert!(fx.sync.diff_summary(session_id).await?.apply_enabled);
    Ok(())
}

#[tokio::test]
async fn concurrent_propagations_are_serialized_per_project() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let session_id = SessionId::new();
    fx.sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    let main = fx
        .data_root
        .join("workspaces")
        .join("main")
        .join(fx.project_id.to_string())
        .join("repo")
        .join("parallel.txt");
    std::fs::write(main, b"from main\n")?;

    let first = fx.sync.propagate(
        session_id,
        PropagationDirection::Sync,
        json!({"kind": "test", "request": "first"}),
    );
    let second = fx.sync.propagate(
        session_id,
        PropagationDirection::Sync,
        json!({"kind": "test", "request": "second"}),
    );
    let (first, second) = tokio::join!(first, second);
    let first = first?;
    let second = second?;
    assert_eq!(
        first.changed_paths.len() + second.changed_paths.len(),
        1,
        "one serialized propagation should perform the transfer"
    );
    let session_revision_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM content_revisions WHERE workspace_handle = ?")
            .bind(WorkspaceHandle::session(session_id).as_str())
            .fetch_one(&fx.pool)
            .await?;
    assert_eq!(session_revision_count, 2);
    Ok(())
}

#[tokio::test]
async fn propagation_conflict_is_persisted_until_session_edit_then_apply() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let session_id = SessionId::new();
    fx.sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    let main = fx
        .data_root
        .join("workspaces")
        .join("main")
        .join(fx.project_id.to_string())
        .join("repo")
        .join("README.md");
    let handle = WorkspaceHandle::session(session_id);

    std::fs::write(&main, b"# main edit\n")?;
    fx.sync
        .apply_file_mutation(
            &handle,
            FileMutation::Write {
                path: "README.md".into(),
                content: b"# session edit\n".to_vec(),
            },
            None,
            "tool.write",
            json!({"kind": "test"}),
        )
        .await?;

    let conflict = fx
        .sync
        .propagate(
            session_id,
            PropagationDirection::Sync,
            json!({"kind": "test"}),
        )
        .await
        .expect_err("different edits to one path must conflict");
    let PropagationError::Conflict(conflict) = conflict else {
        anyhow::bail!("expected a propagation conflict, got {conflict:?}");
    };
    assert_eq!(conflict.paths.len(), 1);
    assert_eq!(conflict.paths[0].path, "README.md");
    assert_eq!(
        fx.sync
            .diff_summary(session_id)
            .await?
            .pending_conflict
            .as_ref()
            .map(|pending| pending.paths.len()),
        Some(1)
    );

    fx.sync
        .apply_file_mutation(
            &handle,
            FileMutation::Write {
                path: "README.md".into(),
                content: b"# merged edit\n".to_vec(),
            },
            None,
            "tool.write",
            json!({"kind": "test"}),
        )
        .await?;
    let result = fx
        .sync
        .propagate(
            session_id,
            PropagationDirection::Apply,
            json!({"kind": "test"}),
        )
        .await?;
    assert_eq!(result.changed_paths, vec!["README.md"]);
    assert_eq!(std::fs::read(main)?, b"# merged edit\n");
    let summary = fx.sync.diff_summary(session_id).await?;
    assert!(summary.pending_conflict.is_none());
    assert!(!summary.sync_enabled);
    assert!(!summary.apply_enabled);
    Ok(())
}

#[tokio::test]
async fn interrupted_propagation_intent_is_replayed_and_cleared() -> anyhow::Result<()> {
    let fx = Fixture::new().await?;
    let session_id = SessionId::new();
    fx.sync
        .ensure_session_copy(fx.project_id, session_id, None, json!({"kind": "test"}))
        .await?;
    let main = fx
        .data_root
        .join("workspaces")
        .join("main")
        .join(fx.project_id.to_string())
        .join("repo")
        .join("README.md");
    std::fs::write(&main, b"# recovered main\n")?;
    let baseline_json: String = sqlx::query_scalar(
        "SELECT baseline_manifest_json FROM propagation_links WHERE session_id = ?",
    )
    .bind(session_id.to_string())
    .fetch_one(&fx.pool)
    .await?;
    let baseline = serde_json::from_str::<serde_json::Value>(&baseline_json)?;
    let main_manifest = fx
        .sync
        .collect_manifest(&WorkspaceHandle::main(fx.project_id))
        .await?;
    let session_manifest = fx
        .sync
        .collect_manifest(&WorkspaceHandle::session(session_id))
        .await?;
    let intent = json!({
        "direction": "sync",
        "actor": {"kind": "recovery-test"},
        "baseline": baseline,
        "main_head": "",
        "session_head": "",
        "paths": ["README.md"],
        "source_preimage": {
            "README.md": serde_json::to_value(main_manifest.nodes.get("README.md"))?
        },
        "target_preimage": {
            "README.md": serde_json::to_value(session_manifest.nodes.get("README.md"))?
        }
    });
    sqlx::query(
        "UPDATE propagation_links SET recovery_state = 'transferring', \
         recovery_intent_json = ? WHERE session_id = ?",
    )
    .bind(intent.to_string())
    .bind(session_id.to_string())
    .execute(&fx.pool)
    .await?;

    assert_eq!(fx.sync.recover_uncertain_propagations().await?, 1);
    let session_file = fx
        .data_root
        .join("workspaces")
        .join("sessions")
        .join(session_id.to_string())
        .join("repo")
        .join("README.md");
    assert_eq!(std::fs::read(session_file)?, b"# recovered main\n");
    let (state, intent): (String, Option<String>) = sqlx::query_as(
        "SELECT recovery_state, recovery_intent_json FROM propagation_links WHERE session_id = ?",
    )
    .bind(session_id.to_string())
    .fetch_one(&fx.pool)
    .await?;
    assert_eq!(state, "idle");
    assert!(intent.is_none());
    Ok(())
}
