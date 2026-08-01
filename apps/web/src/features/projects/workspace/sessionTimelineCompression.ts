/**
 * Timeline compression layer - merges consecutive low-noise tool calls
 *
 * Inspired by Janus-old conversation-action-compression.ts but adapted for
 * the current event-sourced timeline architecture.
 */

import type { SessionTimelineItem } from "./sessionTimeline";

export function compressTimeline(
  items: readonly SessionTimelineItem[],
): readonly SessionTimelineItem[] {
  const result: SessionTimelineItem[] = [];

  for (let index = 0; index < items.length; index += 1) {
    const item = items[index];
    if (!item) continue;

    // Only compress consecutive tool items that are compressible
    if (!isCompressibleTool(item)) {
      result.push(item);
      continue;
    }

    // Collect consecutive compressible tools
    const group = collectConsecutiveTools(items, index);

    if (group.length === 1) {
      result.push(item);
      continue;
    }

    // Create compressed group
    result.push(createCompressedToolGroup(group));
    index += group.length - 1;
  }

  return result;
}

type CompressibleTool = Extract<SessionTimelineItem, { type: "tool" }>;

function isCompressibleTool(item: SessionTimelineItem | undefined): item is CompressibleTool {
  if (item?.type !== "tool") return false;

  // Only compress low-noise tools: read, bash (CLI commands)
  const toolName = item.toolName.toLowerCase();
  const isLowNoise = toolName === "read" || toolName === "bash";

  // Only compress successful or predictable tools
  const isCompressibleStatus =
    item.view.status === "success" ||
    item.view.status === "running" ||
    (item.view.status === "failure" && item.view.body.kind === "command_output");

  return isLowNoise && isCompressibleStatus && item.view.lowNoise !== false;
}

function collectConsecutiveTools(
  items: readonly SessionTimelineItem[],
  startIndex: number,
): CompressibleTool[] {
  const group: CompressibleTool[] = [];

  for (let index = startIndex; index < items.length; index += 1) {
    const item = items[index];

    if (!isCompressibleTool(item)) {
      break;
    }

    group.push(item);
  }

  return group;
}

interface ToolTypeCounts {
  read: number;
  bash: number;
  other: number;
}

function countToolTypes(tools: readonly CompressibleTool[]): ToolTypeCounts {
  const counts = { read: 0, bash: 0, other: 0 };

  for (const tool of tools) {
    const name = tool.toolName.toLowerCase();
    if (name === "read") {
      counts.read += 1;
    } else if (name === "bash") {
      counts.bash += 1;
    } else {
      counts.other += 1;
    }
  }

  return counts;
}

function createCompressedToolGroup(tools: readonly CompressibleTool[]): SessionTimelineItem {
  const counts = countToolTypes(tools);
  const hasFailures = tools.some((t) => t.view.status === "failure");
  const lastToolRunning = tools[tools.length - 1]?.view.status === "running";

  // Build a human-verb phrase per tool type. Bash commands are classified by
  // the verb they perform so a compressed span reads as an activity, not a
  // raw "Ran N Commands" list.
  const parts: string[] = [];
  if (counts.read > 0) {
    parts.push(formatReadVerb(counts.read, lastToolRunning));
  }
  if (counts.bash > 0) {
    parts.push(formatBashVerb(tools, lastToolRunning));
  }
  if (counts.other > 0) {
    parts.push(`${counts.other} ${counts.other === 1 ? "Tool" : "Tools"}`);
  }

  const title = joinParts(parts);
  const status = lastToolRunning ? "running" : hasFailures ? "failure" : "success";

  // Create a synthetic group tool item
  const groupId = `group:${tools[0]?.id ?? "tools"}:${tools.length}`;

  return {
    type: "tool",
    id: groupId,
    sourceKind: "tool_group",
    turnId: tools[0]?.turnId ?? null,
    createdAt: tools[0]?.createdAt ?? new Date().toISOString(),
    itemStatus: status,
    toolName: "tool_group",
    toolStatus: status,
    summary: {
      count: tools.length,
      types: counts,
    },
    view: {
      title,
      status,
      body: {
        kind: "structured",
        value: {
          tools: tools.map((t) => ({
            id: t.id,
            name: t.toolName,
            title: t.view.title,
            status: t.view.status,
          })),
        },
      },
      expandable: true,
      lowNoise: false,
    },
  };
}

/** Pluralize a noun: 1 stays singular, >1 gets an "s". */
function plural(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

/** Format a non-bash tool verb. Read uses "Reading/Read N Files". */
function formatReadVerb(count: number, running: boolean): string {
  const noun = plural(count, "File");
  if (running) return `Reading ${noun}`;
  return `Read ${noun}`;
}

/** Classify bash commands in a compressed group into a single human verb.
 * Greps/ripgrep → "searching for N patterns", cat/head/tail → "reading N
 * files", ls/find/dir → "listing N directories", test/lint/build → "running
 * N commands". If the group mixes verbs, fall back to "running N commands". */
function formatBashVerb(tools: readonly CompressibleTool[], running: boolean): string {
  const commands = tools
    .filter((t) => t.toolName.toLowerCase() === "bash")
    .map((t) => firstCommandWord(t));

  let operation: "search" | "read" | "list" | "run" = "run";
  let noun = "Command";
  const count = commands.length;

  if (commands.every((cmd) => isSearchCommand(cmd))) {
    operation = "search";
    noun = "Pattern";
  } else if (commands.every((cmd) => isReadCommand(cmd))) {
    operation = "read";
    noun = "File";
  } else if (commands.every((cmd) => isListCommand(cmd))) {
    operation = "list";
    noun = "Directory";
  }

  const verbs: Record<typeof operation, readonly [running: string, completed: string]> = {
    search: ["Searching for", "Searched for"],
    read: ["Reading", "Read"],
    list: ["Listing", "Listed"],
    run: ["Running", "Ran"],
  };
  const verb = verbs[operation][running ? 0 : 1];
  return `${verb} ${plural(count, noun).toLowerCase()}`;
}

function firstCommandWord(tool: CompressibleTool): string {
  const body = tool.view.body;
  if (body.kind === "command_output") {
    return body.command.trim().split(/\s+/)[0]?.toLowerCase() ?? "";
  }
  return "";
}

function isSearchCommand(cmd: string): boolean {
  return ["grep", "rg", "ack", "ag"].includes(cmd);
}

function isReadCommand(cmd: string): boolean {
  return ["cat", "head", "tail", "less", "more"].includes(cmd);
}

function isListCommand(cmd: string): boolean {
  return ["ls", "find", "dir", "tree"].includes(cmd);
}

function joinParts(parts: readonly string[]): string {
  if (parts.length === 0) return "Processed Items";
  if (parts.length === 1) return parts[0] ?? "";
  if (parts.length === 2) return `${parts[0]} and ${parts[1]}`;

  const last = parts[parts.length - 1];
  const rest = parts.slice(0, -1).join(", ");
  return `${rest}, and ${last}`;
}
