import { describe, expect, test } from "bun:test";
import type { SessionApplyRecord } from "@janus/shared";
import {
  findLiveApplyReviewTarget,
  upsertApplyRecord,
} from "./useSessionApplyRecords";

describe("session apply record cache helpers", () => {
  test("targets active apply reviews for live refresh", () => {
    const resolving = applyRecord({
      id: "apply-resolving",
      status: "resolving",
    });

    expect(
      findLiveApplyReviewTarget([
        applyRecord({ id: "apply-applied", status: "applied" }),
        resolving,
        applyRecord({ id: "apply-review-ready", status: "review_ready" }),
      ]),
    ).toBe(resolving);
  });

  test("keeps refreshed apply records newest-first in cache", () => {
    const older = applyRecord({
      id: "older",
      createdAt: "2026-07-05T10:00:00.000Z",
      updatedAt: "2026-07-05T10:00:00.000Z",
    });
    const cached = {
      applies: [older],
    };
    const refreshed = applyRecord({
      id: "newer",
      createdAt: "2026-07-05T11:00:00.000Z",
      updatedAt: "2026-07-05T11:00:00.000Z",
    });

    expect(upsertApplyRecord(cached, refreshed)?.applies).toEqual([
      refreshed,
      older,
    ]);
  });
});

function applyRecord(
  patch: Partial<SessionApplyRecord> = {},
): SessionApplyRecord {
  return {
    id: "apply-1",
    projectId: "project-1",
    sessionId: "session-1",
    status: "review_ready",
    source: {
      checkpointId: "checkpoint-1",
      runId: "run-1",
      ref: "refs/janus/checkpoints/session-1/run-1",
      commitSha: "source-sha",
      title: "Checkpoint",
    },
    integration: {
      workspacePath: "/janus/workspaces/_integrations/project-1/apply-1",
      mainRef: "main",
      mainCommitSha: "main-sha",
      baseRef: "main",
      createdAt: "2026-07-05T09:00:00.000Z",
      updatedAt: "2026-07-05T09:00:00.000Z",
    },
    review: {
      diff: {
        sessionId: "session-1",
        files: [],
        patch: "",
        updatedAt: "2026-07-05T09:00:00.000Z",
      },
      conflictedFiles: [],
    },
    conflictResolution: {
      status: "not_required",
    },
    createdAt: "2026-07-05T09:00:00.000Z",
    updatedAt: "2026-07-05T09:00:00.000Z",
    ...patch,
  };
}
