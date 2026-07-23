import { describe, expect, test } from "bun:test";
import type { ActivityEvent, SupervisorRunRecord } from "@janus/shared";
import { latestSupervisorModelRetryEvent } from "./ActiveRunStatusOutput";

describe("latestSupervisorModelRetryEvent", () => {
  test("returns the latest retry while the active run is running", () => {
    const retry = activityEvent({
      id: "event-1",
      sequence: 1,
      type: "supervisor_model_retry",
      message: "Reconnecting (1/5) after supervisor model failure: timeout.",
    });

    expect(latestSupervisorModelRetryEvent([retry], run())).toBe(retry);
  });

  test("clears retry state after a later model recovery event", () => {
    const retry = activityEvent({
      id: "event-1",
      sequence: 1,
      type: "supervisor_model_retry",
      message: "Reconnecting (1/5) after supervisor model failure: timeout.",
    });
    const recovered = activityEvent({
      id: "event-2",
      sequence: 2,
      type: "supervisor_model_recovered",
      level: "info",
      message: "Supervisor model reconnected.",
    });

    expect(latestSupervisorModelRetryEvent([recovered, retry], run())).toBe(
      undefined,
    );
  });

  test("returns the latest retry attempt while reconnecting", () => {
    const firstRetry = activityEvent({
      id: "event-1",
      sequence: 1,
      type: "supervisor_model_retry",
      message: "Reconnecting (1/5) after supervisor model failure: timeout.",
    });
    const secondRetry = activityEvent({
      id: "event-2",
      sequence: 2,
      type: "supervisor_model_retry",
      message: "Reconnecting (2/5) after supervisor model failure: timeout.",
    });

    expect(
      latestSupervisorModelRetryEvent([secondRetry, firstRetry], run()),
    ).toBe(secondRetry);
  });

  test("ignores retry events when the run is no longer active", () => {
    const retry = activityEvent({
      id: "event-1",
      sequence: 1,
      type: "supervisor_model_retry",
      message: "Reconnecting (1/5) after supervisor model failure: timeout.",
    });

    expect(
      latestSupervisorModelRetryEvent(
        [retry],
        run({ status: "completed", completedAt: "2026-07-05T00:01:00.000Z" }),
      ),
    ).toBe(undefined);
  });

  test("ignores non-connection model warnings", () => {
    const warning = activityEvent({
      id: "event-1",
      sequence: 1,
      type: "supervisor_model_warning",
      message:
        "Supervisor title generation failed. Continuing with the current session title.",
    });

    expect(latestSupervisorModelRetryEvent([warning], run())).toBe(undefined);
  });
});

function activityEvent(
  patch: Pick<ActivityEvent, "id" | "message" | "sequence" | "type"> &
    Partial<Pick<ActivityEvent, "level">>,
): ActivityEvent {
  return {
    sessionId: "session-1",
    timestamp: "2026-07-05T00:00:00.000Z",
    level: patch.level ?? "warn",
    ...patch,
  };
}

function run(patch: Partial<SupervisorRunRecord> = {}): SupervisorRunRecord {
  return {
    id: "run-1",
    projectId: "project-1",
    sessionId: "session-1",
    task: "Build the thing.",
    status: "running",
    transcript: [],
    startedAt: "2026-07-05T00:00:00.000Z",
    updatedAt: "2026-07-05T00:00:00.000Z",
    ...patch,
  };
}
