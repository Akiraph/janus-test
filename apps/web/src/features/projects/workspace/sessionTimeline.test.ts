import { describe, expect, test } from "bun:test";
import type { TimelineItemView } from "../../../lib/api";
import {
  decodeSessionTimeline,
  decodeSessionTimelineItem,
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

  test("legacy unversioned summaries are rejected instead of guessed", () => {
    const item = decodeSessionTimelineItem(
      toolItem({ tool_name: "bash", status: "succeeded", summary: { command: "pwd" } }),
    );

    expect(item.type).toBe("tool");
    if (item.type !== "tool") return;
    expect(item.view.title).toBe("Invalid Tool output");
    expect(item.view.status).toBe("failure");
  });

  test("failed tools show the machine-readable error without another expand step", () => {
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
    expect(item.view.title).toBe("Tool error: TOOL_NOT_ALLOWED");
    expect(item.view.expandable).toBe(false);
  });

  test("failed ask_user calls do not render as interactive Ask cards", () => {
    const item = decodeSessionTimelineItem(
      toolItem({
        tool_name: "ask_user",
        status: "failed",
        summary: {
          error: "VALIDATION_FAILED",
          display: {
            version: 1,
            title: "Asked a question",
            status: "failed",
            body: {
              kind: "error",
              code: "VALIDATION_FAILED",
              detail: "non_blocking requires expires_in_ms",
            },
          },
        },
      }),
    );

    expect(item.type).toBe("tool");
    if (item.type !== "tool") return;
    expect(item.view.title).toBe("Tool error: VALIDATION_FAILED");
  });

  test("ask projections keep the prompt and choices after settlement", () => {
    const item = decodeSessionTimelineItem(
      toolItem({
        tool_name: "ask_user",
        status: "succeeded",
        summary: {
          ask_id: "ask-1",
          mode: "blocking",
          prompt: "Pick a language",
          choices: ["Rust", { label: "TypeScript", annotation: "Use this for the web client." }],
          multiple: true,
          answer: ["Rust"],
          status: "answered",
          display: {
            version: 1,
            title: "Asked Pick a language",
            status: "succeeded",
            body: { kind: "none" },
          },
        },
      }),
    );

    expect(item).toMatchObject({
      type: "ask",
      askId: "ask-1",
      prompt: "Pick a language",
      choices: [
        { label: "Rust", annotation: null },
        { label: "TypeScript", annotation: "Use this for the web client." },
      ],
      multiple: true,
      answer: ["Rust"],
      status: "answered",
    });
  });

  test("Ask answer timeline items do not become user bubbles", () => {
    const answer: TimelineItemView = {
      id: "answer-1",
      session_id: "session-1",
      turn_id: "turn-1",
      kind: "user_message",
      source_resource_id: "message-1",
      display_order: 2,
      projection: {
        kind: "user_message",
        text: "Rust",
        source_ask_id: "ask-1",
      },
      status: "active",
      version: "v1",
      created_at: "2026-07-30T00:00:00Z",
    };

    expect(decodeSessionTimeline([answer])).toEqual([]);
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
