import { describe, expect, it } from "bun:test";
import type { SessionSummary, TimelinePage } from "./api";
import { visibleTurnData } from "./queryPolicy";

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

describe("Turn data visibility", () => {
  it("does not render placeholder data as the newly selected turn", () => {
    const previousTurn = { id: "turn-1", status: "completed" };

    expect(visibleTurnData(previousTurn, "turn-2")).toBeUndefined();
    expect(visibleTurnData(previousTurn, "turn-1")).toBe(previousTurn);
  });
});