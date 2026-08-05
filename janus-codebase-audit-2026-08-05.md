# Janus Codebase Audit

- Updated: 2026-08-05
- Scope and exclusions: Full repository audit with emphasis on systemic failures and architecture; exclude `.git`, `target`, `node_modules`, `.janus-dev`, and generated build output.
- Environment and limitations: Read-only audit. Rust build, test, runtime, dependency, and public-surface execution checks are intentionally not run after the user requested no Rust commands during review.
- Mode: 系统
- Decisions: E3 / G4 / Q0 / C3 / D3; user authorizes a deepest read-only re-audit and evaluating rebuild/reset/breaking routes, but this audit does not modify source
- Report status: Complete

## Scope and baseline

- Request: Analyze the current Janus codebase, prioritizing emergent failures and architecture, with stability as the primary quality goal.
- Workspace: `E:/Janus`
- Git root: `E:/Janus/src`
- Baseline commit or branch: `main` at `11dcb87` (`2026-08-04`, `Akiraph checkpoint: optimize workspace and session performance`).
- Pre-existing dirty paths: `janus-codebase-audit-2026-08-05-claude.md` was already untracked; this report is kept separate and that file is preserved.
- Source unchanged after audit: Verified; only audit documents are untracked.

## Assessment note

Scores below are primarily static risk assessments. The independent Claude report was later read and its previously recorded build, test, Clippy, and runtime evidence is incorporated below; those commands were not rerun after the user's explicit no-Rust-command instruction.

## Finding summary

| Severity | Count | Confirmed | Suspected |
| --- | ---: | ---: | ---: |
| Critical | 1 | 1 | 0 |
| High | 12 | 12 | 0 |
| Medium | 13 | 12 | 1 |
| Low | 0 | 0 | 0 |
| Info | 3 | 3 | 0 |
| **Total** | 29 | 28 | 1 |

## Finding ledger

| ID | Severity | Status | Disposition | Finding | Location/evidence | Impact | Action or question |
| --- | --- | --- | --- | --- | --- | --- | --- |
| F-001 | Info | Confirmed | Open | Outer workspace has no root README. | `E:/Janus` listing; `src/README.md` exists. | New contributors and audit tooling must infer that `src` is the Git/Cargo root; this is documentation friction, not a runtime failure. | Decide whether the outer workspace is intentionally a wrapper; if yes, add a short pointer README or record the intentional omission. |
| F-002 | High | Confirmed | Open | Operation completion is not fenced by the worker lease or current operation state. | `crates/infrastructure/src/operations.rs:364-404` updates `operations` by `id` only; `apps/server/src/application/workers.rs:131-154` calls `complete_work`/`fail_work` with the work nonce but `record_operation_success/failure` at `:249-297` calls unguarded `finish`; startup marks stale Operations at `apps/server/src/main.rs:42-53`. | A stale worker can publish a terminal Operation result after a newer lease holder or startup recovery has changed the Operation. Clients can observe succeeded/failed for a work item whose authoritative attempt was interrupted or superseded. | Add the cheapest concurrency probe: force a lease to expire while the first handler is delayed, let a second handler finish, then let the first call `finish`; assert the stale call is rejected and the public Operation/event remains owned by the current attempt. Bound the eventual fix to nonce/version-guarded terminal transitions and a regression test. |
| F-003 | High | Confirmed | Open | Workspace propagation is not serialized against concurrent propagation or Turn start. | `apps/server/src/application/workspace_sync.rs:44-56` checks `get_session`/`ensure_session_idle` before calling `WorkspaceInterface::propagate`; `crates/workspace/src/interface.rs:867-1064` has no session lock or conditional version and performs filesystem transfer outside the database. | A message/Turn can start after the idle check while `sync/apply` mutates the same Session/Main trees. Two propagation requests can read one baseline and overwrite each other's files/cursors, producing lost edits or an incorrect conflict projection. | Run two concurrent public `POST /sessions/{id}/sync|apply` requests while delaying the transfer, plus a concurrent message post; assert one operation is rejected/serialized and revision/cursor state is coherent. |
| F-004 | High | Confirmed | Open | Workspace propagation has no durable multi-step recovery envelope and publishes events after the state mutation. | `crates/workspace/src/interface.rs:985-1056` copies files, updates the baseline (`:1022-1027`), records Session and Main revisions in separate transactions (`:1035-1051`), then updates the cursor and clears conflicts (`:1054-1056`); `apps/server/src/application/workspace_sync.rs:53-61` appends public events only after `propagate` returns. | A crash or SQLite failure between steps can leave filesystem, baseline, revision, cursor, conflict row, and public event history disagreeing. A crash before the event transaction commits gives observers no replayable notification for a durable change. | Inject failure after each boundary and restart against a temporary SQLite/data root; assert a single recovery path either completes or exposes a durable needs-attention state and emits/replays exactly one coherent public event. |
| F-005 | High | Confirmed | Open | Runtime monitor failures leave external work permanently non-terminal. | `crates/runtime/src/service.rs:453-457` finalizes a Job only inside `if let Ok(completion)`; the analogous Service monitor at `:572-578` and Terminal monitor at `:874-880` also discard `wait_*` errors. | A process can have exited while the adapter wait/read path fails; the durable Job/Service/Terminal remains running, a Turn can remain blocked on `waiting_for_job`, and periodic reconciliation has no terminal fact to consume. | Inject an executor `wait_job/wait_service/await_terminal_exit` error after process start; assert the row becomes lost/failed, logs close, public change event is emitted, and a waiting Turn reaches a deterministic outcome. |
| F-006 | High | Confirmed | Open | Readiness is marked successful after startup cleanup failures are only logged or ignored. | `apps/server/src/main.rs:39-55` warns on blob cleanup, converts `stale_running()` failure to an empty list with `unwrap_or_default()`, ignores each `finish()` result, then unconditionally calls `mark_recovery_complete()`. `/health/ready` trusts that flag at `apps/server/src/transport/http/handlers.rs:38-54`. | A degraded restart can advertise HTTP 200 while stale Operations remain ambiguous or crash leftovers remain. Load balancers and clients can immediately issue work against a control plane that has not completed its declared recovery contract. | Make recovery a typed result with explicit fail-closed policy; keep 503 until every required step is committed, or surface `needs_attention` without pretending cleanup succeeded. Add startup failure-injection and readiness assertions. |
| F-007 | High | Confirmed | Open | Failed Turns release the Session but do not advance queued successors. | `crates/sessions/src/types.rs:106-108` makes only `Completed` and `Canceled` advance the queue; `crates/sessions/src/execution.rs:1216-1325` settles `Failed` and clears `active_turn_id`; `apps/server/src/application/execution.rs:132-146` calls the queue promoter only when `after.status.advances_queue()`. | A deterministic provider failure or unexpected execution error leaves later `queued` Turns with no active owner and no scheduler trigger. New messages continue to queue behind the stranded Turn, so a single failed Turn can freeze a Session's conversation queue. | Decide whether `Failed` should advance FIFO or explicitly pause the queue. For the former, include `Failed` in one authoritative terminal-advance path and test failure with two queued messages through the public API. |
| F-008 | High | Confirmed | Open | Non-fatal work failures retry immediately and without a durable attempt/dead-letter bound. | `crates/infrastructure/src/operations.rs:285-297` clears the lease but does not move `not_before` or cap attempts; `apps/server/src/application/workers.rs:146-154` calls `fail_work(..., dead)` and `:249-297` classifies fatality by a few substrings. | A transient or unrecognised permanent error is reclaimed on the next 100 ms sweep, repeatedly consuming Tokio tasks and SQLite writes. Under load this creates a self-amplifying failure loop and can starve unrelated Operations. | Add persisted retry policy (`attempts`, backoff/jitter, next eligibility, max attempts, terminal `needs_attention`) and classify typed errors, not display strings. Probe a permanent external failure and assert bounded retries and stable readiness/DB load. |
| F-009 | Medium | Confirmed | Open | The durable worker has no global or per-kind concurrency budget for external work. | `apps/server/src/application/workers.rs:37-47` loops every 100 ms; `:131-154` spawns a Tokio task for every claimed clone/delete item. Only session lifecycle has a semaphore. | A burst of clone/project-delete work can launch unbounded external Git/filesystem tasks, increasing process count, disk contention and SQLite lock pressure until the failure loop above becomes more likely. | Introduce explicit per-kind and total permits with queue-visible saturation; measure process count, SQLite busy time and latency under a burst before choosing limits. |
| F-010 | High | Confirmed | Open | A recovered operation step in `running` state is re-executed without external-result reconciliation. | `crates/infrastructure/src/operations.rs:299-338` maps every existing non-`succeeded` step to `StepState::Running`; `:345-358` writes `external_ref` but no read path consumes it. `apps/server/src/application/lifecycle.rs:371-404` and `:479-532` then repeat filesystem/runtime side effects after lease expiry or crash. | A crash after an external side effect but before `complete_step` causes the retry to repeat a non-transactional action. This can delete/recreate resources twice, race a newer attempt, or make “exactly once” operation semantics depend on adapter idempotency. | Replace the binary running/succeeded shortcut with owner/lease-aware step state and an explicit reconciliation contract per external side effect. Test crash points before and after the external call. |
| F-011 | Medium | Confirmed | Open | File mutations can leave bytes changed while the optimistic revision remains unchanged. | `crates/workspace/src/interface.rs:703-827` performs filesystem mutation and full manifest scan before `advance_revision`; the source comment explicitly says a lost expected-revision race leaves bytes on disk without advancing identity. | Concurrent editor/tool writes can produce disk content invisible to revision consumers, making later conflict detection and cache invalidation observe an older identity than the actual tree. | Serialize per-workspace mutation or stage/rollback the filesystem change; make the revision CAS and filesystem outcome one recoverable command with a regression test for a forced version race. |
| F-012 | Medium | Confirmed | Open | Live model stream event persistence is best-effort and silently drops storage failures. | `crates/execution/src/interface.rs:341-430` calls `events.append(...).await` through `let _ =` for stream deltas and retry notifications. The model attempt and final Turn can succeed even when the public replay log has gaps. | SSE clients that reconnect from a cursor can miss token/retry diagnostics with no durable error marker, complicating UI convergence and incident reconstruction. | Treat event append failure as an observable delivery state: either fail the stream/turn or record a durable diagnostic and define which event types are intentionally lossy. Add cursor replay tests with injected event-store failures. |
| F-013 | Info | Confirmed | Open | No repository CI workflow was found in the tracked tree. | Inventory found no CI files; the documented checks are local `cargo xtask`/`janus-test` commands. | Formatting, lint, architecture, migration and real-protocol checks can be skipped by ordinary changes, allowing the systemic risks above to regress unnoticed. | Add a minimal protected pipeline with fmt/check/clippy/tests, architecture checks, generated-contract drift checks, and compiled-server SQLite/HTTP/SSE smoke tests. |
| F-014 | Critical | Confirmed | Open | Provider streams can be accepted as successful after an incomplete or truncated body. | `crates/models/src/stream.rs:203-227`, `:281-308`, and `:371-395` call the assembler `finish` after EOF; `crates/models/src/openai_chat.rs:156`, `:282-310`, `crates/models/src/openai_responses.rs:161`, `:183`, `:348`, and `crates/models/src/anthropic.rs:318-345` do not require the protocol terminal event before producing `Completed`. The README promises that a stream without a completion event is failed. The independent Claude review confirmed the missing terminal bit for all three protocols. | A socket close after partial text or partial tool arguments becomes a successful attempt; execution will not retry and may commit incomplete tool calls or final content. This turns transient network truncation into durable incorrect state. | Track a protocol-specific terminal marker (`[DONE]`, `message_stop`, `response.completed`) and fail EOF without it. Add one truncated-stream case per adapter and assert the retry classifier sees a failure. |
| F-015 | High | Confirmed | Open | Configured model failover is stored and read but is not consumed by the execution path. | `crates/models/src/interface.rs:315-414` owns `set_failover`/`failover`; `apps/server/tests/runtime_configuration.rs:265-315` only verifies configuration persistence. `crates/execution/src/interface.rs:887-915` explicitly runs one resolved primary candidate and calls `ModelsInterface::stream_completion_with` repeatedly without loading the configured candidates. | A primary provider outage exhausts in-place retries and parks the Turn instead of trying the configured fallback. Operators believe availability protection is enabled while the user-visible behavior is single-provider retry. | Make candidate order part of the resolved Turn/Round snapshot and run the bounded retry policy per candidate, recording candidate order and attempt type; alternatively remove the configuration surface until execution consumes it. |
| F-016 | Medium | Confirmed | Open | The architecture checker has false-green paths for module imports, table ownership, and public capability boundaries. | `tools/xtask/src/main.rs:298-347` searches only `modules::{name}::`, while production capability code imports `janus_*::interface`; `:349-389` extracts only Rust string literals, while SQL is assembled from constants such as `crates/runtime/src/service.rs:1544` and formatted queries at `:767-770`. `crates/models/src/lib.rs:2-7` also exports provider assembler/types modules publicly instead of keeping all provider-specific shapes behind the normalized interface. | A forbidden direct capability dependency or cross-module SQL access can pass the required architecture check, and callers can couple to provider-specific implementation modules. The repository's primary boundary guard is weaker than its passing result suggests. | Parse Rust imports/paths through the AST, inspect normalized SQL after constant expansion or require a checked query registry, and make provider adapters private unless they are explicitly part of the capability contract. Add deliberately violating fixtures that must fail. |
| F-017 | Medium | Confirmed | Open | The append-only public event log has no retention, compaction, or archival policy. | `crates/infrastructure/src/events.rs:85-158` only inserts, bounds, and reads `public_events`; repository search found no event deletion/retention path. | Model deltas and operational events grow the SQLite file without bound, increasing backup, index, replay, and recovery cost. A future retention change would also make old SSE cursors invalid without a defined archival contract. | Define a retention horizon and cursor contract, then implement bounded pruning/archival with metrics and an explicit `expired` policy for reconnecting clients. Keep audit-critical operation state outside the unbounded stream. |
| F-018 | Medium | Suspected | Open | Hot paths use full workspace scans and fixed-frequency polling without visible backpressure. | `crates/workspace/src/interface.rs:701-702` documents a full rescan after every mutation; manifest/hash work is repeated in propagation paths. `apps/server/src/application/workers.rs:37-47` polls every 100 ms and claims/spawns work, while runtime/ask reconciliation loops also use fixed intervals. | Under a large repository or burst of work, CPU, disk, Tokio task count, and SQLite contention can rise together. The feedback loop can amplify F-008 instead of merely adding latency. No benchmark was run, so the scale threshold is not yet quantified. | Measure scan cost, SQLite busy time, task count, and tail latency at realistic repository sizes. Then add dirty-path or snapshot-based reconciliation, bounded permits, and adaptive idle polling. |
| F-019 | High | Confirmed | Open | Turn scheduling is an in-memory wake after the database commit, with no durable runnable/requeue mechanism. | `apps/server/src/application/session_flow.rs:100-103` commits before calling `ExecutionCoordinator::schedule`; `apps/server/src/application/execution.rs:84-95` only spawns an in-memory task. On restart, `apps/server/src/application/lifecycle.rs:99-117` and `crates/sessions/src/execution.rs:189-252` convert active/waiting Turns to `interrupted` and release the Session; no recovery scan requeues the accepted Turn. | A crash between commit and spawn can leave an accepted Turn queued with no wake. A crash during execution turns work into an interrupted terminal state instead of deterministically requeueing or exposing a resumable command, so users must retry and queued work can remain stranded. | Introduce a durable execution wake/claim record or a startup/periodic scan that atomically promotes eligible queued/interrupted Turns. Make schedule idempotent and prove commit-crash, restart, and multi-instance cases. |
| F-020 | Medium | Confirmed | Open | Critical application and frontend boundary invariants are not covered by matching integration tests. | `apps/server/src/application/workers.rs:308-328` contains the only application-local test block found; `session_flow`, `execution`, and `workspace_sync` have no local test modules. Public-surface coverage at `apps/server/tests/public_surface.rs:96-154` covers one replay/cursor path, while `apps/web/package.json:7-16` has no standard frontend `test` script and `apps/server/tests/runtime_terminal.rs:2-5` explicitly excludes WebSocket transport. | The highest-risk paths can regress while capability/unit tests remain green. This is especially dangerous because the failure modes require timing, SQLite state, external adapters, browser/public protocol observation, and recovery together. | Add `janus-test`/compiled-server/browser scenarios for stale leases, crash boundaries, failed-Turn queue advancement, runtime wait errors, truncated model streams, recovery/readiness failure, SSE/WebSocket replay, and frontend state convergence. Keep focused unit tests for transition predicates. |
| F-021 | Medium | Confirmed | Open | Migration ownership enforcement combines legacy aliases and a filename-specific exception that contradicts the migration header. | `apps/server/migrations/0016_drop_system_prefix_version.sql:1` declares `runtime`, while `tools/xtask/src/main.rs:416-420` unconditionally adds `execution` for that filename; the checker also normalizes historical names such as `supervisor`/`workspace-sync` around `:484`, while `apps/server/migrations/0010_runtime_resilience.sql:1` retains older ownership vocabulary. | The checker can silently accept two owners for one migration and no longer has one source of truth for ownership. Future migrations may pass because a filename or alias is recognized rather than because the migration declares the intended owner and compatibility rule. | Do not edit the applied migration. Record the historical exception and its intended owner in a versioned compatibility map, reject any new implicit filename exception, and make the checker report header owner, normalized owner, and exception reason separately. |
| F-022 | Medium | Confirmed | Open | The Execution capability opens transactions that directly mutate Session-owned tables. | `crates/execution/src/interface.rs:82-128` injects `SessionsInterface` and a shared `UnitOfWork`; `:1286-1310` updates `tool_calls` and then calls `sessions.append_tool_result_in_tx`; `crates/sessions/src/execution.rs:967-995` writes `timeline_items`/message projections. | Cross-capability transaction ownership is hidden inside a capability. Session invariants and Execution settlement can evolve independently but still share one transaction, making rollback, event ordering, and recovery behavior difficult to reason about and violating the declared application boundary. | Move tool settlement orchestration into `apps/server/src/application`, expose capability-local state transitions and application-level commands, and keep each capability's table writes behind its own interface. |
| F-023 | High | Confirmed | Open | Projects and Workspace have duplicate Main Workspace mutation paths. | `crates/projects/README.md` says Workspace owns bytes/revisions and Project must not construct managed paths; nevertheless `crates/projects/src/interface.rs:1755-1813` writes/renames files under `main_repo_dir` and then commits a revision, with analogous direct moves/deletes at `:1831-1900`. `crates/workspace/src/interface.rs:703-827` independently implements mutation. | Two writers can apply different validation, locking, manifest, revision, and conflict rules to the same tree. A fix in Workspace does not protect Project editor routes, and concurrent Main/Session operations can produce divergent identities or lost edits. | Make Workspace the only Main/Session byte mutation owner. Keep Project responsible for readiness and error vocabulary, then route all editor mutations through the Workspace command with one concurrency/revision contract. |
| F-024 | Medium | Confirmed | Open | HTTP attachment handlers coordinate Blob and Session state with manual compensation. | `apps/server/src/transport/http/sessions.rs:377-413` writes the Blob before creating the Session attachment and attempts best-effort cleanup on failure; `:422-448` deletes the Session row before dropping the Blob reference and only warns if cleanup fails. | Protocol handlers own a cross-capability workflow, so partial failures can leave orphaned blobs or a deleted attachment whose storage reference remains. Retry behavior and cleanup durability depend on HTTP request completion instead of an application operation. | Move upload/delete into an application use case with durable Blob reference cleanup or operation work item; keep HTTP responsible only for parsing, authentication, and response mapping. |
| F-025 | Medium | Confirmed | Open | Startup recovery changes Runtime/Job/Service/Terminal rows without corresponding public events. | `crates/runtime/src/service.rs:1197-1251` updates uncertain resources in one transaction; `apps/server/src/application/lifecycle.rs:99-154` appends only Turn and Session recovery events. | Clients reconnecting from an SSE cursor can retain stale runtime status after a restart even though the database is already `lost`/`stopped_after_restart`. UI state, automation, and incident reconstruction disagree until another unrelated event arrives. | Add recovery events in the same transaction/outbox for every changed public resource, or explicitly define restart state as a snapshot-only transition and make clients refresh it on reconnect. |
| F-026 | Info | Confirmed | Open | The command named `check` mutates generated artifacts. | `tools/xtask/src/main.rs:124-144` calls `generate`; `:152-175` writes `generated/openapi.json` and invokes frontend `generate:types`, whose script uses `biome check --write` (`apps/web/package.json:14`). | Running a review/check command can dirty tracked generated files and hide whether the source or generator caused a diff. In a constrained or failed run, the generated contract may be partially updated. | Split `generate` and `check`: make check render to a temporary directory and compare, while only an explicit generate command writes tracked artifacts. Add a clean-worktree assertion to CI. |
| F-027 | Medium | Confirmed | Open | `AppState` remains a service-locator seam for capability access from transport and tests. | `apps/server/src/lib.rs:165-209` exposes public getters for operations, workspace, models, projects, runtime, and sessions; transport uses these directly in `apps/server/src/transport/http/handlers.rs`, `sessions.rs`, and `terminal.rs`. The application README calls this a compatibility surface. | New cross-capability behavior can bypass Application ordering, recovery, event, and idempotency rules. The current exception keeps the old architecture reachable indefinitely and makes it impossible to prove that HTTP is protocol conversion only. | Under the breaking cutover, remove capability getters from `AppState`; expose typed application commands and read projections, keeping only identity/authentication, event-stream, and composition-root concerns at server level. |
| F-028 | High | Confirmed | Open | The current workspace test gate is red. | The independent Claude report records `cargo test --workspace` stopping in `janus-execution --lib` with 15 passed and 2 failed; static inspection of `crates/execution/src/tools.rs:1699-1727` and `:2334-2358` shows the failing sanitizer expectations exercise overlapping host-path and environment redaction branches. The command was not rerun after the no-Rust-command instruction. | A clean baseline cannot prove that failure-path and tool-output behavior remain stable. Even if the failing assertions represent an outdated expected contract rather than a production defect, the repository has no green workspace regression gate. | Decide the sanitizer contract, fix the branch ordering or expectations, then require the complete workspace test gate to pass before control-plane changes proceed. |
| F-029 | Medium | Confirmed | Open | The Clippy gate is red in model tests because `unwrap_used` is denied. | The independent Claude report records `cargo clippy --workspace --all-targets -- -D warnings` failing on seven test-only `unwrap_used` sites in `crates/models/src/openai_responses.rs:445-522`; the command was not rerun after the no-Rust-command instruction. | A required quality gate is already failing, so new failure-handling changes can land without a trustworthy lint baseline and release automation cannot distinguish regressions from existing debt. | Replace the test unwraps with explicit assertions/results or add narrowly justified test-module allowances; do not weaken the production lint policy. |

## Baseline checks

| Check | Command/evidence | Result | Baseline failure? |
| --- | --- | --- | --- |
| Git root | `git rev-parse --show-toplevel` | `E:/Janus/src` | No |
| Git status | `git status --short` | Pre-existing untracked parallel report; no source changes observed before this report | No |
| Inventory | `inventory_codebase.py E:/Janus/src --format markdown --top 30 --large-file-lines 500 --max-marker-samples 0` | 276 tracked text files; 57,555 first-party production lines; 4,165 test lines; 12 test-file candidates; 6 TODO markers | No |
| Architecture | `cargo run -p xtask -- check architecture` | Exit 0 | No |

## Code size baseline

| Area/type | Files | Physical lines | Exclusions/notes |
| --- | ---: | ---: | --- |
| First-party production | 202 | 57,555 | Inventory heuristic; Rust, web, tools and configuration included by classification. |
| Tests | 12 | 4,165 | Inventory test-file candidates. |
| Generated/vendor candidates | 1 | 5,539 | Generated frontend API types; generated OpenAPI is tracked text but excluded from candidate source classification. |
| All candidate source | 215 | 67,259 | 276 tracked text files total, 82,681 physical text lines including docs/config/lock. |

Largest relevant production files are `crates/projects/src/interface.rs` (3,004 lines), `crates/execution/src/tools.rs` (2,387), `crates/execution/src/interface.rs` (2,305), `crates/runtime/src/service.rs` (1,835), `crates/sessions/src/execution.rs` (1,659), and `apps/server/src/adapters/runtime/local.rs` (1,632).

## Architecture facts

- Rust workspace has 12 packages: server, eight capability/infrastructure crates, `janus-test`, and `xtask`.
- Cargo manifests, module manifests, and source imports were inspected statically; no dependency graph command is being rerun under the user's no-Rust-command constraint.
- The architecture checker passed at exit 0, including module manifest, dependency, import, production table-access, migration-owner, and `janus-test` boundary checks.
- The root `E:/Janus` has no README; `src/README.md` and nearest directory READMEs are the available repository guidance.

## Architecture and ownership assessment

The intended architecture is coherent and worth preserving. The server is the composition root and owns the ordered migration set; `application/` is the cross-capability workflow boundary; capability crates expose narrow `interface.rs` surfaces and own their tables; infrastructure owns generic SQLite, operations, events, blobs, IDs, and clocks; HTTP/SSE/CLI are protocol adapters. The static architecture check passed, and no service locator or general-purpose event bus was found in the inspected tree.

The main architectural problem is not the crate graph. It is that the control plane has two competing sources of truth:

1. SQLite contains Turns, Operations, leases, revisions, and external references.
2. Tokio tasks, process monitors, filesystem trees, and in-memory `active_turns` carry the actual progress between transactions.

Those worlds are connected by unguarded callbacks, post-commit spawns, and best-effort cleanup. That is why the most severe findings are emergent: each local function looks plausible, but a crash, timeout, lease expiry, or concurrent request can make the durable state and the external state describe different attempts.

| Boundary | Intended owner | Static assessment |
| --- | --- | --- |
| Composition and deployment | `apps/server` | Correct direction; startup recovery/readiness currently treats partial cleanup as success (F-006). |
| Cross-capability sequencing | `apps/server/src/application` | Correct location; scheduling is in-memory and recovery is not durable (F-007, F-019). |
| Generic durable control state | `crates/infrastructure` | Useful primitives exist, but operation steps lack fencing and reconciliation semantics (F-002, F-008, F-010). |
| Model provider protocol | `crates/models` | Capability owns attempts and failover configuration, but stream terminality and failover execution are incomplete (F-014, F-015). |
| Workspace/filesystem state | `crates/workspace` | Intended owner is correct, but Projects still has direct Main Workspace writers; filesystem and SQLite revision/CAS are not one recoverable command (F-003, F-004, F-011, F-023). |
| Cross-capability transactions | `apps/server/src/application` | Intended owner is correct, but Execution and HTTP transport still coordinate Session/Blob writes directly (F-022, F-024). |
| Public observation | infrastructure EventStore + server SSE | Cursor/replay shape is sound, but some writes are lossy and the log is unbounded (F-012, F-017). |
| Governance checker | `tools/xtask` | Valuable guardrail, but its textual matching can report green while missing real imports/dynamic SQL, and `check` mutates generated artifacts (F-016, F-026). |

### Architecture decision

Do not rewrite every capability crate. Keep the ownership graph and public interface direction, but replace the application/infrastructure control-plane contract around four durable concepts: fenced claims, durable wakes, external-effect reconciliation, and explicit terminal-state transitions. This is the smallest architectural change that addresses the common cause of most High findings. A broad rewrite would recreate the same boundary bugs before the new state model is proven.

## Emergent failure chains

| Chain | Trigger | Durable outcome | User-visible symptom |
| --- | --- | --- | --- |
| Provider truncation | Upstream closes a stream after partial output | Adapter emits `Completed`; retry classifier sees success (F-014) | Partial answer or malformed tool call is accepted as final. |
| Provider outage | Primary returns transient errors | Same primary is retried; configured fallback is never selected (F-015) | Turn enters `waiting_for_model` even though a healthy fallback exists. |
| Failed Turn | Execution returns an unexpected/provider failure | Turn becomes `failed`, Session is released, successor promotion is skipped (F-007) | One failure can freeze later queued messages. |
| Lease expiry | First worker is slow or paused | Second worker claims work; first worker can still finish the Operation (F-002, F-010) | Stale success/failure overwrites the current attempt or repeats an external side effect. |
| Permanent external error | Handler returns an unrecognised nonfatal error | Work is immediately eligible again with no durable retry cap (F-008) | Tight retry loop increases SQLite/task/load pressure (F-009, F-018). |
| Commit then crash | Turn transaction commits before in-memory spawn | No durable wake exists; restart interrupts active work without requeue (F-019) | Accepted work disappears or requires manual retry. |
| Startup degradation | Cleanup/recovery step errors | Main logs the error and marks readiness complete (F-006) | Load balancer sends traffic to a control plane with unresolved state. |
| Workspace race | Sync/apply/message operations overlap | Filesystem, revisions, conflicts, and events can diverge (F-003, F-004, F-011, F-012) | Lost edits, stale conflict view, or missing replay event. |
| Restarted runtime | Recovery updates runtime rows without runtime events | Database says lost/stopped while SSE clients retain the previous status (F-025) | UI and automation disagree until an unrelated event or refresh occurs. |
| Attachment failure | Blob/Session request fails between manual steps | Reference cleanup is best-effort in the HTTP handler (F-024) | Orphaned storage and inconsistent attachment state accumulate over retries. |

The common invariant that is missing is: **a side effect must be owned by a durable attempt, and only that attempt may publish the terminal result**. Until this is explicit, adding retries or more reconciliation loops increases duplicate work rather than reliability.

## Stability lenses

Risk score: 1 is low risk, 5 is high risk. Security is intentionally deferred per the request.

| Lens | Risk | Evidence | Assessment |
| --- | ---: | --- | --- |
| Correctness under concurrency | 5 | F-002, F-003, F-004, F-010, F-011 | Multiple state machines cross process/SQLite/filesystem boundaries without one fenced owner. |
| Restart and recovery | 5 | F-005, F-006, F-019 | Recovery is present, but failures can be swallowed and accepted work is not durably requeued. |
| Queue and provider availability | 5 | F-007, F-008, F-014, F-015 | A single failure can be incorrectly successful, spin, park, or strand downstream work. |
| Architecture and maintainability | 4 | F-016, F-021, F-022, F-023; largest files over 1,600-3,000 lines | Ownership direction is good, but transaction and byte ownership still leak across boundaries. |
| Performance and scalability | 4 | F-009, F-017, F-018 | Unbounded work/event growth and full scans are visible; benchmark thresholds remain unknown. |
| Test and delivery confidence | 5 | F-013, F-020, F-026 | The riskiest cross-boundary behavior is not locked by compiled-server/public-protocol tests or CI, and the check command is not side-effect free. |
| Security | Deferred | Outside this audit priority | No security conclusion is made here. |

## Verification evidence

The following distinction matters: static inspection established the findings, but no runtime claim below should be read as an executed test result.

| Check | Result | Interpretation |
| --- | --- | --- |
| `cargo run -p xtask -- check architecture` | Exit 0, run before the explicit no-Rust-command instruction | Static checker accepts the current declared boundaries; F-016 shows why this is necessary but insufficient evidence. |
| `cargo fmt --all -- --check` | Exit 0, run before the explicit no-Rust-command instruction | Formatting baseline was clean at that point. |
| `cargo check --workspace --all-targets` | Aborted; no usable result | Not rerun because the user explicitly prohibited Rust commands during the audit. |
| `cargo test`, `cargo clippy`, compiled server, real SQLite/HTTP/SSE, `janus-test` | Not run | Explicitly deferred; dynamic race, restart, adapter, and public-contract behavior remain unverified. |
| Web typecheck/lint/build | Not run | Kept out of the review loop to avoid more local load; generated-contract drift is therefore unknown. |
| Static inventory and history | Completed | 276 tracked text files; 57,555 first-party production lines; 4,165 test lines; 12 test-file candidates; 6 TODO markers. |

## Change history and risk concentration

Recent commits show a fast, cross-cutting migration from the server monolith toward capability crates:

| Commit | Scope | Audit interpretation |
| --- | --- | --- |
| `11dcb87` | 94 files, +7,539/-1,183 | Performance/session checkpoint; current baseline still contains the control-plane races. |
| `3d26a7d` | 178 files, +10,868/-2,149 | Documentation/config cleanup plus capability movement; high regression surface. |
| `77e2e35` | 91 files, +1,817/-1,524 | Workspace capability extraction; F-003/F-004/F-011 are the remaining cross-boundary risk. |
| `f9ccb7e` | 80 files, +4,353/-9,281 | Large cleanup/restructure; needs stronger integration invariants than local compilation. |
| `ec8cb68` | 50 files, +3,174/-1,152 | Runtime/UX recovery changes; F-005/F-006 show recovery semantics are still permissive. |

This history supports a stabilization pause: finish the durable state model and contract tests before another broad extraction or performance checkpoint. The current architecture is refactorable, but the change surface is large enough that local green checks are not a reliable safety signal.

## Remediation decisions

### P0: stop incorrect state transitions

1. Make every Operation/step terminal transition conditional on a lease nonce or monotonic attempt epoch. Reject stale callbacks and surface the rejection as a normal reconciliation outcome (F-002).
2. Replace string-based fatal classification with typed retry policy: `attempt`, `next_eligible_at`, `max_attempts`, backoff/jitter, and terminal `needs_attention` (F-008).
3. Require provider-specific terminal events before emitting `Completed`; turn EOF without a terminal marker into a retryable failure (F-014).
4. Make runtime wait/read errors terminally visible as `lost`/`failed`, and keep readiness closed until required recovery commits (F-005, F-006).
5. Define one queue rule for `failed` and `interrupted` Turns. For stability, the recommended default is to advance the queue after a terminal failure while retaining the failed Turn as an explicit diagnostic; if product semantics require pausing, persist that pause and make it visible rather than silently stranding successors (F-007).

### P1: make progress crash-safe

1. Add a durable execution wake/claim record or an atomic startup/periodic scan for eligible Turns. Scheduling after commit may remain an optimization, but it cannot be the only wake path (F-019).
2. Store external-effect intent, attempt epoch, adapter idempotency key, and reconciliation status for clone/delete/runtime operations. Never map every non-success step to a fresh `Running` attempt (F-010).
3. Serialize workspace mutation/propagation per handle, or stage filesystem changes behind a recoverable command log. Do not allow a failed revision CAS to leave untracked bytes (F-003, F-004, F-011).
4. Wire model failover into the execution candidate loop, or remove/hide the configuration endpoint until it is real. The current silent configuration is worse than an explicit unsupported state (F-015).
5. Enforce one Main/Session byte writer in Workspace and move Project editor routes behind it; do not preserve the duplicate direct-file path for compatibility (F-023).
6. Move Blob/Session attachment orchestration and tool settlement out of HTTP/Execution into application use cases, with durable cleanup/transaction ownership (F-022, F-024).
7. Emit restart recovery events for every public runtime resource in the same durable recovery boundary, or force a snapshot refresh on reconnect (F-025).

### P2: bound growth and make governance executable

1. Add total/per-kind worker permits and adaptive idle polling; measure before tuning (F-009, F-018).
2. Define public event retention/archival and preserve only the required operational audit outside the stream (F-017).
3. Strengthen `xtask` with AST imports, normalized SQL/checked query ownership, and an explicit legacy migration exception file (F-016, F-021).
4. Add compiled-server and `janus-test` scenarios for every P0/P1 invariant, then protect them in CI (F-013, F-020).
5. Split generated-output writing from `xtask check`; check should compare temporary output and leave the worktree unchanged (F-026).

### Explicit non-decision

Do not spend the first stabilization pass on broad naming cleanup, generic repository abstractions, service locators, or a full capability rewrite. Those changes do not remove the shared failure cause and would enlarge the regression surface. Security is also intentionally outside this report's conclusion.

## Next probes when Rust execution is allowed

These are the smallest experiments that will convert the remaining dynamic unknowns into facts:

1. Delay a worker handler past lease expiry; finish both owners; assert only the current nonce changes the Operation and event.
2. Kill the process after a Turn commit and before the coordinator spawn; restart; assert the Turn is discovered and resumed/requeued exactly once.
3. Close each provider stream before its terminal event; assert failed attempt, bounded retry, no malformed tool call, and no successful final Turn.
4. Configure a failing primary and healthy fallback; assert candidate order, attempt rows, and final success.
5. Inject runtime wait errors and startup cleanup errors; assert terminal rows and HTTP readiness remain truthful.
6. Run concurrent workspace sync/apply/message operations with a forced revision race; assert no bytes survive without a revision identity.
7. Generate a burst of clone/delete work and a large repository; measure task count, SQLite busy time, scan duration, and event database growth.

## G4/D3/C3/E3 re-audit addendum

This section records the initial static-only G4/D3 hypothesis: rebuild the application/control plane in a new crate and cut over to a fresh data root. It is retained as decision history, but it is superseded as the primary recommendation by the independent Claude evidence and the combined decision below.

### Re-audit verdict

The repository contains enough recoverable domain knowledge for a controlled rebuild. The following are explicit and reusable: Owner-only identity, Project/Git concepts, Main/Session Workspace semantics, Content Revision rules, Session/Turn/Timeline terminology, Model Attempt semantics, Runtime resource states, Operation/Event cursor contracts, migration history, public routes, and the failure scenarios documented above. That makes a full product rewrite unnecessary.

The current application orchestration is not a trusted source of business knowledge. It has high fan-out, leaks Session writes through Execution, leaves transport paths with capability getters, relies on in-memory scheduling, and mixes external effects with incomplete durable recovery. Reusing that orchestration would carry the central failure mode into the new system.

| Candidate | Decision | Reason |
| --- | --- | --- |
| Preserve and incrementally refactor `apps/server/src/application` | Reject | It leaves the old seam, getters, scheduler, and transaction ownership reachable; the regression surface is larger than the apparent code change. |
| Rebuild the control plane and keep capability packages | **Choose** | Retains explicit domain knowledge and adapters while replacing the untrusted state machine, scheduler, recovery, and cross-capability orchestration. |
| Rewrite every crate and every public surface | Reject | Provider, Git, filesystem, runtime, and timeline behavior are not fully specified by tests; a total rewrite would discard valuable edge-case knowledge without improving the core decision. |

### Previously proposed package decision (superseded)

Create `crates/application` with package name `janus-application`. This is now preferable to another module under server because G4/C3 make the application seam a real replacement seam rather than a folder move.

Target dependency direction:

```text
janus-infrastructure
        ^
        +-- capability crates (owned tables, narrow interfaces)
        ^                         ^
        +-------- janus-application
                              ^
                         janus-server
                       (migrations, adapters,
                        deployment, transport)
```

`janus-application` must not depend on Axum, OpenAPI, CLI code, server config, or server migrations. It receives capability interfaces, the generic UnitOfWork, clocks, and external adapters through `ApplicationDependencies`. Its only public seam is `interface.rs` plus typed projections and command results.

Private modules inside the crate should be:

- `scheduler`: durable work inbox, claims, lease epochs, wake coalescing;
- `recovery`: startup recovery, expired claims, reconciliation, readiness result;
- `turns`: Turn queue state machine and successor activation;
- `operations`: typed retry policy, operation steps, external-effect reconciliation;
- `workspace`: serialized mutation/propagation commands and mutation journal;
- `attachments`: staged Blob plus Session attachment workflow;
- `events`: transactional state events and bounded progress publication;
- `tool_runner`: model, Runtime, Workspace, Project adapters invoked by an Execution-owned tool definition.

The old `apps/server/src/application` implementation is removed after cutover. It is not retained as a compatibility facade. Capability getters are removed from `AppState`; transport receives application commands and projections, with only authentication, event-stream transport, and composition-root concerns remaining in server.

### Capability reset

The rebuild also simplifies the crate graph. Capability crates may retain their historical table names and public event names, but cross-capability orchestration is removed from their implementations:

- Execution owns Round/Tool/Ask/context state transitions, not Model/Session/Runtime/Workspace writes.
- Projects owns metadata, Git policy, and Git projections, not Main Workspace bytes or revision mutation.
- Sessions owns Session/Turn/Message/Timeline state, not Workspace copy lifecycle or external execution.
- Workspace is the sole byte/revision/propagation owner.
- Models owns provider protocol, candidate plans, Attempts, usage, and private stream adapters.
- Runtime owns resource projections and executor interactions; Application decides Turn consequences.

The intended shape is a star: capabilities expose deep Interfaces to `janus-application`; capabilities do not call each other's workflow methods. This removes the current Execution fan-out and makes ownership enforceable by the crate graph rather than by comments and `module.toml` inspection.

### Durable model to rebuild

Do not preserve the old control rows as if they were trustworthy. Keep historical table names where the repository contract requires them, but rebuild their semantics through forward migrations and new constraints:

```text
accepted intent
  -> durable work item
  -> claim(epoch, nonce, lease_until)
  -> external effect intent
  -> observed/reconciled result
  -> terminal state + successor wake + public event
```

Every terminal update must match the current epoch and nonce. Every retry must have a typed policy and a finite terminal outcome. Every external effect must have a stable idempotency key and an observable reconciliation path. In-memory task state is only a performance cache.

Turns use the same durable inbox as other application work. A committed Turn always creates a wake. On restart, expired claims are reconciled and safe work is requeued. `Failed` advances the queue; `Interrupted` is either requeued or becomes `needs_attention` according to whether an external side effect can be proven safe to repeat.

Workspace mutations use a durable journal rather than pretending filesystem and SQLite are one transaction. A failed revision CAS cannot leave bytes with no identity. Blob uploads use staged objects and a durable finalize/cleanup step; HTTP never performs compensation directly.

### D3 data strategy

The primary cutover is a clean data-root reset, not an in-place v1-to-v2 compatibility migration:

1. Stop the old server and archive the old data root unchanged.
2. Export or commit any user Git work that must survive; uncommitted managed work is not implicitly preserved.
3. Start a new data root with the rebuilt schema and application state model.
4. Re-import/re-clone Projects and reconfigure credentials if the reset scope includes secrets.
5. Treat the archived root as a read-only recovery artifact, not as a live fallback database.

Applied SQLx migrations remain immutable. Historical names, event names, and owner normalization remain present for repository history; new migrations can rebuild current tables, add claim/journal constraints, or mark obsolete projections. The new binary refuses an old live data root unless an explicit export/reset command has completed.

Rollback is binary/data-root rollback only: before the new system writes, switch back to the archived root and old binary; after new writes, do not attempt to downgrade the new database in place. This is the deliberate C3/D3 tradeoff.

### Hard cutover and deletion list

There is no permanent dual-write phase. The cutover removes these old paths:

- `ExecutionInterface` cross-capability Session transactions;
- direct Projects filesystem writes;
- public `AppState` capability getters;
- in-memory Turn scheduling as an authority;
- unfenced Operation `finish`;
- substring-based fatal retry classification;
- unconditional provider assembler completion at EOF;
- HTTP-level Blob compensation;
- startup readiness after ignored recovery errors;
- public provider assembler modules;
- mutating `xtask check` behavior.

Public HTTP may be released as a new major generated contract at the cutover. Do not preserve v1 merely for internal callers. Preserve historical event names only when their semantics remain identical; changed payload/state semantics receive a new event version. The web client and `janus-test` move in the same cutover.

### Rebuild stages

1. Freeze feature work and write the new state machines, domain glossary, event catalog, and migration/reset contract.
2. Build `janus-application` against the existing capability Interfaces, but implement durable claims, recovery, and outbox behavior first.
3. Shrink Execution, Projects, and Sessions to capability-local writes; move orchestration into the new crate.
4. Replace Workspace mutation/propagation and Blob attachment flows with journaled application commands.
5. Replace model stream termination/failover and Runtime monitor/recovery semantics.
6. Cut over the new server against a fresh data root and public contract.
7. Delete the old server application, getters, compatibility paths, and stale migrations/checker exceptions after the new system has passed the public fault matrix.

The required acceptance gate is not compilation alone. The rebuilt system must pass real SQLite/server/HTTP/SSE/WebSocket/`janus-test` scenarios for stale claims, crash-after-commit, crash-after-effect, provider truncation, failover, runtime wait errors, workspace races, attachment cleanup, readiness failure, and event replay. Those checks remain unexecuted in this review because the no-Rust-command restriction is still active.

## Combined decision after Claude audit

The independent report materially changes the decision. It recorded a passing architecture check, passing workspace compilation, all 11 server integration suites passing individually, 46 remaining crate tests passing, and working SSE cursor resume across restart. Those are strong evidence that the crate graph, capability ownership model, UnitOfWork commit-before-publish rule, single in-process coordinator, lease nonces, and operation idempotency are valuable existing assets.

It also recorded two hard quality failures: the workspace test gate stops at two `janus-execution` library test failures, and Clippy fails on seven denied test unwraps in `janus-models`. These commands were not rerun after the user's no-Rust-command instruction. They are release-baseline evidence from the independent report, not a new local execution result.

One Claude finding is rejected as stale: `rust-toolchain.toml` currently pins channel `1.97.0`, so the claim that it is unpinned `stable` is not applicable to this checkout. The reported cancellation pool-starvation finding is also not promoted to confirmed: the inspected cancellation loop performs individual capability calls and does not hold the `settle_cancel` UnitOfWork transaction across process-kill awaits; it remains an instrumentation probe, not a rewrite driver.

### Selected route

Do not create `janus-application` yet and do not reset the data root. A new crate would mostly relocate a currently useful application seam while adding composition and migration coupling; D3 would discard recoverable user state without evidence that the database is corrupt. The correct deepest decision is a bounded G4 repair of the control plane inside the existing application boundary, with C3 hardening of ownership and E3 failure verification:

1. Create a private `apps/server/src/application/control_plane/` module (or equivalent internal module set), retaining `ExecutionCoordinator`'s single-owner wake coalescing but moving durable scheduler, recovery, retry, readiness, and external-effect reconciliation behind one narrow application seam.
2. Make every committed runnable Turn create a durable wake/claim; in-memory scheduling remains only an optimization. On restart, reconcile expired claims and requeue only work whose external effect is safe to repeat.
3. Fence every operation, step, Turn, and external-effect terminal transition by epoch plus nonce. Replace substring fatality checks with typed error codes, bounded attempts, backoff/jitter, `not_before`, and a visible terminal `needs_attention` state.
4. Make stream completion protocol-aware across Anthropic, OpenAI Chat, and OpenAI Responses. EOF without `message_stop`, `[DONE]`, or `response.completed` is a retryable truncation failure, never `Completed`. Wire configured failover into the candidate loop or remove the dead configuration surface.
5. Make Workspace the only Main/Session byte and revision writer. Serialize propagation per Session, detect Git head movement before committing the baseline, and use a journal/recovery envelope for filesystem transfer plus revision/cursor updates. Move Project editor mutations and Blob attachment coordination behind application commands.
6. Remove transport access to capability getters after command/read-projection migration. Keep capability crates, historical table names, migration files, and public event names where their semantics remain valid; do not preserve bypass paths as compatibility facades.
7. Rebuild the verification gate, not the proven domain model: add failure-injection doubles for truncated provider sockets, dead runtime processes, transient Git errors, crash-after-propagation boundaries, stale leases, and restart recovery. Require one green gate covering formatting, Clippy, workspace tests, architecture, generated-contract drift, compiled server, real SQLite, HTTP/SSE/WebSocket, and `janus-test`.

### What G4/D3/C3/E3 means here

G4 applies to the failure semantics and proof system, not to every crate. D3 is reserved for a separately approved data-loss event or a future major schema replacement after an export/import contract exists; it is not justified by this audit. C3 still applies internally: remove direct capability getters and duplicate writers once the application commands are ready, while preserving public HTTP compatibility unless a deliberate contract correction is required. E3 is mandatory before claiming stability: the fault matrix must exercise crash timing, truncation, retry exhaustion, recovery, concurrent propagation, and stale callbacks against a compiled server and real SQLite.

The old `janus-application`/fresh-data-root plan is therefore a fallback option only: use it if implementation proves that the existing application seam cannot be made authoritative without retaining incompatible state machines, or if a separate export/import exercise establishes that the current data root is not trustworthy. Current evidence does not establish either condition.

## Final audit status

Combined audit complete with 29 findings: 1 Critical, 12 High, 13 Medium, and 3 Info; 28 are confirmed and F-018 remains suspected pending benchmarks. Claude's independent dynamic evidence is incorporated but was not rerun after the no-Rust-command instruction. Source code was not changed; only the two audit markdown files are untracked.

## Unknowns and limits

- Root `E:/Janus` has no README; the source-level README and nearest capability READMEs were used instead.
- No Rust command was run after the user's explicit instruction; the aborted workspace check has no usable pass/fail result.
- The intended product policy for failed/interrupted queue advancement and event retention needs an explicit decision before implementation.
- Security, deployment topology beyond the single-owner model, and real-world workload thresholds were not assessed.
