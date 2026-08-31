# Execution

## Mission

Execution owns Round, Tool Call, plan, context, and stream-diagnostic
projections. Its public behavior is exposed through `interface.rs`; the server
application coordinates model selection, lifecycle recovery, and durable work
around it.

## Observable behavior

- A Turn can create ordered Rounds and accepted Tool Calls, then settle them
  only through the stored status and ownership checks.
- Provider failures are classified into retryable and deterministic faults.
  Retry decisions continue in the runner with a low-frequency backoff after
  repeated reconnects.
- Tool execution validates workspace paths and Runtime capability before it
  performs file, command, or async-task work.
- Context versions and compact summaries are append-oriented projections. A
  new context record does not rewrite prior context history.
- Restart and user cancellation close or mark Execution-owned rows; the server
  application composes that work with Session and Runtime cleanup in one
  transaction.

## Boundaries

`janus-execution` writes only `rounds`, `tool_calls`, `plan_versions`,
`compact_summaries`, and `context_versions`. Schema is the Rust catalog
under the no-compat convention; see `CLAUDE.md`.

Models, Projects, Runtime, Sessions, and Workspace are injected capability
interfaces. Execution does not own their tables. `ExecutionCoordinator`,
cross-capability deletion, scheduling, and recovery remain in
`apps/server/application/`.

## Design decision

The tool registry and process-output helper live with the Execution boundary;
the generic Bash discovery and output decoder live in infrastructure because
the Runtime process adapter also uses them. The server only wires the concrete
adapters and application workflows.
