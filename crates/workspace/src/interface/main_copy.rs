//! Main workspace copy lifecycle.
use super::*;
use futures_util::TryStreamExt;

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

        let existing = self
            .pool
            .collection::<Document>("workspace_copies")
            .find_one(doc! {"_id": handle.as_str()})
            .await?;
        if let Some(revision_id) = existing
            .as_ref()
            .and_then(|document| document.get("current_revision_id").and_then(Bson::as_str))
        {
            return Ok(RevisionRef(revision_id.to_owned()));
        }

        let copy_version = format!("v_{}", Uuid::now_v7());
        let revision_ref = RevisionRef::new(Uuid::now_v7());
        let actor_json = serde_json::to_string(&actor)?;

        let mut session = self.pool.client().start_session().await?;
        session.start_transaction().await?;
        // INSERT OR IGNORE: a copy created concurrently wins; the upsert leaves
        // its row untouched but the revision below still commits independently.
        self.pool
            .collection::<Document>("workspace_copies")
            .update_one(
                doc! {"_id": handle.as_str()},
                doc! {
                    "$setOnInsert": {
                        "project_id": &project_id,
                        "kind": "main",
                        "managed_dir": managed_dir,
                        "current_revision_id": &revision_ref.0,
                        "observation_generation": 0i64,
                        "dirty": 0i64,
                        "version": &copy_version,
                        "created_at": &now,
                        "updated_at": &now,
                    }
                },
            )
            .upsert(true)
            .session(&mut session)
            .await?;
        self.pool
            .collection::<Document>("content_revisions")
            .insert_one(doc! {
                "_id": &revision_ref.0,
                "workspace_handle": handle.as_str(),
                "sequence": 1i64,
                "cause": cause,
                "actor_json": &actor_json,
                "stable": 1i64,
                "occurred_at": &now,
            })
            .session(&mut session)
            .await?;
        session.commit_transaction().await?;
        Ok(revision_ref)
    }

    /// Remove Main clone directories that exist without a registered Main
    /// copy. This covers a crash after `git clone` and before the first
    /// Workspace revision transaction commits.
    pub async fn recover_orphan_main_worktrees(&self) -> Result<usize, WorkspaceError> {
        let mut cursor = self
            .pool
            .collection::<Document>("workspace_copies")
            .find(doc! {"kind": "main"})
            .await?;
        let mut registered = BTreeSet::new();
        while let Some(document) = cursor.try_next().await? {
            if let Ok(managed_dir) = document.get_str("managed_dir") {
                if let Some(name) = Path::new(managed_dir).parent().and_then(Path::file_name) {
                    registered.insert(name.to_string_lossy().to_string());
                }
            }
        }
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
        let document = self
            .pool
            .collection::<Document>("workspace_copies")
            .find_one(doc! {"_id": handle.as_str()})
            .await?;
        let managed_dir = document
            .as_ref()
            .and_then(|document| document.get_str("managed_dir").ok().map(str::to_owned))
            .ok_or(WorkspaceError::NotFound)?;
        Ok(managed_dir)
    }
}
