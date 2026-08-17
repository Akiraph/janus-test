# Runtime

## Mission

`janus-runtime` owns Runtime, async Bash tasks, Terminal, log streams, access
tickets, and restart recovery for external process resources. It
exposes a durable lifecycle interface and an injected `RuntimeExecutor` port.

## Observable behavior

- Runtime rows are written before external process work and move through
  explicit starting/ready/stopping/stopped or failed states.
- Async Bash tasks retain executor nonces, logs, exit summaries, and usage;
  stale or uncertain resources are marked lost during recovery rather than
  guessed complete.
- Terminal tickets are short-lived, hashed, origin-bound, and consumed once.
  Log reads respect cursor boundaries and retention markers, including UTF-8
  boundaries after truncation.

## Invariants

- This crate writes only `runtimes`, `log_streams`, `async_tasks`, `terminals`, and
  `runtime_access_tickets`.
- The executor is an external-side-effect port. Durable state transitions and
  public events remain in this crate; process creation, waiting, and signal
  behavior are supplied by the server adapter.
- Runtime does not decide Turn completion, read model credentials, or mutate
  Main workspace content. Application workflows coordinate those actions.

## Boundaries

The server composition root supplies the ordered migration set, database,
event store, data root, and executor. Project callers use Runtime commands and
projections; they do not read these tables directly. The deployment has one
Local executor and one Project-scoped runtime; there is no container,
network-policy, or capability-probe layer.

## Design decisions

Log persistence is part of Runtime because process output and retention share
the same lifecycle and recovery rules. The adapter receives log stream IDs and
uses the Runtime-owned store at composition time; the capability remains the
owner of log rows and cursor semantics.
