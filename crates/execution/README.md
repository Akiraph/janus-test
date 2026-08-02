# Execution

## Mission

Execution owns Round, Tool Call, Ask, plan, context, and stream-diagnostic
projections. Its public behavior is exposed through `interface.rs`; the server
application coordinates model selection, lifecycle recovery, and durable work
around it.

## Observable behavior

- A Turn can create ordered Rounds and accepted Tool Calls, then settle them
  only through the stored status and ownership checks.
- Provider failures are classified into retryable and deterministic faults.
  Retry decisions stay bounded and can park a Turn for an explicit model retry.
- Tool execution validates workspace paths and Runtime capability before it
  performs file, command, job, service, or delegated-CLI work.
- Context versions and compact summaries are append-oriented projections. A
  new context record does not rewrite prior context history.
- Restart and user cancellation close or mark Execution-owned rows; the server
  application composes that work with Session and Runtime cleanup in one
  transaction.

## Boundaries

`janus-execution` writes only `rounds`, `tool_calls`, `asks`, `plan_versions`,
`compact_summaries`, `context_versions`, and `stream_diagnostics`. The server
owns migration order and must preserve their historical table and event names.

Models, Projects, Runtime, Sessions, and Workspace are injected capability
interfaces. Execution does not own their tables. `ExecutionCoordinator`,
cross-capability deletion, scheduling, and recovery remain in
`apps/server/application/`.

## Design decision

The tool registry and process-output helper moved with the Execution boundary;
the generic Bash discovery and output decoder live in infrastructure because
the Runtime process adapter also uses them. The server retains only a
compatibility re-export for existing adapter imports.
