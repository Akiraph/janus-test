//! Manifest diffs, Apply/Sync propagation, and conflict persistence.
use super::*;

impl WorkspaceInterface {
    /// Path-level Diff summary of Session current tree vs Main current tree.
    pub async fn diff_summary(
        &self,
        session_id: impl Display,
    ) -> Result<DiffSummary, WorkspaceError> {
        let session_id = session_id.to_string();
        let roots = self.copy_roots(&session_id).await?;
        let _lock = self.lock_project(&roots.project_id).await;
        if let Some(intent_json) = self.pending_propagation_intent(&session_id).await? {
            let _ = self
                .recover_propagation_locked(&session_id, &roots, &intent_json)
                .await?;
        }
        let baseline = self
            .ensure_propagation_base(&session_id)
            .await?;
        let mut summary = diff_working_trees(&roots.session_dir, &roots.main_dir)
            .await
            .map_err(WorkspaceError::Internal)?;
        let diff_paths = summary
            .paths
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>();
        let status = self
            .propagation_status(&session_id, &roots, &baseline.manifest(), &diff_paths)
            .await?;
        summary.sync_enabled = status.sync_enabled;
        summary.apply_enabled = status.apply_enabled;
        summary.pending_conflict = status.pending_conflict;
        Ok(summary)
    }

    /// Propagate one workspace side into the other without creating a Git
    /// commit. A three-way preflight uses the last synchronized filesystem
    /// snapshot so unrelated changes are copied while same-path edits surface
    /// as one structured conflict.
    pub async fn propagate(
        &self,
        session_id: impl Display,
        direction: PropagationDirection,
        actor: serde_json::Value,
    ) -> Result<PropagationResult, PropagationError> {
        let session_id = session_id.to_string();
        let roots = self.copy_roots(&session_id).await?;
        let _lock = self.lock_project(&roots.project_id).await;
        if let Some(intent_json) = self.pending_propagation_intent(&session_id).await?
            && let Some(conflict) = self
                .recover_propagation_locked(&session_id, &roots, &intent_json)
                .await?
        {
            return Err(PropagationError::Conflict(conflict));
        }
        let baseline = self
            .ensure_propagation_base(&session_id)
            .await?;
        let previous_manifest = baseline.manifest();
        let (main_result, session_result) = tokio::join!(
            refresh_manifest(
                &roots.main_dir,
                &previous_manifest,
                baseline.main_head.as_deref(),
            ),
            refresh_manifest(
                &roots.session_dir,
                &previous_manifest,
                baseline.session_head.as_deref(),
            ),
        );
        let (main, main_head) = main_result?;
        let (session, session_head) = session_result?;
        let pending = self.pending_conflict(&session_id).await?;

        let mut paths: BTreeSet<String> = previous_manifest
            .nodes
            .keys()
            .chain(main.nodes.keys())
            .chain(session.nodes.keys())
            .filter(|path| {
                !is_workspace_internal_path(path)
                    && baseline
                        .nodes
                        .get(*path)
                        .or_else(|| main.nodes.get(*path))
                        .or_else(|| session.nodes.get(*path))
                        .is_some_and(|node| node.kind == NodeKind::File)
            })
            .cloned()
            .collect();
        if let Some(conflict) = &pending {
            paths.extend(
                conflict
                    .paths
                    .iter()
                    .filter(|path| !is_workspace_internal_path(&path.path))
                    .map(|path| path.path.clone()),
            );
        }

        let pending_paths: BTreeMap<String, &PropagationConflictPath> = pending
            .as_ref()
            .map(|conflict| {
                conflict
                    .paths
                    .iter()
                    .filter(|path| !is_workspace_internal_path(&path.path))
                    .map(|path| (path.path.clone(), path))
                    .collect()
            })
            .unwrap_or_default();
        let mut transfer_paths = BTreeSet::new();
        let mut conflict_paths = Vec::new();

        for path in paths {
            validate_workspace_path(&path).map_err(WorkspaceError::InvalidPath)?;
            let base = previous_manifest.nodes.get(&path);
            let main_node = main.nodes.get(&path);
            let session_node = session.nodes.get(&path);
            let main_changed = !same_node(main_node, base);
            let session_changed = !same_node(session_node, base);
            let sides_match = same_node(main_node, session_node);

            if direction == PropagationDirection::Apply
                && pending_paths.contains_key(&path)
                && pending_path_resolved(pending_paths[&path], main_node, session_node)
            {
                transfer_paths.insert(path.clone());
                continue;
            }

            match direction {
                PropagationDirection::Sync if main_changed => {
                    if session_changed && !sides_match {
                        conflict_paths.push(conflict_path(&path, base, main_node, session_node));
                    } else {
                        if !sides_match {
                            transfer_paths.insert(path);
                        }
                    }
                }
                PropagationDirection::Apply if session_changed => {
                    if main_changed && !sides_match {
                        conflict_paths.push(conflict_path(&path, base, main_node, session_node));
                    } else {
                        if !sides_match {
                            transfer_paths.insert(path);
                        }
                    }
                }
                _ => {}
            }
        }

        if !conflict_paths.is_empty() {
            let conflict = PropagationConflict {
                direction,
                paths: conflict_paths,
            };
            self.store_pending_conflict(&session_id, &roots, &conflict)
                .await?;
            return Err(PropagationError::Conflict(conflict));
        }

        let transfer_path_list = transfer_paths.iter().cloned().collect::<Vec<_>>();
        let source_manifest = match direction {
            PropagationDirection::Sync => &main,
            PropagationDirection::Apply => &session,
        };
        let target_manifest = match direction {
            PropagationDirection::Sync => &session,
            PropagationDirection::Apply => &main,
        };
        let source_preimage = transfer_path_list
            .iter()
            .map(|path| (path.clone(), source_manifest.nodes.get(path).cloned()))
            .collect();
        let target_preimage = transfer_path_list
            .iter()
            .map(|path| (path.clone(), target_manifest.nodes.get(path).cloned()))
            .collect();
        self.store_propagation_intent(
            &session_id,
            &PropagationIntent {
                direction,
                actor: actor.clone(),
                baseline: baseline.clone(),
                main_head: main_head.clone(),
                session_head: session_head.clone(),
                paths: transfer_path_list.clone(),
                source_preimage,
                target_preimage,
            },
        )
        .await?;

        if !transfer_paths.is_empty() {
            let source = match direction {
                PropagationDirection::Sync => roots.main_dir.clone(),
                PropagationDirection::Apply => roots.session_dir.clone(),
            };
            let target = match direction {
                PropagationDirection::Sync => roots.session_dir.clone(),
                PropagationDirection::Apply => roots.main_dir.clone(),
            };
            let transfer_paths_for_copy = transfer_path_list.clone();
            tokio::task::spawn_blocking(move || {
                propagate_paths(&source, &target, &transfer_paths_for_copy)
            })
            .await
            .map_err(|error| WorkspaceError::Internal(anyhow!(error.to_string())))?
            .map_err(WorkspaceError::Internal)?;
        }

        let (session_after, main_after) = if transfer_paths.is_empty() {
            (session, main)
        } else {
            let transfer_path_list = transfer_paths.iter().cloned().collect::<Vec<_>>();
            match direction {
                PropagationDirection::Sync => (
                    rehash_working_tree_paths(&roots.session_dir, &session, &transfer_path_list)
                        .await
                        .map_err(WorkspaceError::Internal)?,
                    main,
                ),
                PropagationDirection::Apply => (
                    session,
                    rehash_working_tree_paths(&roots.main_dir, &main, &transfer_path_list)
                        .await
                        .map_err(WorkspaceError::Internal)?,
                ),
            }
        };
        let next_manifest =
            merge_propagation_baseline(&previous_manifest, &main_after, &session_after);
        let next_baseline =
            PropagationBaseline::from_manifest(next_manifest, Some(main_head), Some(session_head));
        let (session_revision, main_revision) = self
            .finalize_propagation(PropagationFinalizeRequest {
                session_id: &session_id,
                roots: &roots,
                direction,
                next_baseline: &next_baseline,
                session_after: &session_after,
                main_after: &main_after,
                actor: &actor,
                transfer_paths: &transfer_path_list,
            })
            .await?;

        Ok(PropagationResult {
            direction,
            changed_paths: transfer_paths.into_iter().collect(),
            session_revision: session_revision.0,
            main_revision: main_revision.0,
        })
    }

    /// Replay propagation intents that were durably recorded before a process
    /// restart. Copying the same paths is idempotent; finalization reuses an
    /// existing revision with the same manifest root instead of allocating a
    /// second identity.
    pub async fn recover_uncertain_propagations(&self) -> Result<usize, WorkspaceError> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT session_id, recovery_intent_json FROM propagation_links \
             WHERE recovery_state = 'transferring' AND recovery_intent_json IS NOT NULL \
             ORDER BY updated_at, session_id",
        )
        .fetch_all(&self.pool)
        .await?;
        for (session_id, intent_json) in &rows {
            let roots = self.copy_roots(session_id).await?;
            let _lock = self.lock_project(&roots.project_id).await;
            let _ = self
                .recover_propagation_locked(session_id, &roots, intent_json)
                .await?;
        }
        Ok(rows.len())
    }

    async fn pending_propagation_intent(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, WorkspaceError> {
        sqlx::query_scalar(
            "SELECT recovery_intent_json FROM propagation_links \
             WHERE session_id = ? AND recovery_state = 'transferring'",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(WorkspaceError::Storage)
    }

    async fn store_propagation_intent(
        &self,
        session_id: &str,
        intent: &PropagationIntent,
    ) -> Result<(), WorkspaceError> {
        let now = now_utc_str();
        let intent_json = serde_json::to_string(intent)?;
        let result = sqlx::query(
            "UPDATE propagation_links SET recovery_state = 'transferring', \
             recovery_intent_json = ?, recovery_error = NULL, version = ?, updated_at = ? \
             WHERE session_id = ?",
        )
        .bind(intent_json)
        .bind(format!("v_{}", Uuid::now_v7()))
        .bind(now)
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(WorkspaceError::NotFound);
        }
        Ok(())
    }

    async fn clear_propagation_intent(&self, session_id: &str) -> Result<(), WorkspaceError> {
        let result = sqlx::query(
            "UPDATE propagation_links SET recovery_state = 'idle', \
             recovery_intent_json = NULL, recovery_error = NULL, version = ?, updated_at = ? \
             WHERE session_id = ?",
        )
        .bind(format!("v_{}", Uuid::now_v7()))
        .bind(now_utc_str())
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(WorkspaceError::NotFound);
        }
        Ok(())
    }

    async fn clear_propagation_intent_with_error(
        &self,
        session_id: &str,
        error: &str,
    ) -> Result<(), WorkspaceError> {
        let result = sqlx::query(
            "UPDATE propagation_links SET recovery_state = 'idle', \
             recovery_intent_json = NULL, recovery_error = ?, version = ?, updated_at = ? \
             WHERE session_id = ?",
        )
        .bind(error)
        .bind(format!("v_{}", Uuid::now_v7()))
        .bind(now_utc_str())
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(WorkspaceError::NotFound);
        }
        Ok(())
    }

    async fn mark_propagation_recovery_error(
        &self,
        session_id: &str,
        error: &str,
    ) -> Result<(), WorkspaceError> {
        let result = sqlx::query(
            "UPDATE propagation_links SET recovery_error = ?, version = ?, updated_at = ? \
             WHERE session_id = ? AND recovery_state = 'transferring'",
        )
        .bind(error)
        .bind(format!("v_{}", Uuid::now_v7()))
        .bind(now_utc_str())
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(WorkspaceError::NotFound);
        }
        Ok(())
    }

    async fn recover_propagation_locked(
        &self,
        session_id: &str,
        roots: &CopyRoots,
        intent_json: &str,
    ) -> Result<Option<PropagationConflict>, WorkspaceError> {
        let intent: PropagationIntent = serde_json::from_str(intent_json)?;
        let (main_current, session_current) = tokio::join!(
            hash_working_tree(&roots.main_dir),
            hash_working_tree(&roots.session_dir),
        );
        let main_current = main_current.map_err(WorkspaceError::Internal)?;
        let session_current = session_current.map_err(WorkspaceError::Internal)?;
        if intent.paths.len() != intent.source_preimage.len()
            || intent.paths.len() != intent.target_preimage.len()
        {
            self.mark_propagation_recovery_error(
                session_id,
                "propagation intent has no complete source/target preimage",
            )
            .await?;
            return Err(WorkspaceError::Internal(anyhow!(
                "propagation intent for {session_id} needs attention"
            )));
        }
        let source_current = match intent.direction {
            PropagationDirection::Sync => &main_current,
            PropagationDirection::Apply => &session_current,
        };
        let target_current = match intent.direction {
            PropagationDirection::Sync => &session_current,
            PropagationDirection::Apply => &main_current,
        };
        let changed_paths = intent
            .paths
            .iter()
            .filter(|path| {
                !same_node(
                    source_current.nodes.get(*path),
                    intent.source_preimage.get(*path).and_then(Option::as_ref),
                ) || !same_node(
                    target_current.nodes.get(*path),
                    intent.target_preimage.get(*path).and_then(Option::as_ref),
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        if !changed_paths.is_empty() {
            let conflict = PropagationConflict {
                direction: intent.direction,
                paths: changed_paths
                    .iter()
                    .map(|path| {
                        conflict_path(
                            path,
                            intent.baseline.nodes.get(path),
                            main_current.nodes.get(path),
                            session_current.nodes.get(path),
                        )
                    })
                    .collect(),
            };
            self.store_pending_conflict(session_id, roots, &conflict)
                .await?;
            self.clear_propagation_intent_with_error(
                session_id,
                "propagation recovery stopped because a source or target path changed",
            )
            .await?;
            return Ok(Some(conflict));
        }
        let source = match intent.direction {
            PropagationDirection::Sync => roots.main_dir.clone(),
            PropagationDirection::Apply => roots.session_dir.clone(),
        };
        let target = match intent.direction {
            PropagationDirection::Sync => roots.session_dir.clone(),
            PropagationDirection::Apply => roots.main_dir.clone(),
        };
        if !intent.paths.is_empty() {
            let paths = intent.paths.clone();
            tokio::task::spawn_blocking(move || propagate_paths(&source, &target, &paths))
                .await
                .map_err(|error| WorkspaceError::Internal(anyhow!(error.to_string())))?
                .map_err(WorkspaceError::Internal)?;
        }
        let (main_result, session_result) = tokio::join!(
            hash_working_tree(&roots.main_dir),
            hash_working_tree(&roots.session_dir),
        );
        let main_after = main_result.map_err(WorkspaceError::Internal)?;
        let session_after = session_result.map_err(WorkspaceError::Internal)?;
        let next_manifest =
            merge_propagation_baseline(&intent.baseline.manifest(), &main_after, &session_after);
        let next_baseline = PropagationBaseline::from_manifest(
            next_manifest,
            Some(intent.main_head),
            Some(intent.session_head),
        );
        self.finalize_propagation(PropagationFinalizeRequest {
            session_id,
            roots,
            direction: intent.direction,
            next_baseline: &next_baseline,
            session_after: &session_after,
            main_after: &main_after,
            actor: &intent.actor,
            transfer_paths: &intent.paths,
        })
        .await?;
        Ok(None)
    }

    async fn finalize_propagation(
        &self,
        request: PropagationFinalizeRequest<'_>,
    ) -> Result<(RevisionRef, RevisionRef), WorkspaceError> {
        self.store_propagation_baseline(request.session_id, request.next_baseline)
            .await?;

        let (session_revision, main_revision) = if request.transfer_paths.is_empty() {
            (
                self.current_revision(&request.roots.session_handle).await?,
                self.current_revision(&request.roots.main_handle).await?,
            )
        } else {
            let session_revision = self
                .record_manifest_revision_if_needed(
                    &request.roots.session_handle,
                    &request.session_after.root_hash,
                    request.actor.clone(),
                )
                .await?;
            let main_revision = self
                .record_manifest_revision_if_needed(
                    &request.roots.main_handle,
                    &request.main_after.root_hash,
                    request.actor.clone(),
                )
                .await?;
            (session_revision, main_revision)
        };

        self.update_propagation_cursor(
            request.session_id,
            request.direction,
            &session_revision,
            &main_revision,
        )
        .await?;
        self.clear_pending_conflict(request.session_id).await?;
        self.clear_propagation_intent(request.session_id).await?;
        Ok((session_revision, main_revision))
    }

    async fn copy_roots(&self, session_id: &str) -> Result<CopyRoots, WorkspaceError> {
        let session_handle = WorkspaceHandle::session(session_id);
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT session.managed_dir, main.managed_dir, session.project_id \
             FROM workspace_copies AS session \
             JOIN workspace_copies AS main \
               ON main.project_id = session.project_id AND main.kind = 'main' \
             WHERE session.handle = ?",
        )
        .bind(session_handle.as_str())
        .fetch_optional(&self.pool)
        .await?;
        let (session_dir, main_dir, project_id) = row.ok_or(WorkspaceError::NotFound)?;
        Ok(CopyRoots {
            session_handle,
            main_handle: WorkspaceHandle::main(&project_id),
            project_id: project_id.clone(),
            session_dir: self.data_root.join(session_dir),
            main_dir: self.data_root.join(main_dir),
        })
    }

    async fn ensure_propagation_base(&self, session_id: &str) -> Result<PropagationBaseline, WorkspaceError> {
        let stored: Option<Option<String>> = sqlx::query_scalar(
            "SELECT baseline_manifest_json FROM propagation_links WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(Some(json)) = stored else {
            return Err(WorkspaceError::NotFound);
        };
        Ok(serde_json::from_str(&json)?)
    }

    async fn store_propagation_baseline(
        &self,
        session_id: &str,
        baseline: &PropagationBaseline,
    ) -> Result<(), WorkspaceError> {
        let now = now_utc_str();
        let version = format!("v_{}", Uuid::now_v7());
        let json = serde_json::to_string(baseline)?;
        let result = sqlx::query(
            "UPDATE propagation_links SET baseline_manifest_json = ?, version = ?, updated_at = ? WHERE session_id = ?",
        )
        .bind(json)
        .bind(version)
        .bind(now)
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(WorkspaceError::NotFound);
        }
        Ok(())
    }

    async fn propagation_status(
        &self,
        session_id: &str,
        roots: &CopyRoots,
        baseline: &ManifestRoot,
        diff_paths: &[&str],
    ) -> Result<PropagationStatus, WorkspaceError> {
        let mut sync_enabled = false;
        let mut apply_enabled = false;
        for path in diff_paths {
            let base = baseline.nodes.get(*path);
            let main_node = read_manifest_node(&roots.main_dir, path)
                .await
                .map_err(WorkspaceError::Internal)?;
            let session_node = read_manifest_node(&roots.session_dir, path)
                .await
                .map_err(WorkspaceError::Internal)?;
            if same_node(main_node.as_ref(), session_node.as_ref()) {
                continue;
            }
            if !same_node(main_node.as_ref(), base) {
                sync_enabled = true;
            }
            if !same_node(session_node.as_ref(), base) {
                apply_enabled = true;
            }
        }
        Ok(PropagationStatus {
            sync_enabled,
            apply_enabled,
            pending_conflict: self.pending_conflict(session_id).await?,
        })
    }

    async fn pending_conflict(
        &self,
        session_id: &str,
    ) -> Result<Option<PropagationConflict>, WorkspaceError> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT direction, paths_json FROM workspace_propagation_conflicts WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((direction, paths_json)) = row else {
            return Ok(None);
        };
        let direction = match direction.as_str() {
            "sync" => PropagationDirection::Sync,
            "apply" => PropagationDirection::Apply,
            other => {
                return Err(WorkspaceError::Internal(anyhow!(
                    "unknown propagation direction {other}"
                )));
            }
        };
        let paths = serde_json::from_str(&paths_json)?;
        Ok(Some(PropagationConflict { direction, paths }))
    }

    async fn store_pending_conflict(
        &self,
        session_id: &str,
        roots: &CopyRoots,
        conflict: &PropagationConflict,
    ) -> Result<(), WorkspaceError> {
        let session_revision = self.current_revision(&roots.session_handle).await?;
        let main_revision = self.current_revision(&roots.main_handle).await?;
        let now = now_utc_str();
        sqlx::query(
            "INSERT INTO workspace_propagation_conflicts \
             (session_id, direction, session_revision_id, main_revision_id, paths_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(session_id) DO UPDATE SET direction = excluded.direction, \
             session_revision_id = excluded.session_revision_id, main_revision_id = excluded.main_revision_id, \
             paths_json = excluded.paths_json, updated_at = excluded.updated_at",
        )
        .bind(session_id)
        .bind(conflict.direction.as_str())
        .bind(session_revision.0)
        .bind(main_revision.0)
        .bind(serde_json::to_string(&conflict.paths)?)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn clear_pending_conflict(&self, session_id: &str) -> Result<(), WorkspaceError> {
        sqlx::query("DELETE FROM workspace_propagation_conflicts WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn update_propagation_cursor(
        &self,
        session_id: &str,
        direction: PropagationDirection,
        session_revision: &RevisionRef,
        main_revision: &RevisionRef,
    ) -> Result<(), WorkspaceError> {
        let now = now_utc_str();
        let version = format!("v_{}", Uuid::now_v7());
        let result = match direction {
            PropagationDirection::Sync => {
                sqlx::query(
                    "UPDATE propagation_links SET main_to_session_cursor_revision_id = ?, version = ?, updated_at = ? WHERE session_id = ?",
                )
                .bind(&main_revision.0)
                .bind(&version)
                .bind(&now)
                .bind(session_id)
                .execute(&self.pool)
                .await?
            }
            PropagationDirection::Apply => {
                sqlx::query(
                    "UPDATE propagation_links SET session_to_main_cursor_revision_id = ?, version = ?, updated_at = ? WHERE session_id = ?",
                )
                .bind(&session_revision.0)
                .bind(&version)
                .bind(&now)
                .bind(session_id)
                .execute(&self.pool)
                .await?
            }
        };
        if result.rows_affected() == 0 {
            return Err(WorkspaceError::NotFound);
        }
        Ok(())
    }

}

