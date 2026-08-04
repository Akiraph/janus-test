import { describe, expect, it } from "bun:test";
import type { SessionSummary, TimelinePage } from "./api";
import {
  retainSessionTimeline,
  sessionTimelineRefetchInterval,
  visibleTurnData,
} from "./queryPolicy";

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

  it("does not replace a visible timeline with a transient empty response", () => {
    const previous: TimelinePage = {
      items: [
        {
          id: "item-1",
          session_id: "session-1",
          turn_id: "turn-1",
          kind: "user_message",
          source_resource_id: null,
          display_order: 1,
          projection: { text: "keep me" },
          status: "settled",
          version: "v1",
          created_at: "2026-07-31T00:00:00.000Z",
        },
      ],
      has_older: false,
      has_newer: false,
      oldest_cursor: "1",
      newest_cursor: "1",
    };
    const empty: TimelinePage = {
      items: [],
      has_older: false,
      has_newer: false,
      oldest_cursor: null,
      newest_cursor: null,
    };

    expect(retainSessionTimeline(previous, empty, session("turn-1"))).toBe(previous);
  });

  it("does not let an older overlapping response roll back newer rows", () => {
    const previous: TimelinePage = {
      items: [
        {
          id: "item-2",
          session_id: "session-1",
          turn_id: "turn-2",
          kind: "user_message",
          source_resource_id: null,
          display_order: 2,
          projection: { text: "new" },
          status: "active",
          version: "v2",
          created_at: "2026-07-31T00:00:02.000Z",
        },
      ],
      has_older: true,
      has_newer: false,
      oldest_cursor: "2",
      newest_cursor: "2",
    };
    const older: TimelinePage = {
      items: [
        {
          id: "item-1",
          session_id: "session-1",
          turn_id: "turn-1",
          kind: "user_message",
          source_resource_id: null,
          display_order: 1,
          projection: { text: "old" },
          status: "active",
          version: "v1",
          created_at: "2026-07-31T00:00:01.000Z",
        },
      ],
      has_older: false,
      has_newer: true,
      oldest_cursor: "1",
      newest_cursor: "1",
    };

    expect(retainSessionTimeline(previous, older, session("turn-2"))).toBe(previous);
  });

  it("merges a newer snapshot with rows retained from the previous read", () => {
    const previous: TimelinePage = {
      items: [
        {
          id: "item-1",
          session_id: "session-1",
          turn_id: "turn-1",
          kind: "user_message",
          source_resource_id: null,
          display_order: 1,
          projection: { text: "old" },
          status: "active",
          version: "v1",
          created_at: "2026-07-31T00:00:01.000Z",
        },
      ],
      has_older: false,
      has_newer: false,
      oldest_cursor: "1",
      newest_cursor: "1",
    };
    const newer = {
      ...previous,
      items: [
        {
          ...previous.items[0],
          id: "item-2",
          turn_id: "turn-2",
          display_order: 2,
          projection: { text: "new" },
          version: "v2",
        },
      ],
    };

    const result = retainSessionTimeline(previous, newer, session("turn-2"));
    expect(result.items.map((item) => item.id)).toEqual(["item-1", "item-2"]);
    expect(result.newest_cursor).toBe("2");
  });

  it("does not render placeholder data as the newly selected turn", () => {
    const previousTurn = { id: "turn-1", status: "completed" };

    expect(visibleTurnData(previousTurn, "turn-2")).toBeUndefined();
    expect(visibleTurnData(previousTurn, "turn-1")).toBe(previousTurn);
  });
});
