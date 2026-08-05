# Control-plane stability rules

Status: accepted

Janus keeps the existing server application as the cross-capability control-plane seam. The implementation may be reorganized behind private modules, but a new application crate or a destructive data-root reset is not required unless the existing seam cannot become authoritative without preserving incompatible state machines.

## Rules

1. Durable control state is authoritative. SQLite state, leases, claims, operation steps, Turn state, and recovery records define workflow progress. Tokio tasks, process monitors, filesystem scans, and wake notifications are caches or observations, never the only record that work is runnable.

2. A committed runnable intent creates a durable wake in the same UnitOfWork. An in-memory wake may reduce latency, but losing it must not lose work. Startup and reconciliation must find every eligible durable wake.

3. Every claim and external-effect callback is fenced by an attempt epoch and nonce. A callback from an expired, replaced, or recovered attempt is a no-op or reconciliation result; it must not publish a terminal state for the current attempt.

4. Cross-capability workflows belong to the application seam. A capability writes only its own tables through its interface. Workspace is the only owner of managed bytes, content revisions, propagation cursors, and filesystem mutation. HTTP, SSE, and CLI convert protocols and do not orchestrate compensation or transactions.

5. External effects run outside short database transactions. Each effect has a durable intent, a stable idempotency key, an observed result, and a reconciliation rule for the crash points before, during, and after the effect.

6. Retry policy is typed and bounded. Transient failures use an attempt limit, `not_before`, backoff, and jitter. Validation, authentication, and proven permanent failures do not retry automatically. Exhausted or ambiguous work reaches an explicit `needs_attention` state.

7. A Turn is complete only after its authoritative state transition commits. `failed` Turns retain their diagnostic result and advance the FIFO queue. `interrupted` Turns are requeued only when repeating the external effect is proven safe; otherwise they become visible `needs_attention` work.

8. A Provider stream is complete only after protocol terminal evidence. Anthropic requires `message_stop`, OpenAI Chat requires `[DONE]`, and OpenAI Responses requires `response.completed` (an explicit equivalent sentinel may be accepted only by that adapter). EOF without evidence is a retryable truncation failure and can never emit `Completed`.

9. Public state events are emitted from the same durable transition or a durable outbox boundary. Progress deltas may be explicitly lossy, but final state, failures, recovery changes, cursor semantics, and resource ownership are replayable.

10. Readiness is fail-closed. The service is not ready until required startup recovery, stale-claim reconciliation, blob cleanup, and operation cleanup have either committed successfully or produced an explicit operator-visible `needs_attention` result.

11. A stability claim requires failure evidence, not only happy-path coverage. Acceptance tests use the compiled server, real SQLite, public HTTP/SSE/WebSocket, and `janus-test`; failure-injection adapters cover truncation, dead processes, transient Git errors, stale leases, crash-after-commit, crash-after-effect, concurrent propagation, and restart recovery.

## Consequences

The control plane may add forward migrations for claims, wakes, retry state, or external-effect journals, but applied migrations remain immutable. Existing capability crates and public contracts are preserved where their semantics are correct; direct getters, duplicate writers, unbounded retries, and compatibility paths that bypass these rules are removed rather than extended.

The rules deliberately prefer a visible stalled operation over silently accepting partial output, duplicating an external effect, advertising false readiness, or stranding later Turns. This makes recovery behavior explicit and keeps failure handling local to the application seam.

## Rejected alternatives

- Treating an in-memory scheduler as the source of truth.
- Accepting provider EOF as completion because some providers usually send a terminal frame.
- Retrying based on display-text substring matching.
- Holding a database transaction across model, Git, filesystem, network, or process awaits.
- Resetting the live data root before an export/import contract proves that reset is necessary.
