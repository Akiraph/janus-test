import { describe, expect, test } from "bun:test";
import type { TimelineItemView } from "../../../lib/api";
import { decodeSessionTimelineItem, type SessionTimelineItem } from "./sessionTimeline";
import { compressTimeline } from "./sessionTimelineCompression";

function toolItem(projection: unknown): TimelineItemView {
  return {
    id: "item-1",
    session_id: "session-1",
    turn_id: "turn-1",
    kind: "tool_call",
    source_resource_id: "tool-1",
    display_order: 1,
    projection,
    status: "active",
    version: "v1",
    created_at: "2026-07-30T00:00:00Z",
  };
}

function bashItem(id: string, command: string, status: "success" | "running"): SessionTimelineItem {
  return {
    type: "tool",
    id,
    sourceKind: "tool_call",
    turnId: "turn-1",
    createdAt: "2026-07-30T00:00:00Z",
    itemStatus: status,
    toolName: "bash",
    toolStatus: status,
    summary: {},
    view: {
      title: command,
      status,
      body: {
        kind: "command_output",
        command,
        stdout: "",
        stderr: "",
        exitCode: status === "success" ? 0 : null,
        truncated: false,
      },
      expandable: true,
      lowNoise: true,
    },
  };
}

describe("tool timeline presentation", () => {
  test("bash summary includes the command and readable output", () => {
    const item = decodeSessionTimelineItem(
      toolItem({
        tool_name: "bash",
        status: "succeeded",
        summary: {
          display: {
            version: 1,
            title: "Ran git status --short",
            status: "succeeded",
            body: {
              kind: "command_output",
              command: "git status --short",
              stdout: "M file.ts",
              stderr: "",
              exit_code: 0,
              truncated: false,
            },
          },
        },
      }),
    );

    expect(item.type).toBe("tool");
    if (item.type !== "tool") return;
    expect(item.view.title).toBe("Ran git status --short");
    expect(item.view.body).toEqual({
      kind: "command_output",
      command: "git status --short",
      stdout: "M file.ts",
      stderr: "",
      exitCode: 0,
      truncated: false,
    });
  });

  test("unknown tools retain a structured body instead of a blank row", () => {
    const item = decodeSessionTimelineItem(
      toolItem({
        tool_name: "custom_search",
        status: "succeeded",
        summary: {
          display: {
            version: 1,
            title: "Used Custom Search",
            status: "succeeded",
            body: {
              kind: "structured",
              value: { query: "cursor replay", matches: 3 },
            },
          },
        },
      }),
    );

    expect(item.type).toBe("tool");
    if (item.type !== "tool") return;
    expect(item.view.title).toBe("Used Custom Search");
    expect(item.view.expandable).toBe(true);
    expect(item.view.body).toEqual({
      kind: "structured",
      value: { query: "cursor replay", matches: 3 },
    });
  });

  test("legacy unversioned summaries are rejected instead of guessed", () => {
    const item = decodeSessionTimelineItem(
      toolItem({ tool_name: "bash", status: "succeeded", summary: { command: "pwd" } }),
    );

    expect(item.type).toBe("tool");
    if (item.type !== "tool") return;
    expect(item.view.title).toBe("Invalid Tool output");
    expect(item.view.status).toBe("failure");
  });

  test("compressed searches use the search verb", () => {
    const items = compressTimeline([
      bashItem("tool-1", "rg cursor src", "success"),
      bashItem("tool-2", "grep -R replay apps", "success"),
    ]);

    expect(items).toHaveLength(1);
    expect(items[0]?.type).toBe("tool");
    if (items[0]?.type !== "tool") return;
    expect(items[0].view.title).toBe("Searched for 2 patterns");
    expect(items[0].view.status).toBe("success");
  });

  test("a compressed span stays in progress while its last command runs", () => {
    const items = compressTimeline([
      bashItem("tool-1", "bun test", "success"),
      bashItem("tool-2", "cargo clippy", "running"),
    ]);

    expect(items).toHaveLength(1);
    expect(items[0]?.type).toBe("tool");
    if (items[0]?.type !== "tool") return;
    expect(items[0].view.title).toBe("Running 2 commands");
    expect(items[0].view.status).toBe("running");
  });
});
