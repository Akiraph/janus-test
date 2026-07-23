/// <reference types="bun" />

import { describe, expect, test } from "bun:test";
import { parseCliOutputLine } from "./CliJobsSection";

describe("parseCliOutputLine", () => {
  test("drops Codex item lifecycle events without displayable payload", () => {
    expect(
      parseCliOutputLine(
        JSON.stringify({
          type: "item.started",
          item: { type: "reasoning", status: "in_progress" },
        }),
      ),
    ).toBeUndefined();
    expect(
      parseCliOutputLine(
        JSON.stringify({
          type: "item.completed",
          item: { type: "reasoning", status: "completed" },
        }),
      ),
    ).toBeUndefined();
  });

  test("extracts displayable content from completed Codex items", () => {
    expect(
      parseCliOutputLine(
        JSON.stringify({
          type: "item.completed",
          item: {
            type: "message",
            content: [{ type: "output_text", text: "Changed the tests." }],
          },
        }),
      ),
    ).toBe("Changed the tests.");
    expect(
      parseCliOutputLine(
        JSON.stringify({
          type: "item.completed",
          item: { type: "function_call", name: "shell" },
        }),
      ),
    ).toBe("Tool use: shell");
  });
});
