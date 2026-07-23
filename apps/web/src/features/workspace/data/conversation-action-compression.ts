import type {
  ActionStatus,
  ActionView,
  ActivityDetailItem,
  ConversationItem,
  ThoughtConversationItem,
} from "../types";
import { formatCompletedThoughtTitle, formatDuration } from "./thought-title";

export function compressActionGroups(
  items: readonly ConversationItem[],
): readonly ConversationItem[] {
  const result: ConversationItem[] = [];

  for (let index = 0; index < items.length; index += 1) {
    const item = items[index];

    if (item === undefined) {
      continue;
    }

    if (isLowNoiseActivityItem(item)) {
      const group = collectGroup(items, index, isLowNoiseActivityItem);

      if (group.length > 1) {
        result.push(toCompressedActivityItem(group));
        index += group.length - 1;
        continue;
      }
    }

    if (!isCompressibleActionItem(item)) {
      result.push(item);
      continue;
    }

    const group = collectGroup(items, index, isCompressibleActionItem);

    if (group.length === 1) {
      result.push(item);
      continue;
    }

    result.push(toCompressedActionItem(group));
    index += group.length - 1;
  }

  return result;
}

type CompressibleActivityItem = Extract<
  ConversationItem,
  { readonly kind: "action" | "thought" }
>;

function collectGroup<T extends ConversationItem>(
  items: readonly ConversationItem[],
  startIndex: number,
  predicate: (item: ConversationItem | undefined) => item is T,
): T[] {
  const group: T[] = [];

  for (let index = startIndex; index < items.length; index += 1) {
    const item = items[index];

    if (!predicate(item)) {
      break;
    }

    group.push(item);
  }

  return group;
}

function isCompressibleActionItem(
  item: ConversationItem | undefined,
): item is Extract<ConversationItem, { readonly kind: "action" }> {
  return item?.kind === "action" && item.action.compressible === true;
}

function isLowNoiseActivityItem(
  item: ConversationItem | undefined,
): item is CompressibleActivityItem {
  if (item?.kind === "thought") {
    return item.status === "completed";
  }

  return item?.kind === "action" && isLowNoiseAction(item.action);
}

function isLowNoiseAction(action: ActionView): boolean {
  return (
    action.compressible === true &&
    (action.type === "read" || action.type === "cli")
  );
}

function toCompressedActivityItem(
  items: readonly CompressibleActivityItem[],
): ConversationItem {
  const actions = items
    .map((item) => (item.kind === "action" ? item.action : undefined))
    .filter((action): action is ActionView => action !== undefined);
  const thoughts = items
    .map((item) => (item.kind === "thought" ? item : undefined))
    .filter(
      (thought): thought is ThoughtConversationItem => thought !== undefined,
    );
  const counts = countActionTypes(actions);
  const status = compressedStatus(actions);
  const title = compressedActivityTitle(thoughts, counts, items.length);
  const id = `group:${items[0]?.id ?? "activity"}:${items.length}`;

  return {
    kind: "action",
    id,
    at: items[0]?.at ?? "",
    action: {
      id,
      type: dominantActionType(counts),
      title,
      level: status === "failure" ? "warn" : "info",
      status,
      detail: {
        kind: "activity",
        items: items.map(toActivityDetailItem),
      },
    },
  };
}

function toActivityDetailItem(
  item: CompressibleActivityItem,
): ActivityDetailItem {
  return item.kind === "thought"
    ? { kind: "thought", item }
    : { kind: "action", action: item.action };
}

function toCompressedActionItem(
  items: readonly Extract<ConversationItem, { readonly kind: "action" }>[],
): ConversationItem {
  const actions = items.map((item) => item.action);
  const counts = countActionTypes(actions);
  const status = compressedStatus(actions);
  const title = compressedTitle(counts, actions.length);
  const id = `group:${items[0]?.id ?? "actions"}:${actions.length}`;

  return {
    kind: "action",
    id,
    at: items[0]?.at ?? "",
    action: {
      id,
      type: dominantActionType(counts),
      title,
      level: status === "failure" ? "warn" : "info",
      status,
      detail: {
        kind: "actions",
        actions,
      },
    },
  };
}

function compressedStatus(actions: readonly ActionView[]): ActionStatus {
  if (actions.length === 0) {
    return "success";
  }

  if (actions.some((action) => action.status === "failure")) {
    return "failure";
  }

  if (actions.some((action) => action.status === "running")) {
    return "running";
  }

  return "success";
}

interface ActionTypeCounts {
  readonly read: number;
  readonly edit: number;
  readonly command: number;
}

function countActionTypes(actions: readonly ActionView[]): ActionTypeCounts {
  return actions.reduce(
    (counts, action) => ({
      read: counts.read + (action.type === "read" ? 1 : 0),
      edit: counts.edit + (action.type === "edit" ? 1 : 0),
      command: counts.command + (action.type === "cli" ? 1 : 0),
    }),
    { read: 0, edit: 0, command: 0 },
  );
}

function compressedTitle(counts: ActionTypeCounts, total: number): string {
  const parts = [
    counts.edit > 0 ? `Edited ${plural(counts.edit, "File")}` : undefined,
    counts.read > 0 ? `Read ${plural(counts.read, "File")}` : undefined,
    counts.command > 0 ? `Ran ${plural(counts.command, "Command")}` : undefined,
  ].filter((part): part is string => part !== undefined);

  if (parts.length === 0) {
    return `Processed ${plural(total, "Item")}`;
  }

  return joinTitleParts(parts);
}

function compressedActivityTitle(
  thoughts: readonly ThoughtConversationItem[],
  counts: ActionTypeCounts,
  total: number,
): string {
  const parts = [
    thoughts.length > 0 ? thoughtSummaryTitle(thoughts) : undefined,
    counts.read > 0 ? `read ${plural(counts.read, "file")}` : undefined,
    counts.command > 0 ? `ran ${plural(counts.command, "command")}` : undefined,
  ].filter((part): part is string => part !== undefined);

  if (parts.length === 0) {
    return `Processed ${plural(total, "Item")}`;
  }

  return capitalizeTitle(joinSentenceParts(parts));
}

function thoughtSummaryTitle(
  thoughts: readonly ThoughtConversationItem[],
): string {
  if (thoughts.length === 1) {
    const thought = thoughts[0];
    return thought === undefined
      ? "Thought"
      : formatCompletedThoughtTitle(thought);
  }

  const totalMilliseconds = thoughts.reduce(
    (sum, thought) => sum + thoughtDurationMilliseconds(thought),
    0,
  );

  return totalMilliseconds > 0
    ? `Thought for ${formatDuration(totalMilliseconds)}`
    : "Thought";
}

function thoughtDurationMilliseconds(item: ThoughtConversationItem): number {
  const startedAt = Date.parse(item.startedAt ?? item.at);
  const completedAt = Date.parse(item.completedAt ?? item.at);

  if (!Number.isFinite(startedAt) || !Number.isFinite(completedAt)) {
    return 0;
  }

  return Math.max(0, completedAt - startedAt);
}

function joinTitleParts(parts: readonly string[]): string {
  if (parts.length === 1) {
    return parts[0] ?? "";
  }

  if (parts.length === 2) {
    return `${parts[0]} and ${parts[1]}`;
  }

  return `${parts.slice(0, -1).join(", ")}, and ${parts.at(-1)}`;
}

function joinSentenceParts(parts: readonly string[]): string {
  if (parts.length === 1) {
    return parts[0] ?? "";
  }

  if (parts.length === 2) {
    return `${parts[0]} and ${parts[1]}`;
  }

  return `${parts.slice(0, -1).join(", ")} and ${parts.at(-1)}`;
}

function dominantActionType(counts: ActionTypeCounts): ActionView["type"] {
  if (
    counts.edit > 0 &&
    counts.edit >= counts.read &&
    counts.edit >= counts.command
  ) {
    return "edit";
  }

  return counts.read >= counts.command ? "read" : "cli";
}

function plural(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

function capitalizeTitle(value: string): string {
  return value.length === 0
    ? value
    : `${value.charAt(0).toUpperCase()}${value.slice(1)}`;
}
