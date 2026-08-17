import { describe, expect, it } from "bun:test";
import { isTurnRunning } from "./turnPresentation";

describe("Turn action presentation", () => {
  it("shows Cancel for live Turn states", () => {
    for (const status of ["running", "canceling"]) {
      expect(isTurnRunning({ status })).toBe(true);
    }
  });

  it("never exposes Cancel for terminal history", () => {
    for (const status of ["completed", "failed", "canceled", "interrupted"]) {
      expect(isTurnRunning({ status })).toBe(false);
    }
  });
});
