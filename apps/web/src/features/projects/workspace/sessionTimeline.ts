import type { AskAnswer, TimelineItemView, TimelineTurnStatus } from "../../../lib/api";
import { normalizeReasoningSummary } from "../../../lib/modelStream";

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

export interface AskChoice {
  label: string;
  annotation: string | null;
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
      type: "ask";
      askId: string | null;
      prompt: string;
      mode: string;
      status: string;
      choices: AskChoice[];
      multiple: boolean;
      answer: AskAnswer | null;
      expiresAt: string | null;
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
      type: "job";
      jobId: string | null;
      command: string;
      status: string;
      toolStatus: string;
    })
  | (TimelineItemBase & {
      type: "service";
      serviceId: string | null;
      command: string;
      status: string;
      impact: string;
      toolStatus: string;
    })
  | (TimelineItemBase & { type: "unknown"; raw: unknown });

export function decodeSessionTimeline(
  items: TimelineItemView[],
  previous: readonly SessionTimelineItem[] = [],
): SessionTimelineItem[] {
  const previousById = new Map(previous.map((item) => [item.id, item]));
  return items
    .filter((item) => !isAskAnswerTimelineItem(item))
    .map((item) => {
      const cached = previousById.get(item.id);
      if (cached?.version !== undefined && cached.version === item.version) return cached;
      return decodeSessionTimelineItem(item);
    });
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

  // A failed ask_user invocation is an ordinary failed Tool result. It must
  // not become a second interactive Ask card beside the successful retry.
  if (
    isAsk(item.kind, toolName) &&
    normalizeToolStatus(toolStatus(projection, item.status)) !== "failure"
  ) {
    const domain = item.kind === "tool_call" ? summary : projection;
    return {
      ...base,
      type: "ask",
      askId: optionalText(domain.ask_id ?? summary.ask_id ?? projection.ask_id),
      prompt: text(
        domain.prompt ?? projection.prompt ?? projection.text,
        "Waiting for your answer...",
      ),
      mode: text(domain.mode ?? projection.mode, "blocking"),
      status: text(domain.status, "open"),
      choices: decodeAskChoices(domain.choices ?? projection.choices),
      multiple: domain.multiple === true,
      answer: decodeAskAnswer(domain.answer ?? projection.answer),
      expiresAt: optionalText(domain.expires_at ?? projection.expires_at),
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

  if (isJob(item.kind, toolName, projection, summary)) {
    return {
      ...base,
      type: "job",
      jobId: optionalText(summary.job_id ?? projection.job_id ?? projection.id),
      command: text(
        summary.command_summary ?? projection.command_summary ?? projection.command,
        "Job",
      ),
      status: text(summary.status ?? projection.status, "unknown"),
      toolStatus: toolStatus(projection, item.status),
    };
  }

  if (isService(item.kind, toolName, projection, summary)) {
    return {
      ...base,
      type: "service",
      serviceId: optionalText(summary.service_id ?? projection.service_id ?? projection.id),
      command: text(
        summary.command_summary ?? projection.command_summary ?? projection.command,
        "Service",
      ),
      status: text(summary.status ?? projection.status, "unknown"),
      impact: text(summary.impact ?? projection.impact, "unknown"),
      toolStatus: toolStatus(projection, item.status),
    };
  }

  if (item.kind === "tool_call") {
    const view = parseToolView(summary, toolName);
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

function parseToolView(summary: Projection, toolName: string): ToolView {
  const display = asRecord(summary.display);
  const title = text(display.title).trim();
  const version = display.version;
  if (version !== 1 || !title) {
    return {
      title: "Invalid Tool output",
      status: "failure",
      body: {
        kind: "error",
        code: "INVALID_TOOL_DISPLAY",
        detail: "The Tool result does not contain a supported display projection.",
      },
      expandable: true,
      lowNoise: false,
    };
  }
  const body = decodeToolDisplayBody(display.body);
  if (body.kind === "error") {
    return {
      title: `Tool error: ${body.code}`,
      status: "failure",
      body,
      expandable: false,
      lowNoise: false,
    };
  }
  const activity = analyzeToolActivity(toolName, body);
  return {
    title,
    status: normalizeToolStatus(text(display.status)),
    body,
    expandable: body.kind !== "none",
    lowNoise: activity !== null,
    ...(activity === null ? {} : { activity }),
  };
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
function isAsk(kind: string, toolName: string): boolean {
  return kind === "ask" || toolName === "ask_user" || toolName === "ask";
}

function isModel(kind: string, toolName: string): boolean {
  return (
    kind === "model_attempt" ||
    kind === "model_warning" ||
    kind === "model" ||
    toolName.startsWith("model.")
  );
}

function isJob(
  kind: string,
  toolName: string,
  projection: Projection,
  summary: Projection,
): boolean {
  return kind === "job" || toolName === "job" || Boolean(summary.job_id ?? projection.job_id);
}

function isService(
  kind: string,
  toolName: string,
  projection: Projection,
  summary: Projection,
): boolean {
  return (
    kind === "service" ||
    toolName === "service" ||
    Boolean(summary.service_id ?? projection.service_id)
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

function decodeAskChoices(value: unknown): AskChoice[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((value) => {
    if (typeof value === "string") {
      const label = value.trim();
      return label ? [{ label, annotation: null }] : [];
    }
    const choice = asRecord(value);
    const label = text(choice.label ?? choice.text).trim();
    if (!label) return [];
    return [
      {
        label,
        annotation: optionalText(choice.annotation ?? choice.description),
      },
    ];
  });
}

function decodeAskAnswer(value: unknown): AskAnswer | null {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) {
    const values = value
      .filter((entry): entry is string => typeof entry === "string")
      .map((entry) => entry.trim())
      .filter(Boolean);
    return values.length > 0 ? values : null;
  }
  return null;
}

function isAskAnswerTimelineItem(item: TimelineItemView): boolean {
  if (item.kind !== "user_message") return false;
  const projection = asRecord(item.projection);
  return (
    projection.ask_answer === true ||
    (optionalText(projection.source_ask_id) !== null && projection.ask_answer !== false)
  );
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
 * - null/missing → "" (caller shows just "Thought" with no trailing duration).
 */
export function formatThoughtDuration(durationMs: number | null | undefined): string {
  if (durationMs == null || !Number.isFinite(durationMs) || durationMs < 0) return "";
  if (durationMs < 3000) return "for a while";
  const totalSeconds = Math.round(durationMs / 1000);
  if (totalSeconds < 60) return `for ${totalSeconds}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return seconds === 0 ? `for ${minutes}m` : `for ${minutes}m ${seconds}s`;
}
