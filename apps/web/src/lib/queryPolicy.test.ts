import { describe, expect, it } from "bun:test";
import { visibleTurnData } from "./queryPolicy";

describe("Turn data visibility", () => {
  it("does not render placeholder data as the newly selected turn", () => {
    const previousTurn = { id: "turn-1", status: "completed" };

    expect(visibleTurnData(previousTurn, "turn-2")).toBeUndefined();
    expect(visibleTurnData(previousTurn, "turn-1")).toBe(previousTurn);
  });
});
