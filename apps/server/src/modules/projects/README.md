# projects

Owns Projects, Main Workspace handle, GitHub PAT credentials, user Git
operations, and Git Update Conflicts; it does not own Session workspaces, the
three-way merge algorithm body (delegates to system `git`), or Content Revision
storage (that lives in workspace-sync).

Tables owned (migration `0004_projects_git.sql`): `projects`,
`github_credentials`, `project_git_state`, `git_update_conflicts`,
`git_update_conflict_paths`.

Events published: `project.changed`, `project.main_revision_changed`,
`git.state_changed`, `git.update_conflict_changed`. See `docs/08-events-and-errors.md`.

Allowed Module dependency: `workspace-sync` (for Main Workspace handle and
atomic content writes). PATs reuse the M1 `SecretCipher` pipeline.

Test entry: `cargo test --workspace` integration tests + `janus-test` public
Project/Git commands (see `docs/07-http-api.md`).
