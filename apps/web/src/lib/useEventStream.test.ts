import { describe, expect, test } from "bun:test";
import { SESSION_SCOPED_QUERY_KEYS, shouldApplyCursor } from "./useEventStream";

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

describe("session-scoped query invalidation coverage", () => {
  test("covers the queries the SSE snapshot omits", () => {
    const keys = SESSION_SCOPED_QUERY_KEYS.map((key) => key[0]).sort();
    expect(keys).toEqual([
      "async-tasks",
      "git-log",
      "git-status",
      "operations",
      "queued-turns",
      "session",
      "session-context",
      "session-timeline",
      "session-timeline-history",
      "sessions",
      "turn",
    ]);
  });

  test("never includes snapshot-covered keys", () => {
    const keys = new Set(SESSION_SCOPED_QUERY_KEYS.map((key) => key[0]));
    for (const covered of ["bootstrap", "system-info", "projects", "model-providers"]) {
      expect(keys.has(covered)).toBe(false);
    }
  });
});
