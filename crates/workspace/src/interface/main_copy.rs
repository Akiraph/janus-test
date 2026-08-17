//! Main workspace copy lifecycle.
use super::*;

impl WorkspaceInterface {
    /// Create the Main workspace copy for a project and its first Content
    /// Revision. Idempotent: if the copy already exists, its revision is
    /// returned.
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
        let revision_ref = RevisionRef::new(Uuid::now_v7());

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT OR IGNORE INTO workspace_copies \
             (handle, project_id, kind, managed_dir, current_revision_id, \
              observation_generation, dirty, version, created_at, updated_at) \
             VALUES (?, ?, 'main', ?, ?, 0, 0, ?, ?, ?)",
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

    /// Remove Main clone directories that exist without a registered Main
    /// copy. This covers a crash after `git clone` and before the first
    /// Workspace revision transaction commits.
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

    pub(crate) async fn managed_dir_for(
        &self,
        handle: &WorkspaceHandle,
    ) -> Result<String, WorkspaceError> {
        let row: Option<String> =
            sqlx::query_scalar("SELECT managed_dir FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;
        row.ok_or(WorkspaceError::NotFound)
    }
}
