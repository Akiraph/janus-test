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
- Main editor mutations first commit a durable Workspace mutation intent, then
  apply the filesystem effect outside a database write transaction, and finally
  advance the revision plus append `project.main_revision_changed` in one short
  transaction. Restart recovery replays only when affected paths still match
  the recorded pre/post state; an unexpected edit becomes explicit attention.

## Boundaries

`janus-projects` writes only `projects`, `github_credentials`,
`project_git_state`, `git_update_conflicts`, and `git_update_conflict_paths`.
The server still owns the ordered SQLx migrations;
the historical table names and event names must remain unchanged.

`janus-workspace` owns Main file bytes, revisions, and manifests,
and path safety. `janus-source-control` owns the Git port; the concrete process
adapter is injected by the server. Cross-module cleanup, scheduling, and
recovery belong to `application/`.

Do not add new Project helpers that read Workspace tables, construct managed
paths, or shell out to Git. Add the capability operation at its owner and keep
Project responsible for readiness, metadata, and its own event projection.

## Design decision

Project HTTP routes translate the public protocol while Workspace owns Main
file bytes, revisions, tree traversal, and content reads. The Projects
capability remains responsible for readiness, metadata, and its own event
projection.
