# Web UI legacy-codebase cleanup report

- Updated: 2026-07-28
- Scope: `apps/web` UI structure, supervisor workflow, shared UI primitives/tokens, directly required HTTP/application seams, tests, and directly affected guidance.
- Exclusions: rootless-Podman M4 Stage 7; M5+ product work; generated code except contract verification; old React implementation except as a read-only visual reference; production data and live paid providers.
- Environment: Windows development host; repository already contains uncommitted M4 work; no production systems or real data are used.
- Mode: System
- Decisions: `E2 / G4 / Q3 / C3 / D3`
- Report status: In progress

## Before assessment (frozen before first code edit)

Security        ███████░░░  7.0  A   No security defect found in inspected UI/transport paths; runtime coverage is limited
Stability       ████░░░░░░  4.0  C   Primary live workflow is unproved and documented control paths are absent
Performance     █████░░░░░  5.0  B   Terminal is lazy, but 69.17 kB CSS and low-end browser behavior are unmeasured
Testing         ████░░░░░░  4.0  C   Static gates pass; mocked Playwright passes despite failed unexpected requests
Maintainability ███░░░░░░░  3.0  C   4251-line global CSS and mixed 594-line Session owner
Design          ████░░░░░░  4.0  C   Raw projections and unsupported controls cross UI/transport ownership
Release         ██████░░░░  6.0  B   Reproducible build passes; representative live browser gate is missing
─────────────────────────────────────
Overall         ████░░░░░░  4.7  C   Static delivery is healthy, but the product workflow and ownership boundaries are not

| Dimension | Confidence | Scope/evidence |
| --- | --- | --- |
| Security | Low | Browser/UI and directly touched transport only; no adversarial or production test. |
| Stability | Medium | Static message flow plus user report; live reproduction pending. |
| Performance | Low | Build output and code shape only; no device/runtime profile yet. |
| Testing | High | Commands and Playwright network errors are directly observed. |
| Maintainability | High | Physical lines and responsibility/selector ownership inspected directly. |
| Design | High | Current component, API, task, and product contracts inspected directly. |
| Release | Medium | Frontend build is reproducible on this host; full product gate is not run. |

## After assessment

Security        ░░░░░░░░░░  --   -   Pending
Stability       ░░░░░░░░░░  --   -   Pending
Performance     ░░░░░░░░░░  --   -   Pending
Testing         ░░░░░░░░░░  --   -   Pending
Maintainability ░░░░░░░░░░  --   -   Pending
Design          ░░░░░░░░░░  --   -   Pending
Release         ░░░░░░░░░░  --   -   Pending
─────────────────────────────────────
Overall         ░░░░░░░░░░  --   -   Pending

| Dimension | Before | After | Delta | Confidence | Evidence/interpretation |
| --- | ---: | ---: | ---: | --- | --- |
| Security | Pending | Pending | Pending | Pending | Pending. |
| Stability | Pending | Pending | Pending | Pending | Pending. |
| Performance | Pending | Pending | Pending | Pending | Pending. |
| Testing | Pending | Pending | Pending | Pending | Pending. |
| Maintainability | Pending | Pending | Pending | Pending | Pending. |
| Design | Pending | Pending | Pending | Pending | Pending. |
| Release | Pending | Pending | Pending | Pending | Pending. |

## Finding summary

| Severity | Count | Confirmed | Suspected |
| --- | ---: | ---: | ---: |
| Critical | 0 | 0 | 0 |
| High | 5 | 4 | 1 |
| Medium | 9 | 9 | 0 |
| Low | 2 | 2 | 0 |
| Info | 0 | 0 | 0 |
| **Total** | 16 | 15 | 1 |

Rejected candidates remain in the decision record but are excluded from the counts.

## Executive summary

- Current runnable state: Typecheck, lint, build, and one deterministic live browser-to-supervisor workflow pass on this host; mocked suites remain baseline evidence only until their useful assertions move to live coverage.
- Main conclusion: The generic message/Turn/SSE path works, but the Session UI still exposes unfinished command concepts while concentrating query, command, projection, layout, and secondary-view responsibilities in one component and one global stylesheet.
- Highest-priority findings: Keep the live workflow as a gate, then remove transport/UI mismatches and establish one typed timeline boundary before visual cleanup.
- Next smallest action: Refactor the Session projection and component boundaries while preserving the now-proven live path.

## System map

```text
Session UI -> generated/typed API wrapper -> sessions HTTP handler
           -> sessions state machine -> application message routing
           -> supervisor execution -> durable timeline/events
           -> shared SSE invalidation -> Query refetch -> Session UI
```

The old React UI is outside the runtime map and supplies visual/interaction evidence only.

## Code size baseline

| Area/type | Files | Physical lines | Exclusions/notes |
| --- | ---: | ---: | --- |
| First-party frontend production | 43 | 11501 | `apps/web/src` TS/TSX/CSS, excluding generated API and colocated tests. |
| Frontend tests | 6 | 1127 | Five `apps/web/e2e` files plus one colocated test. |
| Generated frontend | 1 | 4795 | `src/generated/api.ts`, recorded separately. |
| Styles | 1 | 4251 | Current monolithic `src/styles.css`; physical lines include blanks/comments. |

The same CSS contains 58 unique custom-property declarations, 592 heuristic rule blocks, 4 media queries, and 30 animation/transition declarations. The inventory helper misclassified `e2e/` as production, so the table uses an explicit path/name classification and retains the helper output only as a navigation aid.

| Largest file/symbol | Physical lines | Role | Finding ID or rationale |
| --- | ---: | --- | --- |
| `src/styles.css` | 4251 | All application tokens and feature styles | F-005 |
| `src/lib/api.ts` | 774 | Typed HTTP wrapper surface | Inspected boundary; generated types remain authoritative. |
| `SessionTabView.tsx` | 594 | Session query, commands, timeline, composer, diff, context, and terminal composition | F-004 |
| `ProjectPage.tsx` | 587 | Project workspace shell and tabs | Candidate pending responsibility review. |

## Baseline checks

| Check | Command/evidence | Result | Baseline failure? |
| --- | --- | --- | --- |
| TypeScript | `bun run typecheck` | Pass in 13.9 s | No |
| Biome | `bun run lint` | Exit 0 in 1.6 s; 3 CSS warnings | Warnings are baseline findings F-005/F-009 |
| Production build | `bun run build` | Pass in 12.4 s; CSS 69.17 kB/11.61 kB gzip; ProjectPage 60.23 kB/18.55 kB gzip; xterm remains a lazy 329.29 kB chunk | No |
| Session Playwright | `bun run test:e2e -- sessions.spec.ts` | 2/2 pass in 20.3 s; Vite logs repeated `ECONNREFUSED` for unmocked requests | Test gap is baseline finding F-006 |
| Representative live supervisor workflow | Browser + local server/fixture | Pending | Pending |

Post-baseline evidence: `bun run test:e2e:live` passes 1/1 with a temporary Git repository, local deterministic SSE provider, real server, browser message submission, durable user/assistant timeline items, SSE-driven refresh, and the Session returning to `ready`. This test was added after the frozen Before snapshot, so it is recorded as F-006 repair evidence rather than rewritten into the baseline.

Baseline screenshots: `apps/web/test-results/sessions-desktop-session-opens-as-project-tab/sessions-tab-desktop.png` and `apps/web/test-results/sessions-mobile-session-opens-as-project-tab/sessions-tab-mobile.png`. The mobile image directly records F-010; both images record F-011 and the unfinished-control presentation already covered by F-002/F-003.

## Before/after comparison

| Measure | Before (frozen) | After | Delta | Interpretation |
| --- | --- | --- | --- | --- |
| First-party production physical LOC | 11501 in 43 files | Pending | Pending | Same path/name classification and physical-line count. |
| Largest relevant file/symbol | `styles.css`: 4251 lines | Pending | Pending | Size is a navigation signal; the finding also requires mixed ownership evidence. |
| Tests / static checks | typecheck/build pass; lint exit 0 with 3 warnings; mocked Playwright 2/2 | Pending | Pending | Same commands and environment. |
| Confirmed findings fixed/open | 0 fixed / 7 open | Pending | Pending | Rejected candidates excluded. |
| Documented functional standards | Product, UX, Session/Supervisor, HTTP, roadmap, M4 design | Pending | Pending | Update only verified conflicts. |
| User-visible/contract behavior | Mocked Session UI only proven | Pending | Pending | Live path remains to be reproduced. |

## Root-cause clusters

- Contract cluster: F-001, F-002, F-003, and F-006 share the absence of one executable browser-to-domain workflow contract.
- Ownership cluster: F-004, F-005, and F-009 concentrate unrelated change reasons and make local UI consistency fixes expensive.
- Guidance cluster: F-007 leaves future work maintaining a motion policy the project explicitly does not want.

## Finding ledger

| ID | Severity | Evidence | Disposition | Finding | Location/evidence | Impact | Action or question |
| --- | --- | --- | --- | --- | --- | --- | --- |
| F-001 | High | Suspected | Open | The reported environment-specific Web Session-to-Supervisor failure remains unexplained. | User report; the new deterministic live Playwright test completes browser submission, server Turn execution, durable timeline updates, SSE refresh, and return to `ready` on this host. | A provider, configuration, repository, or environment-specific failure may still block the user's workflow even though the generic code path is runnable. | The generic path is not repaired because it passed; retain the live regression and diagnose only from reproducible evidence from the affected environment. Current exact cause is unjudgeable. |
| F-002 | Medium | Confirmed | Open | Composer model and reasoning controls have no effect on submitted messages. | `SessionTabView.tsx` stores both values locally; `postSessionMessage` sends only `content` and `expected_session_version`; comments acknowledge the placeholder. | Users can make a choice the system ignores, so the UI misrepresents control over execution. | Remove the controls until a public request contract exists, or wire them through an already documented typed contract if found. |
| F-003 | High | Confirmed | Open | Documented M4 Session controls are omitted or rendered as non-actionable state because public transport is missing. | M4 design and `docs/03-session-and-supervisor.md` require Ask answer, Steer, Cancel, Handoff, model recovery, Job/Service controls, and Context/Compact; Stage 9 notes and current UI explicitly omit or fake several because routes are absent. | Long-running or waiting Turns cannot be controlled from the primary Web UI as specified. | Inventory the current backend interfaces and expose the smallest cohesive typed command set needed by the documented workflow; do not add per-condition UI patches. |
| F-004 | Medium | Confirmed | Open | The Session work surface mixes unrelated responsibilities and repeatedly interprets raw projections. | `SessionTabView.tsx` is 594 lines and owns four queries, submission, textarea sizing, auto-scroll, tabs, timeline decoding, diff normalization/rendering, context, terminal composition, and raw `unknown` casts also used by `SessionCards.tsx`. | Changes spread across unrelated render/state concerns and timeline contract drift can be handled differently by each consumer. | Establish one projection decoder/view-model owner and extract only stateful or substantial feature responsibilities. |
| F-005 | Medium | Confirmed | Open | One 4251-line stylesheet owns global tokens and unrelated feature/page styling. | `apps/web/src/styles.css`; selector ownership spans app shell, auth, projects, workspace, Session, SCM, Terminal, models, system, and responsive behavior. | Token cleanup and feature changes require navigating and validating unrelated regions; duplicate semantic decisions are easy to introduce. | Inventory tokens/selectors, consolidate semantics, and move complete selector groups to stable feature owners. |
| F-006 | Medium | Confirmed | Open | Current browser coverage mixes a real workflow gate with local HTTP simulations that can pass against a disconnected or contract-inaccurate server. | Before: `apps/web/e2e/sessions.spec.ts`, `workspace.spec.ts`, `probe-tabs.spec.ts`, and `project-graph.spec.ts` fulfill application HTTP routes inside Playwright; the Session suite passed while Vite logged repeated `ECONNREFUSED`. Partial repair: `e2e/live-supervisor.spec.ts` and `e2e/support/liveJanus.ts` start a compiled server, real temporary SQLite/Git/workspace state, and a protocol-level provider fixture. | Mock-owned response shapes cannot establish that the shipped browser/server/database/runtime composition works and can preserve obsolete contracts during refactoring. | Move each still-valuable workflow, responsive, and accessibility assertion to compiled-server coverage through public HTTP/SSE/WebSocket and `janus-test`, then delete the superseded mocked and local-only tests; do not delete first and leave a coverage gap. |
| F-007 | Low | Confirmed | Open | Frontend guidance requires reduced-motion checks even though the project intentionally avoids costly animation and does not maintain two motion modes. | `src/AGENTS.md` and `.trellis/spec/frontend/quality-guidelines.md`; explicit user decision in this task. | Future changes are asked to maintain unsupported duplicate behavior and may add unnecessary branches. | Replace the rule with a single low-cost motion/performance constraint and keep accessibility requirements unrelated to motion. |
| F-008 | Medium | Confirmed | Open | The Web UI implements a Session Terminal that the current product decision does not support. | Repository docs and the M4 design describe desktop Session Terminal, but the user has explicitly ruled those statements incorrect for the current target; `SessionTabView.tsx` still exposes and mounts the capability. | The unsupported surface adds navigation, emulator, API, responsive, and testing cost while implying a product capability that should not exist. | Remove the Session Terminal UI and Session-owned integration paths, retain separately consumed project/runtime primitives, and correct only the conflicting product documentation. |
| F-009 | Low | Confirmed | Open | Global CSS relies on two `!important` visibility overrides and has a descending-specificity status rule. | Baseline Biome warnings at `styles.css:1101`, `styles.css:3809/3870`, and `styles.css:4239`. | Hidden-state and status styling depend on fragile cascade ordering and block a warning-clean quality gate. | Make hidden-state ownership explicit and order/scope status selectors without specificity reversal. |
| F-010 | Medium | Confirmed | Open | The mobile workspace stacks the entire sidebar above the active Session document. | Baseline mobile screenshot at 390 px; `styles.css:3551-3571` creates a minimum 220 px sidebar row and keeps the 64 px activity rail beside both rows. | The primary conversation begins below a large mostly empty region, consumes horizontal space, and requires avoidable scrolling before the user can act. | Give the mobile Session workflow one primary surface at a time with a compact way to return to navigation; preserve desktop shell behavior. |
| F-011 | Medium | Confirmed | Open | Ask projection decoding prefers tool-call execution status over the nested Ask status. | The fixture projection has top-level `status: succeeded` and `summary.status: open`; `SessionCards.tsx` reads the top-level value first, and both baseline screenshots render `succeeded` instead of an open Ask. | Waiting state is represented incorrectly and an answer UI would remain disabled even after a transport callback is supplied. | Decode tool envelope and Ask payload separately at the central projection boundary, then render the domain Ask status. |
| F-012 | Medium | Confirmed | Open | Job and Service cards also prefer the completed tool-call envelope over the nested resource lifecycle. | `SessionCards.tsx` reads `projection.status` before `summary.status`; real `job` and `service` tools finish successfully while their returned resources can remain `queued`, `running`, or `starting`. | Long-running resources are displayed as completed even though their lifecycle is still active, obscuring the state that users need to monitor or control. | Preserve tool execution status separately and decode each resource status from its domain summary at the central projection boundary. |
| F-013 | High | Confirmed | Open | Cancel settlement can race past the supervisor's only cancellation guard. | `execute_turn` returns only when a Round boundary reads exactly `canceling`; `settle_cancel` can advance that state to `canceled` or `interrupted` asynchronously, after which the same loop does not recognize the terminal state. | A canceled Turn can start another model Round or tool sequence after cancellation has already been durably settled. | Treat every non-runnable Turn status as a stop condition and cover the public cancel coordinator through HTTP/state-machine tests. |
| F-014 | High | Confirmed | Open | Answering a blocking Ask resumes its Turn without adding the answer to reconstructed model context. | `answer_ask` updates the `asks` row and calls `resume_turn`; `load_chat_history` reads only `messages`, and no answer message or equivalent context record is written. | The resumed Supervisor cannot reliably know the user's answer and may repeat the question or continue with missing input. | Persist an explicitly attributed Ask-answer message in the shared transaction, update the Ask timeline projection, then resume and execute the Turn through one application operation. |
| F-015 | Medium | Confirmed | Open | Steer is rejected while the active Turn is `waiting_for_model`, contrary to the documented recovery contract. | `SessionsInterface::steer` returns `SteerBlockedByModel` for `waiting_for_model`; `SES-FAILOVER-03` and the UX recovery rules explicitly allow Steer or Cancel in that state. | A user cannot redirect work after model exhaustion even though the UI and domain contract define Steer as a recovery action. | Accept a version-bound Steer for every documented interactive active state, including `waiting_for_model`; keep the Turn paused until an explicit retry/model recovery starts the next Round. |
| F-016 | High | Confirmed | Open | Steer consumption uses a cross-Turn message count instead of a persistent current-Turn cursor. | `load_chat_history` loads every active Session message; `drain_pending_steers` counts every loaded user role, then applies that count as an index into only the current Turn's user messages. | On a second or later Turn, a newly persisted Steer can be skipped as though it were already present in model context, so the accepted control has no effect. | Track the last consumed durable message row at the Supervisor boundary and load only newer current-Turn inputs at each safe Round boundary; use the same path for attributed Ask answers. |

## Pending user decisions

| Question | Finding | Decision needed | Why repository evidence is insufficient | Answer/status |
| --- | --- | --- | --- | --- |
| None | - | - | Repository evidence currently resolves product-scope candidates; runtime reproduction remains an engineering check. | Not blocked. |

## Treatment route

The authorized G4 route is a bounded replacement of the Session work surface behind its existing entry, followed by Q3 retirement of the verified superseded implementation. The router, Query/SSE ownership, generated HTTP boundary, project/runtime capabilities, and persisted backend state remain outside the replacement unless evidence identifies them as root causes. Session Terminal is explicitly retired by product decision. C3/D3 authorize necessary breaking or data work, but no such work is currently justified; if that changes, the report will add a compatibility matrix, data state machine, reconciliation checks, and rollback evidence before implementation.

| Slice | Purpose | Change | Verification | Rollback | Exit condition |
| --- | --- | --- | --- | --- | --- |
| 1 | Restore evidence | Baseline and representative workflow reproduction. | Static checks, server/fixture, browser trace. | No source change. | F-001 confirmed or rejected with exact evidence. |
| 2 | Repair contract | Add or correct cohesive API/application/UI command ownership. | Contract/integration tests and browser workflow. | Additive route/component slice can be reverted. | Supported primary and waiting-state commands work end to end. |
| 3 | Replace Session UI | Rebuild the projection decoder and focused Session components behind the stable entry, then retire the superseded paths. | Typecheck and compiled-server Playwright desktop/mobile coverage. | Keep each replacement slice independently revertible until live parity passes. | No repeated raw projection parsing or verified dead Session UI remains. |
| 4 | Simplify visual system | Token consolidation and feature CSS ownership. | Build, screenshots, overlap/overflow checks. | Move selector groups atomically. | Semantics are consistent and global stylesheet no longer owns unrelated feature details. |
| 5 | Close governance | Minimal AGENTS/spec updates and final report. | Trellis check and diff review. | Documentation changes are independent. | Code, tests, docs, and report describe the same verified contract. |

## Changes and verification

| Finding | Before evidence | Change | After evidence / verification | Result |
| --- | --- | --- | --- | --- |
| F-001 | User report plus no representative live browser evidence | Added the F-006 live integration fixture and exercised the generic path without changing its behavior. | `bun run test:e2e:live`: 1/1 pass; exact affected-environment cause remains unjudgeable. | Investigated; open as environment-specific Suspected finding. |
| F-006 | Mocked Session tests passed despite unexpected failed requests. | Added a real server, temporary repository, deterministic SSE provider, and browser workflow test; user then set compiled real-environment tests as the acceptance standard. | `bun run test:e2e:live`: 1/1 pass through durable timeline convergence and Session `ready`; control, responsive, and secondary workflows are not yet migrated. | Partial repair; remains open until equivalent live coverage replaces the mock suites. |

## Documentation feedback

- Functional standards consulted: `docs/01-product.md`, `02-user-experience.md`, `03-session-and-supervisor.md`, `07-http-api.md`, `11-testing-and-performance.md`, `14-implementation-roadmap.md`, M4 task artifacts, `src/AGENTS.md`, and frontend specs.
- Documentation updated: Pending confirmed implementation results.
- Missing/stale documentation findings: F-007 confirmed; no product-doc change justified yet.

## Remaining risk and uninspected areas

- Deferred/accepted/open findings: F-001 through F-016 remain open.
- Checks not run and why: The affected user's exact provider/configuration failure has not been reproduced; Stage 7 is excluded by host capability.
- Currently unjudgeable: Real low-end device performance, production provider behavior, and Linux container behavior; none will be reported as verified without evidence.
- Uninspected areas: Non-workspace pages remain in the declared UI scope for token/semantic consistency, but detailed issue enumeration follows the code-size and workflow priority order.
