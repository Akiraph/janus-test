import { describe, expect, test } from "bun:test";
import type { TimelineItemView } from "../../lib/api";
import {
  decodeSessionTimeline,
  decodeSessionTimelineItem,
  formatThoughtDuration,
  type SessionTimelineItem,
} from "./sessionTimeline";
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
    turnStatus: null,
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

function thoughtItem(id: string, durationMs: number): SessionTimelineItem {
  return {
    type: "assistant",
    id,
    sourceKind: "assistant_message",
    turnId: "turn-1",
    createdAt: "2026-07-30T00:00:00.000Z",
    itemStatus: "settled",
    turnStatus: null,
    text: "",
    reasoning: "Inspecting the workspace before responding.",
    roundId: "round-1",
    durationMs,
  };
}

describe("tool timeline presentation", () => {
  test("completed thoughts always have a non-empty duration label", () => {
    expect(formatThoughtDuration(null)).toBe("for a while");
    expect(formatThoughtDuration(0)).toBe("for a while");
    expect(formatThoughtDuration(4_200)).toBe("for 4s");
  });

  test("reuses unchanged decoded timeline rows across polling", () => {
    const first = decodeSessionTimeline([toolItem({ tool_name: "bash", status: "succeeded" })]);
    const second = decodeSessionTimeline(
      [toolItem({ tool_name: "bash", status: "succeeded" })],
      first,
    );
    const changed = decodeSessionTimeline(
      [{ ...toolItem({ tool_name: "bash", status: "failed" }), version: "v2" }],
      second,
    );

    expect(second[0]).toBe(first[0]);
    expect(changed[0]).not.toBe(second[0]);
  });

  test("rebuilds a row when only its joined Turn status changes", () => {
    const base = toolItem({ tool_name: "bash", status: "succeeded" });
    const first = decodeSessionTimeline([
      {
        ...base,
        turn_status: {
          id: "turn-1",
          status: "running",
          cancellation_reason: null,
          completion_reason: null,
          created_at: "2026-07-30T00:00:00Z",
          updated_at: "2026-07-30T00:00:01Z",
        },
      },
    ]);
    const second = decodeSessionTimeline(
      [
        {
          ...base,
          turn_status: {
            id: "turn-1",
            status: "completed",
            cancellation_reason: null,
            completion_reason: "finish",
            created_at: "2026-07-30T00:00:00Z",
            updated_at: "2026-07-30T00:00:02Z",
          },
        },
      ],
      first,
    );

    expect(second[0]).not.toBe(first[0]);
    expect(second[0]?.turnStatus?.status).toBe("completed");
  });

  test("reuses unchanged compressed groups across polling", () => {
    const source = [
      bashItem("tool-1", "echo --- && ls -la", "success"),
      bashItem("tool-2", "cat README.md", "success"),
    ];
    const first = compressTimeline(source);
    const second = compressTimeline(source, first);

    expect(second[0]).toBe(first[0]);
  });

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

  test("invalid display summaries expose the attempted tool", () => {
    const item = decodeSessionTimelineItem(
      toolItem({ tool_name: "bash", status: "succeeded", summary: { command: "pwd" } }),
    );

    expect(item.type).toBe("tool");
    if (item.type !== "tool") return;
    expect(item.view.title).toBe("Ran pwd");
    expect(item.view.status).toBe("failure");
  });

  test("failed tools keep the attempted call title and expose the error detail", () => {
    const item = decodeSessionTimelineItem(
      toolItem({
        tool_name: "",
        status: "failed",
        summary: {
          display: {
            version: 1,
            title: "Used",
            status: "failed",
            body: {
              kind: "error",
              code: "TOOL_NOT_ALLOWED",
              detail: "The requested tool is not registered.",
            },
          },
        },
      }),
    );

    expect(item.type).toBe("tool");
    if (item.type !== "tool") return;
    expect(item.view.title).toBe("Ran tool");
    expect(item.view.expandable).toBe(true);
  });

  test("generic failure titles are replaced by the attempted bash command", () => {
    const item = decodeSessionTimelineItem(
      toolItem({
        tool_name: "bash",
        status: "failed",
        summary: {
          command: "git status --short",
          display: {
            version: 1,
            title: "Tool error",
            status: "failed",
            body: {
              kind: "error",
              code: "PROCESS_FAILED",
              detail: "process exited before producing a result",
            },
          },
        },
      }),
    );

    expect(item.type).toBe("tool");
    if (item.type !== "tool") return;
    expect(item.view.title).toBe("Ran git status --short");
    expect(item.view.body).toMatchObject({ kind: "error" });
  });

  test("only read-only compound bash calls are low-noise", () => {
    const readOnly = decodeSessionTimelineItem(
      toolItem({
        tool_name: "bash",
        summary: {
          display: {
            version: 1,
            title: "Ran workspace inspection",
            status: "succeeded",
            body: {
              kind: "command_output",
              command: "pwd && echo --- && ls -la",
              stdout: "",
              stderr: "",
              exit_code: 0,
              truncated: false,
            },
          },
        },
      }),
    );
    const write = decodeSessionTimelineItem(
      toolItem({
        tool_name: "bash",
        summary: {
          display: {
            version: 1,
            title: "Changed a file",
            status: "succeeded",
            body: {
              kind: "command_output",
              command: "echo content > file.txt",
              stdout: "",
              stderr: "",
              exit_code: 0,
              truncated: false,
            },
          },
        },
      }),
    );

    expect(readOnly.type === "tool" && readOnly.view.lowNoise).toBe(true);
    expect(write.type === "tool" && write.view.lowNoise).toBe(false);
  });

  test("ignores safe stderr redirection and shell plumbing", () => {
    const item = decodeSessionTimelineItem(
      toolItem({
        tool_name: "bash",
        summary: {
          display: {
            version: 1,
            title: "Inspected the environment",
            status: "succeeded",
            body: {
              kind: "command_output",
              command: "pwd && ls -la /workspace 2>/dev/null && env | sort",
              stdout: "",
              stderr: "",
              exit_code: 0,
              truncated: false,
            },
          },
        },
      }),
    );

    expect(item.type === "tool" && item.view.lowNoise).toBe(true);
  });

  test("does not compress a standalone echo but keeps echo plus ls compressible", () => {
    const echo = decodeSessionTimelineItem(
      toolItem({
        tool_name: "bash",
        summary: {
          display: {
            version: 1,
            title: "Printed a separator",
            status: "succeeded",
            body: {
              kind: "command_output",
              command: "echo ---",
              stdout: "---",
              stderr: "",
              exit_code: 0,
              truncated: false,
            },
          },
        },
      }),
    );
    const compound = decodeSessionTimelineItem(
      toolItem({
        tool_name: "bash",
        summary: {
          display: {
            version: 1,
            title: "Listed the workspace",
            status: "succeeded",
            body: {
              kind: "command_output",
              command: "echo --- && ls -la",
              stdout: "",
              stderr: "",
              exit_code: 0,
              truncated: false,
            },
          },
        },
      }),
    );

    expect(echo.type === "tool" && echo.view.lowNoise).toBe(false);
    expect(compound.type === "tool" && compound.view.lowNoise).toBe(true);
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

  test("compresses mixed safe bash activities into one detailed summary", () => {
    const items = compressTimeline([
      bashItem("tool-1", "echo --- && ls -la", "success"),
      bashItem("tool-2", "cat README.md", "success"),
      bashItem("tool-3", "rg workspace apps", "success"),
    ]);

    expect(items).toHaveLength(1);
    expect(items[0]?.type).toBe("tool");
    if (items[0]?.type !== "tool") return;
    expect(items[0].view.title).toBe("Read 1 file, searched for 1 pattern, and listed 1 directory");
    expect(items[0].view.body.kind).toBe("activity");
  });

  test("groups a completed thought with following low-noise tools", () => {
    const items = compressTimeline([
      thoughtItem("thought-1", 5000),
      bashItem("tool-1", "cat README.md", "success"),
    ]);

    expect(items).toHaveLength(1);
    expect(items[0]?.type).toBe("tool");
    if (items[0]?.type !== "tool") return;
    expect(items[0].view.title).toBe("Thought for 5s and read 1 file");
    expect(items[0].view.body.kind).toBe("activity");
  });

  test("uses present-tense activity while the latest compressed tool is running", () => {
    const items = compressTimeline([
      thoughtItem("thought-1", 5000),
      bashItem("tool-1", "cat README.md", "running"),
    ]);

    expect(items).toHaveLength(1);
    expect(items[0]?.type).toBe("tool");
    if (items[0]?.type !== "tool") return;
    expect(items[0].view.title).toBe("Thinking for 5s and reading 1 file");
  });

  test("does not compress unrelated bash commands", () => {
    const items = compressTimeline([
      bashItem("tool-1", "bun test", "success"),
      bashItem("tool-2", "cargo clippy", "running"),
    ]);

    expect(items).toHaveLength(2);
  });
});
