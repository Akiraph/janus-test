/// <reference types="bun" />

import { describe, expect, test } from "bun:test";
import type { ThoughtConversationItem } from "../types";
import { formatCompletedThoughtTitle } from "./thought-title";

describe("formatCompletedThoughtTitle", () => {
  test("uses a generic title for sub-second thoughts", () => {
    expect(
      formatCompletedThoughtTitle(
        thought({
          startedAt: "2026-07-05T00:00:00.000Z",
          completedAt: "2026-07-05T00:00:00.400Z",
        }),
      ),
    ).toBe("Thought for a while");
  });

  test("keeps duration labels for longer thoughts", () => {
    expect(
      formatCompletedThoughtTitle(
        thought({
          startedAt: "2026-07-05T00:00:00.000Z",
          completedAt: "2026-07-05T00:00:05.000Z",
        }),
      ),
    ).toBe("Thought for 5s");
  });
});

function thought(
  patch: Partial<ThoughtConversationItem>,
): ThoughtConversationItem {
  return {
    kind: "thought",
    id: "thought-1",
    title: "Thinking",
    text: "Reasoning.",
    at: "2026-07-05T00:00:00.000Z",
    status: "completed",
    ...patch,
  };
}
