import { describe, expect, test } from "bun:test";
import {
  clearModelStreamText,
  clearModelStreamUsage,
  isModelStreamOutputDurable,
  type ModelStreamOutput,
  modelStreamOutput,
  initStreamTextListener,
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
};

/** Dispatch a stream-text custom event as the server would push it. */
function dispatchStreamText(id: string, data: Record<string, unknown>, target: EventTarget) {
  target.dispatchEvent(
    new CustomEvent("janus:stream-text", { detail: { id, data } }),
  );
}

describe("model stream projection handoff", () => {
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
      dispatchStreamText("session-summary:turn-summary", {
        text: "",
        reasoning: "Summarizing visible harness capabilities",
        seq: 1,
      }, target);
      dispatchStreamText("session-summary:turn-summary", {
        text: "",
        reasoning: "Summarizing visible harness capabilities\nDetailing harness tool capabilities and constraints",
        seq: 2,
      }, target);
      dispatchStreamText("session-summary:turn-summary", {
        text: "",
        reasoning: "Summarizing visible harness capabilities\nDetailing harness tool capabilities and constraints\nOutlining harness behavioral contract",
        seq: 3,
      }, target);

      expect(modelStreamOutput("session-summary", "turn-summary")?.reasoning).toBe(
        "Summarizing visible harness capabilities\nDetailing harness tool capabilities and constraints\nOutlining harness behavioral contract",
      );
    } finally {
      cleanup?.();
    }
  });

  test("accumulates Turn usage from latest stream delta", () => {
    const target = new EventTarget();
    const cleanup = initStreamTextListener(target);
    try {
      dispatchStreamText("session-usage:turn-usage", {
        text: "first",
        reasoning: "",
        seq: 1,
        usage: { input_tokens: 100, output_tokens: 4, cache_tokens: 900 },
      }, target);
      dispatchStreamText("session-usage:turn-usage", {
        text: "first response",
        reasoning: "",
        seq: 2,
        usage: { input_tokens: 100, output_tokens: 7, cache_tokens: 900 },
      }, target);

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
});