import { describe, expect, test } from "bun:test";
import type { GitLogEntryView } from "../../../lib/api";
import { buildCommitGraph } from "./commitGraph";

function commit(sha: string, parents: string[]): GitLogEntryView {
  return {
    sha,
    parents,
    author: "Akiraph",
    committed_at: "2026-07-24T10:13:00+08:00",
    message: `commit ${sha}`,
    changed_files: 1,
    insertions: 2,
    deletions: 1,
  };
}

describe("buildCommitGraph", () => {
  test("opens a merge lane and rejoins an existing parent lane", () => {
    const graph = buildCommitGraph([
      commit("merge", ["left", "right"]),
      commit("left", ["base"]),
      commit("right", ["base"]),
      commit("base", []),
    ]);

    expect(graph.laneCount).toBe(2);
    expect(graph.rows[0]?.parentLanes).toEqual([0, 1]);
    expect(graph.rows[0]?.opensBranch).toBe(true);
    expect(graph.rows[2]?.commitLane).toBe(1);
    expect(graph.rows[2]?.parentLanes).toEqual([0]);
    expect(graph.rows[3]?.commitLane).toBe(0);
  });

  test("keeps a linear history on one lane", () => {
    const graph = buildCommitGraph([
      commit("three", ["two"]),
      commit("two", ["one"]),
      commit("one", []),
    ]);

    expect(graph.laneCount).toBe(1);
    expect(graph.rows.map((row) => row.commitLane)).toEqual([0, 0, 0]);
  });
});
