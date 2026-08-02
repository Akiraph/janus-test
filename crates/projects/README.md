# Projects

## Mission

Projects owns project metadata, repository credentials, runtime policy, and the
Project-owned Git state projection. Its public surface is `interface.rs`; the
server composes it with the Git adapter and durable operation worker.

## Observable behavior

- Clone, update, push, and delete use the stable operation kinds
  `project.clone`, `git.update`, `git.push`, and their related values. Intent is
  recorded before background work runs, so restart recovery does not rely on an
  in-memory task.
- Main Workspace file reads and directory listings keep the Project readiness
  check and Project error vocabulary, then delegate path validation and bytes
  to `janus-workspace`.
- Main editor mutations advance the Workspace revision and append the
  `project.main_revision_changed` event in the same short database transaction.
  The filesystem write occurs before that transaction; a revision mismatch can
  therefore leave the latest bytes on disk without advancing the identity.

## Boundaries

`janus-projects` writes only `projects`, `github_credentials`,
`project_git_state`, `git_update_conflicts`, `git_update_conflict_paths`,
`project_runtime_configs`, `project_runtime_secrets`, `project_egress_rules`,
and `project_cli_configs`. The server still owns the ordered SQLx migrations;
the historical table names and event names must remain unchanged.

`janus-workspace` owns file bytes, copy lifecycle, revisions, manifests, diffs,
and path safety. `janus-source-control` owns the Git port; the concrete process
adapter is injected by the server. Cross-module cleanup, scheduling, and
recovery belong to `application/`.

Do not add new Project helpers that read Workspace tables, construct managed
paths, or shell out to Git. Add the capability operation at its owner and keep
Project responsible for readiness, metadata, and its own event projection.

## Design decision

The existing Project HTTP routes remain stable while their Workspace reads are
delegated. This keeps the public API and error mapping compatible while
removing duplicate file metadata, tree traversal, and content-read logic from
the Projects capability.
