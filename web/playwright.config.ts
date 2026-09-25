// E2E tests against the static export (web/out) and the typed mock agent.
//
//   npm run test:e2e        next build, then all specs
//   npm run test:e2e:only   specs only — web/out must already be current
//
// The web server below serves web/out on 127.0.0.1:4391. Mock agents are
// started by the test fixtures (tests/e2e/support/fixtures.ts), one per
// worker on :9300+n (and :9400+n for flow tests that need a clean one),
// because the mock honours only the newest login token and keeps writes in
// memory. Nothing here talks to a real device.
import { defineConfig, devices } from "@playwright/test";
import { homedir } from "node:os";
import { existsSync } from "node:fs";

const CHROME =
  process.env.E2E_CHROME ??
  `${homedir()}/Library/Caches/ms-playwright/chromium-1234/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing`;
const WEB_PORT = Number(process.env.E2E_WEB_PORT ?? 4391);

export default defineConfig({
  testDir: "tests/e2e",
  outputDir: "test-results",
  fullyParallel: true,
  workers: process.env.CI ? 2 : 6,
  retries: 0,
  timeout: 30_000,
  expect: { timeout: 5_000 },
  reporter: [["list"]],
  // Inventory summary (inventory.spec.ts) is printed after the run.
  globalSetup: "./tests/e2e/support/inventory-setup.ts",
  globalTeardown: "./tests/e2e/support/inventory-teardown.ts",
  use: {
    baseURL: `http://127.0.0.1:${WEB_PORT}`,
    locale: "zh-CN",
    timezoneId: "Asia/Taipei",
    trace: "retain-on-failure",
    launchOptions: existsSync(CHROME) ? { executablePath: CHROME } : {},
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: `node tests/e2e/support/serve-out.mjs ${WEB_PORT}`,
    url: `http://127.0.0.1:${WEB_PORT}/`,
    reuseExistingServer: false,
    stdout: "ignore",
    stderr: "pipe",
  },
});
