import { describe, expect, it } from "bun:test";
import type { SessionSummary } from "./api";
import { sessionTimelineRefetchInterval } from "./queryPolicy";

function session(activeTurnId: string | null): SessionSummary {
  return {
    id: "session-1",
    project_id: "project-1",
    kind: "regular",
    state: activeTurnId ? "active" : "ready",
    workspace_handle: "workspace-1",
    active_turn_id: activeTurnId,
    source_main_revision_id: "revision-1",
    version: "v_session",
    created_at: "2026-07-31T00:00:00.000Z",
    updated_at: "2026-07-31T00:00:00.000Z",
    last_activity_at: "2026-07-31T00:00:00.000Z",
  };
}

describe("Session timeline polling policy", () => {
  it("polls while the authoritative Session has an active Turn", () => {
    expect(sessionTimelineRefetchInterval(1, session("turn-1"))).toBe(3000);
  });

  it("fully pauses once the Session is idle", () => {
    expect(sessionTimelineRefetchInterval(1, session(null))).toBe(false);
  });

  it("does not poll an empty timeline", () => {
    expect(sessionTimelineRefetchInterval(0, session("turn-1"))).toBe(false);
  });
});
