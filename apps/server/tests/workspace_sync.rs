//! Gate-1 integration tests for Session copy + Merkle + Diff + delete.
//!
//! These exercise the public `WorkspaceSyncInterface` against a real temp
//! data root (SQLite migrations + BlobStore + filesystem), without HTTP.

use std::path::PathBuf;

use janus_server::modules::workspace_sync::interface::{
    FileMutation, WorkspaceHandle, WorkspaceSyncInterface,
};
use janus_server::platform::{
    database::Database,
    id::{ProjectId, SessionId},
    managed_storage::BlobStore,
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
    sync: WorkspaceSyncInterface,
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
        std::fs::create_dir_all(main_abs.join("src"))?;
        std::fs::write(main_abs.join("src").join("lib.rs"), b"fn main() {}\n")?;
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&main_abs)
                .env("GIT_OPTIONAL_LOCKS", "0")
                .status()?;
            assert!(status.success(), "git {:?} failed in fixture", args);
            Ok::<(), anyhow::Error>(())
        };
        git(&["init", "--initial-branch=main"])?;
        git(&["config", "user.email", "janus@local"])?;
        git(&["config", "user.name", "Janus"])?;
        git(&["add", "-A"])?;
        git(&["commit", "-m", "main baseline"])?;

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
        summary.modified, 0,
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
    assert!(!summary.apply_enabled);
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
