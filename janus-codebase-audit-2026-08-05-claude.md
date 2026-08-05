# Janus Codebase Audit (Claude 独立视角)

- Updated: 2026-08-05
- Scope and exclusions: Full repository audit with emphasis on emergent/systemic failures and architecture; exclude `.git`, `target`, `node_modules`, `.janus-dev` (except as evidence), and generated build output.
- Mode: 系统
- Decisions: E3 / G4 / Q3 / C3 / D3 (deepest inspection; compatibility constraints removed for evaluation — but **read-only**: judgments and rebuild/repair verdicts only, no source modified)
- Report status: Complete (read-only, E3 pass added)
- Independence note: This audit ran in parallel with another agent's report (`janus-codebase-audit-2026-08-05.md`). Per the user's instruction, the two reports deliberately did not read each other's findings files, so overlapping findings indicate independent confirmation and divergent findings indicate complementary coverage. Where the other report's existence overlapped (F-C01-F-C05), findings were kept but all downstream evidence was gathered independently.

## Scope and baseline

- Request: 以涌现性故障与架构为重点、以提升系统稳定性(不触发意料之外的问题)为目标的全面代码库审计;安全维度降级;其他维度也要覆盖。
- Workspace: `E:/Janus`
- Git root: `E:/Janus/src`
- Baseline commit: `11dcb8733d9e62f5f0bb7e8bdf96d8c7f5d6961e` (`main`, 2026-08-04, "checkpoint: optimize workspace and session performance")
- Pre-existing dirty paths: none (clean tree before this report; the only untracked paths are the two audit reports)
- Source unchanged after audit: yes — `git status --short` shows only the two audit markdown files; no source file was modified

## Environment and commands

| Check | Command | Exit | Evidence |
| --- | --- | ---: | --- |
| Architecture gate | `cargo run -p xtask -- check architecture` | 0 | pass |
| Format gate | `cargo fmt --all -- --check` | 0 | pass |
| Compile | `cargo check --workspace --all-targets` | 0 | 2m04s, pass |
| Unit tests (lib targets) | `cargo test --workspace` | **101** | stops at first failed target: `janus-execution --lib`, 15 passed / **2 failed** |
| Integration tests (11 suites) | `cargo test -p janus-server --test <name>` each | 0 | all 11 pass individually (41 tests), 0.04s-15s each |
| Remaining crate tests | `cargo test -p janus-{infrastructure,sessions,runtime,models,workspace,projects,identity,source-control}` | 0 | 46 tests pass |
| Clippy | `cargo clippy --workspace --all-targets -- -D warnings` | **101** | 7 `unwrap_used` deny-errors in `janus-models` test code |
| Runtime logs | `.janus-dev/*.log`, `.janus-dev/janus.db` | — | 43 MB DB after 11 days; SSE resume works across restarts (cursor 21879) |

## Architecture and core flows

12-package Cargo workspace. Dependency direction is clean and machine-enforced: `infrastructure` (ID/clock/SQLite/UoW/public events/operations/blob) → 8 capability crates (`identity, models, projects, source-control, runtime, sessions, execution, workspace`) each exposing only `interface.rs` → `apps/server` composition root (`application/` cross-capability workflows + `adapters/` external effects + `transport/http` protocol conversion). `xtask check architecture` (exit 0) enforces: table single-ownership, event single-publisher, acyclic module graph, interface-only imports, migration owner headers, and `janus-test` boundary.

Core flows traced (owner → durable state → external effect → observer):
- **Session message → model execution**: HTTP → `session_flow::post_session_message` (one UoW tx: route/activate/queue + events) → commit → `ExecutionCoordinator::schedule` (in-memory HashSet claim) → `execute_turn` loop (per-round tx insert → external model stream OUTSIDE tx → accept/settle in tx) → SSE.
- **Job settlement**: Runtime broadcast → `spawn_job_wake` → `settle_job` (one tx: record result + reconcile blockers) → commit → schedule. Fallback: 500 ms `reconcile_waiting_jobs` sweep.
- **Cancel**: `cancel_active_turn` (accept → bound-cancel jobs outside tx → `settle_cancel` one tx) → commit → schedule.
- **Startup recovery**: `AppState::initialize` → `recover_execution_state` (one tx: interrupt runtimes/attempts/rounds/turns) → blob cleanup + stale-operation sweep → `ready=200`.
- **Workspace propagation**: `propagate_session_workspace` (idle check) → `workspace.propagate` (hash both trees, copy, store filesystem baseline, record revisions) → events.

## Seven lenses

### Orientation — Assessed (static + gates + runtime logs)
Clean enforced dependency graph; ownership metadata real; `module.toml` + interface.rs pattern holds. Finding F-C16 (manifest specs/tests fields decorative).

### Maintainability — Assessed
Hotspots: `projects/interface.rs` 3,004 lines, `execution/tools.rs` 2,387, `execution/interface.rs` 2,305. Zero `unwrap()` in production paths (all hits are `unwrap_or*` defaults); `expect`/`panic!` only in tests. Findings F-C10 (checker bypass pattern), F-C11 (hard-coded migration exception), F-C12 (unpinned toolchain).

### Refactoring readiness — Assessed
Architecture checker gives a real safety net for the in-flight crate extraction (workspace extracted 08-02, projects/sessions pending). But the net has holes: F-C10 (const-SQL bypass), F-C15 (sessions/projects have zero unit tests), F-C19 + F-C21 (two of three quality gates red, so the net is currently down).

### Stability — Assessed (primary lens)
The coordination design is genuinely good: commit-before-publish is enforced structurally by `UnitOfWork` (notify only after commit), all wake-ups funnel through one coordinator, duplicate wakes are coalesced, leases use nonces, idempotency reuses operation_id. The systemic risks that remain are: F-C01 (in-memory turn claims lost on crash), F-C03 (work-queue retry amplification + backwards fatality classifier), F-C06 (truncated streams accepted as complete), F-C07 (failover configured but never used), F-C08 (pool starvation via cancel), F-C17 (propagation TOCTOU), F-C19 (recovery contracts unverifiable because gates are red).

### Performance — Assessed (measured where possible)
No benchmark harness exists. Measured: 43 MB DB after 11 light days (F-C09, ~4 MB/day event growth). Fixed-rate idle loops: 100 ms work poll, 500 ms job reconcile, 1 s ask sweep (F-C05) — cheap queries but unconditional; on a contended single-writer SQLite they add pressure. SSE heartbeat 15 s is a comment (no row) — correct.

### Security — Deprioritized per request (Partial)
Not audited in depth. Incidental observations only: no secrets in logs found; WebAuthn RP ID handling noted in `xtask dev`; path/command guards exist in execution tools (with tests, two of which are the currently-failing F-C19 assertions). Credential handling at clone time uses an askpass script that is removed after the command (`adapters/git.rs:155-157`).

### Project health — Assessed
Very young, very fast codebase: 24 commits in 14 days (2026-07-22 → 08-04), many "checkpoint" commits, one revert. 57,555 first-party production lines + 4,165 test lines (12 test-file candidates) + 4,139 server integration lines. Defect-fix commits are rare (4 of 24) relative to feature/refactor volume — consistent with a project moving faster than its verification, corroborated by F-C19/F-C21. Active architectural extraction (capability crates) is underway and the ownership checker is a real asset for it.

## Systemic failure and regression map

| Invariant | Writers / triggers | Duplicate / retry behavior | Crash boundary | Restart behavior | Public observer | Regression test |
| --- | --- | --- | --- | --- | --- | --- |
| A committed Running turn is executed exactly once | `post_session_message`, `answer_ask`, `expire_asks`, `settle_cancel`, `retry_waiting_model`, `spawn_job_wake` | Coalesced by in-memory `active_turns` HashSet | **Gap (F-C01):** commit→schedule window loses the turn; recovery interrupts it instead of re-running | Interrupted at startup; no requeue of never-started turns | `turn.status_changed` SSE | None covering the crash window (F-C18) |
| Wake-ups are harmless when duplicated | All six entry points | Claim insert fails → coalesced | Claim drop on panic releases turn | Clean (in-memory only) | coordinator debug log | Not directly tested |
| Events visible only after commit | `UnitOfWorkTransaction::commit` | n/a | Notify strictly after commit (`unit_of_work.rs:66-77`) | Event store durable; cursor = rowid | SSE replay from cursor | `public_surface` passes |
| Work items converge, not duplicate | `workers::spawn` → `claim_work` (nonce lease) | Idempotent via operation_id re-injection | Lease expiry re-claims | Stale lease reclaimed after TTL | `operation.changed` | `runtime_contract` passes |
| Retry never amplifies | Model retry (6 attempts, 10 s fixed) + work-queue retry | **Violation (F-C03):** work-queue retry has no cap/backoff and backwards fatality classification | n/a | Work items durable, re-claimed | `model.attempt_retrying` SSE | retry classifier unit-tested; queue retry NOT tested |
| A Finished round is complete output | 3 provider assemblers → `stream_completion_with` | n/a | **Violation (F-C06):** truncated stream → Completed with partial output | n/a | `model.stream_delta`, turn result | No truncation test (mock provider always terminates cleanly) |
| Configured failover takes over | `try_round_stream` | **Violation (F-C07):** failover table written, never read at request time | n/a | n/a | none | None |
| Cancel leaves no orphan work | `cancel_active_turn` | Idempotent (Canceling/Canceled re-entry returns current) | In-flight stream abandoned; DURABLE state settles correctly; provider-side generation continues (accepted) | Recovery interrupts Canceling | `turn.status_changed` | `session_cancellation` passes (3 tests) |
| Propagation baseline matches recorded revisions | `propagate_session_workspace` | **Suspected violation (F-C17):** baseline write and revision record are not atomic; crash between → skipped changes | Gap between `store_propagation_baseline` and `record_manifest_revision` | Baseline is a filesystem dir; survives restart | `workspace.diff_changed` SSE | `workspace` suite passes happy path only |
| Recovery strands nothing | `recover_execution_state`, stale-op sweep | Idempotent (status guards) | n/a | **Untested (F-C18)** | readiness 503→200, `system.started` | **None** |

## Findings

Full ledger below. Severity follows the evidence rubric; confidence Confirmed means direct code/command/runtime evidence, Suspected means a falsifiable scenario with one missing probe.

| ID | Severity | Status | Finding | Location |
| --- | --- | --- | --- | --- |
| F-C19 | Critical | Confirmed | Workspace test gate is red and integration tests never run in it | `crates/execution/src/tools.rs:1699-1727` vs `:2334-2358` |
| F-C06 | Critical | Confirmed | Truncated provider stream accepted as successful completion (partial output persisted as final) | `crates/models/src/stream.rs:203-227,281-308,371-395`; `anthropic.rs:244-318` |
| F-C01 | High | Confirmed | In-memory turn scheduling lost across crash; recovery strands never-started turns | `application/execution.rs:84-169`; `lifecycle.rs:99-160` |
| F-C03 | High | Confirmed | Work-queue retry: no cap/backoff + backwards fatality classifier (clone transient → dead) | `operations.rs:241`; `workers.rs:142-306`; `projects/interface.rs:1577-1612` |
| F-C07 | High | Confirmed | Failover candidates configured but never used at request time | `models/interface.rs:388-414`; `execution/interface.rs:899-958` |
| F-C10 | High | Confirmed | Architecture checker's table-ownership gate bypassable via const SQL + `format!` | `xtask/main.rs:349-599`; `runtime/service.rs:767-770` |
| F-C21 | High | Confirmed | Clippy gate red: `unwrap_used=deny` vs 7 test unwraps in janus-models | `Cargo.toml:65-68`; `openai_responses.rs:445-522` |
| F-C15 | High | Confirmed | `projects` and `sessions` (state machines) have zero unit tests | `crates/*/module.toml`; cfg(test) counts |
| F-C17 | High | Suspected | Propagation baseline/revision not atomic; TOCTOU on concurrent propagate or crash mid-write | `workspace_sync.rs:44-70`; `workspace/interface.rs:867-1064` |
| F-C02 | Medium | Confirmed | Mutex poison silently swallowed in coordinator claim | `application/execution.rs:160-169,574-582` |
| F-C05 | Medium | Confirmed | Unconditional fixed-rate control-plane sweeps (100 ms/500 ms/1 s) | `workers.rs:30,52,65,93-104` |
| F-C08 | Medium | Confirmed | Cancel flow can hold ~4-5 of 8 pool connections across process-kill awaits | `session_flow.rs:811-916`; `database.rs:40-41` |
| F-C09 | Medium | Confirmed | No public_events retention; 43 MB DB in 11 light days | `events.rs:135-145`; `.janus-dev/janus.db` |
| F-C11 | Medium | Confirmed | Migration-ownership checker hard-codes a filename exception | `xtask/main.rs:416-420` |
| F-C12 | Medium | Confirmed | `rust-toolchain.toml` is unpinned `stable` | `src/rust-toolchain.toml` |
| F-C18 | Medium | Confirmed | Startup recovery has no integration test | `apps/server/tests/*` (zero restart assertions) |
| F-C20 | Medium | Confirmed | Propagation does not detect Git head movement during its run | `workspace/interface.rs:879-1055` |
| F-C04 | Medium | Confirmed | Session-lifecycle semaphore does not cover clone; per-process only | `workers.rs:34-55,106-157` |
| F-C11→(moved) | — | — | (renumbered into list above) | — |
| F-C16 | Low | Confirmed | module.toml specs/tests fields never validated | `xtask/main.rs:233` |
| F-C13 | Info | Confirmed | SSE resume path verified sound (cursor replay + wake-up hint only) | `sse.rs:101-151`; `events.rs:66-92` |
| F-C14 | Info | Confirmed | No `unwrap()` in production paths; panics confined to tests | rg sweeps |

## Top risks and bounded actions

1. **The verification pyramid is down (F-C19 + F-C21 + F-C18 + F-C15).** Every emergent-failure contract above is only as good as its proof, and two of three quality gates are red while the recovery contract has no test at all.
   - smallest next probe: reorder the two branches in `sanitize_workspace_text` (env-key before host-path) and run `cargo test --workspace`; add `#[allow(clippy::unwrap_used)]` to the models test module and run clippy.
   - bounded handoff: one implement task — "make fmt+clippy+test green on clean main, then add a restart-recovery integration test (seed running turn/attempt/operation, re-initialize on same data root, assert interrupted/needs_attention)."
   - verification: all three gates exit 0; new test passes.
2. **Truncated streams produce silently-partial turns (F-C06).**
   - smallest next probe: in one assembler, track terminal-frame receipt (`message_stop` / `[DONE]` / `response.completed`); if bytes end first, return `Failed(PROVIDER_STREAM_FAILED, "truncated")`.
   - bounded handoff: implement task across the 3 assemblers + a truncation test using a real socket closed early.
   - verification: truncation test passes; a truncated stream surfaces `Reconnecting` then `waiting_for_model`, never a half-answer.
3. **Work-queue retry amplifies and misclassifies (F-C03).**
   - smallest next probe: read `fail_work`; confirm `not_before` is unchanged and attempts uncapped (already confirmed — see operations.rs:241).
   - bounded handoff: implement task — attempts cap + exponential `not_before` + typed error codes from `run_clone` (AuthFailed/Validation/Transient) replacing substring matching.
   - verification: a transiently-failing clone retries with growing gaps then dies; a poison item dies after N attempts without 10 Hz churn.
4. **Crash-window turn loss (F-C01).**
   - smallest next probe: none — code-confirmed.
   - bounded handoff: implement task — either recovery requeues `running`+`active` turns that have no `running` round, or scheduling is driven by a durable wake item so the window closes.
   - verification: kill -9 between commit and schedule in a test; next startup resumes the turn.
5. **Failover is a dead config surface (F-C07).**
   - smallest next probe: none — code-confirmed.
   - bounded handoff: decide contract (wire candidates into `try_round_stream` on Transient exhaustion, or remove/mark experimental); add a public-surface test.
   - verification: failed primary advances to failover candidate in a test, or the config surface returns a clear "not yet supported".

## Unknowns and limits

- **F-C17 is Suspected, not Confirmed**: the propagation TOCTOU was established by code reading (baseline write and revision record are separate awaits with no lock); the cheapest confirming probe (write file → delete propagation-base dir → Sync → diff) was not run because the audit is read-only. Impact described is the predicted outcome, not an observed one.
- **Performance is largely unmeasured**: no benchmark harness exists; DB-size growth was measured on the real dev install, but query-plan, lock-contention, and memory claims are static-risk only.
- **Security was deprioritized per request** (Partial lens): no dependency-advisory scan, no auth-bypass attempt, no secrets audit beyond incidental reading.
- **The other agent's report was not read** (per instruction); convergence/divergence between the two reports is itself a signal for the user.
- **Frontend (`apps/web`) was not audited** beyond confirming it exists and has 2 e2e specs; the audit focused on the Rust control plane where the emergent-failure risk lives.

## G4-level assessment (E3 depth — no code changed, judgment only)

This pass re-examined each Confirmed emergent failure with compatibility constraints removed (G4/D3/C3/E3): the question is no longer "what is the smallest safe patch" but "is this component worth repairing, or does its design guarantee recurrence". Judgments only; no source was edited.

### F-C06 upgraded to per-protocol certainty

The truncation hole is not one bug but three independent instances of the same missing state bit, confirmed per protocol:

| Provider | Terminal frame that MUST arrive | Tracked anywhere? | Evidence |
| --- | --- | --- | --- |
| Anthropic | `message_stop` | **No flag exists at all.** `message_stop` is never matched; `message_delta` reads `usage` and `stop_reason` but sets no "stream ended" bit; struct has no terminal field. | `anthropic.rs:134-143` (fields), `:244-276` (message_delta), `rg message_stop` → none |
| OpenAI Chat | `[DONE]` sentinel | **No.** `[DONE]` short-circuits `ingest_data` returning `Ok(vec![])` — it is consumed and discarded, never recorded. | `openai_chat.rs:156-158` |
| OpenAI Responses | `response.completed` | **No.** The handler updates usage/items and returns `Ok(vec![])`; no flag set. `finish()` ignores whether it ever fired. | `openai_responses.rs:183-189,348-376` |

In all three, `stream_openai_chat` / `stream_openai_responses` / `stream_anthropic` then call `assembler.finish(attempt_id)` **unconditionally** after the byte loop, and `finish()` always builds `ModelStreamEvent::Completed`. So the signal "the server told us the turn is done" is thrown away at the exact layer that owns it, and the layer above (`try_round_stream`) can no longer distinguish complete from truncated.

Judgment: this is a **missing terminal-state invariant in the assembler design**, not a localized mistake. Patching one assembler leaves the other two. The correct fix is a single shared rule — *`finish()` must return `Failed(PROVIDER_STREAM_FAILED, "truncated")` unless a terminal frame was observed* — enforced identically across all three (a shared `saw_terminal: bool` or a trait-level `finish_terminal()`). Because it is the same invariant in three places, it belongs in the stream contract, not three if-statements. Severity stays Critical; confidence is now per-protocol Confirmed, not inferred.

### Why the existing tests structurally cannot see any of these

The gaps share one root: **the test seams never simulate the failure dimension that matters.** Concretely:

- `model_streaming.rs` uses a mock provider that always terminates its stream cleanly — so F-C06 (truncation) is unreachable by construction. No test owns a socket that closes early.
- `session_cancellation.rs` seeds rows directly and never restarts the process — so F-C01 (crash-window turn loss) and F-C18 (recovery) are unreachable. No test re-initializes `AppState` on a dirty data root.
- `runtime_contract.rs` / `runtime_local.rs` exercise the real local executor happy path — so F-C03 (queue retry amplification) is unreachable because nothing injects a transiently-failing work item and watches it spin.
- `workspace.rs` runs the propagation happy path only — so F-C17 (crash between baseline write and revision record) is unreachable because nothing kills the process mid-propagate.

This is the systemic finding behind the individual ones: **the suite tests behavior, not failure.** Injection seams already exist (`Arc<dyn GitRunner>` at `projects/interface.rs:413`, `Arc<dyn RuntimeExecutor>` at `runtime/interface.rs:1052`, `pub trait GitRunner` at `source-control/interface.rs:142`) but are only used to substitute a *working* double, never a *failing* one. The cheapest high-leverage change to the whole verification posture is a small set of **failure-injection doubles** (a GitRunner that errors transiently, an executor whose process dies mid-stream, a provider socket that closes early, a propagator that crashes between writes) wired into the existing integration harness. That single addition makes F-C01, F-C03, F-C06, F-C17, F-C18 all testable without touching production code.

### G4 rebuild/repair verdict per component

- **Stream assemblers (models)**: repair, not rebuild — the protocol logic is fine; only the terminal invariant is missing. Add it once, shared.
- **Work-queue retry (workers.rs + operations.rs)**: repair the mechanism, replace the classifier. The lease/nonce/idempotency core is sound; the retry policy (no cap/backoff) and `is_fatal` substring matching should be replaced with typed error codes. Not a rebuild.
- **Architecture checker (xtask)**: repair — resolve `const` SQL via a syn visitor over const items, and fix the `insert [or replace] into` tokenizer skip. The gate is worth keeping; it is currently auditing an idealized subset of reality.
- **Turn scheduling (coordinator)**: repair — close the commit→spawn window (durable wake item, or recovery requeue). The single-coordinator design is correct and should be kept.
- **Propagation (workspace)**: repair — make baseline + revision atomic (one transaction or fs journal) and serialize per session. Design is sound; the atomicity gap is localized.
- **Verification posture**: this is the one place a **rebuild-level** change is warranted — not of code, but of the gate. Today `fmt`, `clippy`, `test` are three separate commands of which two are red, and no single command must be green. Make one gate (extend `xtask check`) run fmt-check + clippy + workspace tests and fail if any fails, so "the pyramid is up" is itself enforced rather than assumed.

No component requires G4 rewrite. The architecture's bones (ownership, commit-before-publish, single coordinator, lease+nonce, idempotency) are good; the risk is concentrated in missing failure-path invariants and a verification harness that never injects failure.


| Dimension | Score | Confidence | Evidence |
| --- | --- | ---: | --- | --- |
| Security | ░░░░░░░░░░ n/a (deprioritized) | low | not audited per request |
| Stability | █████░░░░░ 5 | high | strong coordination design, but 2 Critical + 5 High emergent-failure gaps |
| Performance | ██████░░░░ 6 | medium | no pathological patterns found; unbounded event growth + idle polling are the main drags |
| Testing | ███░░░░░░░ 3 | high | 46+41 tests exist and pass individually, but workspace gate is red, clippy is red, recovery + truncation + queue-retry untested, projects/sessions have no unit tests |
| Maintainability | ███████░░░ 7 | high | clean enforced boundaries, near-zero unwrap/panic, but 3 k-line interfaces and checker holes |
| Design | ████████░░ 8 | high | ownership + commit-before-publish + single coordinator are genuinely well-architected |
| Release | ████░░░░░░ 4 | high | unpinned toolchain, red gates, decorative manifest fields, no single green gate command |
| **Overall** | **█████░░░░░ 5.5** | high | excellent bones; the gap between the architecture's promises and its verifiable reality is the risk |
