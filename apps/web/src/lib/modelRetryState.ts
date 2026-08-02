import { createSignal } from "solid-js";

/**
 * Live model-retry state driven by `model.attempt_retrying` SSE events.
 *
 * The SSE event is emitted by execution just *before* it sleeps for a
 * retry, so the UI can render `Reconnecting (X/5): reason` while the backoff
 * elapses — the most useful information the user can have is "it failed, but
 * it's trying again, here's why". When the turn settles (success, terminal
 * failure, or park), the per-session turn query's `model_attempt` field takes
 * over as the durable source; `clearRetryState` is called on settle to stop a
 * stale "reconnecting" indicator lingering after a successful retry.
 */

export interface RetryState {
  attemptId: string;
  attempt: number;
  maxAttempts: number;
  detail: string;
  retryAt: number;
}

const MAX_RETAINED = 32;
const [states, setStates] = createSignal<ReadonlyMap<string, RetryState>>(new Map());

function key(sessionId: string, turnId: string): string {
  return `${sessionId}:${turnId}`;
}

export function ingestRetryEvent(eventType: string | undefined, payload: unknown): void {
  if (eventType !== "model.attempt_retrying" || !isRecord(payload)) return;
  const sessionId = stringValue(payload.session_id);
  const turnId = stringValue(payload.turn_id);
  const attemptId = stringValue(payload.attempt_id);
  const attempt = numberValue(payload.attempt);
  const maxAttempts = numberValue(payload.max_attempts);
  const retryAfterMs = numberValue(payload.retry_after_ms);
  const detail = stringValue(payload.detail);
  if (
    !sessionId ||
    !turnId ||
    !attemptId ||
    attempt == null ||
    maxAttempts == null ||
    retryAfterMs == null
  )
    return;

  const k = key(sessionId, turnId);
  setStates((current) => {
    const next = new Map(current);
    next.delete(k);
    next.set(k, {
      attemptId,
      attempt,
      maxAttempts,
      detail,
      retryAt: Date.now() + retryAfterMs,
    });
    while (next.size > MAX_RETAINED) {
      const oldest = next.keys().next().value as string | undefined;
      if (!oldest) break;
      next.delete(oldest);
    }
    return next;
  });
}

export function clearRetryStateOnStreamDelta(payload: unknown): void {
  if (!isRecord(payload)) return;
  const sessionId = stringValue(payload.session_id);
  const turnId = stringValue(payload.turn_id);
  const attemptId = stringValue(payload.attempt_id);
  if (!sessionId || !turnId || !attemptId) return;
  const current = states().get(key(sessionId, turnId));
  if (current && current.attemptId !== attemptId) clearRetryState(sessionId, turnId);
}

export function retryState(sessionId: string, turnId: string | undefined): RetryState | null {
  if (!turnId) return null;
  return states().get(key(sessionId, turnId)) ?? null;
}

export function clearRetryState(sessionId: string, turnId: string | undefined): void {
  if (!turnId) return;
  const k = key(sessionId, turnId);
  setStates((current) => {
    if (!current.has(k)) return current;
    const next = new Map(current);
    next.delete(k);
    return next;
  });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function numberValue(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : null;
}
