import type { TimelineItemView, TimelineTurnStatus } from "../../lib/api";
import { normalizeReasoningSummary } from "../../lib/modelStream";

type Projection = Record<string, unknown>;

interface TimelineItemBase {
  id: string;
  sourceKind: string;
  version?: string;
  turnId: string | null;
  createdAt: string;
  itemStatus: string;
  turnStatus: TimelineTurnStatus | null;
}

export interface TimelinePlanStep {
  text: string;
  status: string | null;
}

export interface TimelineAttachment {
  id: string;
  name: string;
  mime: string;
  byteSize: number;
}

export type ToolStatus = "success" | "failure" | "running";

export type ToolActivityKind = "read" | "search" | "list";

export interface ToolActivityCount {
  kind: ToolActivityKind;
  count: number;
}

export type ToolActivityDetail =
  | {
      kind: "thought";
      id: string;
      title: string;
      text: string;
      durationMs: number | null;
    }
  | {
      kind: "tool";
      id: string;
      name: string;
      view: ToolView;
    };

export type ToolDisplayBody =
  | { kind: "none" }
  | { kind: "text"; text: string }
  | { kind: "structured"; value: unknown }
  | { kind: "patch"; patch: string }
  | {
      kind: "command_output";
      command: string;
      stdout: string;
      stderr: string;
      exitCode: number | null;
      truncated: boolean;
    }
  | { kind: "activity"; items: readonly ToolActivityDetail[] }
  | { kind: "error"; code: string; detail: string };

export interface ToolView {
  title: string;
  status: ToolStatus;
  body: ToolDisplayBody;
  expandable: boolean;
  lowNoise: boolean;
  activity?: readonly ToolActivityCount[];
}

export type SessionTimelineItem =
  | (TimelineItemBase & { type: "user"; text: string; attachments: TimelineAttachment[] })
  | (TimelineItemBase & {
      type: "assistant";
      text: string;
      reasoning: string;
      roundId: string | null;
      /** Wall-clock ms from the first reasoning delta to answer/tool output. */
      durationMs: number | null;
    })
  | (TimelineItemBase & { type: "steer"; text: string })
  | (TimelineItemBase & {
      type: "tool";
      toolName: string;
      toolStatus: string;
      summary: unknown;
      view: ToolView;
    })
  | (TimelineItemBase & {
      type: "plan";
      title: string;
      sequence: string | null;
      steps: TimelinePlanStep[];
      toolStatus: string;
    })
  | (TimelineItemBase & {
      type: "model";
      model: string;
      status: string;
      detail: string;
      attempt: string | null;
      warning: boolean;
    })
  | (TimelineItemBase & {
      type: "context";
      title: string;
      sourceFirst: string | null;
      sourceLast: string | null;
      itemCount: number | null;
      /** Model-generated summary text; null on the degraded digest path. */
      summaryText: string | null;
      /** Degradation marker when the summary model pass failed. */
      modelStatus: string | null;
    })
  | (TimelineItemBase & {
      type: "async_task";
      taskId: string | null;
      status: string;
      command: string;
      output: string;
    })
  | (TimelineItemBase & { type: "unknown"; raw: unknown });

export function decodeSessionTimeline(
  items: TimelineItemView[],
  previous: readonly SessionTimelineItem[] = [],
): SessionTimelineItem[] {
  const previousById = new Map(previous.map((item) => [item.id, item]));
  return items.map((item) => {
    const cached = previousById.get(item.id);
    // A Turn status is joined onto timeline rows and has its own version
    // clock (`turn.updated_at`). The timeline item version does not change
    // when the Turn moves from running to completed/interrupted, so it must
    // be part of the cache identity or the durable status row freezes until
    // a full page reload.
    if (
      cached?.version !== undefined &&
      cached.version === item.version &&
      sameTurnStatus(cached.turnStatus, item.turn_status ?? null)
    ) {
      return cached;
    }
    return decodeSessionTimelineItem(item);
  });
}

function sameTurnStatus(
  previous: TimelineTurnStatus | null,
  next: TimelineTurnStatus | null,
): boolean {
  if (previous === next) return true;
  if (!previous || !next) return false;
  return (
    previous.id === next.id &&
    previous.status === next.status &&
    previous.cancellation_reason === next.cancellation_reason &&
    previous.completion_reason === next.completion_reason &&
    previous.created_at === next.created_at &&
    previous.updated_at === next.updated_at
  );
}

export function decodeSessionTimelineItem(item: TimelineItemView): SessionTimelineItem {
  const projection = asRecord(item.projection);
  const summary = asRecord(projection.summary);
  const toolName = text(projection.tool_name ?? summary.tool_name).toLowerCase();
  const base: TimelineItemBase = {
    id: item.id,
    sourceKind: item.kind,
    version: item.version,
    turnId: item.turn_id ?? null,
    createdAt: item.created_at,
    itemStatus: item.status,
    turnStatus: item.turn_status ?? null,
  };

  switch (item.kind) {
    case "user_message":
      return {
        ...base,
        type: "user",
        text: text(projection.text),
        attachments: decodeAttachments(projection.attachments),
      };
    case "assistant_message":
      return {
        ...base,
        type: "assistant",
        text: text(projection.text),
        reasoning: normalizeReasoningSummary(text(projection.reasoning)),
        roundId: optionalText(projection.round_id),
        durationMs: typeof projection.duration_ms === "number" ? projection.duration_ms : null,
      };
    case "steer":
      return { ...base, type: "steer", text: text(projection.text) };
    case "async_task_result":
      return {
        ...base,
        type: "async_task",
        taskId: optionalText(projection.task_id),
        status: text(projection.status, "unknown"),
        command: text(projection.command, "bash"),
        output: text(projection.output ?? projection.text),
      };
    case "context_compacted":
      return {
        ...base,
        type: "context",
        title: text(projection.title, "Context Compacted"),
        sourceFirst: optionalText(projection.source_first_timeline_id),
        sourceLast: optionalText(projection.source_last_timeline_id),
        itemCount:
          typeof projection.item_count === "number"
            ? projection.item_count
            : typeof summary.item_count === "number"
              ? summary.item_count
              : null,
        summaryText: optionalText(summary.text ?? projection.text),
        modelStatus: optionalText(summary.summary_model_status),
      };
  }

  if (isPlan(item.kind, toolName)) {
    const plan = firstRecord(summary.plan, projection.plan, summary, projection);
    return {
      ...base,
      type: "plan",
      title: text(plan.title ?? projection.title, "Plan update"),
      sequence: displayValue(summary.sequence ?? projection.sequence),
      steps: decodePlanSteps(plan.steps ?? plan.items ?? summary.steps ?? projection.steps),
      toolStatus: toolStatus(projection, item.status),
    };
  }

  if (isModel(item.kind, toolName)) {
    const status = text(
      summary.status ?? projection.classification ?? projection.status,
      "attempt",
    );
    return {
      ...base,
      type: "model",
      model: text(
        summary.model_id ?? summary.model ?? projection.model_id ?? projection.model,
        "model",
      ),
      status,
      detail: text(summary.detail ?? summary.error ?? projection.detail ?? projection.message),
      attempt: displayValue(summary.attempt_number ?? projection.attempt_number),
      warning:
        item.kind === "model_warning" ||
        status.includes("fail") ||
        status.includes("cooldown") ||
        projection.warning === true,
    };
  }

  if (item.kind === "tool_call") {
    const view = parseToolView(summary, toolName, item.status);
    return {
      ...base,
      type: "tool",
      toolName: text(projection.tool_name, "Tool"),
      toolStatus: toolStatus(projection, item.status),
      summary: projection.summary ?? {},
      view,
    };
  }

  return { ...base, type: "unknown", raw: item.projection };
}

function normalizeToolStatus(raw: string): ToolStatus {
  const lower = raw.toLowerCase();
  if (lower === "failed" || lower === "failure" || lower === "error") return "failure";
  if (lower === "running" || lower === "requested" || lower === "pending") return "running";
  return "success";
}

function parseToolView(summary: Projection, toolName: string, itemStatus: string): ToolView {
  const display = asRecord(summary.display);
  const rawTitle = text(display.title).trim();
  const version = display.version;
  if (version !== 1 || !rawTitle) {
    return {
      title: fallbackToolTitle(toolName, summary, display.body),
      status: "failure",
      body: {
        kind: "error",
        code: "TOOL_DISPLAY_UNAVAILABLE",
        detail: text(summary.detail ?? summary.error, "The Tool returned no display projection."),
      },
      expandable: true,
      lowNoise: false,
    };
  }
  const title = normalizeToolTitle(rawTitle, toolName, summary, display.body);
  const body = decodeToolDisplayBody(display.body);
  const activity = analyzeToolActivity(toolName, body);
  return {
    title,
    status:
      body.kind === "error" ? "failure" : normalizeToolStatus(text(display.status, itemStatus)),
    body,
    expandable: body.kind !== "none",
    lowNoise: activity !== null,
    ...(activity === null ? {} : { activity }),
  };
}

function normalizeToolTitle(
  title: string,
  toolName: string,
  summary: Projection,
  displayBody: unknown,
): string {
  const normalized = title.replace(/\s+/g, " ").trim();
  if (!/^(?:used|ran tool|tool error|tool failed|tool execution failed)$/i.test(normalized)) {
    return normalized;
  }
  return fallbackToolTitle(toolName, summary, displayBody);
}

function fallbackToolTitle(toolName: string, summary: Projection, displayBody?: unknown): string {
  const body = asRecord(displayBody);
  const command = text(summary.command ?? summary.command_summary ?? body.command)
    .replace(/\s+/g, " ")
    .trim();
  if (command) return `Ran ${command}`;
  if (toolName === "bash") {
    return "Ran bash";
  }
  if (!toolName) return "Ran tool";
  return `Used ${toolName
    .split(/[_-]+/)
    .filter(Boolean)
    .map((part) => `${part.charAt(0).toUpperCase()}${part.slice(1)}`)
    .join(" ")}`;
}

/** Low-noise tools whose consecutive calls can compress into a summary. */
export function analyzeToolActivity(
  toolName: string,
  body: ToolDisplayBody,
): readonly ToolActivityCount[] | null {
  const normalizedToolName = toolName.trim().toLowerCase();
  if (normalizedToolName === "read") return [{ kind: "read", count: 1 }];
  if (normalizedToolName !== "bash" || body.kind !== "command_output") return null;
  return analyzeShellCommand(body.command);
}

const READ_COMMANDS = new Set(["cat", "file", "head", "sed", "sort", "stat", "tail", "type", "wc"]);
const SEARCH_COMMANDS = new Set(["ack", "ag", "grep", "rg"]);
const LIST_COMMANDS = new Set(["dir", "find", "ls", "pwd", "tree", "where", "which"]);
const READ_GIT_COMMANDS = new Set(["branch", "diff", "log", "rev-parse", "show", "status"]);
const SEARCH_GIT_COMMANDS = new Set(["grep"]);
const LIST_GIT_COMMANDS = new Set(["ls-files"]);
const SHELL_PLUMBING_COMMANDS = new Set(["env", "printf", "tr", "true", "uniq"]);

/** Presentation-only shell classification. This is deliberately conservative
 * and is not the execution security boundary. */
export function analyzeShellCommand(command: string): readonly ToolActivityCount[] | null {
  const commandForAnalysis = removeSafeShellRedirections(command);
  if (!commandForAnalysis.trim() || /[<>`]|\$\(|\$\{/.test(commandForAnalysis)) return null;
  const segments = splitShellSegments(commandForAnalysis);
  if (segments.length === 0) return null;

  const counts: Record<ToolActivityKind, number> = { read: 0, search: 0, list: 0 };
  let meaningful = false;
  for (const segment of segments) {
    const words = shellWords(segment);
    if (words === null || words.length === 0) return null;
    const activity = classifyShellSegment(words[0] ?? "", words.slice(1));
    if (activity === null) return null;
    for (const entry of activity) counts[entry.kind] += entry.count;
    meaningful ||= activity.length > 0;
  }

  if (!meaningful) return null;
  return (Object.entries(counts) as [ToolActivityKind, number][])
    .filter(([, count]) => count > 0)
    .map(([kind, count]) => ({ kind, count }));
}

function splitShellSegments(command: string): string[] {
  const segments: string[] = [];
  let current = "";
  let quote: "'" | '"' | null = null;
  let escaped = false;
  for (let index = 0; index < command.length; index += 1) {
    const character = command[index];
    const next = command[index + 1];
    if (escaped) {
      current += character;
      escaped = false;
      continue;
    }
    if (character === "\\" && quote !== "'") {
      current += character;
      escaped = true;
      continue;
    }
    if (quote) {
      current += character;
      if (character === quote) quote = null;
      continue;
    }
    if (character === "'" || character === '"') {
      quote = character;
      current += character;
      continue;
    }
    if (
      character === ";" ||
      character === "\n" ||
      character === "|" ||
      (character === "&" && next === "&")
    ) {
      if (current.trim()) segments.push(current.trim());
      current = "";
      if (character === "&") index += 1;
      continue;
    }
    current += character;
  }
  if (current.trim()) segments.push(current.trim());
  if (quote !== null) return [];
  return segments;
}

function shellWords(segment: string): string[] | null {
  const words: string[] = [];
  let current = "";
  let quote: "'" | '"' | null = null;
  let escaped = false;
  for (let index = 0; index < segment.length; index += 1) {
    const character = segment[index];
    if (character === undefined) continue;
    if (escaped) {
      current += character;
      escaped = false;
      continue;
    }
    if (character === "\\" && quote !== "'") {
      escaped = true;
      continue;
    }
    if (quote !== null) {
      if (character === quote) quote = null;
      else current += character;
      continue;
    }
    if (character === "'" || character === '"') {
      quote = character;
      continue;
    }
    if (/\s/.test(character)) {
      if (current) words.push(current);
      current = "";
      continue;
    }
    current += character;
  }
  if (escaped || quote !== null) return null;
  if (current) words.push(current);
  return words;
}

function classifyShellSegment(
  rawCommand: string,
  args: readonly string[],
): readonly ToolActivityCount[] | null {
  const command = rawCommand.split(/[\\/]/).pop()?.toLowerCase() ?? "";
  if (!command) return null;
  if (command === "echo" || SHELL_PLUMBING_COMMANDS.has(command)) return [];
  if (READ_COMMANDS.has(command)) {
    if (command === "sed" && args.some((arg) => arg === "--in-place" || /-[^-]*i/.test(arg))) {
      return null;
    }
    if (command === "sort" && args.some((arg) => arg === "-o" || arg === "--output")) {
      return null;
    }
    return [{ kind: "read", count: 1 }];
  }
  if (SEARCH_COMMANDS.has(command)) return [{ kind: "search", count: 1 }];
  if (LIST_COMMANDS.has(command)) {
    if (
      command === "find" &&
      args.some((arg) => ["-exec", "-execdir", "-delete", "-ok"].includes(arg))
    ) {
      return null;
    }
    return [{ kind: "list", count: 1 }];
  }
  if (command === "git") return classifyGitSegment(args);
  if (command === "bash" || command === "sh" || command === "zsh") {
    const commandIndex = args.findIndex((arg) => ["-c", "-lc", "--command"].includes(arg));
    const script = commandIndex >= 0 ? args[commandIndex + 1] : undefined;
    return script ? analyzeShellCommand(script) : null;
  }
  return null;
}

/** Ignore output-only stderr/stdout redirection when deciding whether a row is
 * safe to summarize. File redirection remains non-compressible. */
function removeSafeShellRedirections(command: string): string {
  return command.replace(/(?:^|\s)(?:[12]>\s*\/dev\/null|2>&1)(?=\s|[;&|]|$)/g, " ").trim();
}

function classifyGitSegment(args: readonly string[]): readonly ToolActivityCount[] | null {
  if (
    args.some((arg) =>
      [
        "commit",
        "push",
        "pull",
        "reset",
        "clean",
        "restore",
        "switch",
        "merge",
        "rebase",
        "cherry-pick",
      ].includes(arg),
    )
  ) {
    return null;
  }
  if (args.some((arg) => ["-d", "-D"].includes(arg))) return null;
  if (args.some((arg) => SEARCH_GIT_COMMANDS.has(arg))) return [{ kind: "search", count: 1 }];
  if (args.some((arg) => LIST_GIT_COMMANDS.has(arg))) return [{ kind: "list", count: 1 }];
  if (args.some((arg) => READ_GIT_COMMANDS.has(arg))) return [{ kind: "read", count: 1 }];
  return null;
}

function decodeToolDisplayBody(value: unknown): ToolDisplayBody {
  const body = asRecord(value);
  switch (body.kind) {
    case "none":
      return { kind: "none" };
    case "text":
      return { kind: "text", text: text(body.text) };
    case "structured":
      return { kind: "structured", value: body.value };
    case "patch":
      return { kind: "patch", patch: text(body.patch) };
    case "command_output":
      return {
        kind: "command_output",
        command: text(body.command),
        stdout: text(body.stdout),
        stderr: text(body.stderr),
        exitCode: typeof body.exit_code === "number" ? body.exit_code : null,
        truncated: body.truncated === true,
      };
    case "error":
      return {
        kind: "error",
        code: text(body.code, "TOOL_EXECUTION_FAILED"),
        detail: text(body.detail, "Tool execution failed"),
      };
    default:
      return {
        kind: "error",
        code: "INVALID_TOOL_DISPLAY_BODY",
        detail: "The Tool result body is not supported.",
      };
  }
}

function isPlan(kind: string, toolName: string): boolean {
  return kind === "plan" || kind === "plan_version" || toolName === "todo";
}
function isModel(kind: string, toolName: string): boolean {
  return (
    kind === "model_attempt" ||
    kind === "model_warning" ||
    kind === "model" ||
    toolName.startsWith("model.")
  );
}

function decodePlanSteps(value: unknown): TimelinePlanStep[] {
  if (!Array.isArray(value)) return [];
  return value.map((value) => {
    const step = asRecord(value);
    return {
      text: text(step.text ?? step.title ?? step.label, text(value, JSON.stringify(value))),
      status: optionalText(step.status),
    };
  });
}

function decodeAttachments(value: unknown): TimelineAttachment[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((value) => {
    const attachment = asRecord(value);
    const id = text(attachment.id);
    const name = text(attachment.name);
    if (!id || !name) return [];
    return [
      {
        id,
        name,
        mime: text(attachment.mime, "application/octet-stream"),
        byteSize: typeof attachment.byte_size === "number" ? attachment.byte_size : 0,
      },
    ];
  });
}

function toolStatus(projection: Projection, fallback: string): string {
  return text(projection.status, fallback || "unknown");
}

function firstRecord(...values: unknown[]): Projection {
  for (const value of values) {
    if (isRecord(value)) return value;
  }
  return {};
}

function asRecord(value: unknown): Projection {
  return isRecord(value) ? value : {};
}

function isRecord(value: unknown): value is Projection {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function text(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function optionalText(value: unknown): string | null {
  const result = text(value).trim();
  return result ? result : null;
}

function displayValue(value: unknown): string | null {
  return typeof value === "string" || typeof value === "number" ? String(value) : null;
}

/**
 * Human-readable reasoning time for an assistant message, e.g. "for 4s",
 * "for 1m 12s". Rendered in the Thought row title as "Thought for {duration}".
 * - <3000ms → "for a while" (too short to be meaningful as a stopwatch).
 * - null/missing → "for a while" so a completed thought never flashes a bare
 *   "Thought" while its durable duration catches up.
 */
export function formatThoughtDuration(durationMs: number | null | undefined): string {
  if (durationMs == null || !Number.isFinite(durationMs) || durationMs < 0) {
    return "for a while";
  }
  if (durationMs < 3000) return "for a while";
  const totalSeconds = Math.round(durationMs / 1000);
  if (totalSeconds < 60) return `for ${totalSeconds}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return seconds === 0 ? `for ${minutes}m` : `for ${minutes}m ${seconds}s`;
}
