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

export type SessionTimelineItem =
  | (TimelineItemBase & { type: "user"; text: string })
  | (TimelineItemBase & { type: "assistant"; text: string })
  | (TimelineItemBase & { type: "steer"; text: string })
  | (TimelineItemBase & {
      type: "tool";
      toolName: string;
      toolStatus: string;
      summary: unknown;
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
      return { ...base, type: "user", text: text(projection.text) };
    case "assistant_message":
      return { ...base, type: "assistant", text: text(projection.text) };
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
      detail: text(
        summary.detail ?? summary.error ?? projection.detail ?? projection.message,
      ),
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
    return {
      ...base,
      type: "tool",
      toolName: text(projection.tool_name, "Tool"),
      toolStatus: toolStatus(projection, item.status),
      summary: projection.summary ?? {},
    };
  }

  return { ...base, type: "unknown", raw: item.projection };
}

function isPlan(kind: string, toolName: string): boolean {
  return kind === "plan" || kind === "plan_version" || toolName === "update_plan";
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
    ? value
        .map((choice) => (typeof choice === "string" ? choice : String(choice)))
        .filter(Boolean)
    : [];
}
