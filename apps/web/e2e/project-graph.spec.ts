import { expect, test, type Page } from "@playwright/test";

const PROJECT_ID = "project-graph";

async function mockProjectGraph(page: Page) {
  await page.route("**/api/v1/bootstrap", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: {
          state: "initialized",
          development_auth: true,
          webauthn_rp_name: "Janus",
          version: "0.1.0",
          limits: {
            max_file_bytes: 20_971_520,
            max_message_bytes: 26_214_400,
            max_attachments: 20,
          },
        },
      },
    });
  });
  await page.route("**/api/v1/me", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: {
          id: "owner-test",
          tenant_id: "tenant-test",
          display_name: "Owner",
          authentication_mode: "development",
          csrf_token: "development",
        },
      },
    });
  });
  await page.route("**/api/v1/events**", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "text/event-stream",
      body: "event: cursor\ndata: {\"cursor\":\"0\"}\n\n",
    });
  });
  await page.route(`**/api/v1/projects/${PROJECT_ID}`, async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: {
          id: PROJECT_ID,
          name: "Graph fixture",
          repository: { url: "https://example.test/repo.git", branch: "main" },
          current_branch: "main",
          state: "ready",
          main_revision: "rev-1",
        },
      },
    });
  });
  await page.route(`**/api/v1/projects/${PROJECT_ID}/files/tree*`, async (route) => {
    await route.fulfill({ contentType: "application/json", json: { data: [] } });
  });
  await page.route(`**/api/v1/projects/${PROJECT_ID}/git/status`, async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: {
          branch: "main",
          head_sha: "head",
          ahead: 0,
          behind: 0,
          working: [],
          untracked: [],
          index: [],
        },
      },
    });
  });
  await page.route(`**/api/v1/projects/${PROJECT_ID}/git/log*`, async (route) => {
    const entry = (sha: string, parents: string[], message: string) => ({
      sha,
      parents,
      author: "Akiraph",
      committed_at: "2026-07-24T10:13:00+08:00",
      message,
      changed_files: 2,
      insertions: 20,
      deletions: 4,
    });
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: {
          entries: [
            entry("head", ["three"], "feat: complete M2 git update"),
            entry("three", ["two"], "fix: harden M2 clone operations"),
            entry("two", ["one"], "feat: deliver M2 Project workspace"),
            entry("one", [], "feat: build executable Janus foundation"),
          ],
        },
      },
    });
  });
}

test.beforeEach(async ({ page }) => {
  await mockProjectGraph(page);
  await page.goto(`/projects/${PROJECT_ID}`, { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Source Control" }).click();
  await expect(page.locator(".scm-graph-row")).toHaveCount(4);
});

test("project graph is a compact branch-labelled timeline", async ({ page }) => {
  const rows = page.locator(".scm-graph-row");
  await expect(rows.first()).toContainText("feat: complete M2 git update");
  await expect(rows.first()).toContainText("main");
  await expect(rows.first().locator(".scm-graph-meta")).toHaveCount(0);

  const firstRowBox = await rows.first().boundingBox();
  expect(firstRowBox?.height).toBeLessThanOrEqual(30);
  await expect(rows.nth(1).locator(".scm-graph-node")).toHaveAttribute(
    "fill",
    "var(--graph-lane-0)",
  );
});

test("commit tooltip remains next to the mouse near a viewport edge", async ({ page }) => {
  const row = page.locator(".scm-graph-row").last();
  await row.scrollIntoViewIfNeeded();
  const rowBox = await row.boundingBox();
  if (!rowBox) throw new Error("Commit row has no layout box");

  const pointer = { x: rowBox.x + rowBox.width - 4, y: rowBox.y + rowBox.height - 4 };
  await page.mouse.move(pointer.x, pointer.y);
  const tooltip = page.getByRole("tooltip");
  await expect(tooltip).toBeVisible();
  const tooltipBox = await tooltip.boundingBox();
  if (!tooltipBox) throw new Error("Commit tooltip has no layout box");

  const dx = Math.max(tooltipBox.x - pointer.x, 0, pointer.x - (tooltipBox.x + tooltipBox.width));
  const dy = Math.max(tooltipBox.y - pointer.y, 0, pointer.y - (tooltipBox.y + tooltipBox.height));
  expect(Math.hypot(dx, dy)).toBeLessThanOrEqual(18);
});
