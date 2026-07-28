import { expect, test } from "@playwright/test";
import { type LiveJanusEnvironment, startLiveJanus } from "./support/liveJanus";

test.describe.configure({ mode: "serial" });

let live: LiveJanusEnvironment;

test.beforeAll(async () => {
  live = await startLiveJanus();
});

test.afterAll(async () => {
  await live?.stop();
});

test("a browser message completes through the live supervisor", async ({ page }) => {
  test.setTimeout(90_000);

  await page.goto(`/projects/${live.projectId}?view=sessions`, {
    waitUntil: "domcontentloaded",
  });

  const sessionRow = page.getByRole("button", { name: live.sessionTitle, exact: true });
  await expect(sessionRow).toBeVisible({ timeout: 15_000 });
  await sessionRow.click();

  await expect(
    page.getByRole("tablist", { name: "Session views" }).getByRole("tab", { name: "Terminal" }),
  ).toHaveCount(0);

  const composer = page.getByPlaceholder(/Send a message/i);
  await expect(composer).toBeVisible();
  await composer.fill("Complete this through the live supervisor");
  await page.getByRole("button", { name: "Send message" }).click();

  await expect(page.getByText("Complete this through the live supervisor")).toBeVisible();
  await expect(page.getByText("Live fixture reply")).toBeVisible({ timeout: 30_000 });

  const session = await page.request.get(`/api/v1/sessions/${live.sessionId}`);
  expect(session.ok()).toBe(true);
  expect((await session.json()).data.state).toBe("ready");

  const timeline = await page.request.get(`/api/v1/sessions/${live.sessionId}/timeline`);
  expect(timeline.ok()).toBe(true);
  const timelineBody = await timeline.json();
  expect(timelineBody.data.items).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ kind: "user_message" }),
      expect.objectContaining({ kind: "assistant_message" }),
    ]),
  );
});

test("mobile workspace switches between navigation and the active Session", async ({
  page,
}, testInfo) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`/projects/${live.projectId}`, { waitUntil: "domcontentloaded" });

  const sessionRow = page.getByRole("button", { name: live.sessionTitle, exact: true });
  await expect(sessionRow).toBeVisible({ timeout: 15_000 });
  await expect(page.locator(".ide-main")).toBeHidden();
  await sessionRow.click();

  const composer = page.getByPlaceholder(/Send a message/i);
  await expect(composer).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Workspace activity" })).toBeHidden();

  await page.screenshot({ path: testInfo.outputPath("live-session-mobile.png"), fullPage: true });

  await page.getByRole("button", { name: "Open workspace navigation" }).click();
  await expect(sessionRow).toBeVisible();
  await expect(page.locator(".ide-main")).toBeHidden();

  await sessionRow.click();
  await expect(composer).toBeVisible();
});
