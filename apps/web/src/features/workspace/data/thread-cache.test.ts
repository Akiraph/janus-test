import { describe, expect, test } from "bun:test";
import type { SessionRecord, SupervisorRunRecord } from "@janus/shared";
import {
  removeThread,
  renameThread,
  threadFromSession,
  updateThreadFromRun,
  upsertThread,
} from "./thread-cache";

describe("workspace thread cache helpers", () => {
  test("upserts a session thread and keeps newest threads first", () => {
    const older = threadFromSession(sessionRecord({ id: "older" }), {
      updatedAt: "2026-06-06T00:00:00.000Z",
    });
    const newer = threadFromSession(sessionRecord({ id: "newer" }), {
      updatedAt: "2026-06-06T00:00:01.000Z",
    });

    const first = upsertThread(undefined, older);
    const second = upsertThread(first, newer);

    expect(second.threads.map((thread) => thread.sessionId)).toEqual([
      "newer",
      "older",
    ]);
  });

  test("renames and removes a cached session thread", () => {
    const thread = threadFromSession(sessionRecord({ id: "session-1" }));
    const renamed = renameThread(
      upsertThread(undefined, thread),
      "session-1",
      "Renamed session",
    );

    expect(renamed?.threads[0]?.title).toBe("Renamed session");
    expect(removeThread(renamed, "session-1")?.threads).toEqual([]);
  });

  test("updates thread status from run updates and keeps newest first", () => {
    const session1 = threadFromSession(sessionRecord({ id: "session-1" }));
    const session2 = threadFromSession(sessionRecord({ id: "session-2" }), {
      updatedAt: "2026-06-06T00:00:01.000Z",
    });
    const current = upsertThread(upsertThread(undefined, session1), session2);
    const updated = updateThreadFromRun(
      current,
      supervisorRunRecord({
        sessionId: "session-1",
        status: "running",
        updatedAt: "2026-06-06T00:00:02.000Z",
      }),
    );

    expect(updated?.threads.map((thread) => thread.sessionId)).toEqual([
      "session-1",
      "session-2",
    ]);
    expect(updated?.threads[0]?.status).toBe("running");
  });
});

function sessionRecord(patch: Partial<SessionRecord> = {}): SessionRecord {
  return {
    id: "session-1",
    projectId: "project-1",
    title: "New session",
    cli: "claude-code",
    status: "running",
    modelGatewayUrl: "http://localhost:4317/api/model-gateway/anthropic",
    startedAt: "2026-06-06T00:00:00.000Z",
    ...patch,
  };
}

function supervisorRunRecord(
  patch: Partial<SupervisorRunRecord> = {},
): SupervisorRunRecord {
  return {
    id: "run-1",
    projectId: "project-1",
    sessionId: "session-1",
    task: "Do work",
    status: "running",
    transcript: [],
    startedAt: "2026-06-06T00:00:00.000Z",
    updatedAt: "2026-06-06T00:00:00.000Z",
    ...patch,
  };
}
