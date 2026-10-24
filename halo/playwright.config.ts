import { defineConfig } from "@playwright/test";
import { fileURLToPath } from "node:url";

export default defineConfig({
  testDir: "./tests",
  workers: 1,
  retries: 0,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  use: {
    baseURL: process.env.HALO_TEST_ORIGIN,
    ignoreHTTPSErrors: true,
    browserName: "chromium",
    viewport: { width: 1440, height: 1000 },
    trace: "off",
    screenshot: "only-on-failure",
  },
  reporter: [
    ["list"],
    [fileURLToPath(new URL("./tools/reporter.ts", import.meta.url))],
  ],
});
