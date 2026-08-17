import { createSignal } from "solid-js";

export interface StreamUsage {
  inputTokens: number;
  outputTokens: number;
}

export type StreamDirection = "upload" | "download";

export interface ModelStreamOutput {
  roundId: string;
  attemptId: string;
  sequence: number;
  text: string;
  reasoning: string;
  usage: StreamUsage | null;
  reasoningFirstSeenAt: number | null;
  textFirstSeenAt: number | null;
  /** Server-measured thinking duration (ms), present from the first answer delta. */
  reasoningDurationMs: number | null;
  /** Total non-cached exchange tokens for the whole Turn so far. */
  turnExchangeTokens?: number | null;
  /** Input/upload portion of the authoritative whole-Turn exchange. */
  turnInputTokens?: number | null;
  /** Output/download portion of the authoritative whole-Turn exchange. */
  turnOutputTokens?: number | null;
  /** Direction of the current model exchange. */
  direction?: StreamDirection | null;
}

export interface RetryState {
  attemptId: string;
  attempt: number;
  detail: string;
  retryAt: number;
}

const MAX_RETAINED_OUTPUTS = 32;
const [outputs, setOutputs] = createSignal<ReadonlyMap<string, ModelStreamOutput>>(new Map());
const [retryStates, setRetryStates] = createSignal<ReadonlyMap<string, RetryState>>(new Map());
interface RetainedUsage {
  attemptKey: string;
  usage: StreamUsage;
}

// Provider usage frames are cumulative for one model attempt. Retain only the
// current round/attempt and normalize input to non-cached exchange tokens so a
// retry or a new round cannot make the status line jump by summing old frames.
const usageByTurn = new Map<string, RetainedUsage>();

const SUMMARY_START =
  /^(?:summarizing|detailing|outlining|inspecting|reviewing|checking|planning|searching|reading|analyzing|comparing|verifying|tracing|exploring|preparing|updating|implementing|testing|running)\b/i;

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

export function normalizeReasoningSummary(value: string): string {
  return value.replace(
    /([a-z0-9)])(?=(?:Summarizing|Detailing|Outlining|Inspecting|Reviewing|Checking|Planning|Searching|Reading|Analyzing|Comparing|Verifying|Tracing|Exploring|Preparing|Updating|Implementing|Testing|Running)\b)/g,
    "$1\n",
  );
}

interface StreamTextPayload {
  text?: string;
  reasoning?: string;
  seq?: number;
  round_id?: string;
  usage?: { input_tokens?: number; output_tokens?: number; cache_tokens?: number } | null;
  turn_input_tokens?: number;
  turn_output_tokens?: number;
  turn_exchange_tokens?: number;
  direction?: StreamDirection;
  reasoning_duration_ms?: number | null;
  retrying?: boolean;
  attempt?: number;
  detail?: string;
  retry_after_ms?: number;
  attempt_id?: string;
}

/** Listen for stream-text CustomEvents dispatched by useEventStream. */
export function initStreamTextListener(eventTarget?: EventTarget) {
  const target = eventTarget ?? (typeof window !== "undefined" ? window : null);
  if (!target) return;
  const handler = (event: Event) => {
    const detail = (event as CustomEvent).detail as
      | { id: string; data: StreamTextPayload }
      | undefined;
    if (!detail) return;
    const { id: key, data } = detail;
    if (!key || !data) return;
    const [sessionId, turnId] = key.split(":");
    if (!sessionId || !turnId) return;

    if (data.retrying) {
      // Retry state update
      setRetryStates((current) => {
        const next = new Map(current);
        const k = retryKey(sessionId, turnId);
        if (data.attempt && data.retry_after_ms != null && data.attempt_id) {
          next.set(k, {
            attemptId: data.attempt_id,
            attempt: data.attempt,
            detail: data.detail ?? "",
            retryAt: Date.now() + data.retry_after_ms,
          });
        } else {
          next.delete(k);
        }
        while (next.size > MAX_RETAINED_OUTPUTS) {
          const oldest = next.keys().next().value as string | undefined;
          if (!oldest) break;
          next.delete(oldest);
        }
        return next;
      });
      // Reset text on retry
      setOutputs((current) => {
        const prev = current.get(key);
        if (!prev) return current;
        const next = new Map(current);
        next.set(key, {
          ...prev,
          text: "",
          reasoning: "",
          reasoningDurationMs: null,
          direction: data.direction ?? "upload",
          ...turnTokenFields(data, prev),
        });
        return next;
      });
      usageByTurn.delete(key);
      return;
    }

    // Regular stream delta — text is already accumulated from the server
    const seq = data.seq ?? 0;
    const fullText = data.text ?? "";
    const fullReasoning = data.reasoning ?? "";
    const usage = parseUsage(data.usage ?? null);
    const reasoningDurationMs = numberValue(data.reasoning_duration_ms ?? null);
    const roundId = data.round_id ?? "stream";
    const attemptId = data.attempt_id ?? "stream";
    const direction = data.direction ?? (fullText || fullReasoning ? "download" : undefined);

    setOutputs((current) => {
      const previous = current.get(key);
      const next = new Map(current);
      next.delete(key);

      // Server pushes accumulated text, so we use it directly. The round_id is
      // the same value the durable assistant timeline item carries, so once
      // that item appears isModelStreamOutputDurable can retire this overlay.
      const previousReasoningAt = previous?.reasoningFirstSeenAt ?? null;
      const previousTextAt = previous?.textFirstSeenAt ?? null;
      const totalUsage = usage ? recordUsage(key, roundId, attemptId, usage) : usageTotal(key);

      next.set(key, {
        roundId,
        attemptId: "stream",
        sequence: seq,
        text: fullText,
        reasoning: fullReasoning,
        usage: totalUsage,
        reasoningFirstSeenAt:
          fullReasoning && previousReasoningAt === null ? Date.now() : previousReasoningAt,
        textFirstSeenAt: fullText && previousTextAt === null ? Date.now() : previousTextAt,
        reasoningDurationMs: reasoningDurationMs ?? previous?.reasoningDurationMs ?? null,
        ...turnTokenFields(data, previous),
        direction: direction ?? previous?.direction ?? null,
      });
      while (next.size > MAX_RETAINED_OUTPUTS) {
        const oldest = next.keys().next().value as string | undefined;
        if (!oldest) break;
        next.delete(oldest);
        usageByTurn.delete(oldest);
      }
      return next;
    });
  };

  target.addEventListener("janus:stream-text", handler);
  return () => target.removeEventListener("janus:stream-text", handler);
}

function parseUsage(value: unknown): StreamUsage | null {
  if (!isRecord(value)) return null;
  const inputTokens = numberValue(value.input_tokens);
  const outputTokens = numberValue(value.output_tokens);
  if (inputTokens === null && outputTokens === null) return null;
  return {
    // The Rust provider contract already makes input_tokens non-cached. Keep
    // cache_tokens as accounting metadata only; subtracting it here would
    // count cached input twice and make the live indicator jump backwards.
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
  const attemptKey = JSON.stringify([roundId, attemptId]);
  const previous = usageByTurn.get(key);
  const retained =
    previous?.attemptKey === attemptKey
      ? {
          inputTokens: Math.max(previous.usage.inputTokens, usage.inputTokens),
          outputTokens: Math.max(previous.usage.outputTokens, usage.outputTokens),
        }
      : usage;
  usageByTurn.set(key, { attemptKey, usage: retained });
  return retained;
}

function usageTotal(key: string): StreamUsage | null {
  return usageByTurn.get(key)?.usage ?? null;
}

function turnTokenFields(
  data: StreamTextPayload,
  previous: ModelStreamOutput | undefined,
): Pick<ModelStreamOutput, "turnExchangeTokens" | "turnInputTokens" | "turnOutputTokens"> {
  const input = numberValue(data.turn_input_tokens);
  const output = numberValue(data.turn_output_tokens);
  const exchange = numberValue(data.turn_exchange_tokens);
  const previousInput = previous?.turnInputTokens ?? null;
  const previousOutput = previous?.turnOutputTokens ?? null;
  const previousExchange = previous?.turnExchangeTokens ?? null;
  const nextInput = input === null ? previousInput : Math.max(previousInput ?? 0, input);
  const nextOutput = output === null ? previousOutput : Math.max(previousOutput ?? 0, output);
  const nextExchange =
    exchange === null
      ? nextInput !== null || nextOutput !== null
        ? (nextInput ?? 0) + (nextOutput ?? 0)
        : previousExchange
      : Math.max(previousExchange ?? 0, exchange);
  return {
    turnInputTokens: nextInput,
    turnOutputTokens: nextOutput,
    turnExchangeTokens: nextExchange,
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

export function retryState(sessionId: string, turnId: string | undefined): RetryState | null {
  if (!turnId) return null;
  return retryStates().get(retryKey(sessionId, turnId)) ?? null;
}

export function clearRetryState(sessionId: string, turnId: string | undefined): void {
  if (!turnId) return;
  const k = retryKey(sessionId, turnId);
  setRetryStates((current) => {
    if (!current.has(k)) return current;
    const next = new Map(current);
    next.delete(k);
    return next;
  });
}

function outputKey(sessionId: string, turnId: string): string {
  return `${sessionId}:${turnId}`;
}

function retryKey(sessionId: string, turnId: string): string {
  return `${sessionId}:${turnId}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function numberValue(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : null;
}
