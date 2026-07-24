import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  outputDir: "./test-results",
  reporter: "line",
  use: {
    baseURL: process.env.JANUS_WEB_URL ?? "http://127.0.0.1:5173",
    channel: "msedge",
    colorScheme: "light",
    reducedMotion: "no-preference",
  },
  webServer: {
    command: "bun run dev -- --host 127.0.0.1 --port 5173",
    url: "http://127.0.0.1:5173",
    reuseExistingServer: true,
    timeout: 120_000,
  },
});
