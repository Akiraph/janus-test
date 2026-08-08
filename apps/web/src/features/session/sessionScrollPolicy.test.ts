import { describe, expect, test } from "bun:test";

import {
  isNearLatest,
  keepLatestContentVisible,
  scrollTopForContentChange,
} from "./sessionScrollPolicy";

describe("conversation scroll policy", () => {
  test("follows when the viewport is at the latest content", () => {
    expect(isNearLatest(1200, 900, 300)).toBe(true);
  });

  test("stops following after the user scrolls away from the latest content", () => {
    expect(isNearLatest(1200, 500, 300)).toBe(false);
  });

  test("allows a small layout remainder at the bottom", () => {
    expect(isNearLatest(1200, 820, 300, 80)).toBe(true);
  });

  test("keeps the current viewport when the user is away from the latest content", () => {
    expect(scrollTopForContentChange(240, 1600, 600, false)).toBe(240);
  });

  test("moves the viewport to the actual bottom while following", () => {
    expect(scrollTopForContentChange(240, 1600, 600, true)).toBe(1000);
  });

  test("keeps streaming content pinned through rapid height changes", () => {
    const viewport = { scrollTop: 240, scrollHeight: 1600, clientHeight: 600 };

    keepLatestContentVisible(viewport, true);
    expect(viewport.scrollTop).toBe(1000);

    viewport.scrollHeight = 2200;
    keepLatestContentVisible(viewport, true);
    expect(viewport.scrollTop).toBe(1600);
  });
});
