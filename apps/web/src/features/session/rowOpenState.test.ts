import { describe, expect, test } from "bun:test";
import { clearRowOpenState, rowOpenState, toggleRowOpen } from "./rowOpenState";

describe("row open state", () => {
  test("the first toggle inverts an automatically open row", () => {
    const id = "thought:auto-open";
    clearRowOpenState(id);
    const open = rowOpenState(id, true);

    expect(open()).toBe(true);
    toggleRowOpen(id, open());
    expect(open()).toBe(false);
  });

  test("explicit state survives later automatic defaults", () => {
    const id = "thought:terminal";
    clearRowOpenState(id);
    const initiallyClosed = rowOpenState(id, false);
    toggleRowOpen(id, initiallyClosed());

    expect(initiallyClosed()).toBe(true);
    expect(rowOpenState(id, false)()).toBe(true);
  });
});
