/// <reference types="bun" />

import { describe, expect, test } from "bun:test";
import type { SessionDiff, SupervisorRunRecord } from "@janus/shared";
import { toSessionConversationItems } from "./mappers";

describe("toSessionConversationItems", () => {
  test("orders late output from an older run by transcript time", () => {
    const items = toSessionConversationItems([
      runRecord({
        id: "run-1",
        task: "Start the CLI job",
        startedAt: "2026-06-27T01:00:00.000Z",
        transcript: [
          userEntry(
            "run-1-user-1",
            "Start the CLI job",
            "2026-06-27T01:00:00.000Z",
          ),
          assistantEntry(
            "assistant-1",
            "Waiting for CLI job to finish...",
            "2026-06-27T02:00:00.000Z",
          ),
          assistantEntry(
            "assistant-2",
            "Original CLI result is ready.",
            "2026-06-27T06:00:00.000Z",
          ),
        ],
      }),
      runRecord({
        id: "run-2",
        task: "Follow-up while waiting",
        startedAt: "2026-06-27T04:00:00.000Z",
        transcript: [
          userEntry(
            "run-2-user-1",
            "Follow-up while waiting",
            "2026-06-27T04:00:00.000Z",
          ),
          assistantEntry(
            "assistant-3",
            "Follow-up response.",
            "2026-06-27T05:00:00.000Z",
          ),
        ],
      }),
    ]);

    expect(
      items.map((item) => ("text" in item ? item.text : item.action.title)),
    ).toEqual([
      "Start the CLI job",
      "Waiting for CLI job to finish...",
      "Follow-up while waiting",
      "Follow-up response.",
      "Original CLI result is ready.",
    ]);
  });

  test("filters queued runs delivered into an active run", () => {
    const items = toSessionConversationItems([
      runRecord({
        id: "run-active",
        task: "Original task",
        startedAt: "2026-06-27T01:00:00.000Z",
        status: "running",
        transcript: [
          userEntry(
            "run-active-user-1",
            "Original task",
            "2026-06-27T01:00:00.000Z",
          ),
          userEntry(
            "delivered-user",
            "Please adjust the current work.",
            "2026-06-27T03:00:00.000Z",
          ),
        ],
      }),
      runRecord({
        id: "run-delivered",
        task: "Please adjust the current work.",
        startedAt: "2026-06-27T02:00:00.000Z",
        status: "completed",
        deliveredToRunId: "run-active",
        deliveredAt: "2026-06-27T03:00:00.000Z",
        completedAt: "2026-06-27T03:00:00.000Z",
      }),
    ]);

    expect(
      items.map((item) => ("text" in item ? item.text : item.action.title)),
    ).toEqual(["Original task", "Please adjust the current work."]);
  });

  test("links dispatch actions to their CLI job", () => {
    const items = toSessionConversationItems([
      runRecord({
        id: "run-1",
        task: "Dispatch work",
        startedAt: "2026-06-27T01:00:00.000Z",
        transcript: [
          userEntry(
            "run-1-user-1",
            "Dispatch work",
            "2026-06-27T01:00:00.000Z",
          ),
          {
            id: "tool-entry-1",
            kind: "tool",
            toolUseId: "tool-use-1",
            name: "dispatch_codex",
            input: {
              instruction: "Implement the change.",
              description: "Implement UI change",
            },
            status: "completed",
            output: "cli_jobs_started: 1/1",
            startedAt: "2026-06-27T02:00:00.000Z",
            completedAt: "2026-06-27T02:00:00.000Z",
          },
        ],
        cliJobs: [
          {
            id: "job-1",
            toolUseId: "tool-use-1",
            cli: "codex",
            description: "Implement UI change",
            instruction: "Implement the change.",
            launch: { access: "full-access" },
            status: "running",
            startedAt: "2026-06-27T02:00:00.000Z",
          },
        ],
      }),
    ]);
    const action = items.find((item) => item.kind === "action");

    expect(action?.kind).toBe("action");
    if (action?.kind !== "action") {
      throw new Error("Expected a dispatch action.");
    }
    expect(action.action.cliJobId).toBe("job-1");
  });

  test("maps thought transcript entries to forced-visible conversation items", () => {
    const items = toSessionConversationItems([
      runRecord({
        id: "run-1",
        task: "Think through this",
        startedAt: "2026-06-27T01:00:00.000Z",
        transcript: [
          userEntry(
            "run-1-user-1",
            "Think through this",
            "2026-06-27T01:00:00.000Z",
          ),
          {
            id: "thought-1",
            kind: "thought",
            title: "Thinking",
            text: "Checked the provider behavior.",
            at: "2026-06-27T02:00:00.000Z",
            status: "completed",
            startedAt: "2026-06-27T01:59:55.000Z",
            completedAt: "2026-06-27T02:00:00.000Z",
          },
        ],
      }),
    ]);
    const thought = items.find((item) => item.kind === "thought");

    expect(thought).toEqual({
      kind: "thought",
      id: "thought-1",
      title: "Thinking",
      text: "Checked the provider behavior.",
      at: "2026-06-27T02:00:00.000Z",
      status: "completed",
      startedAt: "2026-06-27T01:59:55.000Z",
      completedAt: "2026-06-27T02:00:00.000Z",
    });
  });

  test("maps structured bash output to terminal output detail", () => {
    const items = toSessionConversationItems([
      runRecord({
        id: "run-1",
        task: "Run a command",
        startedAt: "2026-06-27T01:00:00.000Z",
        transcript: [
          userEntry(
            "run-1-user-1",
            "Run a command",
            "2026-06-27T01:00:00.000Z",
          ),
          toolEntry({
            id: "bash-1",
            name: "bash",
            command: "pwd",
            output: [
              "exit_code: 1",
              "stdout:",
              "project root",
              "stderr:",
              "permission denied",
            ].join("\n"),
            at: "2026-06-27T02:00:00.000Z",
          }),
        ],
      }),
    ]);
    const action = items.find((item) => item.kind === "action");

    expect(action?.kind).toBe("action");
    if (action?.kind !== "action") {
      throw new Error("Expected a bash action.");
    }
    expect(action.action.detail).toEqual({
      kind: "terminalOutput",
      output: {
        exitCode: 1,
        stdout: "project root",
        stderr: "permission denied",
      },
    });
  });

  test("folds completed thought, read-file, and safe bash activity", () => {
    const items = toSessionConversationItems([
      runRecord({
        id: "run-1",
        task: "Inspect files",
        startedAt: "2026-06-27T01:00:00.000Z",
        transcript: [
          userEntry(
            "run-1-user-1",
            "Inspect files",
            "2026-06-27T01:00:00.000Z",
          ),
          {
            id: "thought-1",
            kind: "thought",
            title: "Thinking",
            text: "Checked which files matter.",
            at: "2026-06-27T02:00:00.000Z",
            status: "completed",
            startedAt: "2026-06-27T01:59:55.000Z",
            completedAt: "2026-06-27T02:00:00.000Z",
          },
          toolEntry({
            id: "read-1",
            name: "read_file",
            path: "README.md",
            at: "2026-06-27T02:00:01.000Z",
          }),
          toolEntry({
            id: "bash-1",
            name: "bash",
            command: "pwd",
            at: "2026-06-27T02:00:02.000Z",
          }),
        ],
      }),
    ]);
    const action = items.find((item) => item.kind === "action");

    expect(action?.kind).toBe("action");
    if (action?.kind !== "action") {
      throw new Error("Expected a compressed activity action.");
    }
    expect(action.action.title).toBe(
      "Thought for 5s, read 1 file and ran 1 command",
    );
    expect(action.action.detail?.kind).toBe("activity");
    if (action.action.detail?.kind !== "activity") {
      throw new Error("Expected activity detail.");
    }
    expect(action.action.detail.items.map((item) => item.kind)).toEqual([
      "thought",
      "action",
      "action",
    ]);
  });

  test("summarizes consecutive file edits without pretending they were review results", () => {
    const items = toSessionConversationItems([
      runRecord({
        id: "run-1",
        task: "Update files",
        startedAt: "2026-06-27T01:00:00.000Z",
        transcript: [
          userEntry("run-1-user-1", "Update files", "2026-06-27T01:00:00.000Z"),
          toolEntry({
            id: "edit-1",
            name: "edit_file",
            path: "README.md",
            at: "2026-06-27T02:00:00.000Z",
          }),
          toolEntry({
            id: "edit-2",
            name: "write_file",
            path: "docs/notes.md",
            at: "2026-06-27T02:00:01.000Z",
          }),
        ],
      }),
    ]);
    const action = items.find((item) => item.kind === "action");

    expect(action?.kind).toBe("action");
    if (action?.kind !== "action") {
      throw new Error("Expected a compressed action.");
    }
    expect(action.action.title).toBe("Edited 2 Files");
    expect(action.action.title).not.toContain("Reviewed");
    expect(action.action.detail?.kind).toBe("actions");
  });

  test("maps new write-file changes to created diff actions", () => {
    const items = toSessionConversationItems([
      runRecord({
        id: "run-1",
        task: "Create a file",
        startedAt: "2026-06-27T01:00:00.000Z",
        diff: diffRecord({
          path: "docs/new.md",
          status: "untracked",
          additions: 3,
          deletions: 0,
        }),
        transcript: [
          userEntry(
            "run-1-user-1",
            "Create a file",
            "2026-06-27T01:00:00.000Z",
          ),
          toolEntry({
            id: "write-1",
            name: "write_file",
            path: "docs/new.md",
            at: "2026-06-27T02:00:00.000Z",
          }),
        ],
      }),
    ]);
    const action = singleAction(items);

    expect(action.title).toBe("Created docs/new.md (+3)");
    expect(action.detail).toMatchObject({
      kind: "diff",
      path: "docs/new.md",
    });
  });

  test("keeps write-file and edit-file verbs stable for modified files", () => {
    const items = toSessionConversationItems([
      runRecord({
        id: "run-1",
        task: "Update files",
        startedAt: "2026-06-27T01:00:00.000Z",
        diff: diffRecord({
          path: "README.md",
          status: "modified",
          additions: 2,
          deletions: 1,
        }),
        transcript: [
          userEntry("run-1-user-1", "Update files", "2026-06-27T01:00:00.000Z"),
          toolEntry({
            id: "write-1",
            name: "write_file",
            path: "README.md",
            at: "2026-06-27T02:00:00.000Z",
          }),
        ],
      }),
      runRecord({
        id: "run-2",
        task: "Edit files",
        startedAt: "2026-06-27T03:00:00.000Z",
        diff: diffRecord({
          path: "src/app.ts",
          status: "modified",
          additions: 1,
          deletions: 1,
        }),
        transcript: [
          userEntry("run-2-user-1", "Edit files", "2026-06-27T03:00:00.000Z"),
          toolEntry({
            id: "edit-1",
            name: "edit_file",
            path: "src/app.ts",
            at: "2026-06-27T04:00:00.000Z",
          }),
        ],
      }),
    ]);
    const actions = items
      .filter((item) => item.kind === "action")
      .map((item) => item.action);

    expect(actions.map((action) => action.title)).toEqual([
      "Wrote README.md (+2 -1)",
      "Edited src/app.ts (+1 -1)",
    ]);
    expect(actions.map((action) => action.detail?.kind)).toEqual([
      "diff",
      "diff",
    ]);
  });

  test("does not show unrelated run diff when a file tool path is absent", () => {
    const items = toSessionConversationItems([
      runRecord({
        id: "run-1",
        task: "Update a file",
        startedAt: "2026-06-27T01:00:00.000Z",
        diff: diffRecord({
          path: "src/other.ts",
          status: "modified",
          additions: 1,
          deletions: 0,
        }),
        transcript: [
          userEntry(
            "run-1-user-1",
            "Update a file",
            "2026-06-27T01:00:00.000Z",
          ),
          toolEntry({
            id: "write-1",
            name: "write_file",
            path: "README.md",
            output: "Wrote README.md",
            at: "2026-06-27T02:00:00.000Z",
          }),
        ],
      }),
    ]);
    const action = singleAction(items);

    expect(action.title).toBe("Wrote README.md");
    expect(action.detail).toEqual({ kind: "raw", lines: ["Wrote README.md"] });
  });
});

function singleAction(items: ReturnType<typeof toSessionConversationItems>) {
  const actionItem = items.find((item) => item.kind === "action");

  expect(actionItem?.kind).toBe("action");
  if (actionItem?.kind !== "action") {
    throw new Error("Expected an action item.");
  }

  return actionItem.action;
}

function runRecord(
  patch: Partial<SupervisorRunRecord> &
    Pick<SupervisorRunRecord, "id" | "task" | "startedAt">,
): SupervisorRunRecord {
  const status = patch.status ?? "completed";
  return {
    projectId: "project-1",
    sessionId: "session-1",
    status,
    transcript: [],
    updatedAt: patch.completedAt ?? patch.startedAt,
    ...(status === "queued" || status === "running"
      ? {}
      : { completedAt: patch.completedAt ?? patch.startedAt }),
    ...patch,
  };
}

function userEntry(
  id: string,
  text: string,
  at: string,
): SupervisorRunRecord["transcript"][number] {
  return {
    id,
    kind: "user",
    text,
    at,
  };
}

function assistantEntry(
  id: string,
  text: string,
  at: string,
): SupervisorRunRecord["transcript"][number] {
  return {
    id,
    kind: "assistant",
    text,
    at,
    status: "completed",
  };
}

function toolEntry({
  id,
  name,
  path,
  command,
  output,
  at,
}: {
  readonly id: string;
  readonly name: "bash" | "edit_file" | "read_file" | "write_file";
  readonly path?: string;
  readonly command?: string;
  readonly output?: string;
  readonly at: string;
}): SupervisorRunRecord["transcript"][number] {
  const input =
    name === "bash"
      ? { command: command ?? "pwd" }
      : { path: path ?? "README.md" };

  return {
    id,
    kind: "tool",
    toolUseId: `${id}:tool-use`,
    name,
    input,
    status: "completed",
    output: output ?? `${name} completed`,
    startedAt: at,
    completedAt: at,
  };
}

function diffRecord({
  path,
  status,
  additions,
  deletions,
}: {
  readonly path: string;
  readonly status: SessionDiff["files"][number]["status"];
  readonly additions: number;
  readonly deletions: number;
}): SessionDiff {
  return {
    sessionId: "session-1",
    files: [{ path, status, additions, deletions }],
    patch: [
      `diff --git a/${path} b/${path}`,
      `--- a/${path}`,
      `+++ b/${path}`,
      "@@ -1 +1 @@",
      "-old",
      "+new",
    ].join("\n"),
    updatedAt: "2026-06-27T02:00:00.000Z",
  };
}
