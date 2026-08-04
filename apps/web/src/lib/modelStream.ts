import { createSignal } from "solid-js";

export interface StreamUsage {
  /** Provider input tokens excluding its cached-input portion. */
  inputTokens: number;
  /** Provider output tokens. */
  outputTokens: number;
}

export interface ModelStreamOutput {
  roundId: string;
  attemptId: string;
  sequence: number;
  text: string;
  reasoning: string;
  /** Total usage observed for this Turn, deduplicated by Round and attempt. */
  usage: StreamUsage | null;
  /** Wall-clock ms (Date.now()) when the first reasoning delta arrived.
   * Used to render "thinking for Xs" once the elapsed time exceeds 5s. */
  reasoningFirstSeenAt: number | null;
  /** Wall-clock ms when the first assistant text delta arrived. */
  textFirstSeenAt: number | null;
}

const MAX_RETAINED_OUTPUTS = 32;
const [outputs, setOutputs] = createSignal<ReadonlyMap<string, ModelStreamOutput>>(new Map());
const usageByTurn = new Map<string, Map<string, StreamUsage>>();

const SUMMARY_START =
  /^(?:summarizing|detailing|outlining|inspecting|reviewing|checking|planning|searching|reading|analyzing|comparing|verifying|tracing|exploring|preparing|updating|implementing|testing|running)\b/i;

/** Join provider status-summary chunks without gluing two complete sentences
 * together. Tokenized reasoning still uses the provider's own whitespace. */
export function appendReasoningDelta(previous: string, delta: string): string {
  if (!previous || !delta) return previous + delta;
  if (/^[\s\r\n]/.test(delta) || /[\s\r\n]$/.test(previous)) {
    return previous + delta;
  }
  if (delta.length >= 20 && SUMMARY_START.test(delta)) {
    return `${previous}\n${delta}`;
  }
  return previous + delta;
}

/** Repair already-persisted summary streams produced before the adapter
 * inserted boundaries between Responses status updates. */
export function normalizeReasoningSummary(value: string): string {
  return value.replace(
    /([a-z0-9)])(?=(?:Summarizing|Detailing|Outlining|Inspecting|Reviewing|Checking|Planning|Searching|Reading|Analyzing|Comparing|Verifying|Tracing|Exploring|Preparing|Updating|Implementing|Testing|Running)\b)/g,
    "$1\n",
  );
}

export function ingestModelStreamEvent(eventType: string | undefined, payload: unknown) {
  if (eventType !== "model.stream_delta" || !isRecord(payload)) return;
  const sessionId = stringValue(payload.session_id);
  const turnId = stringValue(payload.turn_id);
  const roundId = stringValue(payload.round_id);
  const attemptId = stringValue(payload.attempt_id);
  const delta = stringValue(payload.delta);
  const sequence = numberValue(payload.sequence);
  if (
    !sessionId ||
    !turnId ||
    !roundId ||
    !attemptId ||
    sequence === null ||
    (payload.channel !== "text" && payload.channel !== "reasoning_summary") ||
    payload.provisional !== true
  ) {
    return;
  }

  const usage = parseUsage(payload.usage);
  const isReasoning = payload.channel === "reasoning_summary";

  const key = outputKey(sessionId, turnId);
  setOutputs((current) => {
    const previous = current.get(key);
    const sameAttempt = previous?.attemptId === attemptId && previous?.roundId === roundId;
    // Same-attempt out-of-order or duplicate text deltas are dropped. Usage-only
    // events are still accepted because providers may report a later snapshot
    // without emitting more text.
    if (sameAttempt && sequence <= previous.sequence && delta) {
      return current;
    }

    const next = new Map(current);
    next.delete(key);

    const previousText = sameAttempt ? previous.text : "";
    const previousReasoning = sameAttempt ? previous.reasoning : "";
    const previousReasoningAt = sameAttempt ? previous.reasoningFirstSeenAt : null;
    const previousTextAt = sameAttempt ? previous.textFirstSeenAt : null;
    const totalUsage = usage ? recordUsage(key, roundId, attemptId, usage) : usageTotal(key);

    next.set(key, {
      roundId,
      attemptId,
      sequence: sameAttempt ? Math.max(previous.sequence, sequence) : sequence,
      text: sameAttempt ? previousText + (isReasoning ? "" : delta) : isReasoning ? "" : delta,
      reasoning: sameAttempt
        ? isReasoning
          ? appendReasoningDelta(previousReasoning, delta)
          : previousReasoning
        : isReasoning
          ? delta
          : "",
      usage: totalUsage,
      reasoningFirstSeenAt:
        isReasoning && previousReasoningAt === null ? Date.now() : previousReasoningAt,
      textFirstSeenAt:
        !isReasoning && delta && previousTextAt === null ? Date.now() : previousTextAt,
    });
    while (next.size > MAX_RETAINED_OUTPUTS) {
      const oldest = next.keys().next().value as string | undefined;
      if (!oldest) break;
      next.delete(oldest);
      usageByTurn.delete(oldest);
    }
    return next;
  });
}

function parseUsage(value: unknown): StreamUsage | null {
  if (!isRecord(value)) return null;
  const inputTokens = numberValue(value.input_tokens);
  const outputTokens = numberValue(value.output_tokens);
  if (inputTokens === null && outputTokens === null) return null;
  // The server normalizes input_tokens before publishing the stream event.
  // cache_tokens is intentionally ignored here: the indicator has one input
  // count and must never add cached input a second time.
  return {
    inputTokens: inputTokens ?? 0,
    outputTokens: outputTokens ?? 0,
  };
}

function recordUsage(
  key: string,
  roundId: string,
  attemptId: string,
  usage: StreamUsage,
): StreamUsage {
  const attempts = usageByTurn.get(key) ?? new Map<string, StreamUsage>();
  const attemptKey = JSON.stringify([roundId, attemptId]);
  const previous = attempts.get(attemptKey);
  attempts.set(attemptKey, {
    inputTokens: Math.max(previous?.inputTokens ?? 0, usage.inputTokens),
    outputTokens: Math.max(previous?.outputTokens ?? 0, usage.outputTokens),
  });
  usageByTurn.set(key, attempts);
  return usageTotal(key) ?? { inputTokens: 0, outputTokens: 0 };
}

function usageTotal(key: string): StreamUsage | null {
  const attempts = usageByTurn.get(key);
  if (!attempts || attempts.size === 0) return null;
  let inputTokens = 0;
  let outputTokens = 0;
  for (const usage of attempts.values()) {
    inputTokens += usage.inputTokens;
    outputTokens += usage.outputTokens;
  }
  return { inputTokens, outputTokens };
}

export function modelStreamOutput(
  sessionId: string,
  turnId: string | undefined,
): ModelStreamOutput | null {
  if (!turnId) return null;
  return outputs().get(outputKey(sessionId, turnId)) ?? null;
}

export function isModelStreamOutputDurable(
  output: ModelStreamOutput | null,
  durableRoundIds: ReadonlySet<string>,
): boolean {
  return output !== null && durableRoundIds.has(output.roundId);
}

export function clearModelStreamText(sessionId: string, turnId: string | undefined) {
  if (!turnId) return;
  const key = outputKey(sessionId, turnId);
  setOutputs((current) => {
    const previous = current.get(key);
    if (!previous || (!previous.text && !previous.reasoning)) return current;
    const next = new Map(current);
    next.set(key, { ...previous, text: "", reasoning: "" });
    return next;
  });
}

export function clearModelStreamUsage(sessionId: string, turnId: string | undefined) {
  if (!turnId) return;
  const key = outputKey(sessionId, turnId);
  usageByTurn.delete(key);
  setOutputs((current) => {
    if (!current.has(key)) return current;
    const next = new Map(current);
    next.delete(key);
    return next;
  });
}

function outputKey(sessionId: string, turnId: string): string {
  return `${sessionId}:${turnId}`;
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
