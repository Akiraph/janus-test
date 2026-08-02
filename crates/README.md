# Crate Boundaries

`crates/` contains technical foundations and capability modules that can be
compiled and tested along the dependency graph. A split moves ownership and
`interface.rs` first, then moves implementation. Cross-capability transactions,
scheduling, recovery, and composition remain in `apps/server/application/`.

## Current layout

| Crate | Current ownership | Dependency direction |
| --- | --- | --- |
| `janus-infrastructure` | Generic IDs, clocks, SQLite, transactions, public events, operation journals, and Blob storage | Generic technical libraries only |
| `janus-workspace` | Main/Session copies, content revisions, snapshots, manifests, diffs, and controlled file mutations | `infrastructure` |
| `janus-source-control` | Git errors, status/log values, update outcomes, and the `GitRunner` port | Generic serialization and standard future types |
| `janus-identity` | Single-owner passkeys, recovery grants, and authentication state | `infrastructure` plus WebAuthn implementation |
| `janus-models` | Provider credentials, model configuration, failover attempts, usage, and stream adapters | `infrastructure` |
| `janus-runtime` | Runtime configuration, lifecycle state, logs, tickets, and recovery projections | `infrastructure` |
| `janus-projects` | Project metadata, repository credentials, runtime policy, and Project Git projections | `infrastructure`, `runtime`, `source-control`, `workspace` |
| `janus-sessions` | Session, Turn, Message, timeline, checkpoint, upload, and attachment projections | `infrastructure`, `workspace` |
| `janus-execution` | Round, Tool Call, Ask, plan, context, and stream-diagnostic projections | `infrastructure`, `models`, `projects`, `runtime`, `sessions`, `workspace` |

`janus-source-control` is currently at the interface-migration stage. The Git
process adapter remains in server, and Projects temporarily owns the Git
projection and conflict tables. Moving those tables and transactions later
must preserve historical table names, event names, and migration-owner
normalization.

## Boundary rules

- A capability exposes public behavior only through its own `interface.rs`.
- A crate writes only tables it owns. Cross-capability writes are composed by an
  application workflow in a short transaction.
- Do not add generic repositories, service locators, or a global event bus for
  small amounts of reuse.
- Keep dependencies evidence-based. Remove an unused direct dependency before
  introducing a shared layer for a few lines of code.
- Do not suppress lint or boundary problems with blanket attributes. Fix the
  code, narrow the interface, or add a focused test.
- Applied SQLx migrations are immutable. Historical types and public event
  names remain compatible during extraction.

## Documentation rules

Each capability README records only maintenance-relevant context: mission,
observable behavior, invariants, boundaries, temporary compatibility
conditions, and design decisions. Comments explain behavior causes or external
constraints; they do not restate the code.

## Verification

```text
cargo fmt --all -- --check
cargo check -p janus-infrastructure
cargo check -p janus-workspace
cargo check -p janus-source-control
cargo run -p xtask -- check architecture
git diff --check
```

When server wiring or real file/SQLite behavior changes, also verify with the
compiled server, real SQLite, public HTTP/SSE, and `janus-test`.
