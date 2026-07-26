# workspace-sync

Owns physical workspace copies (`workspace_copies`), immutable Content Revision
identity (`content_revisions`), Session Merkle snapshots (`workspace_snapshots`),
and bidirectional propagation cursor rows (`propagation_links`). It does **not**
own user Git operations, Session messages, or Apply/Sync execution (M5).

## M3 ownership

| Kind | Names |
| --- | --- |
| Tables | `workspace_copies`, `content_revisions` (migration `0005`); `workspace_snapshots`, `propagation_links` (migration `0006_workspace_sync_session.sql`) |
| Events | `session.revision_changed`, `workspace.diff_changed` |
| IDs | `RevisionId` (platform), `SnapshotId` (snapshot rows) |

## Dependencies

Allowed Module dependency: `projects` (restricted Main queries for copy source /
diff base).

## Notes

- Session directory: `data/workspaces/sessions/<session-id>/repo/`.
- Handle: `session:<session-id>` (symmetric to Main `main:<project-id>`).
- M3 fills `content_revisions.manifest_root_hash` on Session create and each
  managed tool write; M2 left it NULL for Main-only copies.
- Diff summary is read-only; Apply/Sync buttons stay disabled until M5.
