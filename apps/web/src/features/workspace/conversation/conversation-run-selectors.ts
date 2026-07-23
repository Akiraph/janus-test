import type {
  SessionCheckpointRecord,
  SupervisorAskRequestRecord,
  SupervisorRunRecord,
} from "@janus/shared";
import type { ConversationItem } from "../types";

export interface CheckpointVersionGraph {
  readonly groupsByParentRunId: ReadonlyMap<string, readonly string[]>;
  readonly childRunIdsByParentRunId: ReadonlyMap<string, readonly string[]>;
}

export function pendingAskRequests(
  runs: readonly SupervisorRunRecord[],
): readonly {
  readonly runId: string;
  readonly ask: SupervisorAskRequestRecord;
}[] {
  return runs
    .flatMap((run) =>
      (run.askRequests ?? [])
        .filter((ask) => ask.status === "pending")
        .map((ask) => ({ runId: run.id, ask })),
    )
    .sort((left, right) =>
      left.ask.requestedAt === right.ask.requestedAt
        ? left.ask.id.localeCompare(right.ask.id)
        : left.ask.requestedAt.localeCompare(right.ask.requestedAt),
    );
}

export function activeSessionRuns(
  runs: readonly SupervisorRunRecord[],
): readonly SupervisorRunRecord[] {
  return runs
    .filter(
      (run) =>
        run.deliveredToRunId === undefined &&
        (run.status === "queued" || run.status === "running"),
    )
    .sort((left, right) =>
      left.startedAt === right.startedAt
        ? left.id.localeCompare(right.id)
        : left.startedAt.localeCompare(right.startedAt),
    );
}

export function buildCheckpointVersionGraph(
  checkpoints: readonly SessionCheckpointRecord[],
): CheckpointVersionGraph {
  const ordered = [...checkpoints].sort(compareCheckpointCreatedAt);
  const checkpointsById = new Map(
    ordered.map((checkpoint) => [checkpoint.id, checkpoint]),
  );
  const checkpointByRunId = new Map<string, SessionCheckpointRecord>();
  const childRunIdsByParentRunId = new Map<string, string[]>();

  for (const checkpoint of ordered) {
    if (checkpoint.runId !== undefined) {
      checkpointByRunId.set(checkpoint.runId, checkpoint);
    }

    if (
      checkpoint.parentCheckpointId === undefined ||
      checkpoint.runId === undefined
    ) {
      continue;
    }

    const parent = checkpointsById.get(checkpoint.parentCheckpointId);
    if (parent?.runId === undefined) {
      continue;
    }

    const children = childRunIdsByParentRunId.get(parent.runId) ?? [];
    children.push(checkpoint.runId);
    childRunIdsByParentRunId.set(parent.runId, children);
  }

  const groupsByParentRunId = new Map<string, readonly string[]>();
  for (const [parentRunId, childRunIds] of childRunIdsByParentRunId.entries()) {
    const parentCheckpoint = checkpointByRunId.get(parentRunId);
    const hasBranchingCheckpoint =
      parentCheckpoint?.origin === "branch" ||
      parentCheckpoint?.origin === "rewind" ||
      childRunIds.some((runId) => {
        const checkpoint = checkpointByRunId.get(runId);
        return (
          checkpoint?.origin === "branch" || checkpoint?.origin === "rewind"
        );
      });

    if (!hasBranchingCheckpoint) {
      continue;
    }

    const runIds = uniqueRunIds([parentRunId, ...childRunIds]);
    if (runIds.length > 1) {
      groupsByParentRunId.set(parentRunId, runIds);
    }
  }

  return { groupsByParentRunId, childRunIdsByParentRunId };
}

export function applyCheckpointVersionSelections(
  runs: readonly SupervisorRunRecord[],
  graph: CheckpointVersionGraph,
  selectedVersionRunIds: Readonly<Record<string, string>>,
): readonly SupervisorRunRecord[] {
  const hiddenRunIds = new Set<string>();

  for (const [
    parentRunId,
    versionRunIds,
  ] of graph.groupsByParentRunId.entries()) {
    const selectedRunId =
      selectedVersionRunIds[parentRunId] ?? versionRunIds.at(-1) ?? parentRunId;

    for (const versionRunId of versionRunIds) {
      if (versionRunId === parentRunId || versionRunId === selectedRunId) {
        continue;
      }

      hideRunSubtree(
        versionRunId,
        graph.childRunIdsByParentRunId,
        hiddenRunIds,
      );
    }

    if (selectedRunId === parentRunId) {
      for (const childRunId of graph.childRunIdsByParentRunId.get(
        parentRunId,
      ) ?? []) {
        hideRunSubtree(
          childRunId,
          graph.childRunIdsByParentRunId,
          hiddenRunIds,
        );
      }
    }
  }

  return runs.filter((run) => !hiddenRunIds.has(run.id));
}

export function withVersionNavigation(
  items: readonly ConversationItem[],
  graph: CheckpointVersionGraph,
  selectedVersionRunIds: Readonly<Record<string, string>>,
): readonly ConversationItem[] {
  return items.map((item) => {
    if (item.kind !== "user") {
      return item;
    }

    const versionRunIds = graph.groupsByParentRunId.get(item.runId);
    if (versionRunIds === undefined) {
      return item;
    }

    const selectedRunId =
      selectedVersionRunIds[item.runId] ?? versionRunIds.at(-1) ?? item.runId;
    const selectedIndex = Math.max(0, versionRunIds.indexOf(selectedRunId));

    return {
      ...item,
      versionNavigation: {
        current: selectedIndex + 1,
        total: versionRunIds.length,
        ...(versionRunIds[selectedIndex - 1] === undefined
          ? {}
          : { previousRunId: versionRunIds[selectedIndex - 1] }),
        ...(versionRunIds[selectedIndex + 1] === undefined
          ? {}
          : { nextRunId: versionRunIds[selectedIndex + 1] }),
      },
    };
  });
}

function compareCheckpointCreatedAt(
  left: SessionCheckpointRecord,
  right: SessionCheckpointRecord,
): number {
  return left.createdAt === right.createdAt
    ? left.id.localeCompare(right.id)
    : left.createdAt.localeCompare(right.createdAt);
}

function hideRunSubtree(
  runId: string,
  childRunIdsByParentRunId: ReadonlyMap<string, readonly string[]>,
  hiddenRunIds: Set<string>,
) {
  if (hiddenRunIds.has(runId)) {
    return;
  }

  hiddenRunIds.add(runId);
  for (const childRunId of childRunIdsByParentRunId.get(runId) ?? []) {
    hideRunSubtree(childRunId, childRunIdsByParentRunId, hiddenRunIds);
  }
}

function uniqueRunIds(runIds: readonly string[]): readonly string[] {
  return [...new Set(runIds)];
}
