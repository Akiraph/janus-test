import { expect, test } from "@playwright/test";

test("an uninitialized deployment gates protected application requests", async ({ page }) => {
  const protectedRequests: string[] = [];

  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/v1/bootstrap") {
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          data: {
            state: "uninitialized",
            development_auth: false,
            webauthn_rp_name: "Janus",
            version: "0.1.0",
            limits: {
              max_file_bytes: 20_971_520,
              max_message_bytes: 26_214_400,
              max_attachments: 20,
            },
          },
        }),
      });
      return;
    }

    protectedRequests.push(path);
    await route.fulfill({
      status: 401,
      contentType: "application/json",
      body: JSON.stringify({ code: "AUTH_REQUIRED", detail: "authentication is required" }),
    });
  });

  await page.goto("/", { waitUntil: "domcontentloaded" });

  await expect(page.getByRole("heading", { name: "Initialize Janus" })).toBeVisible();
  await expect(page.getByLabel("Initialization token")).toBeVisible();
  await page.waitForTimeout(250);
  expect(protectedRequests).toEqual([]);
});
