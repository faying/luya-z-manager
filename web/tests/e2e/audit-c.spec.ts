// Audit C, first batch (10-04): what the touch screen showed and the web
// lacked — per-client traffic, Tailscale direct/relay and key expiry, RSSI /
// PLMN / QCI·AMBR, the SIM's number.
import { test, expect } from "./support/fixtures";

test.use({ viewport: { width: 1440, height: 1000 }, setup: { authed: true, lang: "zh" } });

test("each Wi-Fi client shows its rate and total", async ({ ready, page }) => {
  await ready.goto("/clients/");
  const lines = page.getByTestId("client-traffic");
  await expect(lines.first()).toContainText(/↓ [\d.]+ ↑ [\d.]+ Mbps · 连上后共 1\.94 GB/);
  // the first pass has no rate yet: only the total
  await expect(lines.filter({ hasText: "连上后共 6.80 MB" })).not.toContainText("Mbps");
});

test("Tailscale: subnets, key expiry, direct vs relay", async ({ ready, page }) => {
  await ready.goto("/services/tailscale/");
  await expect(page.getByText("子网路由")).toBeVisible();
  await expect(page.getByText("192.168.0.0/24")).toBeVisible();
  await expect(page.getByText(/还剩 1[78]\d 天/)).toBeVisible();
  await expect(page.getByText("2 台 · 直连 1 · 中继 1")).toBeVisible();
  await expect(page.getByTestId("ts-path").first()).toHaveText("直连");
  await expect(page.getByTestId("ts-path").nth(1)).toHaveText("中继 tok");
});

test.describe("key about to expire", () => {
  test.use({ freshMock: true });
  test("the status block warns", async ({ ready, page, mock }) => {
    await mock.scenario("ts-keysoon");
    await ready.goto("/services/tailscale/");
    await expect(page.getByText("密钥 5 天后到期")).toBeVisible();
    await mock.scenario("normal");
  });
});

test("signal page: RSSI, PLMN, QCI · AMBR", async ({ ready, page }) => {
  await ready.goto("/signal/");
  await expect(page.getByText("466-92")).toBeVisible();
  await expect(page.getByText("QCI 9 · ↓ 150 ↑ 75 Mbps")).toBeVisible();
  await expect(page.getByRole("button", { name: "RSSI" })).toBeVisible();
});

test("device info: the SIM's number", async ({ ready, page }) => {
  await ready.goto("/device-info/");
  await expect(page.getByText("本机号码")).toBeVisible();
  await expect(page.getByText("+886900000000")).toBeVisible();
});

test("last 5 minutes: four charts fill in from the page's own samples", async ({ ready, page }) => {
  test.setTimeout(30000);
  await ready.goto("/trends/");
  await expect(page.getByRole("heading", { name: "近 5 分钟" })).toBeVisible();
  await expect(page.getByText("开着这页时每 3 秒采一次")).toBeVisible();
  // two samples (≈6 s) and every chart draws
  await expect(page.getByRole("img")).toHaveCount(4, { timeout: 15000 });
  await expect(page.getByRole("img", { name: "近 5 分钟 CPU 占用" })).toBeVisible();
  await expect(page.getByText(/^\d+% · [+−][\d.]+ W$/)).toBeVisible();
});

// Second batch (10-04): switches, all through datad on the device.
test.describe("switches", () => {
  test.use({ freshMock: true });

  test("stop charging: two-step, says the battery runs the device, read back", async ({ ready, page }) => {
    await ready.goto("/router/device/");
    await page.getByRole("switch", { name: "停止充电" }).click({ force: true });
    await expect(page.getByText("切断充电输入，设备改用电池供电，插着电电量也会往下掉。")).toBeVisible();
    const before = ready.requests.filter((r) => r.method() === "PUT").length;
    await page.getByRole("group").getByRole("button", { name: "停止充电" }).click();
    await expect.poll(() => ready.requests.filter((r) => r.method() === "PUT" && r.url().includes("/api/device/charge-control")).length).toBe(before + 1);
    await expect(page.getByText("已生效")).toBeVisible();
    await expect(page.getByRole("switch", { name: "停止充电" })).toBeChecked();
  });

  test("power off: the strongest confirm, says only the button turns it on", async ({ ready, page }) => {
    await ready.goto("/router/device/");
    await page.getByRole("button", { name: "关机" }).click();
    const dialog = page.getByRole("dialog", { name: "要关机吗？" });
    await expect(dialog).toContainText("在设备上长按电源键约 3 秒。");
    await expect(dialog).toContainText("这里和远程都开不了机");
    await dialog.getByRole("button", { name: "取消" }).click();
    await expect(dialog).toBeHidden();
    expect(ready.requests.some((r) => r.url().includes("/api/device/poweroff"))).toBe(false);
    await page.getByRole("button", { name: "关机" }).click();
    await dialog.getByRole("button", { name: "关机" }).click();
    await expect.poll(() => ready.requests.some((r) => r.method() === "POST" && r.url().includes("/api/device/poweroff"))).toBe(true);
  });

  test("Wi-Fi power save and NFC apply at once and read back", async ({ ready, page }) => {
    await ready.goto("/router/wifi/");
    const box = page.getByTestId("wifi-extras");
    await box.getByRole("switch", { name: "Wi-Fi 节能" }).click({ force: true });
    await expect(box.getByText("已生效")).toBeVisible();
    await expect(box.getByRole("switch", { name: "Wi-Fi 节能" })).toBeChecked();
    await box.getByRole("switch", { name: "NFC 碰一碰" }).click({ force: true });
    await expect(box.getByRole("switch", { name: "NFC 碰一碰" })).toBeChecked();
    const puts = ready.requests.filter((r) => r.method() === "PUT").map((r) => new URL(r.url()).pathname);
    expect(puts).toEqual(expect.arrayContaining(["/api/wifi/power-save", "/api/nfc"]));
  });
});
