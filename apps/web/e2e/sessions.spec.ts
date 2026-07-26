import { expect, test } from "@playwright/test";

const viewports = [
  { name: "desktop", width: 1440, height: 900 },
  { name: "mobile", width: 390, height: 844 },
] as const;

const projectId = "proj-session-e2e";
const sessionId = "sess-session-e2e";

async function mockSessionApis(page: import("@playwright/test").Page) {
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
  await page.route(`**/api/v1/projects/${projectId}`, async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: {
          id: projectId,
          name: "Session Demo",
          state: "ready",
          repository: {
            access: "public_https",
            url: "https://example.com/demo.git",
            branch: "main",
          },
          current_branch: "main",
          main_revision: "rev_test",
          version: "v_test",
          created_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
          last_activity_at: new Date().toISOString(),
        },
      },
    });
  });
  // File tree empty so Explorer is quiet.
  await page.route(`**/api/v1/projects/${projectId}/files/**`, async (route) => {
    await route.fulfill({ contentType: "application/json", json: { data: [] } });
  });
  await page.route(`**/api/v1/projects/${projectId}/sessions**`, async (route) => {
    if (route.request().method() === "POST") {
      await route.fulfill({
        status: 201,
        contentType: "application/json",
        json: {
          data: {
            id: sessionId,
            project_id: projectId,
            kind: "regular",
            title: "New session",
            state: "ready",
            workspace_handle: `session:${sessionId}`,
            workspace_revision: "rev_s1",
            source_main_revision_id: "rev_test",
            active_turn_id: null,
            version: "v_sess_1",
            created_at: new Date().toISOString(),
            updated_at: new Date().toISOString(),
            last_activity_at: new Date().toISOString(),
          },
        },
      });
      return;
    }
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: [
          {
            id: sessionId,
            project_id: projectId,
            kind: "regular",
            title: "Demo chat",
            state: "ready",
            workspace_handle: `session:${sessionId}`,
            workspace_revision: "rev_s1",
            source_main_revision_id: "rev_test",
            active_turn_id: null,
            version: "v_sess_1",
            created_at: new Date().toISOString(),
            updated_at: new Date().toISOString(),
            last_activity_at: new Date().toISOString(),
          },
        ],
      },
    });
  });
  await page.route(`**/api/v1/sessions/${sessionId}`, async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: {
          id: sessionId,
          project_id: projectId,
          kind: "regular",
          title: "Demo chat",
          state: "ready",
          workspace_handle: `session:${sessionId}`,
          workspace_revision: "rev_s1",
          source_main_revision_id: "rev_test",
          active_turn_id: null,
          version: "v_sess_1",
          created_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
          last_activity_at: new Date().toISOString(),
        },
      },
    });
  });
  await page.route(`**/api/v1/sessions/${sessionId}/timeline**`, async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: {
          items: [
            {
              id: "tl-1",
              session_id: sessionId,
              turn_id: "turn-1",
              kind: "user_message",
              source_resource_id: "msg-1",
              display_order: 1,
              projection: { kind: "user_message", text: "List the files" },
              status: "active",
              version: "v1",
              created_at: new Date().toISOString(),
            },
            {
              id: "tl-2",
              session_id: sessionId,
              turn_id: "turn-1",
              kind: "tool_call",
              source_resource_id: "tc-1",
              display_order: 2,
              projection: {
                kind: "tool_call",
                tool_name: "fs.list",
                status: "succeeded",
                summary: { count: 2 },
              },
              status: "active",
              version: "v1",
              created_at: new Date().toISOString(),
            },
            {
              id: "tl-3",
              session_id: sessionId,
              turn_id: "turn-1",
              kind: "assistant_message",
              source_resource_id: "msg-2",
              display_order: 3,
              projection: {
                kind: "assistant_message",
                text: "Found README.md and src/.",
              },
              status: "active",
              version: "v1",
              created_at: new Date().toISOString(),
            },
          ],
          oldest_cursor: "1",
          newest_cursor: "3",
          has_older: false,
          has_newer: false,
        },
      },
    });
  });
  await page.route(`**/api/v1/sessions/${sessionId}/diff**`, async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: {
          apply_enabled: false,
          sync_enabled: false,
          note: "Apply/Sync land in M5",
          summary: {
            files: [{ path: "hello.txt", change: "added" }],
          },
        },
      },
    });
  });
  await page.route(`**/api/v1/sessions/${sessionId}/messages`, async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: {
          route: "started",
          message_id: "msg-new",
          turn_id: "turn-new",
          session_version: "v_sess_2",
        },
      },
    });
  });
}

for (const viewport of viewports) {
  test(`${viewport.name} session opens as project tab`, async ({ page }, testInfo) => {
    test.setTimeout(60_000);
    await page.setViewportSize(viewport);
    await mockSessionApis(page);

    await page.goto(`/projects/${projectId}?view=sessions`, {
      waitUntil: "domcontentloaded",
    });

    // Sessions activity rail + list (sidebar)
    await expect(page.getByRole("button", { name: "Sessions" }).first()).toBeVisible();
    await expect(page.getByRole("button", { name: /Demo chat/i })).toBeVisible();

    // Open as a main-area tab (not a route change)
    await page.getByRole("button", { name: /Demo chat/i }).click();
    await expect(
      page.getByRole("tablist", { name: "Open documents" }).getByRole("tab").filter({
        hasText: "Demo chat",
      }),
    ).toBeVisible();
    await expect(page).toHaveURL(new RegExp(`/projects/${projectId}`));

    await expect(page.getByText("List the files")).toBeVisible();
    await expect(page.getByText("fs.list")).toBeVisible();
    await expect(page.getByText("Found README.md and src/.")).toBeVisible();

    // Diff is a Session sub-tab (UX-SES-02). Name may include a count badge ("Diff 1").
    await page
      .getByRole("tablist", { name: "Session views" })
      .getByRole("tab", { name: /^Diff/i })
      .click();
    await expect(page.getByText("Apply disabled")).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText("hello.txt")).toBeVisible();

    await page
      .getByRole("tablist", { name: "Session views" })
      .getByRole("tab", { name: /^Main$/i })
      .click();
    await page.getByPlaceholder(/Message the Supervisor/i).fill("Thanks");
    await page.getByRole("button", { name: "Send" }).click();

    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth > document.documentElement.clientWidth,
    );
    expect(overflow).toBe(false);
    await page.screenshot({
      path: testInfo.outputPath(`sessions-tab-${viewport.name}.png`),
      fullPage: true,
    });
  });
}
