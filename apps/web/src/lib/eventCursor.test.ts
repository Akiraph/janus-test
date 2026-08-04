import { afterEach, describe, expect, test } from "bun:test";
import {
  clearEventCursor,
  getLastEventCursor,
  reconcileEventCursor,
  reconcileEventCursorBounds,
  setLastEventCursor,
} from "./eventCursor";

afterEach(() => clearEventCursor());

describe("event cursor recovery", () => {
  test("clears a cursor that is ahead of the server high-water mark", () => {
    setLastEventCursor("7070");

    expect(reconcileEventCursor("7011")).toBeNull();
    expect(getLastEventCursor()).toBeNull();
  });

  test("keeps a cursor that the server can still resume", () => {
    setLastEventCursor("7000");

    expect(reconcileEventCursor("7011")).toBe("7000");
    expect(getLastEventCursor()).toBe("7000");
  });

  test("compares decimal cursors without losing precision", () => {
    setLastEventCursor("90071992547409930");

    expect(reconcileEventCursor("9007199254740992")).toBeNull();
  });

  test("moves an expired cursor to the first retained event", () => {
    setLastEventCursor("10");

    expect(reconcileEventCursorBounds("20", "30")).toBe("19");
    expect(getLastEventCursor()).toBe("19");
  });
});
