import { chromium } from "@playwright/test";
import { mkdirSync } from "node:fs";

const outDir = "tmp-shots";
mkdirSync(outDir, { recursive: true });

const browser = await chromium.launch({ channel: "msedge" });

async function shoot(url: string, name: string) {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, colorScheme: "light" });
  // mock bootstrap + me so the shell renders instead of Setup/Login
  await page.route("**/api/v1/bootstrap", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: { data: { state: "initialized", development_auth: true, webauthn_rp_name: "Janus" } },
    });
  });
  await page.route("**/api/v1/me", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: { data: { display_name: "Owner", csrf_token: "x" } },
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
          events: { min_cursor: "0", max_cursor: "1024" },
          capabilities: [
            { id: "delegated_cli.access", scope: "deployment", state: "ready" },
            { id: "event_streaming.follow", scope: "deployment", state: "ready" },
          ],
          update_available: false,
        },
      },
    });
  });
  await page.route("**/api/v1/model-providers", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: [
          {
            id: "p1",
            display_name: "Anthropic",
            kind: "anthropic",
            base_url: "https://api.anthropic.com",
            api_key_is_set: true,
            enabled: true,
          },
        ],
      },
    });
  });
  await page.route("**/api/v1/models", async (route) => {
    await route.fulfill({
      contentType: "application/json",
      json: {
        data: [
          {
            id: "m1",
            provider_id: "p1",
            display_name: "Claude Opus",
            upstream_model_id: "claude-opus-4-8",
            context_window: "1m",
            supports_tools: true,
            supports_images: true,
            enabled: true,
          },
        ],
      },
    });
  });
  await page.goto(url, { waitUntil: "networkidle" });
  await page.waitForTimeout(600);
  await page.screenshot({ path: `${outDir}/${name}.png`, fullPage: true });
  await page.close();
}

await shoot("http://127.0.0.1:5173/", "home");
await shoot("http://127.0.0.1:5173/system", "system");
await shoot("http://127.0.0.1:5173/settings", "settings");
await shoot("http://127.0.0.1:5173/security", "security");

await browser.close();
console.log("done");
