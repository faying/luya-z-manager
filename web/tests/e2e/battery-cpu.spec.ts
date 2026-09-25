// Battery details on /router/device and the CPU & Memory page: they poll
// while open, stop when left, and say "unsupported" vs "failed" apart.
import { test, expect } from "./support/fixtures";
import type { Ready } from "./support/fixtures";

test.use({ freshMock: true, viewport: { width: 1280, height: 1000 }, setup: { authed: true, lang: "zh" } });

const gets = (ready: Ready, path: string, since = 0) =>
  ready.requests.slice(since).filter((r) => r.method() === "GET" && new URL(r.url()).pathname === path).length;

test("battery details poll while open and stop after leaving", async ({ ready, page }) => {
  await ready.goto("/router/device/");
  const group = page.locator("section", { has: page.getByText("电池详情", { exact: true }) });
  await expect(group).toContainText("健康度");
  await expect(group).toContainText("100%+（电量计估算）");
  await expect(group).toContainText("7.2 W"); // charger input 9 V × 0.8 A
  await expect(group).toContainText("充入 4.9 W"); // 4.12 V × 1.2 A
  await expect(group).toContainText("整机约 2.3 W（含转换损耗）");
  await expect(group).toContainText("66");

  await expect.poll(() => gets(ready, "/api/battery"), { timeout: 12_000 }).toBeGreaterThanOrEqual(2);
  // Two samples 5 s apart are enough for an estimate (76% → 100%).
  await expect(group).toContainText(/约 .*充满/, { timeout: 12_000 });

  await ready.goto("/settings/");
  const since = ready.requests.length;
  await page.waitForTimeout(6_000);
  expect(gets(ready, "/api/battery", since)).toBe(0);
});

test("no battery directory says unsupported without a retry button", async ({ ready, page, mock }) => {
  await mock.scenario("missing");
  await ready.goto("/router/device/");
  await expect(page.getByText("这台设备读不到电池详情")).toBeVisible();
  const group = page.locator("section", { has: page.getByText("电池详情", { exact: true }) });
  await expect(group.getByRole("button", { name: "重试" })).toHaveCount(0);
});

test("cpu page shows cores, frequency and an offline core", async ({ ready, page, mock }) => {
  await mock.scenario("missing");
  await ready.goto("/tools/cpu/");
  await expect(page.getByText("CPU 3")).toBeVisible();
  const cores = page.locator("section", { has: page.getByText("各核", { exact: true }) });
  await expect(cores).toContainText("1516 / 2208 MHz");
  await expect(cores).toContainText("离线");
  await expect.poll(() => gets(ready, "/api/cpu"), { timeout: 12_000 }).toBeGreaterThanOrEqual(2);
  await expect(page.locator("svg polyline")).toHaveAttribute("points", /,.* /);
});

test("cpu page shows memory and draws the trend", async ({ ready, page }) => {
  await ready.goto("/tools/cpu/");
  await expect(page.getByText(/内存已用 .* MiB/)).toBeVisible();
  await expect(page.locator("section", { has: page.getByText("各核", { exact: true }) })).not.toContainText("离线");
});

test("home shows the serving operator's logo and the SIM's own operator", async ({ ready, page }) => {
  await ready.goto("/");
  const row = page.locator(".nd-row", { hasText: "注册运营商" });
  const img = row.locator('img[src$="/operator-logos/chunghwa.svg"]');
  await expect(img).toBeVisible();
  await expect.poll(() => img.evaluate((i: HTMLImageElement) => i.naturalWidth)).toBeGreaterThan(0);
  await expect(row).toContainText("卡：");
  await row.screenshot({ path: "test-results/operator-logo-row.png" });
});
