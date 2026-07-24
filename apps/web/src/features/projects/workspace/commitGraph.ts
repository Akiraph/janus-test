import type { GitLogEntryView } from "../../../lib/api";

export interface CommitGraphRow {
  readonly commit: GitLogEntryView;
  readonly lanesBefore: readonly (string | undefined)[];
  readonly lanesAfter: readonly (string | undefined)[];
  readonly commitLane: number;
  readonly parentLanes: readonly number[];
  readonly opensBranch: boolean;
}

export interface CommitGraph {
  readonly rows: readonly CommitGraphRow[];
  readonly laneCount: number;
}

/**
 * Projects the newest-first Git log into stable visual lanes.
 * Parent SHAs are the topology source; no branch shape is inferred from row
 * position alone. A lane remains occupied until its expected commit is read.
 */
export function buildCommitGraph(commits: readonly GitLogEntryView[]): CommitGraph {
  let lanes: (string | undefined)[] = [];
  const rows: CommitGraphRow[] = [];
  let laneCount = 1;

  for (const commit of commits) {
    const lanesBefore = lanes.slice();
    let commitLane = lanesBefore.indexOf(commit.sha);

    if (commitLane === -1) {
      commitLane = firstEmptyLane(lanesBefore);
      lanesBefore[commitLane] = commit.sha;
    }

    const lanesAfter = lanesBefore.slice();
    lanesAfter[commitLane] = undefined;
    const parentLanes: number[] = [];

    commit.parents.forEach((parent, parentIndex) => {
      let parentLane = lanesAfter.indexOf(parent);
      if (parentLane === -1) {
        const preferredStart = parentIndex === 0 ? commitLane : commitLane + 1;
        parentLane = firstEmptyLane(lanesAfter, preferredStart);
        lanesAfter[parentLane] = parent;
      }
      parentLanes.push(parentLane);
    });

    rows.push({
      commit,
      lanesBefore,
      lanesAfter,
      commitLane,
      parentLanes,
      opensBranch: commit.parents.length > 1,
    });
    laneCount = Math.max(
      laneCount,
      lanesBefore.length,
      lanesAfter.length,
      commitLane + 1,
      ...parentLanes.map((lane) => lane + 1),
    );
    lanes = compactTrailingEmptyLanes(lanesAfter);
  }

  return { rows, laneCount };
}

function firstEmptyLane(lanes: readonly (string | undefined)[], startIndex = 0): number {
  for (let index = startIndex; index < lanes.length; index += 1) {
    if (lanes[index] === undefined) return index;
  }
  return lanes.length;
}

function compactTrailingEmptyLanes(lanes: readonly (string | undefined)[]): (string | undefined)[] {
  const compacted = lanes.slice();
  while (compacted.length > 0 && compacted[compacted.length - 1] === undefined) {
    compacted.pop();
  }
  return compacted;
}
