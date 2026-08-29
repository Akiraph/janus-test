//! Content-revision identity, manifest collection, and revision advancement.
use super::*;
use futures_util::TryStreamExt;
use mongodb::ClientSession;

impl WorkspaceInterface {
    /// Read the current revision identity for any workspace copy.
    pub async fn current_revision(
        &self,
        handle: &WorkspaceHandle,
    ) -> Result<RevisionRef, WorkspaceError> {
        let document = self
            .pool
            .collection::<Document>("workspace_copies")
            .find_one(doc! {"_id": handle.as_str()})
            .await?;
        match document.as_ref().and_then(|document| {
            document
                .get("current_revision_id")
                .and_then(Bson::as_str)
                .map(str::to_owned)
        }) {
            Some(revision_id) => Ok(RevisionRef(revision_id)),
            None if document.is_some() => Err(WorkspaceError::Internal(anyhow!(
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
        let handles = handles
            .iter()
            .map(WorkspaceHandle::as_str)
            .collect::<Vec<_>>();
        let mut cursor = self
            .pool
            .collection::<Document>("workspace_copies")
            .find(doc! {"_id": {"$in": handles}})
            .await?;
        let mut revisions = HashMap::new();
        while let Some(document) = cursor.try_next().await? {
            let handle = document.get_str("_id")?.to_owned();
            if let Some(revision) = document
                .get("current_revision_id")
                .and_then(Bson::as_str)
            {
                revisions.insert(handle, RevisionRef(revision.to_owned()));
            }
        }
        Ok(revisions)
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
        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        let revision = self
            .advance_revision_in_tx(
                &mut session,
                handle,
                expected,
                cause,
                actor,
                manifest_root_hash.zip(snapshot_purpose),
            )
            .await?;
        session.commit_transaction().await?;
        Ok(revision)
    }

    pub(crate) async fn check_expected_revision_in_tx(
        &self,
        tx: &mut ClientSession,
        handle: &WorkspaceHandle,
        expected: Option<&RevisionRef>,
    ) -> Result<(), WorkspaceError> {
        let Some(expected) = expected else {
            return Ok(());
        };
        let document = self
            .pool
            .collection::<Document>("workspace_copies")
            .find_one(doc! {"_id": handle.as_str()})
            .session(&mut *tx)
            .await?;
        let current = document
            .ok_or(WorkspaceError::NotFound)?
            .get("current_revision_id")
            .and_then(Bson::as_str)
            .map(str::to_owned)
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
        tx: &mut ClientSession,
        handle: &WorkspaceHandle,
        expected: Option<&RevisionRef>,
        cause: &str,
        actor: serde_json::Value,
        snapshot: Option<(&str, &str)>,
    ) -> Result<RevisionRef, WorkspaceError> {
        let document = self
            .pool
            .collection::<Document>("workspace_copies")
            .find_one(doc! {"_id": handle.as_str()})
            .session(&mut *tx)
            .await?;
        let current = document
            .ok_or(WorkspaceError::NotFound)?
            .get("current_revision_id")
            .and_then(Bson::as_str)
            .map(str::to_owned)
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
        let max_sequence = self
            .pool
            .collection::<Document>("content_revisions")
            .find_one(doc! {"workspace_handle": handle.as_str()})
            .sort(doc! {"sequence": -1})
            .session(&mut *tx)
            .await?;
        let next_sequence = max_sequence
            .as_ref()
            .and_then(|document| document.get("sequence").and_then(Bson::as_i64))
            .unwrap_or(0)
            + 1;
        let revision_ref = RevisionRef::new(Uuid::now_v7());
        let copy_version = format!("v_{}", Uuid::now_v7());
        let actor_json = serde_json::to_string(&actor)?;

        let mut document = doc! {
            "_id": &revision_ref.0,
            "workspace_handle": handle.as_str(),
            "sequence": next_sequence,
            "cause": cause,
            "actor_json": &actor_json,
            "prev_revision_id": &current,
            "stable": 1i64,
            "occurred_at": &now,
        };
        if let Some((root, _)) = snapshot {
            document.insert("manifest_root_hash", root);
        }
        self.pool
            .collection::<Document>("content_revisions")
            .insert_one(document)
            .session(&mut *tx)
            .await?;
        if let Some((root, purpose)) = snapshot {
            let snapshot_id = Uuid::now_v7();
            self.pool
                .collection::<Document>("workspace_snapshots")
                .insert_one(doc! {
                    "snapshot_id": snapshot_id.to_string(),
                    "revision_id": &revision_ref.0,
                    "manifest_root_hash": root,
                    "purpose": purpose,
                    "integrity_state": "complete",
                    "created_at": &now,
                })
                .session(&mut *tx)
                .await?;
        }
        self.pool
            .collection::<Document>("workspace_copies")
            .update_one(
                doc! {"_id": handle.as_str()},
                doc! {
                    "$set": {
                        "current_revision_id": &revision_ref.0,
                        "version": &copy_version,
                        "updated_at": &now,
                    }
                },
            )
            .session(&mut *tx)
            .await?;
        Ok(revision_ref)
    }
}
