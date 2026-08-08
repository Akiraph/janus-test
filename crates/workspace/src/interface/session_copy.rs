//! Main and Session workspace copy lifecycle: ensure, recover, delete.
use super::*;

impl WorkspaceInterface {
    pub fn session_repo_path(&self, session_id: impl Display) -> PathBuf {
        session_repo_abs(&self.data_root, session_id)
    }

    /// Create the Main workspace copy for a project and its first Content
    /// Revision. Idempotent: if the copy already exists, the existing revision
    /// is returned. Main revisions leave `manifest_root_hash` NULL; Session
    /// revisions always populate it (see [`Self::ensure_session_copy`]).
    pub async fn ensure_main_copy(
        &self,
        project_id: impl Display,
        managed_dir: &str,
        cause: &str,
        actor: serde_json::Value,
    ) -> Result<RevisionRef, WorkspaceError> {
        let project_id = project_id.to_string();
        let _lock = self.lock_project(&project_id).await;
        let handle = WorkspaceHandle::main(&project_id);
        let now = now_utc_str();

        let existing: Option<(Option<String>,)> =
            sqlx::query_as("SELECT current_revision_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;

        if let Some((Some(revision_id),)) = existing {
            return Ok(RevisionRef(revision_id));
        }

        let copy_version = format!("v_{}", Uuid::now_v7());
        let revision_id = Uuid::now_v7();
        let revision_ref = RevisionRef::new(revision_id);

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT OR IGNORE INTO workspace_copies \
             (handle, project_id, session_id, kind, managed_dir, current_revision_id, \
              observation_generation, dirty, version, created_at, updated_at) \
             VALUES (?, ?, NULL, 'main', ?, ?, 0, 0, ?, ?, ?)",
        )
        .bind(handle.as_str())
        .bind(&project_id)
        .bind(managed_dir)
        .bind(revision_ref.0.clone())
        .bind(&copy_version)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO content_revisions \
             (revision_id, workspace_handle, sequence, manifest_root_hash, cause, \
              actor_json, prev_revision_id, stable, occurred_at) \
             VALUES (?, ?, 1, NULL, ?, ?, NULL, 1, ?)",
        )
        .bind(revision_ref.0.clone())
        .bind(handle.as_str())
        .bind(cause)
        .bind(serde_json::to_string(&actor)?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(revision_ref)
    }

    /// Create a Session workspace copy from Project Main.
    ///
    /// Idempotent: if the Session handle already exists, returns the existing
    /// revision without touching its worktree. Creates a Git worktree from
    /// Main, seeds only dirty Main paths when necessary, records the current
    /// Merkle manifest as the persisted propagation baseline, writes revision
    /// sequence=1, and initializes `propagation_links` cursors to that pair.
    pub async fn ensure_session_copy(
        &self,
        project_id: impl Display,
        session_id: impl Display,
        source_main_revision: Option<&RevisionRef>,
        actor: serde_json::Value,
    ) -> Result<SessionCopyResult, WorkspaceError> {
        let project_id = project_id.to_string();
        let _lock = self.lock_project(&project_id).await;
        let session_id = session_id.to_string();
        let handle = WorkspaceHandle::session(&session_id);
        let existing: Option<ExistingSessionCopy> = sqlx::query_as(
            "SELECT current_revision_id, \
                    (SELECT manifest_root_hash FROM content_revisions \
                     WHERE revision_id = workspace_copies.current_revision_id), \
                    managed_dir, \
                    (SELECT initial_main_revision_id FROM propagation_links \
                     WHERE session_id = workspace_copies.session_id), \
                    (SELECT baseline_manifest_json FROM propagation_links \
                     WHERE session_id = workspace_copies.session_id) \
             FROM workspace_copies WHERE handle = ?",
        )
        .bind(handle.as_str())
        .fetch_optional(&self.pool)
        .await?;

        if let Some((Some(revision_id), root, managed_dir, Some(source_main_revision), _)) =
            existing
        {
            return Ok(SessionCopyResult {
                handle,
                revision: RevisionRef(revision_id),
                source_main_revision: RevisionRef(source_main_revision),
                manifest_root_hash: root.unwrap_or_default(),
                managed_dir,
            });
        }

        let main_handle = WorkspaceHandle::main(&project_id);
        let main_row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT managed_dir, current_revision_id FROM workspace_copies WHERE handle = ?",
        )
        .bind(main_handle.as_str())
        .fetch_optional(&self.pool)
        .await?;
        let (main_managed_dir, main_revision_id) = main_row.ok_or(WorkspaceError::NotFound)?;
        let main_revision_id = main_revision_id.ok_or_else(|| {
            WorkspaceError::Internal(anyhow!("main copy has no current revision"))
        })?;
        if let Some(expected) = source_main_revision
            && expected.0 != main_revision_id
        {
            return Err(WorkspaceError::RevisionMismatch {
                expected: expected.0.clone(),
                current: main_revision_id,
            });
        }

        let managed_dir = session_managed_dir(&session_id);
        let session_abs = session_repo_abs(&self.data_root, &session_id);
        let main_abs = main_repo_abs(&self.data_root, &main_managed_dir);

        // Session copy is a git worktree of the Main clone - shared .git object
        // store, detached-HEAD checkout at Main's current tree. No file copy,
        // no re-init; the Session inherits Main's history.
        let main_for_copy = main_abs.clone();
        let session_for_copy = session_abs.clone();
        let (head, main_was_clean) = tokio::task::spawn_blocking(move || {
            let head = git_head(&main_for_copy)?;
            let clean = main_worktree_is_clean(&main_for_copy)?;
            create_session_worktree(&main_for_copy, &session_for_copy)?;
            Ok::<(String, bool), anyhow::Error>((head, clean))
        })
        .await
        .map_err(|error| WorkspaceError::Internal(anyhow!("workspace copy task failed: {error}")))?
        .map_err(WorkspaceError::Internal)?;

        let base_manifest = match cached_head_manifest(&head) {
            Some(manifest) => manifest,
            None => {
                let manifest = hash_working_tree(&session_abs)
                    .await
                    .map_err(WorkspaceError::Internal)?;
                cache_head_manifest(&head, &manifest);
                manifest
            }
        };
        let manifest = if main_was_clean {
            base_manifest
        } else {
            let main_for_seed = main_abs.clone();
            let session_for_seed = session_abs.clone();
            let changed_paths = tokio::task::spawn_blocking(move || {
                seed_session_from_main(&main_for_seed, &session_for_seed)
            })
            .await
            .map_err(|error| {
                WorkspaceError::Internal(anyhow!("workspace seed task failed: {error}"))
            })?
            .map_err(WorkspaceError::Internal)?;
            rehash_working_tree_paths(&session_abs, &base_manifest, &changed_paths)
                .await
                .map_err(WorkspaceError::Internal)?
        };
        let root_hash = manifest.root_hash.clone();
        let baseline =
            PropagationBaseline::from_manifest(manifest, Some(head.clone()), Some(head.clone()));
        let baseline_json = serde_json::to_string(&baseline)?;
        let now = now_utc_str();
        let copy_version = format!("v_{}", Uuid::now_v7());
        let revision_ref = RevisionRef::new(Uuid::now_v7());
        let snapshot_id = Uuid::now_v7();
        let link_version = format!("v_{}", Uuid::now_v7());

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO workspace_copies \
             (handle, project_id, session_id, kind, managed_dir, current_revision_id, \
              observation_generation, dirty, version, created_at, updated_at) \
             VALUES (?, ?, ?, 'session', ?, ?, 0, 0, ?, ?, ?)",
        )
        .bind(handle.as_str())
        .bind(project_id.to_string())
        .bind(session_id.to_string())
        .bind(&managed_dir)
        .bind(revision_ref.0.clone())
        .bind(&copy_version)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO content_revisions \
             (revision_id, workspace_handle, sequence, manifest_root_hash, cause, \
              actor_json, prev_revision_id, stable, occurred_at) \
             VALUES (?, ?, 1, ?, 'session.create', ?, NULL, 1, ?)",
        )
        .bind(revision_ref.0.clone())
        .bind(handle.as_str())
        .bind(&root_hash)
        .bind(serde_json::to_string(&actor)?)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO workspace_snapshots \
             (snapshot_id, revision_id, manifest_root_hash, purpose, integrity_state, created_at) \
             VALUES (?, ?, ?, 'session_create', 'complete', ?)",
        )
        .bind(snapshot_id.to_string())
        .bind(revision_ref.0.clone())
        .bind(&root_hash)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO propagation_links \
             (project_id, session_id, source_branch, initial_main_revision_id, \
              main_to_session_cursor_revision_id, session_to_main_cursor_revision_id, \
              version, created_at, updated_at, baseline_manifest_json) \
             VALUES (?, ?, 'main', ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(project_id.to_string())
        .bind(session_id.to_string())
        .bind(&main_revision_id)
        .bind(&main_revision_id)
        .bind(revision_ref.0.clone())
        .bind(&link_version)
        .bind(&now)
        .bind(&now)
        .bind(baseline_json)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(SessionCopyResult {
            handle,
            revision: revision_ref,
            source_main_revision: RevisionRef(main_revision_id),
            manifest_root_hash: root_hash,
            managed_dir,
        })
    }

    /// Remove Session worktree directories that were created before their
    /// `workspace_copies` row committed. The directory is Workspace-owned, so
    /// an absent registration is sufficient evidence that it is recoverable
    /// startup debris rather than a user-managed path.
    pub async fn recover_orphan_session_worktrees(&self) -> Result<usize, WorkspaceError> {
        let registered: BTreeSet<String> = sqlx::query_scalar(
            "SELECT session_id FROM workspace_copies \
             WHERE kind = 'session' AND session_id IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect();
        let sessions_root = self.data_root.join("workspaces").join("sessions");
        let mut entries = match tokio::fs::read_dir(&sessions_root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(WorkspaceError::Internal(error.into())),
        };
        let mut removed = 0;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| WorkspaceError::Internal(error.into()))?
        {
            if !entry
                .file_type()
                .await
                .map_err(|error| WorkspaceError::Internal(error.into()))?
                .is_dir()
            {
                continue;
            }
            let session_id = entry.file_name().to_string_lossy().to_string();
            if registered.contains(&session_id) {
                continue;
            }
            let data_root = self.data_root.clone();
            tokio::task::spawn_blocking(move || remove_session_tree(&data_root, &session_id))
                .await
                .map_err(|error| WorkspaceError::Internal(anyhow!(error.to_string())))?
                .map_err(WorkspaceError::Internal)?;
            removed += 1;
        }
        Ok(removed)
    }

    /// Remove Main clone directories that exist without a registered Main copy.
    /// This covers a crash after `git clone` and before the first Workspace
    /// revision transaction commits.
    pub async fn recover_orphan_main_worktrees(&self) -> Result<usize, WorkspaceError> {
        let registered: BTreeSet<String> =
            sqlx::query_scalar("SELECT managed_dir FROM workspace_copies WHERE kind = 'main'")
                .fetch_all(&self.pool)
                .await?
                .into_iter()
                .filter_map(|managed_dir: String| {
                    Path::new(&managed_dir)
                        .parent()
                        .and_then(Path::file_name)
                        .map(|name| name.to_string_lossy().to_string())
                })
                .collect();
        let main_root = self.data_root.join("workspaces").join("main");
        let mut entries = match tokio::fs::read_dir(&main_root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(WorkspaceError::Internal(error.into())),
        };
        let mut removed = 0;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| WorkspaceError::Internal(error.into()))?
        {
            if !entry
                .file_type()
                .await
                .map_err(|error| WorkspaceError::Internal(error.into()))?
                .is_dir()
            {
                continue;
            }
            let project_id = entry.file_name().to_string_lossy().to_string();
            if registered.contains(&project_id) {
                continue;
            }
            tokio::fs::remove_dir_all(entry.path())
                .await
                .map_err(|error| WorkspaceError::Internal(error.into()))?;
            removed += 1;
        }
        Ok(removed)
    }

    /// Cascade-delete a Session copy: directory tree + DB rows for that handle
    /// (workspace_copies cascades content_revisions/snapshots; links by session_id).
    /// Does **not** touch Main or Runtime.
    pub async fn delete_session_copy(
        &self,
        session_id: impl Display,
    ) -> Result<(), WorkspaceError> {
        let session_id = session_id.to_string();
        let handle = WorkspaceHandle::session(&session_id);
        let project_id: Option<String> =
            sqlx::query_scalar("SELECT project_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;
        let Some(project_id) = project_id else {
            // Idempotent: already gone is success.
            self.cleanup_session_tree(&session_id).await;
            return Ok(());
        };
        let _lock = self.lock_project(&project_id).await;

        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM workspace_propagation_conflicts WHERE session_id = ?")
            .bind(&session_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM propagation_links WHERE session_id = ?")
            .bind(&session_id)
            .execute(&mut *tx)
            .await?;
        // content_revisions / workspace_snapshots cascade from workspace_copies.
        sqlx::query("DELETE FROM workspace_copies WHERE handle = ?")
            .bind(handle.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        // The durable deletion is complete once the metadata transaction is
        // committed. Files may contain tool-created trees or open handles, so
        // keep cleanup bounded and prevent one bad worktree from stalling the
        // session lifecycle queue.
        self.cleanup_session_tree(&session_id).await;
        Ok(())
    }

    async fn cleanup_session_tree(&self, session_id: &str) {
        let data_root = self.data_root.clone();
        let session_id = session_id.to_owned();
        let cleanup_session_id = session_id.clone();
        let cleanup = tokio::task::spawn_blocking(move || {
            remove_session_tree(&data_root, &cleanup_session_id)
        });
        match tokio::time::timeout(SESSION_TREE_CLEANUP_TIMEOUT, cleanup).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => {
                warn!(%error, %session_id, "session worktree cleanup failed after metadata deletion");
            }
            Ok(Err(error)) => {
                warn!(%error, %session_id, "session worktree cleanup task failed");
            }
            Err(_) => {
                warn!(%session_id, "session worktree cleanup exceeded its timeout; leaving it detached");
            }
        }
    }

    pub(crate) async fn managed_dir_for(&self, handle: &WorkspaceHandle) -> Result<String, WorkspaceError> {
        let row: Option<String> =
            sqlx::query_scalar("SELECT managed_dir FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;
        row.ok_or(WorkspaceError::NotFound)
    }

}

