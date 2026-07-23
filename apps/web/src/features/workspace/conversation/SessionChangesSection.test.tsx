/// <reference types="bun" />

import { describe, expect, test } from "bun:test";
import type { SessionDiff, SupervisorRunRecord } from "@janus/shared";
import { diffTotals, latestRunDiff } from "./SessionChangesSection";

describe("session changes diff helpers", () => {
  test("selects the newest run diff and totals changed lines", () => {
    const older = diffRecord({
      path: "README.md",
      additions: 1,
      deletions: 0,
      updatedAt: "2026-06-27T01:00:00.000Z",
    });
    const newer = diffRecord({
      path: "hello-janus.txt",
      additions: 2,
      deletions: 1,
      updatedAt: "2026-06-27T02:00:00.000Z",
    });

    const diff = latestRunDiff([
      runRecord({ id: "run-1", diff: older }),
      runRecord({ id: "run-2", diff: newer }),
    ]);

    expect(diff?.files[0]?.path).toBe("hello-janus.txt");
    expect(diffTotals(diff)).toEqual({
      files: 1,
      additions: 2,
      deletions: 1,
    });
  });
});

function runRecord({
  id,
  diff,
}: {
  readonly id: string;
  readonly diff: SessionDiff;
}): SupervisorRunRecord {
  return {
    id,
    projectId: "project-1",
    sessionId: "session-1",
    task: "Update files",
    status: "completed",
    transcript: [],
    diff,
    startedAt: diff.updatedAt,
    updatedAt: diff.updatedAt,
    completedAt: diff.updatedAt,
  };
}

function diffRecord({
  path,
  additions,
  deletions,
  updatedAt,
}: {
  readonly path: string;
  readonly additions: number;
  readonly deletions: number;
  readonly updatedAt: string;
}): SessionDiff {
  return {
    sessionId: "session-1",
    files: [{ path, status: "modified", additions, deletions }],
    patch: `diff --git a/${path} b/${path}`,
    updatedAt,
  };
}
