import { createSignal } from "solid-js";

export interface StreamUsage {
  inputTokens: number;
  outputTokens: number;
}

export interface ModelStreamOutput {
  roundId: string;
  attemptId: string;
  sequence: number;
  text: string;
  reasoning: string;
  /** Latest usage reported by the provider for the current attempt. */
  usage: StreamUsage | null;
  /** Wall-clock ms (Date.now()) when the first reasoning delta arrived.
   * Used to render "thinking for Xs" once the elapsed time exceeds 5s. */
  reasoningFirstSeenAt: number | null;
  /** Wall-clock ms when the first assistant text delta arrived. */
  textFirstSeenAt: number | null;
}

const MAX_RETAINED_OUTPUTS = 32;
const [outputs, setOutputs] = createSignal<ReadonlyMap<string, ModelStreamOutput>>(new Map());

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
    // Same-attempt out-of-order or duplicate deltas are dropped.
    if (previous?.attemptId === attemptId && sequence <= previous.sequence && !usage) {
      return current;
    }

    const next = new Map(current);
    next.delete(key);

    const sameAttempt = previous?.attemptId === attemptId;
    const previousText = sameAttempt ? previous.text : "";
    const previousReasoning = sameAttempt ? previous.reasoning : "";
    const previousUsage = sameAttempt ? previous.usage : null;
    const previousReasoningAt = sameAttempt ? previous.reasoningFirstSeenAt : null;
    const previousTextAt = sameAttempt ? previous.textFirstSeenAt : null;

    next.set(key, {
      roundId,
      attemptId,
      sequence,
      text: sameAttempt ? previousText + (isReasoning ? "" : delta) : isReasoning ? "" : delta,
      reasoning: sameAttempt
        ? previousReasoning + (isReasoning ? delta : "")
        : isReasoning
          ? delta
          : "",
      usage: usage ?? previousUsage,
      reasoningFirstSeenAt:
        isReasoning && previousReasoningAt === null ? Date.now() : previousReasoningAt,
      textFirstSeenAt:
        !isReasoning && delta && previousTextAt === null ? Date.now() : previousTextAt,
    });
    while (next.size > MAX_RETAINED_OUTPUTS) {
      const oldest = next.keys().next().value as string | undefined;
      if (!oldest) break;
      next.delete(oldest);
    }
    return next;
  });
}

function parseUsage(value: unknown): StreamUsage | null {
  if (!isRecord(value)) return null;
  const inputTokens = numberValue(value.input_tokens);
  const outputTokens = numberValue(value.output_tokens);
  if (inputTokens === null && outputTokens === null) return null;
  return {
    inputTokens: inputTokens ?? 0,
    outputTokens: outputTokens ?? 0,
  };
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
