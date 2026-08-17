//! Content-revision identity, manifest collection, and revision advancement.
use super::*;

impl WorkspaceInterface {
    /// Read the current revision identity for any workspace copy.
    pub async fn current_revision(
        &self,
        handle: &WorkspaceHandle,
    ) -> Result<RevisionRef, WorkspaceError> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT current_revision_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some((Some(revision_id),)) => Ok(RevisionRef(revision_id)),
            Some((None,)) => Err(WorkspaceError::Internal(anyhow!(
                "copy has no current revision"
            ))),
            None => Err(WorkspaceError::NotFound),
        }
    }

    /// Read current revisions for several workspace copies in one query.
    /// Missing or revision-less copies are omitted, matching the optional
    /// behavior used by session summaries while a copy is being created.
    pub async fn current_revisions(
        &self,
        handles: &[WorkspaceHandle],
    ) -> Result<HashMap<String, RevisionRef>, WorkspaceError> {
        if handles.is_empty() {
            return Ok(HashMap::new());
        }

        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT handle, current_revision_id FROM workspace_copies WHERE handle IN (",
        );
        let mut separated = query.separated(", ");
        for handle in handles {
            separated.push_bind(handle.as_str());
        }
        separated.push_unseparated(")");

        let rows: Vec<(String, Option<String>)> =
            query.build_query_as().fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .filter_map(|(handle, revision)| {
                revision.map(|revision| (handle, RevisionRef(revision)))
            })
            .collect())
    }

    /// Advance a copy to a new revision without collecting a Merkle root
    /// (Main editor path). Prefer [`Self::apply_file_mutation`] for
    /// Session tool writes so `manifest_root_hash` is populated.
    pub async fn bump_revision(
        &self,
        handle: &WorkspaceHandle,
        expected: Option<&RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
    ) -> Result<RevisionRef, WorkspaceError> {
        self.advance_revision(handle, expected, cause, actor, None, None)
            .await
    }

    /// Full Merkle scan of a workspace copy. Used by Diff and tests.
    pub async fn collect_manifest(
        &self,
        handle: &WorkspaceHandle,
    ) -> Result<ManifestRoot, WorkspaceError> {
        let _lock = self.acquire_mutation_lock(handle).await?;
        let managed_dir = self.managed_dir_for(handle).await?;
        let root = self.data_root.join(&managed_dir);
        walk_manifest(&root, &self.blobs, handle.as_str())
            .await
            .map_err(WorkspaceError::Internal)
    }

    async fn advance_revision(
        &self,
        handle: &WorkspaceHandle,
        expected: Option<&RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
        manifest_root_hash: Option<&str>,
        snapshot_purpose: Option<&str>,
    ) -> Result<RevisionRef, WorkspaceError> {
        let mut tx = self.pool.begin().await?;
        let revision = self
            .advance_revision_in_tx(
                &mut tx,
                handle,
                expected,
                cause,
                actor,
                manifest_root_hash.zip(snapshot_purpose),
            )
            .await?;
        tx.commit().await?;
        Ok(revision)
    }

    pub(crate) async fn check_expected_revision_in_tx(
        &self,
        tx: &mut SqliteConnection,
        handle: &WorkspaceHandle,
        expected: Option<&RevisionRef>,
    ) -> Result<(), WorkspaceError> {
        let Some(expected) = expected else {
            return Ok(());
        };
        let current: Option<Option<String>> =
            sqlx::query_scalar("SELECT current_revision_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        let current = current
            .ok_or(WorkspaceError::NotFound)?
            .ok_or_else(|| WorkspaceError::Internal(anyhow!("copy has no revision")))?;
        if expected.0 != current {
            return Err(WorkspaceError::RevisionMismatch {
                expected: expected.0.clone(),
                current,
            });
        }
        Ok(())
    }

    pub(crate) async fn advance_revision_in_tx(
        &self,
        tx: &mut SqliteConnection,
        handle: &WorkspaceHandle,
        expected: Option<&RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
        snapshot: Option<(&str, &str)>,
    ) -> Result<RevisionRef, WorkspaceError> {
        let current: Option<Option<String>> =
            sqlx::query_scalar("SELECT current_revision_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        let current = current
            .ok_or(WorkspaceError::NotFound)?
            .ok_or_else(|| WorkspaceError::Internal(anyhow!("copy has no revision")))?;
        if let Some(expected_ref) = expected
            && expected_ref.0 != current
        {
            return Err(WorkspaceError::RevisionMismatch {
                expected: expected_ref.0.clone(),
                current,
            });
        }

        let now = now_utc_str();
        let next_sequence: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM content_revisions \
             WHERE workspace_handle = ?",
        )
        .bind(handle.as_str())
        .fetch_one(&mut *tx)
        .await?;
        let revision_ref = RevisionRef::new(Uuid::now_v7());
        let copy_version = format!("v_{}", Uuid::now_v7());

        sqlx::query(
            "INSERT INTO content_revisions \
             (revision_id, workspace_handle, sequence, manifest_root_hash, cause, \
              actor_json, prev_revision_id, stable, occurred_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?)",
        )
        .bind(revision_ref.0.clone())
        .bind(handle.as_str())
        .bind(next_sequence)
        .bind(snapshot.map(|(root, _)| root))
        .bind(cause)
        .bind(serde_json::to_string(&actor)?)
        .bind(&current)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        if let Some((root, purpose)) = snapshot {
            let snapshot_id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO workspace_snapshots \
                 (snapshot_id, revision_id, manifest_root_hash, purpose, integrity_state, created_at) \
                 VALUES (?, ?, ?, ?, 'complete', ?)",
            )
            .bind(snapshot_id.to_string())
            .bind(revision_ref.0.clone())
            .bind(root)
            .bind(purpose)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE workspace_copies SET current_revision_id = ?, version = ?, updated_at = ? \
             WHERE handle = ?",
        )
        .bind(revision_ref.0.clone())
        .bind(&copy_version)
        .bind(&now)
        .bind(handle.as_str())
        .execute(&mut *tx)
        .await?;
        Ok(revision_ref)
    }
}
