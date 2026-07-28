import { expect, test } from "@playwright/test";

const PROJECT_ID = "probe-tabs";

/**
 * Bug 1 复现: 标签页关不掉。
 * mock 文件树给两个可编辑文件, 打开一个, 数 .ide-tab, 点关闭按钮, 再数。
 * 期望修复前: close 后 tab 数不变 (关了又被 openRequest effect 加回来)。
 * 期望修复后: close 后 tab 数 -1。
 */
async function mockProjectShell(page: import("@playwright/test").Page) {
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
      body: 'event: cursor\ndata: {"cursor":"0"}\n\n',
    });
  });
  // project fetch
  await page.route(`**/api/v1/projects/${PROJECT_ID}`, async (route) => {
    if (route.request().method() === "GET") {
      await route.fulfill({
        contentType: "application/json",
        json: {
          data: {
            id: PROJECT_ID,
            name: "Probe",
            repository: { url: "https://x", branch: "main" },
            current_branch: "main",
            state: "ready",
            main_revision: "rev-1",
          },
        },
      });
    }
  });
  // file tree root: two editable files
  await page.route(`**/api/v1/projects/${PROJECT_ID}/files/tree*`, async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: [
          { kind: "file", path: "AGENTS.md", size: 10 },
          { kind: "file", path: "TODO.txt", size: 12 },
        ],
      },
    });
  });
  await page.route(`**/api/v1/projects/${PROJECT_ID}/files/meta*`, async (route) => {
    const u = new URL(route.request().url());
    const p = u.searchParams.get("path") ?? "";
    await route.fulfill({
      contentType: "application/json",
      json: { data: { editable: true, path: p, size: 11, main_revision: "rev-1" } },
    });
  });
  await page.route(`**/api/v1/projects/${PROJECT_ID}/files/content*`, async (route) => {
    const u = new URL(route.request().url());
    const p = u.searchParams.get("path") ?? "";
    await route.fulfill({ status: 200, contentType: "text/plain", body: `content of ${p}` });
  });
  // git status (for SCM panel if switched)
  await page.route(`**/api/v1/projects/${PROJECT_ID}/git/status`, async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: {
          branch: "main",
          head_sha: "merge",
          ahead: 0,
          behind: 0,
          working: ["src/main.ts"],
          untracked: ["README.md"],
          index: ["Cargo.toml"],
        },
      },
    });
  });
  // git log
  await page.route(`**/api/v1/projects/${PROJECT_ID}/git/log*`, async (route) => {
    const entry = (
      sha: string,
      parents: string[],
      message: string,
      changedFiles: number,
      insertions: number,
      deletions: number,
    ) => ({
      sha,
      parents,
      author: "Akiraph",
      committed_at: "2026-07-24T10:13:00+08:00",
      message,
      changed_files: changedFiles,
      insertions,
      deletions,
    });
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: {
          entries: [
            entry("merge", ["left", "right"], "feat: workspace graph", 11, 2664, 99),
            entry("left", ["base"], "fix: harden clone", 2, 20, 4),
            entry("right", ["base"], "feat: deliver project", 4, 120, 8),
            entry("base", [], "feat: build foundation", 8, 300, 0),
          ],
        },
      },
    });
  });
}

test("tabs close when X clicked (Bug 1)", async ({ page }) => {
  test.setTimeout(60_000);
  await mockProjectShell(page);
  await page.goto(`http://127.0.0.1:5173/projects/${PROJECT_ID}`, {
    waitUntil: "domcontentloaded",
  });
  await page.waitForSelector(".ide-shell", { timeout: 15000 });
  await page.waitForSelector(".ide-tree-item", { timeout: 10000 });
  await page.waitForTimeout(400);

  // open first file
  await page.locator(".ide-tree-item").first().click();
  await page.waitForTimeout(600);
  let tabCount = await page.locator(".ide-tab").count();
  console.log("[PROBE] tabs after open first:", tabCount);
  expect(tabCount).toBeGreaterThanOrEqual(1);

  // open second file
  await page.locator(".ide-tree-item").nth(1).click();
  await page.waitForTimeout(600);
  tabCount = await page.locator(".ide-tab").count();
  console.log("[PROBE] tabs after open second:", tabCount);
  expect(tabCount).toBe(2);

  // click close on the first tab
  const firstClose = page.locator(".ide-tab-close").first();
  await firstClose.click();
  await page.waitForTimeout(800);

  const tabCountAfterClose = await page.locator(".ide-tab").count();
  console.log("[PROBE] tabs after close:", tabCountAfterClose);
  expect(tabCountAfterClose).toBe(1);
});

test("legacy workspace shell exposes SCM and selectable Git Graph details", async ({
  page,
}, testInfo) => {
  test.setTimeout(60_000);
  await mockProjectShell(page);
  await page.goto(`/projects/${PROJECT_ID}`, { waitUntil: "domcontentloaded" });

  await expect(page.locator(".workspace-topbar")).toContainText("Workspace:");
  await expect(page.locator(".workspace-topbar")).toContainText("Probe");
  await expect(page.getByRole("button", { name: "Explorer" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Source Control" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Graph" })).toBeVisible();

  await page.getByRole("button", { name: "Source Control" }).click();
  await expect(page.locator(".scm-panel")).toContainText("src/main.ts");
  await expect(page.locator(".scm-panel")).toContainText("README.md");

  await page.getByRole("button", { name: "Graph" }).click();
  await expect(page.locator(".graph-workspace")).toBeVisible();
  await expect(page.locator(".graph-row")).toHaveCount(4);
  await expect(page.locator(".graph-detail")).toContainText("feat: workspace graph");
  await expect(page.locator(".graph-stats")).toContainText("11 files changed");
  await expect(page.locator(".graph-stats")).toContainText("2664 insertions(+)");
  await expect(page.locator(".graph-stats")).toContainText("99 deletions(-)");

  await page.locator('.graph-row[data-sha="right"]').click();
  await expect(page.locator(".graph-detail")).toContainText("feat: deliver project");
  await expect(page.locator(".graph-detail")).toContainText("right");
  await page.screenshot({ path: testInfo.outputPath("project-graph.png"), fullPage: true });
});

test("switching through Graph preserves an editor draft", async ({ page }) => {
  test.setTimeout(60_000);
  await mockProjectShell(page);
  await page.goto(`/projects/${PROJECT_ID}`, { waitUntil: "domcontentloaded" });

  await page.locator(".ide-tree-item").first().click();
  const editor = page.locator(".files-textarea");
  await expect(editor).toBeVisible();
  await editor.fill("unsaved graph-safe draft");

  await page.getByRole("button", { name: "Graph" }).click();
  await expect(page.locator(".graph-workspace")).toBeVisible();
  await page.getByRole("button", { name: "Explorer" }).click();

  await expect(editor).toBeVisible();
  await expect(editor).toHaveValue("unsaved graph-safe draft");
  await expect(page.locator(".ide-tab--dirty")).toHaveCount(1);
});
