import { describe, expect, test } from "bun:test";
import {
  clearModelStreamText,
  clearModelStreamUsage,
  ingestModelStreamEvent,
  isModelStreamOutputDurable,
  type ModelStreamOutput,
  modelStreamOutput,
} from "./modelStream";

const output: ModelStreamOutput = {
  roundId: "round-2",
  attemptId: "attempt-3",
  sequence: 7,
  text: "new answer",
  reasoning: "new thought",
  usage: null,
  reasoningFirstSeenAt: null,
  textFirstSeenAt: null,
};

describe("model stream projection handoff", () => {
  test("only the matching durable round suppresses provisional output", () => {
    expect(isModelStreamOutputDurable(output, new Set(["round-1"]))).toBe(false);
    expect(isModelStreamOutputDurable(output, new Set(["round-1", "round-2"]))).toBe(true);
  });

  test("missing provisional output is never treated as durable", () => {
    expect(isModelStreamOutputDurable(null, new Set(["round-2"]))).toBe(false);
  });

  test("reasoning status summaries stay on separate lines", () => {
    const base = {
      event_type: "model.stream_delta",
      session_id: "session-summary",
      turn_id: "turn-summary",
      round_id: "round-summary",
      attempt_id: "attempt-summary",
      provisional: true,
      channel: "reasoning_summary",
    };
    ingestModelStreamEvent("model.stream_delta", {
      ...base,
      sequence: 1,
      delta: "Summarizing visible harness capabilities",
    });
    ingestModelStreamEvent("model.stream_delta", {
      ...base,
      sequence: 2,
      delta: "Detailing harness tool capabilities and constraints",
    });
    ingestModelStreamEvent("model.stream_delta", {
      ...base,
      sequence: 3,
      delta: "Outlining harness behavioral contract in Chinese",
    });

    expect(modelStreamOutput("session-summary", "turn-summary")?.reasoning).toBe(
      "Summarizing visible harness capabilities\nDetailing harness tool capabilities and constraints\nOutlining harness behavioral contract in Chinese",
    );
  });

  test("accumulates Turn usage across rounds and deduplicates snapshots", () => {
    const base = {
      event_type: "model.stream_delta",
      session_id: "session-usage",
      turn_id: "turn-usage",
      provisional: true,
      channel: "text",
    };
    ingestModelStreamEvent("model.stream_delta", {
      ...base,
      round_id: "round-1",
      attempt_id: "attempt-1",
      sequence: 1,
      delta: "first",
      usage: { input_tokens: 100, output_tokens: 4, cache_tokens: 900 },
    });
    ingestModelStreamEvent("model.stream_delta", {
      ...base,
      round_id: "round-1",
      attempt_id: "attempt-1",
      sequence: 2,
      delta: " response",
      usage: { input_tokens: 100, output_tokens: 7, cache_tokens: 900 },
    });
    ingestModelStreamEvent("model.stream_delta", {
      ...base,
      round_id: "round-1",
      attempt_id: "attempt-1",
      sequence: 2,
      delta: " response",
      usage: { input_tokens: 100, output_tokens: 7, cache_tokens: 900 },
    });
    ingestModelStreamEvent("model.stream_delta", {
      ...base,
      round_id: "round-2",
      attempt_id: "attempt-2",
      sequence: 1,
      delta: "second",
      usage: { input_tokens: 120, output_tokens: 3, cache_tokens: 1_000 },
    });

    expect(modelStreamOutput("session-usage", "turn-usage")?.usage).toEqual({
      inputTokens: 220,
      outputTokens: 10,
    });
    clearModelStreamText("session-usage", "turn-usage");
    clearModelStreamUsage("session-usage", "turn-usage");
  });
});
