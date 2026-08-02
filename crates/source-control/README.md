# Source Control

## Mission

`janus-source-control` owns the narrow contract for Git operations. It defines
normalized errors, Git projections, update outcomes, conflict path values, and
the object-safe `GitRunner` port.

## Observable behavior

- `GitRunner` accepts repository paths and returns boxed futures, so the server
  can inject the system process adapter or a deterministic test double.
- `GitError::code` preserves the public `GIT_*` error vocabulary at the crate
  boundary.
- `UpdateOutcome::Conflict` reports paths and tree identities without writing
  conflict rows or deciding how a workflow continues.

## Boundaries

This crate does not depend on Projects, SQLite, migrations, HTTP, operations,
or a process launcher. The server adapter implements `GitRunner`; the current
Projects capability still owns Git projections and conflict transactions while
the next extraction moves those tables behind this interface.

## Design decisions

Git protocol values stay independent of project identifiers and database row
types. `DiffView::args` is public because only the process adapter should turn
the protocol choice into command arguments. Credentials are passed as values;
the adapter owns the short-lived `GIT_ASKPASS` handling.
