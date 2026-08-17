import { describe, expect, test } from "bun:test";
import {
  clearModelStreamText,
  clearModelStreamUsage,
  initStreamTextListener,
  isModelStreamOutputDurable,
  type ModelStreamOutput,
  modelStreamOutput,
} from "./modelStream";

const output: ModelStreamOutput = {
  roundId: "stream",
  attemptId: "stream",
  sequence: 7,
  text: "new answer",
  reasoning: "new thought",
  usage: null,
  reasoningFirstSeenAt: null,
  textFirstSeenAt: null,
  reasoningDurationMs: null,
};

/** Dispatch a stream-text custom event as the server would push it. */
function dispatchStreamText(id: string, data: Record<string, unknown>, target: EventTarget) {
  target.dispatchEvent(new CustomEvent("janus:stream-text", { detail: { id, data } }));
}

describe("model stream projection lifecycle", () => {
  test("only the matching durable round suppresses provisional output", () => {
    expect(isModelStreamOutputDurable(output, new Set(["round-1"]))).toBe(false);
    expect(isModelStreamOutputDurable(output, new Set(["round-1", "round-2"]))).toBe(false);
    // The new stream protocol uses roundId "stream", not "round-2"
    expect(isModelStreamOutputDurable(output, new Set(["stream"]))).toBe(true);
  });

  test("missing provisional output is never treated as durable", () => {
    expect(isModelStreamOutputDurable(null, new Set(["round-2"]))).toBe(false);
  });

  test("reasoning status summaries use accumulated text from server", () => {
    const target = new EventTarget();
    const cleanup = initStreamTextListener(target);
    try {
      // Server sends accumulated text on each frame
      dispatchStreamText(
        "session-summary:turn-summary",
        {
          text: "",
          reasoning: "Summarizing visible harness capabilities",
          seq: 1,
        },
        target,
      );
      dispatchStreamText(
        "session-summary:turn-summary",
        {
          text: "",
          reasoning:
            "Summarizing visible harness capabilities\nDetailing harness tool capabilities and constraints",
          seq: 2,
        },
        target,
      );
      dispatchStreamText(
        "session-summary:turn-summary",
        {
          text: "",
          reasoning:
            "Summarizing visible harness capabilities\nDetailing harness tool capabilities and constraints\nOutlining harness behavioral contract",
          seq: 3,
        },
        target,
      );

      expect(modelStreamOutput("session-summary", "turn-summary")?.reasoning).toBe(
        "Summarizing visible harness capabilities\nDetailing harness tool capabilities and constraints\nOutlining harness behavioral contract",
      );
    } finally {
      cleanup?.();
    }
  });

  test("keeps provider non-cached usage for the current model exchange", () => {
    const target = new EventTarget();
    const cleanup = initStreamTextListener(target);
    try {
      dispatchStreamText(
        "session-usage:turn-usage",
        {
          text: "first",
          reasoning: "",
          seq: 1,
          usage: { input_tokens: 100, output_tokens: 4, cache_tokens: 900 },
        },
        target,
      );
      dispatchStreamText(
        "session-usage:turn-usage",
        {
          text: "first response",
          reasoning: "",
          seq: 2,
          usage: { input_tokens: 100, output_tokens: 7, cache_tokens: 900 },
        },
        target,
      );

      expect(modelStreamOutput("session-usage", "turn-usage")?.usage).toEqual({
        inputTokens: 100,
        outputTokens: 7,
      });
      clearModelStreamText("session-usage", "turn-usage");
      clearModelStreamUsage("session-usage", "turn-usage");
    } finally {
      cleanup?.();
    }
  });

  test("retains the whole turn exchange and its current direction", () => {
    const target = new EventTarget();
    const cleanup = initStreamTextListener(target);
    try {
      dispatchStreamText(
        "session-total:turn-total",
        {
          round_id: "round-1",
          direction: "upload",
          turn_input_tokens: 120,
          turn_output_tokens: 0,
          turn_exchange_tokens: 120,
          seq: 0,
        },
        target,
      );
      expect(modelStreamOutput("session-total", "turn-total")).toMatchObject({
        turnExchangeTokens: 120,
        direction: "upload",
      });

      dispatchStreamText(
        "session-total:turn-total",
        {
          round_id: "round-1",
          direction: "download",
          usage: { input_tokens: 120, output_tokens: 9, cache_tokens: 500 },
          turn_input_tokens: 120,
          turn_output_tokens: 9,
          turn_exchange_tokens: 129,
          text: "done",
          seq: 1,
        },
        target,
      );
      expect(modelStreamOutput("session-total", "turn-total")).toMatchObject({
        turnExchangeTokens: 129,
        turnInputTokens: 120,
        turnOutputTokens: 9,
        direction: "download",
        usage: { inputTokens: 120, outputTokens: 9 },
      });
      clearModelStreamUsage("session-total", "turn-total");
    } finally {
      cleanup?.();
    }
  });

  test("keeps authoritative turn totals monotonic across replayed frames", () => {
    const target = new EventTarget();
    const cleanup = initStreamTextListener(target);
    try {
      dispatchStreamText(
        "session-monotonic:turn-monotonic",
        { turn_input_tokens: 80, turn_output_tokens: 10, turn_exchange_tokens: 90, seq: 2 },
        target,
      );
      dispatchStreamText(
        "session-monotonic:turn-monotonic",
        { turn_input_tokens: 20, turn_output_tokens: 2, turn_exchange_tokens: 22, seq: 1 },
        target,
      );
      expect(modelStreamOutput("session-monotonic", "turn-monotonic")).toMatchObject({
        turnInputTokens: 80,
        turnOutputTokens: 10,
        turnExchangeTokens: 90,
      });
      clearModelStreamUsage("session-monotonic", "turn-monotonic");
    } finally {
      cleanup?.();
    }
  });

  test("does not carry usage from an earlier round or retry", () => {
    const target = new EventTarget();
    const cleanup = initStreamTextListener(target);
    try {
      dispatchStreamText(
        "session-rounds:turn-rounds",
        {
          round_id: "round-1",
          text: "first",
          usage: { input_tokens: 100, output_tokens: 4, cache_tokens: 0 },
          seq: 1,
        },
        target,
      );
      dispatchStreamText(
        "session-rounds:turn-rounds",
        {
          round_id: "round-2",
          text: "second",
          usage: { input_tokens: 20, output_tokens: 2, cache_tokens: 0 },
          seq: 1,
        },
        target,
      );

      expect(modelStreamOutput("session-rounds", "turn-rounds")?.usage).toEqual({
        inputTokens: 20,
        outputTokens: 2,
      });
      clearModelStreamUsage("session-rounds", "turn-rounds");
    } finally {
      cleanup?.();
    }
  });

  test("server reasoning duration arrives with the first answer delta", () => {
    const target = new EventTarget();
    const cleanup = initStreamTextListener(target);
    try {
      // Thinking phase: no duration yet.
      dispatchStreamText(
        "session-think:turn-think",
        {
          text: "",
          reasoning: "inspecting the workspace",
          seq: 1,
        },
        target,
      );
      expect(modelStreamOutput("session-think", "turn-think")?.reasoningDurationMs).toBe(null);

      // First answer delta carries the server-measured duration.
      dispatchStreamText(
        "session-think:turn-think",
        {
          text: "done",
          reasoning: "inspecting the workspace",
          seq: 2,
          reasoning_duration_ms: 14_200,
        },
        target,
      );
      expect(modelStreamOutput("session-think", "turn-think")?.reasoningDurationMs).toBe(14_200);
      clearModelStreamText("session-think", "turn-think");
    } finally {
      cleanup?.();
    }
  });
});
