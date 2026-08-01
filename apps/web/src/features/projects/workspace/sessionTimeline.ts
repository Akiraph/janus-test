import type { TimelineItemView } from "../../../lib/api";

type Projection = Record<string, unknown>;

interface TimelineItemBase {
  id: string;
  sourceKind: string;
  turnId: string | null;
  createdAt: string;
  itemStatus: string;
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
  | { kind: "error"; code: string; detail: string };

export interface ToolView {
  title: string;
  status: ToolStatus;
  body: ToolDisplayBody;
  expandable: boolean;
  lowNoise: boolean;
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
      choices: string[];
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

export function decodeSessionTimeline(items: TimelineItemView[]): SessionTimelineItem[] {
  return items.map(decodeSessionTimelineItem);
}

export function decodeSessionTimelineItem(item: TimelineItemView): SessionTimelineItem {
  const projection = asRecord(item.projection);
  const summary = asRecord(projection.summary);
  const toolName = text(projection.tool_name ?? summary.tool_name).toLowerCase();
  const base: TimelineItemBase = {
    id: item.id,
    sourceKind: item.kind,
    turnId: item.turn_id ?? null,
    createdAt: item.created_at,
    itemStatus: item.status,
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
        reasoning: text(projection.reasoning),
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

  if (isAsk(item.kind, toolName)) {
    const domain = item.kind === "tool_call" ? summary : projection;
    return {
      ...base,
      type: "ask",
      askId: optionalText(domain.ask_id ?? summary.ask_id ?? projection.ask_id),
      prompt: text(domain.prompt ?? projection.prompt ?? projection.text, "Waiting for an answer"),
      mode: text(domain.mode ?? projection.mode, "blocking"),
      status: text(domain.status, "open"),
      choices: stringList(domain.choices ?? projection.choices),
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
  return {
    title,
    status: normalizeToolStatus(text(display.status)),
    body,
    expandable: body.kind !== "none",
    lowNoise: lowNoiseTool(toolName),
  };
}

/** Low-noise read-only tools whose consecutive calls compress into a summary. */
function lowNoiseTool(toolName: string): boolean {
  return toolName === "read" || toolName === "bash";
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

function stringList(value: unknown): string[] {
  return Array.isArray(value)
    ? value.map((choice) => (typeof choice === "string" ? choice : String(choice))).filter(Boolean)
    : [];
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
