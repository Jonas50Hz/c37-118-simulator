import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  testMatch: "poll-open-select.spec.mjs",
  timeout: 8_000,
  expect: {
    timeout: 1_000,
  },
  workers: 1,
  outputDir: "/tmp/c37-118-console-playwright-results",
  use: {
    browserName: "chromium",
    viewport: {
      width: 1280,
      height: 900,
    },
  },
});