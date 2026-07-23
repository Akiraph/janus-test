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
});
