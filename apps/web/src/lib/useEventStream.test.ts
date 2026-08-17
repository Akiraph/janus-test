import { describe, expect, test } from "bun:test";
import { shouldApplyCursor } from "./useEventStream";

describe("SSE cursor ordering", () => {
  test("accepts a first cursor and only advances monotonically", () => {
    expect(shouldApplyCursor(null, "10")).toBe(true);
    expect(shouldApplyCursor(10, "10")).toBe(true);
    expect(shouldApplyCursor(10, "11")).toBe(true);
    expect(shouldApplyCursor(11, "9")).toBe(false);
  });

  test("accepts frames without a durable cursor", () => {
    expect(shouldApplyCursor(42, null)).toBe(true);
    expect(shouldApplyCursor(null, "not-a-number")).toBe(false);
  });
});
