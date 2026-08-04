/** Compress consecutive low-noise timeline activity into Bun-style rows. */

import type { SessionTimelineItem, ToolStatus } from "./sessionTimeline";
import {
  analyzeToolActivity,
  formatThoughtDuration,
  type ToolActivityCount,
  type ToolActivityDetail,
} from "./sessionTimeline";

export function compressTimeline(
  items: readonly SessionTimelineItem[],
  previous: readonly SessionTimelineItem[] = [],
): readonly SessionTimelineItem[] {
  const result: SessionTimelineItem[] = [];
  const previousById = new Map(previous.map((item) => [item.id, item]));

  for (let index = 0; index < items.length; index += 1) {
    const item = items[index];
    if (!item) continue;

    if (!isCompressibleActivityItem(item)) {
      result.push(item);
      continue;
    }

    const group = collectActivityGroup(items, index);
    if (group.length < 2) {
      result.push(item);
      continue;
    }

    const isLatestGroup = index + group.length === items.length;
    const compressed = createCompressedActivityGroup(group, isLatestGroup);
    const cached = previousById.get(compressed.id);
    result.push(
      cached?.version !== undefined && cached.version === compressed.version ? cached : compressed,
    );
    index += group.length - 1;
  }

  return result;
}

type CompressibleTool = Extract<SessionTimelineItem, { type: "tool" }>;
type CompressibleThought = Extract<SessionTimelineItem, { type: "assistant" }>;
type CompressibleActivityItem = CompressibleTool | CompressibleThought;

function isCompressibleActivityItem(
  item: SessionTimelineItem | undefined,
): item is CompressibleActivityItem {
  if (!item) return false;
  if (item.type === "assistant") {
    return item.reasoning.trim().length > 0 && item.text.trim().length === 0;
  }
  return isCompressibleTool(item);
}

function isCompressibleTool(item: SessionTimelineItem | undefined): item is CompressibleTool {
  if (item?.type !== "tool" || !item.view.lowNoise || item.view.status === "failure") {
    return false;
  }
  return toolActivities(item).length > 0;
}

function collectActivityGroup(
  items: readonly SessionTimelineItem[],
  startIndex: number,
): CompressibleActivityItem[] {
  const group: CompressibleActivityItem[] = [];
  for (let index = startIndex; index < items.length; index += 1) {
    const item = items[index];
    if (!isCompressibleActivityItem(item)) break;
    group.push(item);
  }
  return group;
}

function createCompressedActivityGroup(
  items: readonly CompressibleActivityItem[],
  isLatestGroup: boolean,
): SessionTimelineItem {
  const tools = items.filter((item): item is CompressibleTool => item.type === "tool");
  const thoughts = items.filter((item): item is CompressibleThought => item.type === "assistant");
  const counts = countActivities(tools);
  const status = compressedStatus(tools);
  const isLive = isLatestGroup && (status === "running" || hasActiveTurn(items));
  const title = formatActivityTitle(thoughts, counts, isLive);
  const groupId = `group:${items[0]?.id ?? "activity"}`;
  const version = `${isLatestGroup ? "latest" : "history"}:${items
    .map((item) => `${item.id}:${item.version ?? ""}`)
    .join("|")}`;
  const turnStatus = [...items].reverse().find((item) => item.turnStatus)?.turnStatus ?? null;

  return {
    type: "tool",
    id: groupId,
    sourceKind: "tool_group",
    version,
    turnId: items[0]?.turnId ?? null,
    createdAt: items[0]?.createdAt ?? new Date().toISOString(),
    itemStatus: status,
    turnStatus,
    toolName: "tool_group",
    toolStatus: status,
    summary: {
      count: items.length,
      activities: counts,
    },
    view: {
      title,
      status,
      body: {
        kind: "activity",
        items: items.map(toActivityDetail),
      },
      expandable: true,
      lowNoise: false,
      activity: counts,
    },
  };
}

function toActivityDetail(item: CompressibleActivityItem): ToolActivityDetail {
  if (item.type === "assistant") {
    return {
      kind: "thought",
      id: item.id,
      title: formatThoughtTitle([item], false),
      text: item.reasoning,
      durationMs: item.durationMs,
    };
  }
  return { kind: "tool", id: item.id, name: item.toolName, view: item.view };
}

function toolActivities(tool: CompressibleTool): readonly ToolActivityCount[] {
  return tool.view.activity ?? analyzeToolActivity(tool.toolName, tool.view.body) ?? [];
}

function countActivities(tools: readonly CompressibleTool[]): ToolActivityCount[] {
  const counts: Record<ToolActivityCount["kind"], number> = {
    read: 0,
    search: 0,
    list: 0,
  };
  for (const tool of tools) {
    for (const activity of toolActivities(tool)) counts[activity.kind] += activity.count;
  }
  return (Object.entries(counts) as [ToolActivityCount["kind"], number][])
    .filter(([, count]) => count > 0)
    .map(([kind, count]) => ({ kind, count }));
}

function compressedStatus(tools: readonly CompressibleTool[]): ToolStatus {
  if (tools.some((tool) => tool.view.status === "failure")) return "failure";
  if (tools.some((tool) => tool.view.status === "running")) return "running";
  return "success";
}

function hasActiveTurn(items: readonly CompressibleActivityItem[]): boolean {
  return items.some((item) => {
    const status = item.turnStatus?.status;
    return (
      status !== undefined &&
      [
        "queued",
        "running",
        "waiting_for_job",
        "waiting_for_ask",
        "waiting_for_model",
        "canceling",
      ].includes(status)
    );
  });
}

function formatActivityTitle(
  thoughts: readonly CompressibleThought[],
  counts: readonly ToolActivityCount[],
  live: boolean,
): string {
  const parts: string[] = [];
  if (thoughts.length > 0) parts.push(formatThoughtTitle(thoughts, live));

  for (const activity of counts) {
    const noun = plural(
      activity.count,
      activity.kind === "read" ? "file" : activity.kind === "search" ? "pattern" : "directory",
    );
    if (activity.kind === "read") parts.push(`${live ? "reading" : "read"} ${noun}`);
    if (activity.kind === "search")
      parts.push(`${live ? "searching for" : "searched for"} ${noun}`);
    if (activity.kind === "list") parts.push(`${live ? "listing" : "listed"} ${noun}`);
  }

  if (parts.length === 0) return "Activity";
  return capitalize(joinParts(parts));
}

function formatThoughtTitle(thoughts: readonly CompressibleThought[], live: boolean): string {
  const durationMs = thoughts.reduce((sum, thought) => sum + (thought.durationMs ?? 0), 0);
  const duration = durationMs > 0 ? ` ${formatThoughtDuration(durationMs)}` : "";
  return `${live ? "Thinking" : "Thought"}${duration}`;
}

function joinParts(parts: readonly string[]): string {
  if (parts.length === 1) return parts[0] ?? "";
  if (parts.length === 2) return `${parts[0]} and ${parts[1]}`;
  return `${parts.slice(0, -1).join(", ")}, and ${parts.at(-1)}`;
}

function plural(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

function capitalize(value: string): string {
  return value.length === 0 ? value : `${value.charAt(0).toUpperCase()}${value.slice(1)}`;
}
