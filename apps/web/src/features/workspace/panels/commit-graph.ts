import type { GitCommit } from "@janus/shared";

export interface CommitGraphRow {
  readonly commit: GitCommit;
  readonly lanesBefore: readonly (string | undefined)[];
  readonly lanesAfter: readonly (string | undefined)[];
  readonly commitLane: number;
  readonly parentLanes: readonly number[];
  readonly opensBranch: boolean;
}

export function buildCommitGraph(commits: readonly GitCommit[]): {
  readonly rows: readonly CommitGraphRow[];
  readonly laneCount: number;
} {
  let lanes: (string | undefined)[] = [];
  const rows: CommitGraphRow[] = [];
  let laneCount = 1;

  for (const commit of commits) {
    const parents = commit.parentShas ?? [];
    const lanesBefore = lanes.slice();
    let commitLane = lanesBefore.indexOf(commit.sha);

    if (commitLane === -1) {
      commitLane = firstEmptyLane(lanesBefore);
      lanesBefore[commitLane] = commit.sha;
    }

    const lanesAfter = lanesBefore.slice();
    const parentLanes: number[] = [];

    if (parents.length === 0) {
      lanesAfter[commitLane] = undefined;
    } else {
      lanesAfter[commitLane] = parents[0];
      parentLanes.push(commitLane);

      for (const parent of parents.slice(1)) {
        let parentLane = lanesAfter.indexOf(parent);

        if (parentLane === -1) {
          parentLane = firstEmptyLane(lanesAfter, commitLane + 1);
          lanesAfter[parentLane] = parent;
        }

        parentLanes.push(parentLane);
      }
    }

    const row: CommitGraphRow = {
      commit,
      lanesBefore,
      lanesAfter,
      commitLane,
      parentLanes,
      opensBranch: parents.length > 1,
    };
    rows.push(row);
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

function firstEmptyLane(
  lanes: readonly (string | undefined)[],
  startIndex = 0,
): number {
  for (let index = startIndex; index < lanes.length; index += 1) {
    if (lanes[index] === undefined) {
      return index;
    }
  }

  return lanes.length;
}

function compactTrailingEmptyLanes(
  lanes: readonly (string | undefined)[],
): (string | undefined)[] {
  const compacted = lanes.slice();

  while (
    compacted.length > 0 &&
    compacted[compacted.length - 1] === undefined
  ) {
    compacted.pop();
  }

  return compacted;
}
