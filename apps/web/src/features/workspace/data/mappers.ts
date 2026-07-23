import type {
  SessionDiff,
  SessionDiffFile,
  SupervisorCliJobRecord,
  SupervisorRunRecord,
  SupervisorToolName,
} from "@janus/shared";
import { classifyShellCommand } from "@janus/shared";
import type {
  ActionStatus,
  ActionView,
  CliJobView,
  ConversationItem,
  GroupDiscussionView,
  TodoItemView,
} from "../types";
import { compressActionGroups } from "./conversation-action-compression";
import { parseTerminalOutputText } from "./terminal-output";

interface TimedItem {
  readonly at: string;
  readonly item: ConversationItem;
}

export function toCliJobViews(
  runs: readonly SupervisorRunRecord[],
): readonly CliJobView[] {
  return runs
    .flatMap((run) => (run.cliJobs ?? []).map((job) => toCliJobView(run, job)))
    .sort((left, right) =>
      left.startedAt === right.startedAt
        ? left.id.localeCompare(right.id)
        : left.startedAt.localeCompare(right.startedAt),
    );
}

function toCliJobView(
  run: SupervisorRunRecord,
  job: SupervisorCliJobRecord,
): CliJobView {
  return {
    id: job.id,
    runId: run.id,
    sessionId: run.sessionId,
    cli: job.cli,
    description: job.description ?? oneLine(job.instruction),
    status: job.status,
    startedAt: job.startedAt,
    ...(job.completedAt === undefined ? {} : { completedAt: job.completedAt }),
    ...(job.stdout === undefined ? {} : { stdout: job.stdout }),
    ...(job.stderr === undefined ? {} : { stderr: job.stderr }),
    ...(job.stdoutTruncated === undefined
      ? {}
      : { stdoutTruncated: job.stdoutTruncated }),
    ...(job.stderrTruncated === undefined
      ? {}
      : { stderrTruncated: job.stderrTruncated }),
    ...(job.exitCode === undefined ? {} : { exitCode: job.exitCode }),
  };
}

export function toDiscussionViews(
  runs: readonly SupervisorRunRecord[],
): readonly GroupDiscussionView[] {
  return runs
    .flatMap((run) =>
      (run.groupDiscussions ?? []).map((discussion) => ({
        id: discussion.id,
        runId: run.id,
        topic: discussion.topic,
        depth: discussion.depth,
        status: discussion.status,
        participants: discussion.participants.map((participant) => ({
          participantId: participant.participantId,
          modelId: participant.modelId,
          displayName: participant.displayName,
          status: participant.status,
          ...(participant.stance === undefined
            ? {}
            : { stance: participant.stance }),
          keyPoints: participant.keyPoints,
          risks: participant.risks,
          recommendations: participant.recommendations,
          ...(participant.rawOutput === undefined
            ? {}
            : { rawOutput: participant.rawOutput }),
          ...(participant.error === undefined
            ? {}
            : { error: participant.error }),
        })),
        ...(discussion.summary === undefined
          ? {}
          : { summary: discussion.summary }),
        consensus: discussion.consensus,
        disagreements: discussion.disagreements,
        risks: discussion.risks,
        recommendations: discussion.recommendations,
        startedAt: discussion.startedAt,
        ...(discussion.completedAt === undefined
          ? {}
          : { completedAt: discussion.completedAt }),
      })),
    )
    .sort((left, right) =>
      left.startedAt === right.startedAt
        ? left.id.localeCompare(right.id)
        : left.startedAt.localeCompare(right.startedAt),
    );
}

export function toRunConversationItems(
  run: SupervisorRunRecord,
  options: { readonly todoAlreadyExists?: boolean } = {},
): readonly ConversationItem[] {
  return compressActionGroups(
    sortTimedItems(toRunTimedItems(run, options)).map((entry) => entry.item),
  );
}

export function toSessionConversationItems(
  runs: readonly SupervisorRunRecord[],
): readonly ConversationItem[] {
  const items: TimedItem[] = [];
  let todoAlreadyExists = false;

  for (const run of sortRunsByStart(runs).filter(
    (candidate) => candidate.deliveredToRunId === undefined,
  )) {
    const runItems = toRunTimedItems(run, { todoAlreadyExists });
    items.push(...runItems, ...toRunCompletionItems(run, runItems));

    if (runHasTodo(run)) {
      todoAlreadyExists = true;
    }
  }

  return compressActionGroups(sortTimedItems(items).map((entry) => entry.item));
}

/**
 * The session-level todo = the most recent todo snapshot across all runs.
 * Each `todo` tool call carries a full list snapshot (not a delta), so the
 * newest one is the session's current todo. Returns undefined when no run has
 * produced a todo yet. Runs are sorted by startedAt descending so the result is
 * independent of the server's return order.
 */
export function latestTodoFromRuns(
  runs: readonly SupervisorRunRecord[],
): readonly TodoItemView[] | undefined {
  const ordered = [...runs].sort((left, right) =>
    right.startedAt.localeCompare(left.startedAt),
  );
  for (const run of ordered) {
    const items = latestTodoFromRun(run);
    if (items !== undefined) {
      return items;
    }
  }
  return undefined;
}

export function runHasTodo(run: SupervisorRunRecord): boolean {
  return latestTodoFromRun(run) !== undefined;
}

function latestTodoFromRun(
  run: SupervisorRunRecord,
): readonly TodoItemView[] | undefined {
  for (let index = run.transcript.length - 1; index >= 0; index -= 1) {
    const entry = run.transcript[index];
    if (entry?.kind === "tool" && entry.name === "todo") {
      return todoItemsFromInput(entry.input);
    }
  }

  return undefined;
}

function toRunTimedItems(
  run: SupervisorRunRecord,
  options: { readonly todoAlreadyExists?: boolean },
): TimedItem[] {
  const items: TimedItem[] = [
    {
      at: run.startedAt,
      item: {
        kind: "user",
        id: `${run.id}:task`,
        runId: run.id,
        text: run.task,
        at: run.startedAt,
        ...(run.attachments === undefined
          ? {}
          : { attachments: run.attachments }),
      },
    },
  ];
  let todoSeen = options.todoAlreadyExists ?? false;

  for (const entry of run.transcript) {
    if (entry.kind === "user" && entry.id === `${run.id}-user-1`) {
      continue;
    }

    const transcriptItems = toTranscriptItems(entry, run, {
      cliJobs: run.cliJobs ?? [],
      todoAlreadyExists: todoSeen,
    });
    items.push(...transcriptItems);

    if (entry.kind === "tool" && entry.name === "todo") {
      todoSeen = true;
    }
  }

  return items;
}

function sortTimedItems(items: TimedItem[]): TimedItem[] {
  return [...items].sort((left, right) => {
    const byTime = left.at.localeCompare(right.at);
    return byTime !== 0 ? byTime : left.item.id.localeCompare(right.item.id);
  });
}

function sortRunsByStart(
  runs: readonly SupervisorRunRecord[],
): readonly SupervisorRunRecord[] {
  return [...runs].sort((left, right) =>
    left.startedAt === right.startedAt
      ? left.id.localeCompare(right.id)
      : left.startedAt.localeCompare(right.startedAt),
  );
}

function toTranscriptItems(
  entry: SupervisorRunRecord["transcript"][number],
  run: SupervisorRunRecord,
  options: {
    readonly cliJobs: readonly SupervisorCliJobRecord[];
    readonly todoAlreadyExists: boolean;
  },
): TimedItem[] {
  switch (entry.kind) {
    case "user":
      return [
        {
          at: entry.at,
          item: {
            kind: "user",
            id: entry.id,
            runId: run.id,
            text: entry.text,
            at: entry.at,
          },
        },
      ];
    case "assistant":
      return [
        {
          at: entry.at,
          item: {
            kind: "supervisor",
            id: entry.id,
            text: entry.text,
            at: entry.at,
          },
        },
      ];
    case "thought":
      return [
        {
          at: entry.at,
          item: {
            kind: "thought",
            id: entry.id,
            title: entry.title ?? "Thinking",
            text: entry.text,
            at: entry.at,
            status: entry.status,
            ...(entry.startedAt === undefined
              ? {}
              : { startedAt: entry.startedAt }),
            ...(entry.completedAt === undefined
              ? {}
              : { completedAt: entry.completedAt }),
          },
        },
      ];
    case "tool":
      return [
        {
          at: entry.startedAt,
          item: {
            kind: "action",
            id: entry.id,
            at: entry.startedAt,
            action: toToolAction(entry, run.diff, options),
          },
        },
      ];
  }
}

function toRunCompletionItems(
  run: SupervisorRunRecord,
  runItems: readonly TimedItem[],
): readonly TimedItem[] {
  if (run.status === "failed") {
    return [
      {
        at: run.completedAt ?? run.updatedAt,
        item: {
          kind: "supervisor",
          id: `${run.id}:failed`,
          text: `Task failed: ${run.lastError ?? "Unknown error."}`,
          at: run.completedAt ?? run.updatedAt,
          tone: "error",
        },
      },
    ];
  }

  if (run.status === "canceled") {
    return [
      {
        at: run.completedAt ?? run.updatedAt,
        item: {
          kind: "supervisor",
          id: `${run.id}:canceled`,
          text: "Run canceled.",
          at: run.completedAt ?? run.updatedAt,
          tone: "muted",
        },
      },
    ];
  }

  if (run.status !== "completed") {
    return [];
  }

  if (runItems.some((entry) => entry.item.kind === "supervisor")) {
    return [];
  }

  const at = run.completedAt ?? run.updatedAt;
  return [
    {
      at,
      item: {
        kind: "supervisor",
        id: `${run.id}:completed`,
        text: run.finalResponse ?? "Request completed.",
        at,
      },
    },
  ];
}

type ToolTranscriptEntry = Extract<
  SupervisorRunRecord["transcript"][number],
  { readonly kind: "tool" }
>;

function toToolAction(
  entry: ToolTranscriptEntry,
  diff: SessionDiff | undefined,
  options: {
    readonly cliJobs: readonly SupervisorCliJobRecord[];
    readonly todoAlreadyExists: boolean;
  },
): ActionView {
  switch (entry.name) {
    case "todo":
      return toTodoAction(entry, options.todoAlreadyExists);
    case "read_file":
      return toReadFileAction(entry);
    case "write_file":
      return toWriteFileAction(entry, diff);
    case "edit_file":
      return toEditFileAction(entry, diff);
    case "dispatch_claude_code":
      return toCliDispatchAction(
        entry,
        "claude-code",
        "Started Claude Code",
        options.cliJobs,
      );
    case "dispatch_codex":
      return toCliDispatchAction(
        entry,
        "codex",
        "Started Codex",
        options.cliJobs,
      );
    case "read_cli_job":
      return toCliDispatchAction(entry, undefined, "Read CLI job");
    case "stop_cli_job":
      return toCliDispatchAction(entry, undefined, "Stopped CLI job");
    case "group_discussion":
      return toGroupDiscussionAction(entry);
    case "read_group_discussion":
      return toReadGroupDiscussionAction(entry);
    case "ask_user":
      return toAskUserAction(entry);
    case "browser_action":
      return toBrowserAction(entry);
    case "bash":
      return toBashAction(entry);
    default:
      // Unknown tool names (e.g. newly added supervisor tools not yet mapped)
      // render as a generic read/info action so the transcript still shows them
      // instead of breaking the type-check or crashing.
      return rawToolAction(entry, {
        type: "read",
        title: entry.name,
      });
  }
}

function toTodoAction(
  entry: ToolTranscriptEntry,
  todoAlreadyExists: boolean,
): ActionView {
  const items = todoItemsFromInput(entry.input);
  return {
    ...toolActionState(entry),
    type: "read",
    title: todoAlreadyExists ? "Updated Todo" : "Created Todo",
    meta: `${items.length} ${items.length === 1 ? "item" : "items"}`,
    detail: {
      kind: "raw",
      lines:
        items.length === 0
          ? [entry.output]
          : items.map(
              (item, index) =>
                `${index + 1}. [${item.status.replace(/_/g, " ")}] ${
                  item.content
                }`,
            ),
    },
  };
}

function todoItemsFromInput(
  input: Readonly<Record<string, unknown>>,
): readonly TodoItemView[] {
  const items = input.items;

  if (!Array.isArray(items)) {
    return [];
  }

  return items
    .map((item, index): TodoItemView | undefined => {
      if (typeof item !== "object" || item === null) {
        return undefined;
      }

      const content =
        "content" in item && typeof item.content === "string"
          ? item.content.trim()
          : undefined;
      const status =
        "status" in item ? todoStatusField(item.status) : "pending";

      if (content === undefined || content.length === 0) {
        return undefined;
      }

      return { id: `todo:${index}:${status}:${content}`, content, status };
    })
    .filter((item): item is TodoItemView => item !== undefined);
}

function todoStatusField(value: unknown): TodoItemView["status"] {
  return value === "in_progress" || value === "completed" ? value : "pending";
}

function toReadFileAction(entry: ToolTranscriptEntry): ActionView {
  const path = stringField(entry.input, "path") ?? "workspace file";

  return {
    ...toolActionState(entry),
    type: "read",
    title: `Read ${path}`,
    compressible: true,
  };
}

function toWriteFileAction(
  entry: ToolTranscriptEntry,
  diff: SessionDiff | undefined,
): ActionView {
  return toFileChangeAction(entry, "write_file", diff);
}

function toEditFileAction(
  entry: ToolTranscriptEntry,
  diff: SessionDiff | undefined,
): ActionView {
  return toFileChangeAction(entry, "edit_file", diff);
}

function toFileChangeAction(
  entry: ToolTranscriptEntry,
  toolName: Extract<SupervisorToolName, "write_file" | "edit_file">,
  diff: SessionDiff | undefined,
): ActionView {
  const path = stringField(entry.input, "path") ?? "workspace file";
  const file = findDiffFile(diff, path);

  return {
    ...toolActionState(entry),
    type: "edit",
    title: formatFileChangeTitle(toolName, path, file),
    compressible: true,
    detail:
      diff === undefined || file === undefined
        ? { kind: "raw", lines: [entry.output] }
        : { kind: "diff", diff, path },
  };
}

function toBashAction(entry: ToolTranscriptEntry): ActionView {
  const command = stringField(entry.input, "command") ?? "shell command";
  const output = parseTerminalOutputText(entry.output);

  return {
    ...toolActionState(entry),
    type: "cli",
    title: `Ran ${oneLine(command)}`,
    compressible: classifyShellCommand(command).compressible,
    detail:
      output === undefined
        ? { kind: "raw", lines: [entry.output] }
        : { kind: "terminalOutput", output },
  };
}

function toCliDispatchAction(
  entry: ToolTranscriptEntry,
  cli: "claude-code" | "codex" | undefined,
  fallbackTitle: string,
  cliJobs: readonly SupervisorCliJobRecord[] = [],
): ActionView {
  const description = stringField(entry.input, "description");
  const title = description ?? fallbackTitle;
  const cliJobId = cliJobs.find((job) => job.toolUseId === entry.toolUseId)?.id;

  return rawToolAction(entry, {
    type: "cli",
    title,
    ...(description === undefined ? {} : { meta: fallbackTitle }),
    ...(cli === undefined ? {} : { cli, variant: "dispatch" }),
    ...(cliJobId === undefined ? {} : { cliJobId }),
  });
}

function toGroupDiscussionAction(entry: ToolTranscriptEntry): ActionView {
  const topic = stringField(entry.input, "topic") ?? "group discussion";
  return rawToolAction(entry, {
    type: "read",
    title: `Started Group Discussion: ${oneLine(topic)}`,
  });
}

function toReadGroupDiscussionAction(entry: ToolTranscriptEntry): ActionView {
  const discussionId =
    stringField(entry.input, "discussionId") ?? "group discussion";
  return rawToolAction(entry, {
    type: "read",
    title: `Read Group Discussion ${discussionId}`,
  });
}

function toAskUserAction(entry: ToolTranscriptEntry): ActionView {
  const question = stringField(entry.input, "question") ?? "question";
  return rawToolAction(entry, {
    type: "read",
    title: `Asked User: ${oneLine(question)}`,
  });
}

function toBrowserAction(entry: ToolTranscriptEntry): ActionView {
  const url = stringField(entry.input, "url") ?? "page";
  return rawToolAction(entry, {
    type: "read",
    title: `Browsed ${oneLine(url)}`,
  });
}

function findDiffFile(
  diff: SessionDiff | undefined,
  path: string,
): SessionDiffFile | undefined {
  return diff?.files.find((file) => file.path === path);
}

function diffLineSummary(file: SessionDiffFile): string {
  switch (file.status) {
    case "added":
    case "untracked":
      return `(+${file.additions})`;
    case "deleted":
      return `(-${file.deletions})`;
    case "modified":
    case "renamed":
      return `(+${file.additions} -${file.deletions})`;
  }
}

function formatFileChangeTitle(
  toolName: Extract<SupervisorToolName, "write_file" | "edit_file">,
  path: string,
  file: SessionDiffFile | undefined,
): string {
  const verb =
    toolName === "write_file" &&
    (file?.status === "added" || file?.status === "untracked")
      ? "Created"
      : toolName === "write_file"
        ? "Wrote"
        : "Edited";
  const lineSummary = file === undefined ? undefined : diffLineSummary(file);

  return lineSummary === undefined
    ? `${verb} ${path}`
    : `${verb} ${path} ${lineSummary}`;
}

function stringField(
  input: Readonly<Record<string, unknown>>,
  key: string,
): string | undefined {
  const value = input[key];
  return typeof value === "string" && value.trim().length > 0
    ? value
    : undefined;
}

function oneLine(value: string): string {
  return value.replace(/\s+/g, " ").trim();
}

function statusFromTool(status: ToolTranscriptEntry["status"]): ActionStatus {
  return status === "failed" ? "failure" : "success";
}

function toolActionState(
  entry: ToolTranscriptEntry,
): Pick<ActionView, "id" | "level" | "status"> {
  return {
    id: entry.id,
    level: entry.status === "failed" ? "warn" : "info",
    status: statusFromTool(entry.status),
  };
}

function rawToolAction(
  entry: ToolTranscriptEntry,
  action: Omit<ActionView, "id" | "level" | "status" | "detail">,
): ActionView {
  return {
    ...toolActionState(entry),
    ...action,
    detail: { kind: "raw", lines: [entry.output] },
  };
}
