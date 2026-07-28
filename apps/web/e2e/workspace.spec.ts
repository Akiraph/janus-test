import { expect, test } from "@playwright/test";

const viewports = [
  { name: "desktop", width: 1440, height: 900 },
  { name: "mobile", width: 390, height: 844 },
] as const;

async function mockWorkspaceShell(page: import("@playwright/test").Page) {
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
  await page.route("**/api/v1/projects**", async (route) => {
    if (route.request().method() === "GET") {
      await route.fulfill({
        contentType: "application/json",
        json: { data: [] },
      });
      return;
    }
    await route.fulfill({
      status: 202,
      contentType: "application/json",
      json: {
        data: {
          id: "op-test",
          kind: "project.clone",
          status: "queued",
          target_kind: "project",
          target_id: "proj-test",
          version: "v_test",
          correlation_id: "corr-test",
          created_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
        },
      },
    });
  });
}

for (const viewport of viewports) {
  test(`${viewport.name} workspace shell`, async ({ page }, testInfo) => {
    test.setTimeout(60_000);
    await page.setViewportSize(viewport);
    await mockWorkspaceShell(page);
    await page.goto("/", { waitUntil: "domcontentloaded" });

    await expect(page.getByRole("heading", { name: "Projects", exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Create project" }).first()).toBeVisible();
    await expect(page.getByRole("link", { name: "Settings" })).toBeVisible();

    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth > document.documentElement.clientWidth,
    );
    expect(overflow).toBe(false);
    await page.screenshot({ path: testInfo.outputPath(`${viewport.name}.png`), fullPage: true });
  });
}

test("system route remains usable with reduced motion", async ({ page }) => {
  await page.route("**/api/v1/bootstrap", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: {
          state: "initialized",
          development_auth: true,
          webauthn_rp_name: "Janus",
        },
      },
    });
  });
  await page.route("**/api/v1/system/info", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: {
          version: "0.1.0",
          schema_version: 1,
          mode: "single",
          database: { engine: "sqlite", journal_mode: "wal", ready: true },
          events: { min_cursor: "0", max_cursor: "0" },
          capabilities: [{ id: "delegated_cli.access", scope: "deployment", state: "ready" }],
          update_available: false,
        },
      },
    });
  });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/system");
  await expect(page.getByRole("heading", { name: "System", level: 2 })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Capabilities" })).toBeVisible();
});

test("settings surface keeps M1 model actions", async ({ page }) => {
  await page.goto("/settings");

  await expect(page.getByRole("navigation", { name: "Settings navigation" })).toBeVisible();
  await expect(page.getByText("Model Providers", { exact: true }).first()).toBeVisible();
  await expect(page.getByRole("button", { name: "Add Model Provider" })).toBeVisible();
  await expect(page.locator(".settings-group")).toHaveCount(1);
  await expect(page.getByRole("button", { name: /^Models/ })).toHaveCount(0);

  await page.getByRole("button", { name: "Add Model Provider" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect(page.getByRole("heading", { name: "Add model provider" })).toBeVisible();
  await page.getByRole("button", { name: "Close" }).click();
  await expect(page.getByRole("dialog")).not.toBeVisible();
});

test("settings navigation remains compact on mobile", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/settings");

  const navigation = page.getByRole("navigation", { name: "Settings navigation" });
  await expect(navigation).toBeVisible();
  expect((await navigation.boundingBox())?.height).toBeLessThan(80);

  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth > document.documentElement.clientWidth,
  );
  expect(overflow).toBe(false);
});

test("models render inside their provider card", async ({ page }) => {
  await page.route("**/api/v1/model-providers", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: [
          {
            id: "provider-1",
            display_name: "OpenAI",
            kind: "openai_chat",
            base_url: "https://api.openai.com/v1/",
            api_key_is_set: true,
            api_key_preview: "sk-r********-key",
            models: [
              {
                display_name: "GPT-5",
                upstream_model_id: "gpt-5",
                supports_1m: false,
                supports_images: false,
                enabled: true,
              },
              {
                display_name: "GPT-5-1M",
                upstream_model_id: "gpt-5-1m",
                supports_1m: true,
                supports_images: true,
                enabled: true,
              },
            ],
            enabled: true,
          },
        ],
      },
    });
  });
  // The standalone /models endpoint no longer exists; models embed in providers.

  await page.goto("/settings");

  const providerCard = page.locator(".provider-card");
  await expect(providerCard).toHaveCount(1);
  await expect(providerCard.getByText("GPT-5", { exact: true })).toBeVisible();
  await expect(providerCard.getByText("gpt-5", { exact: true })).toBeVisible();
  // The masked API key preview is shown on the card.
  await expect(providerCard.getByText("sk-r********-key")).toBeVisible();
  // A 1m-capable model shows its 1M chip on the card.
  await expect(providerCard.getByText("GPT-5-1M", { exact: true })).toBeVisible();

  // The single add-provider dialog exposes model rows in place.
  await page.getByRole("button", { name: "Add Model Provider" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect(page.getByRole("heading", { name: "Add model provider" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Add model", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Close" }).click();
});
