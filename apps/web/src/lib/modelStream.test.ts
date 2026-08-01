import { describe, expect, test } from "bun:test";
import { isModelStreamOutputDurable, type ModelStreamOutput } from "./modelStream";

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
});
