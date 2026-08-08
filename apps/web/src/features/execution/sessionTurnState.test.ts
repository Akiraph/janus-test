import { describe, expect, test } from "bun:test";

import { renderSessionTurnId, stableSessionTurnId } from "./sessionTurnState";

describe("stableSessionTurnId", () => {
  test("keeps the last visible turn while queries are transiently empty", () => {
    expect(
      stableSessionTurnId("turn-1", {
        active: undefined,
        pending: undefined,
        latest: undefined,
      }),
    ).toBe("turn-1");
  });

  test("uses the active turn before the pending or latest turn", () => {
    expect(
      stableSessionTurnId("turn-0", {
        active: "turn-active",
        pending: "turn-pending",
        latest: "turn-latest",
      }),
    ).toBe("turn-active");
  });

  test("keeps a newly accepted pending turn visible before it reaches timeline", () => {
    expect(
      stableSessionTurnId(undefined, {
        active: undefined,
        pending: "turn-pending",
        latest: undefined,
      }),
    ).toBe("turn-pending");
  });

  test("does not replace the visible turn with an older timeline snapshot", () => {
    expect(
      stableSessionTurnId("turn-new", {
        active: undefined,
        pending: undefined,
        latest: "turn-old",
      }),
    ).toBe("turn-new");
  });

  test("keeps a started turn ahead of the stale active-turn query", () => {
    expect(
      renderSessionTurnId(
        "turn-old",
        {
          active: "turn-old",
          pending: "turn-new",
          latest: "turn-old",
        },
        { id: "turn-new", route: "started" },
      ),
    ).toBe("turn-new");
  });

  test("does not replace a running turn with a queued message", () => {
    expect(
      renderSessionTurnId(
        "turn-active",
        {
          active: "turn-active",
          pending: "turn-queued",
          latest: "turn-active",
        },
        { id: "turn-queued", route: "queued" },
      ),
    ).toBe("turn-active");
  });

  test("keeps the previous turn while a queued route briefly loses active state", () => {
    expect(
      renderSessionTurnId(
        "turn-active",
        {
          active: undefined,
          pending: "turn-queued",
          latest: undefined,
        },
        { id: "turn-queued", route: "queued" },
      ),
    ).toBe("turn-active");
  });
});
