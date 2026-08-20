//! Working-tree file operations and the controlled file-mutation
//! pipeline (write/patch/delete/move) against a workspace copy.
use super::*;

impl WorkspaceInterface {
    pub async fn workspace_root(
        &self,
        handle: &WorkspaceHandle,
    ) -> Result<PathBuf, WorkspaceError> {
        let managed_dir = self.managed_dir_for(handle).await?;
        tokio::fs::canonicalize(self.data_root.join(managed_dir))
            .await
            .map_err(|error| WorkspaceError::Internal(error.into()))
    }

    pub async fn file_meta(
        &self,
        handle: &WorkspaceHandle,
        raw_path: &str,
    ) -> Result<FileMetaView, WorkspaceError> {
        let rel = validate_workspace_path(raw_path)?;
        let _lock = self.acquire_mutation_lock(handle).await?;
        let abs = self.workspace_root(handle).await?.join(&rel);
        let meta = tokio::fs::metadata(&abs)
            .await
            .map_err(|error| read_error(raw_path, error))?;
        let revision = self
            .current_revision(handle)
            .await
            .ok()
            .map(|revision| revision.0);
        Ok(FileMetaView {
            path: raw_path.to_owned(),
            size: meta.len(),
            editable: meta.len() <= 10 * 1024 * 1024 && is_utf8_text_file(&abs).await,
            mime: guess_mime(&abs),
            main_revision: revision,
        })
    }

    pub async fn file_content(
        &self,
        handle: &WorkspaceHandle,
        raw_path: &str,
    ) -> Result<Vec<u8>, WorkspaceError> {
        let rel = validate_workspace_path(raw_path)?;
        let _lock = self.acquire_mutation_lock(handle).await?;
        let abs = self.workspace_root(handle).await?.join(rel);
        let meta = tokio::fs::metadata(&abs)
            .await
            .map_err(|error| read_error(raw_path, error))?;
        if meta.is_dir() {
            return Err(WorkspaceError::NotEditable(raw_path.to_owned()));
        }
        tokio::fs::read(&abs)
            .await
            .map_err(|error| read_error(raw_path, error))
    }

    pub async fn file_tree(
        &self,
        handle: &WorkspaceHandle,
        raw_path: &str,
    ) -> Result<Vec<FileTreeView>, WorkspaceError> {
        let rel = if raw_path.is_empty() {
            PathBuf::new()
        } else {
            validate_workspace_path(raw_path)?
        };
        let _lock = self.acquire_mutation_lock(handle).await?;
        let abs = self.workspace_root(handle).await?.join(&rel);
        let mut entries = tokio::fs::read_dir(&abs)
            .await
            .map_err(|error| read_error(raw_path, error))?;
        let mut out = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| WorkspaceError::Internal(error.into()))?
        {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".git" {
                continue;
            }
            let meta = entry
                .metadata()
                .await
                .map_err(|error| WorkspaceError::Internal(error.into()))?;
            let child_path = if rel.as_os_str().is_empty() {
                name.clone()
            } else {
                format!("{}/{name}", rel.to_string_lossy())
            };
            out.push(FileTreeView {
                path: child_path,
                kind: if meta.is_dir() { "dir" } else { "file" }.into(),
                size: meta.len(),
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// Apply one filesystem mutation through its durable journal.
    pub async fn apply_file_mutation(
        &self,
        handle: &WorkspaceHandle,
        mutation: FileMutation,
        expected: Option<&RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
    ) -> Result<RevisionRef, WorkspaceError> {
        let lock = self.acquire_mutation_lock(handle).await?;
        let prepared = self
            .prepare_file_mutation(
                &lock,
                FileMutationRequest {
                    handle,
                    mutation,
                    expected,
                    cause,
                    actor,
                    event: None,
                },
            )
            .await?;
        let applied = self.apply_prepared_file_mutation(&lock, &prepared).await?;
        let mut tx = self.pool.begin().await?;
        let revision = self
            .finalize_file_mutation_in_tx(&lock, &mut tx, &prepared, &applied)
            .await?;
        tx.commit().await?;
        Ok(revision)
    }

    /// Commit a pending filesystem mutation intent before running its effect.
    pub async fn prepare_file_mutation(
        &self,
        lock: &WorkspaceMutationGuard,
        request: FileMutationRequest<'_>,
    ) -> Result<PreparedFileMutation, WorkspaceError> {
        self.assert_guard_handle(lock, request.handle).await?;
        let managed_dir = self.managed_dir_for(request.handle).await?;
        let root = self.data_root.join(&managed_dir);
        validate_file_mutation(&root, &request.mutation).await?;
        let pre_manifest = hash_working_tree(&root)
            .await
            .map_err(WorkspaceError::Internal)?;
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT id FROM workspace_mutation_intents \
             WHERE workspace_handle = ? AND state IN ('pending', 'applied', 'awaiting_event') \
             ORDER BY created_at, id LIMIT 1",
        )
        .bind(request.handle.as_str())
        .fetch_optional(&self.pool)
        .await?;
        if let Some(existing) = existing {
            return Err(WorkspaceError::Internal(anyhow!(
                "workspace mutation {existing} requires reconciliation"
            )));
        }
        let intent = StoredFileMutationIntent {
            id: format!("mutation_{}", Uuid::now_v7()),
            handle: request.handle.clone(),
            project_id: lock.project_id.clone(),
            mutation: request.mutation,
            expected_revision: request.expected.cloned(),
            cause: request.cause.to_owned(),
            actor: request.actor,
            pre_manifest,
            event: request.event,
        };
        let now = now_utc_str();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        self.check_expected_revision_in_tx(
            &mut tx,
            &intent.handle,
            intent.expected_revision.as_ref(),
        )
        .await?;
        sqlx::query(
            "INSERT INTO workspace_mutation_intents \
             (id, workspace_handle, project_id, mutation_json, expected_revision_id, cause, \
              actor_json, pre_manifest_json, event_json, state, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)",
        )
        .bind(&intent.id)
        .bind(intent.handle.as_str())
        .bind(&intent.project_id)
        .bind(serde_json::to_string(&intent.mutation)?)
        .bind(
            intent
                .expected_revision
                .as_ref()
                .map(|revision| &revision.0),
        )
        .bind(&intent.cause)
        .bind(serde_json::to_string(&intent.actor)?)
        .bind(serde_json::to_string(&intent.pre_manifest)?)
        .bind(
            intent
                .event
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(PreparedFileMutation { intent })
    }

    /// Run a prepared filesystem effect outside any database write transaction.
    pub async fn apply_prepared_file_mutation(
        &self,
        lock: &WorkspaceMutationGuard,
        prepared: &PreparedFileMutation,
    ) -> Result<AppliedFileMutation, WorkspaceError> {
        self.assert_guard_handle(lock, &prepared.intent.handle)
            .await?;
        let managed_dir = self.managed_dir_for(&prepared.intent.handle).await?;
        let root = self.data_root.join(&managed_dir);
        apply_file_mutation_fs(&root, &prepared.intent.mutation).await?;
        let manifest = hash_working_tree(&root)
            .await
            .map_err(WorkspaceError::Internal)?;
        let changed = sqlx::query(
            "UPDATE workspace_mutation_intents SET state = 'applied', \
             observed_manifest_root_hash = ?, updated_at = ? \
             WHERE id = ? AND state = 'pending'",
        )
        .bind(&manifest.root_hash)
        .bind(now_utc_str())
        .bind(&prepared.intent.id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed == 0 {
            return Err(WorkspaceError::Internal(anyhow!(
                "workspace mutation {} is no longer pending",
                prepared.intent.id
            )));
        }
        Ok(AppliedFileMutation {
            intent_id: prepared.intent.id.clone(),
            manifest_root_hash: manifest.root_hash,
        })
    }

    /// Finalize revision identity and caller-owned events in one short transaction.
    pub async fn finalize_file_mutation_in_tx(
        &self,
        lock: &WorkspaceMutationGuard,
        tx: &mut SqliteConnection,
        prepared: &PreparedFileMutation,
        applied: &AppliedFileMutation,
    ) -> Result<RevisionRef, WorkspaceError> {
        self.assert_guard_handle_in_tx(lock, tx, &prepared.intent.handle)
            .await?;
        if prepared.intent.id != applied.intent_id {
            return Err(WorkspaceError::Internal(anyhow!(
                "workspace mutation intent mismatch"
            )));
        }
        let state: Option<String> =
            sqlx::query_scalar("SELECT state FROM workspace_mutation_intents WHERE id = ?")
                .bind(&prepared.intent.id)
                .fetch_optional(&mut *tx)
                .await?;
        if !matches!(state.as_deref(), Some("applied")) {
            return Err(WorkspaceError::Internal(anyhow!(
                "workspace mutation {} is not applied",
                prepared.intent.id
            )));
        }
        let revision = self
            .advance_revision_in_tx(
                tx,
                &prepared.intent.handle,
                prepared.intent.expected_revision.as_ref(),
                &prepared.intent.cause,
                prepared.intent.actor.clone(),
                Some((&applied.manifest_root_hash, "tool_write")),
            )
            .await?;
        let state = if prepared.intent.event.is_some() {
            "awaiting_event"
        } else {
            "completed"
        };
        sqlx::query(
            "UPDATE workspace_mutation_intents SET state = ?, revision_id = ?, updated_at = ? \
             WHERE id = ? AND state = 'applied'",
        )
        .bind(state)
        .bind(&revision.0)
        .bind(now_utc_str())
        .bind(&prepared.intent.id)
        .execute(&mut *tx)
        .await?;
        Ok(revision)
    }

    pub async fn acknowledge_file_mutation_event_in_tx(
        &self,
        tx: &mut SqliteConnection,
        intent_id: &str,
        revision: &RevisionRef,
    ) -> Result<(), WorkspaceError> {
        let changed = sqlx::query(
            "UPDATE workspace_mutation_intents SET state = 'completed', updated_at = ? \
             WHERE id = ? AND state = 'awaiting_event' AND revision_id = ?",
        )
        .bind(now_utc_str())
        .bind(intent_id)
        .bind(&revision.0)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed == 0 {
            return Err(WorkspaceError::Internal(anyhow!(
                "workspace mutation event acknowledgement lost for {intent_id}"
            )));
        }
        Ok(())
    }

    /// Reconcile effects left by a process restart. Main editor events are
    /// returned to the application seam after the revision transaction commits.
    pub async fn recover_uncertain_file_mutations(
        &self,
    ) -> Result<Vec<RecoveredFileMutation>, WorkspaceError> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM workspace_mutation_intents \
             WHERE state IN ('pending', 'applied', 'awaiting_event') \
             ORDER BY updated_at, id",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut recovered = Vec::new();
        for id in rows {
            let intent = self.load_file_mutation_intent(&id).await?;
            let lock = self.lock_project(&intent.project_id).await;
            if let Some(event) = self.reconcile_file_mutation_locked(&lock, &intent).await? {
                recovered.push(event);
            }
        }
        Ok(recovered)
    }

    async fn reconcile_file_mutation_locked(
        &self,
        lock: &WorkspaceMutationGuard,
        intent: &StoredFileMutationIntent,
    ) -> Result<Option<RecoveredFileMutation>, WorkspaceError> {
        let state = self.intent_state(&intent.id).await?;
        if state.as_deref() == Some("awaiting_event") {
            let revision = self.intent_revision(&intent.id).await?.ok_or_else(|| {
                WorkspaceError::Internal(anyhow!("mutation {} has no revision", intent.id))
            })?;
            return Ok(intent.event.clone().map(|event| RecoveredFileMutation {
                intent_id: intent.id.clone(),
                revision: RevisionRef(revision),
                event,
            }));
        }
        self.assert_guard_handle(lock, &intent.handle).await?;
        let managed_dir = self.managed_dir_for(&intent.handle).await?;
        let root = self.data_root.join(&managed_dir);
        let current = hash_working_tree(&root)
            .await
            .map_err(WorkspaceError::Internal)?;
        let scope = mutation_scope(&intent.pre_manifest, &intent.mutation);
        let expected_post = expected_post_manifest(&intent.pre_manifest, &intent.mutation)?;
        if !manifests_match_scope(&current, &expected_post, &scope) {
            if manifests_match_scope(&current, &intent.pre_manifest, &scope) {
                apply_file_mutation_fs(&root, &intent.mutation).await?;
            } else {
                self.mark_file_mutation_attention(&intent.id, "workspace changed during recovery")
                    .await?;
                return Err(WorkspaceError::Internal(anyhow!(
                    "workspace mutation {} needs attention",
                    intent.id
                )));
            }
        }
        let observed = hash_working_tree(&root)
            .await
            .map_err(WorkspaceError::Internal)?;
        let prepared = PreparedFileMutation {
            intent: intent.clone(),
        };
        let applied = AppliedFileMutation {
            intent_id: intent.id.clone(),
            manifest_root_hash: observed.root_hash,
        };
        sqlx::query(
            "UPDATE workspace_mutation_intents SET state = 'applied', \
             observed_manifest_root_hash = ?, updated_at = ? \
             WHERE id = ? AND state IN ('pending', 'applied')",
        )
        .bind(&applied.manifest_root_hash)
        .bind(now_utc_str())
        .bind(&intent.id)
        .execute(&self.pool)
        .await?;
        let mut tx = self.pool.begin().await?;
        let revision = self
            .finalize_file_mutation_in_tx(lock, &mut tx, &prepared, &applied)
            .await?;
        tx.commit().await?;
        Ok(intent.event.clone().map(|event| RecoveredFileMutation {
            intent_id: intent.id.clone(),
            revision,
            event,
        }))
    }

    async fn load_file_mutation_intent(
        &self,
        id: &str,
    ) -> Result<StoredFileMutationIntent, WorkspaceError> {
        let row: Option<StoredFileMutationIntentRow> = sqlx::query_as(
            "SELECT id, workspace_handle, project_id, mutation_json, expected_revision_id, \
             cause, actor_json, pre_manifest_json, event_json \
             FROM workspace_mutation_intents WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((
            id,
            handle,
            project_id,
            mutation_json,
            expected_revision_id,
            cause,
            actor_json,
            pre_manifest_json,
            event_json,
        )) = row
        else {
            return Err(WorkspaceError::NotFound);
        };
        Ok(StoredFileMutationIntent {
            id,
            handle: WorkspaceHandle(handle),
            project_id,
            mutation: serde_json::from_str(&mutation_json)?,
            expected_revision: expected_revision_id.map(RevisionRef),
            cause,
            actor: serde_json::from_str(&actor_json)?,
            pre_manifest: serde_json::from_str(&pre_manifest_json)?,
            event: event_json
                .map(|json| serde_json::from_str(&json))
                .transpose()?,
        })
    }

    async fn intent_state(&self, id: &str) -> Result<Option<String>, WorkspaceError> {
        Ok(
            sqlx::query_scalar("SELECT state FROM workspace_mutation_intents WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    async fn intent_revision(&self, id: &str) -> Result<Option<String>, WorkspaceError> {
        Ok(
            sqlx::query_scalar("SELECT revision_id FROM workspace_mutation_intents WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    async fn mark_file_mutation_attention(
        &self,
        id: &str,
        error: &str,
    ) -> Result<(), WorkspaceError> {
        sqlx::query(
            "UPDATE workspace_mutation_intents SET state = 'needs_attention', error = ?, updated_at = ? WHERE id = ?",
        )
        .bind(error)
        .bind(now_utc_str())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn assert_guard_handle(
        &self,
        lock: &WorkspaceMutationGuard,
        handle: &WorkspaceHandle,
    ) -> Result<(), WorkspaceError> {
        let project_id: Option<String> =
            sqlx::query_scalar("SELECT project_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&self.pool)
                .await?;
        if project_id.as_deref() != Some(lock.project_id.as_str()) {
            return Err(WorkspaceError::Internal(anyhow!(
                "mutation guard does not own workspace handle {}",
                handle.as_str()
            )));
        }
        Ok(())
    }

    async fn assert_guard_handle_in_tx(
        &self,
        lock: &WorkspaceMutationGuard,
        tx: &mut SqliteConnection,
        handle: &WorkspaceHandle,
    ) -> Result<(), WorkspaceError> {
        let project_id: Option<String> =
            sqlx::query_scalar("SELECT project_id FROM workspace_copies WHERE handle = ?")
                .bind(handle.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        if project_id.as_deref() != Some(lock.project_id.as_str()) {
            return Err(WorkspaceError::Internal(anyhow!(
                "mutation guard does not own workspace handle {}",
                handle.as_str()
            )));
        }
        Ok(())
    }
}

/// Separate the filesystem refusals a user can act on from a genuinely missing
/// path. Reporting every read failure as `PathNotFound` tells a user that a file
/// they are looking at in the tree does not exist, and hides the real cause.
fn read_error(raw_path: &str, error: std::io::Error) -> WorkspaceError {
    match error.kind() {
        std::io::ErrorKind::NotFound => WorkspaceError::PathNotFound(raw_path.to_owned()),
        std::io::ErrorKind::PermissionDenied => {
            WorkspaceError::PermissionDenied(raw_path.to_owned())
        }
        _ => WorkspaceError::Internal(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::read_error;
    use crate::interface::WorkspaceError;

    #[test]
    fn read_failures_keep_their_cause() {
        let missing = read_error("src/main.rs", std::io::ErrorKind::NotFound.into());
        assert!(matches!(missing, WorkspaceError::PathNotFound(_)));
        assert_eq!(missing.to_string(), "path not found: src/main.rs");

        let denied = read_error("src/main.rs", std::io::ErrorKind::PermissionDenied.into());
        assert!(matches!(denied, WorkspaceError::PermissionDenied(_)));
        assert_eq!(
            denied.to_string(),
            "permission denied by the filesystem: src/main.rs"
        );
    }
}
