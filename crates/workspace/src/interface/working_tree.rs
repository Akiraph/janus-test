//! Working-tree file operations and the controlled file-mutation
//! pipeline (write/patch/delete/move) against a workspace copy.
use super::*;
use futures_util::TryStreamExt;
use mongodb::ClientSession;

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
        let root = self.workspace_root(handle).await?;
        let abs = resolve_workspace_path(&root, &rel, raw_path).await?;
        let meta = tokio::fs::symlink_metadata(&abs)
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
        let root = self.workspace_root(handle).await?;
        let abs = resolve_workspace_path(&root, &rel, raw_path).await?;
        let meta = tokio::fs::symlink_metadata(&abs)
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
        let root = self.workspace_root(handle).await?;
        let abs = if rel.as_os_str().is_empty() {
            root
        } else {
            resolve_workspace_path(&root, &rel, raw_path).await?
        };
        let dir_meta = tokio::fs::symlink_metadata(&abs)
            .await
            .map_err(|error| read_error(raw_path, error))?;
        if is_link_or_reparse(&dir_meta) {
            return Err(WorkspaceError::PermissionDenied(raw_path.to_owned()));
        }
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
            if is_link_or_reparse(&meta) {
                continue;
            }
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
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        let revision = self
            .finalize_file_mutation_in_tx(&lock, &mut session, &prepared, &applied)
            .await?;
        session.commit_transaction().await?;
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
        let existing = self
            .pool
            .collection::<Document>("workspace_mutation_intents")
            .find_one(doc! {
                "workspace_handle": request.handle.as_str(),
                "state": {"$in": ["pending", "applied", "awaiting_event"]},
            })
            .sort(doc! {"created_at": 1, "_id": 1})
            .await?;
        if let Some(existing) = existing {
            return Err(WorkspaceError::Internal(anyhow!(
                "workspace mutation {} requires reconciliation",
                existing.get_str("_id")?
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
        let mutation_json = serde_json::to_string(&intent.mutation)?;
        let actor_json = serde_json::to_string(&intent.actor)?;
        let pre_manifest_json = serde_json::to_string(&intent.pre_manifest)?;
        let event_json = intent
            .event
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        // The project lock replaces the SQLite `BEGIN IMMEDIATE` write lock.
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        self.check_expected_revision_in_tx(
            &mut session,
            &intent.handle,
            intent.expected_revision.as_ref(),
        )
        .await?;
        let mut document = doc! {
            "_id": &intent.id,
            "workspace_handle": intent.handle.as_str(),
            "project_id": &intent.project_id,
            "mutation_json": &mutation_json,
            "cause": &intent.cause,
            "actor_json": &actor_json,
            "pre_manifest_json": &pre_manifest_json,
            "state": "pending",
            "created_at": &now,
            "updated_at": &now,
        };
        if let Some(expected) = &intent.expected_revision {
            document.insert("expected_revision_id", &expected.0);
        }
        if let Some(event) = &event_json {
            document.insert("event_json", event);
        }
        self.pool
            .collection::<Document>("workspace_mutation_intents")
            .insert_one(document)
            .session(&mut session)
            .await?;
        session.commit_transaction().await?;
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
        let updated_at = now_utc_str();
        let changed = self
            .pool
            .collection::<Document>("workspace_mutation_intents")
            .update_one(
                doc! {"_id": &prepared.intent.id, "state": "pending"},
                doc! {
                    "$set": {
                        "state": "applied",
                        "observed_manifest_root_hash": &manifest.root_hash,
                        "updated_at": &updated_at,
                    }
                },
            )
            .await?;
        if changed.matched_count == 0 {
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
        tx: &mut ClientSession,
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
        let document = self
            .pool
            .collection::<Document>("workspace_mutation_intents")
            .find_one(doc! {"_id": &prepared.intent.id})
            .session(&mut *tx)
            .await?;
        let state = document
            .as_ref()
            .and_then(|document| document.get_str("state").ok())
            .map(str::to_owned);
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
        let updated_at = now_utc_str();
        self.pool
            .collection::<Document>("workspace_mutation_intents")
            .update_one(
                doc! {"_id": &prepared.intent.id, "state": "applied"},
                doc! {
                    "$set": {
                        "state": state,
                        "revision_id": &revision.0,
                        "updated_at": &updated_at,
                    }
                },
            )
            .session(&mut *tx)
            .await?;
        Ok(revision)
    }

    pub async fn acknowledge_file_mutation_event_in_tx(
        &self,
        tx: &mut ClientSession,
        intent_id: &str,
        revision: &RevisionRef,
    ) -> Result<(), WorkspaceError> {
        let updated_at = now_utc_str();
        let changed = self
            .pool
            .collection::<Document>("workspace_mutation_intents")
            .update_one(
                doc! {
                    "_id": intent_id,
                    "state": "awaiting_event",
                    "revision_id": &revision.0,
                },
                doc! {"$set": {"state": "completed", "updated_at": &updated_at}},
            )
            .session(&mut *tx)
            .await?;
        if changed.matched_count == 0 {
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
        let mut cursor = self
            .pool
            .collection::<Document>("workspace_mutation_intents")
            .find(doc! {"state": {"$in": ["pending", "applied", "awaiting_event"]}})
            .sort(doc! {"updated_at": 1, "_id": 1})
            .await?;
        let mut ids = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            ids.push(document.get_str("_id")?.to_owned());
        }
        let mut recovered = Vec::new();
        for id in ids {
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
        let updated_at = now_utc_str();
        self.pool
            .collection::<Document>("workspace_mutation_intents")
            .update_one(
                doc! {"_id": &intent.id, "state": {"$in": ["pending", "applied"]}},
                doc! {
                    "$set": {
                        "state": "applied",
                        "observed_manifest_root_hash": &applied.manifest_root_hash,
                        "updated_at": &updated_at,
                    }
                },
            )
            .await?;
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        let revision = self
            .finalize_file_mutation_in_tx(lock, &mut session, &prepared, &applied)
            .await?;
        session.commit_transaction().await?;
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
        let document = self
            .pool
            .collection::<Document>("workspace_mutation_intents")
            .find_one(doc! {"_id": id})
            .await?;
        let Some(document) = document else {
            return Err(WorkspaceError::NotFound);
        };
        let id = document.get_str("_id")?.to_owned();
        let handle = document.get_str("workspace_handle")?.to_owned();
        let project_id = document.get_str("project_id")?.to_owned();
        let mutation = serde_json::from_str(document.get_str("mutation_json")?)?;
        let expected_revision = document
            .get("expected_revision_id")
            .and_then(Bson::as_str)
            .map(|revision| RevisionRef(revision.to_owned()));
        let cause = document.get_str("cause")?.to_owned();
        let actor = serde_json::from_str(document.get_str("actor_json")?)?;
        let pre_manifest = serde_json::from_str(document.get_str("pre_manifest_json")?)?;
        let event = document
            .get("event_json")
            .and_then(Bson::as_str)
            .map(serde_json::from_str)
            .transpose()?;
        Ok(StoredFileMutationIntent {
            id,
            handle: WorkspaceHandle(handle),
            project_id,
            mutation,
            expected_revision,
            cause,
            actor,
            pre_manifest,
            event,
        })
    }

    async fn intent_state(&self, id: &str) -> Result<Option<String>, WorkspaceError> {
        let document = self
            .pool
            .collection::<Document>("workspace_mutation_intents")
            .find_one(doc! {"_id": id})
            .await?;
        Ok(document
            .as_ref()
            .and_then(|document| document.get_str("state").ok())
            .map(str::to_owned))
    }

    async fn intent_revision(&self, id: &str) -> Result<Option<String>, WorkspaceError> {
        let document = self
            .pool
            .collection::<Document>("workspace_mutation_intents")
            .find_one(doc! {"_id": id})
            .await?;
        Ok(document
            .as_ref()
            .and_then(|document| document.get("revision_id").and_then(Bson::as_str))
            .map(str::to_owned))
    }

    async fn mark_file_mutation_attention(
        &self,
        id: &str,
        error: &str,
    ) -> Result<(), WorkspaceError> {
        let updated_at = now_utc_str();
        self.pool
            .collection::<Document>("workspace_mutation_intents")
            .update_one(
                doc! {"_id": id},
                doc! {
                    "$set": {
                        "state": "needs_attention",
                        "error": error,
                        "updated_at": &updated_at,
                    }
                },
            )
            .await?;
        Ok(())
    }

    async fn assert_guard_handle(
        &self,
        lock: &WorkspaceMutationGuard,
        handle: &WorkspaceHandle,
    ) -> Result<(), WorkspaceError> {
        let document = self
            .pool
            .collection::<Document>("workspace_copies")
            .find_one(doc! {"_id": handle.as_str()})
            .await?;
        let project_id = document
            .as_ref()
            .and_then(|document| document.get_str("project_id").ok())
            .map(str::to_owned);
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
        tx: &mut ClientSession,
        handle: &WorkspaceHandle,
    ) -> Result<(), WorkspaceError> {
        let document = self
            .pool
            .collection::<Document>("workspace_copies")
            .find_one(doc! {"_id": handle.as_str()})
            .session(&mut *tx)
            .await?;
        let project_id = document
            .as_ref()
            .and_then(|document| document.get_str("project_id").ok())
            .map(str::to_owned);
        if project_id.as_deref() != Some(lock.project_id.as_str()) {
            return Err(WorkspaceError::Internal(anyhow!(
                "mutation guard does not own workspace handle {}",
                handle.as_str()
            )));
        }
        Ok(())
    }
}

/// Resolve a validated relative path beneath the workspace root, rejecting any
/// symlink or reparse point along the way so a link planted inside the
/// workspace cannot redirect a read outside it.
async fn resolve_workspace_path(
    root: &Path,
    rel: &Path,
    raw_path: &str,
) -> Result<PathBuf, WorkspaceError> {
    let mut current = root.to_path_buf();
    for component in rel.components() {
        current.push(component.as_os_str());
        let meta = tokio::fs::symlink_metadata(&current)
            .await
            .map_err(|error| read_error(raw_path, error))?;
        if is_link_or_reparse(&meta) {
            return Err(WorkspaceError::PermissionDenied(
                current.to_string_lossy().into_owned(),
            ));
        }
    }
    Ok(current)
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
    use std::path::Path;

    use super::{read_error, resolve_workspace_path};
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

    #[tokio::test]
    async fn resolve_workspace_path_rejects_link_components() {
        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        let link = root.path().join("external");
        std::fs::write(outside.path().join("secret.txt"), b"outside").expect("outside file");

        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &link).expect("create directory link");
        #[cfg(windows)]
        {
            let result = std::process::Command::new("cmd")
                .args([
                    "/C",
                    "mklink",
                    "/J",
                    link.to_str().expect("link path"),
                    outside.path().to_str().expect("outside path"),
                ])
                .output()
                .expect("create directory junction");
            assert!(
                result.status.success(),
                "failed to create junction: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }

        let denied = resolve_workspace_path(root.path(), Path::new("external/secret.txt"), "external/secret.txt")
            .await
            .expect_err("link must be rejected");
        assert!(matches!(denied, WorkspaceError::PermissionDenied(_)));
    }
}
