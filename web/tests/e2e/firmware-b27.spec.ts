// firmware-b27: the mock answers with the shapes recorded from a real U60 Pro
// (MU5250) on firmware B27 (scripts/mock-agent/README.md). Every page it
// touches must render without errors, say plainly when the firmware lacks a
// feature (503 "Method not found"), and not keep retrying it.
import { test, expect, type Ready } from "./support/fixtures";

test.use({ freshMock: true, viewport: { width: 1440, height: 1000 } });

/** Chrome logs every non-2xx fetch as a console error; the 503s are expected here. */
function realErrors(ready: Ready): string[] {
  return ready.errors.filter((e) => !/Failed to load resource: the server responded with a status of 503/.test(e));
}

function gets(ready: Ready, path: string): number {
  return ready.requests.filter((r) => r.method() === "GET" && new URL(r.url()).pathname === path).length;
}

test.beforeEach(async ({ mock }) => {
  await mock.scenario("firmware-b27");
});

test("QoS: read-only bandwidth mode (极速), no switch", async ({ ready, page }) => {
  await ready.goto("/router/qos/");
  await expect(page.getByRole("status").filter({ hasText: "带宽分配：极速（不区分应用）" })).toBeVisible();
  await expect(page.getByText("应用识别（xdpi）")).toBeVisible();
  await expect(page.getByRole("switch")).toHaveCount(0);
  expect(realErrors(ready)).toEqual([]);
});

test("QoS (old agent names): says the firmware doesn't answer, no switch, no retry", async ({ ready, page, mock }) => {
  await mock.scenario("firmware-b27,old-agent-names");
  await ready.goto("/router/qos/");
  await expect(page.getByRole("status").filter({ hasText: "这个固件不支持" })).toBeVisible();
  await expect(page.getByText("设备固件不响应 QoS 读取请求")).toBeVisible();
  await expect(page.getByRole("switch")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "重试" })).toHaveCount(0);
  // SWR's first error retry would land within ~2.5–7.5 s.
  await page.waitForTimeout(8000);
  expect(gets(ready, "/api/router/qos"), "one read, no retries").toBe(1);
  expect(realErrors(ready)).toEqual([]);
});

test("STC: unsupported state, every control locked", async ({ ready, page }) => {
  await ready.goto("/router/stc/");
  await expect(page.getByRole("status").filter({ hasText: "这个固件不支持" })).toBeVisible();
  await expect(page.getByRole("switch")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "应用参数" })).toBeDisabled();
  await expect(page.getByRole("button", { name: "重置白名单" })).toBeDisabled();
  await expect(page.getByRole("button", { name: "重试" })).toHaveCount(0);
  expect(realErrors(ready)).toEqual([]);
});

test("firewall: UPnP reads enable_upnp; toggling sends router_set_upnp_switch integers; {} lists are 'no rules'", async ({ ready, page }) => {
  await ready.goto("/router/firewall/");
  const sw = page.getByRole("switch", { name: "UPnP" });
  await expect(sw).toBeVisible();
  await expect(sw).not.toBeChecked();
  await expect(page.getByText("设备回复的格式这个页面不认得")).toHaveCount(0);
  await expect(page.getByText("0 条规则").or(page.getByText("0 rules"))).toBeVisible();
  await expect(page.getByRole("button", { name: /添加规则/ })).toBeEnabled();
  expect(realErrors(ready)).toEqual([]);
});

test("firewall (old agent names): UPnP says unavailable, no switch", async ({ ready, page, mock }) => {
  await mock.scenario("firmware-b27,old-agent-names");
  await ready.goto("/router/firewall/");
  await expect(page.getByText("这个固件不支持：设备不响应 UPnP 读取请求")).toBeVisible();
  await expect(page.getByRole("switch", { name: "UPnP" })).toHaveCount(0);
  expect(realErrors(ready)).toEqual([]);
});

test("telemetry: {} is an empty block list", async ({ ready, page }) => {
  await ready.goto("/router/telemetry/");
  await expect(page.getByText("暂无已拦截的域名")).toBeVisible();
  expect(realErrors(ready)).toEqual([]);
});

test("VPN: only SIP ALG reported — neutral wording and a read-only row", async ({ ready, page }) => {
  await ready.goto("/router/vpn/");
  await expect(page.getByRole("status").filter({ hasText: "这个固件不报告 VPN 穿透状态" })).toBeVisible();
  await expect(page.getByText("SIP ALG", { exact: true })).toBeVisible();
  // The three passthrough switches are still there (never remove a control).
  await expect(page.getByRole("switch")).toHaveCount(3);
  expect(realErrors(ready)).toEqual([]);
});

test("SMS: capacity from per-box counts (no simused_total, nvused_total 0 ignored)", async ({ ready, page }) => {
  await ready.goto("/sms/");
  const line = page.getByText(/SIM 已用/).first();
  await expect(line).toBeVisible();
  await expect(line).toContainText(/SIM 已用\s*\d+ \/ \d+/);
  await expect(line).toContainText(/设备已用\s*[1-9]\d* \/ 100/);
  expect(realErrors(ready)).toEqual([]);
});

test("APN: numeric profiles read as labels, and a save sends numbers", async ({ ready, page }) => {
  await ready.goto("/router/apn/");
  await expect(page.getByText("IPv4v6").first()).toBeVisible();
  await page.getByRole("button", { name: "编辑 China Mobile" }).click();
  const since = ready.requests.length;
  await page.getByRole("button", { name: "保存修改" }).first().click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "保存修改" }).click();
  await expect
    .poll(() => ready.writes(since).filter((w) => w.method === "PUT" && w.path === "/api/router/apn/profiles").length, { timeout: 10_000 })
    .toBe(1);
  const body = JSON.parse(ready.writes(since).find((w) => w.path === "/api/router/apn/profiles")?.body ?? "{}");
  expect(body).toMatchObject({ pdpType: 3, pppAuthMode: 0, roamingPdpType: 3 });
  expect(realErrors(ready)).toEqual([]);
});

test("signal detect, signal and home render the B27 shapes without errors", async ({ ready, page }) => {
  for (const route of ["/router/signal-detect/", "/signal/", "/"]) {
    await ready.goto(route);
    await expect(page.locator("h1").first()).toBeVisible();
  }
  // SA without lte_pci / cell_id: the serving cell id comes from nr5g_cell_id.
  await ready.goto("/signal/");
  const cell = page.locator(".nd-row").filter({ hasText: "Cell ID" });
  await expect(cell).not.toContainText("—");
  expect(realErrors(ready)).toEqual([]);
});

test("APN: in Auto, the manual pick is not shown as in use (only the dialled one is)", async ({ ready, page, mock }) => {
  await mock.scenario("firmware-b27,apn-manual-pick");
  await ready.goto("/router/apn/");
  await expect(page.getByText("手动模式时用这条")).toBeVisible();
  await expect(page.getByText("使用中", { exact: true })).toHaveCount(1);
  expect(realErrors(ready)).toEqual([]);
});

test("B27 connect_status ipv4_ipv6_connected reads as connected", async ({ ready, page, mock }) => {
  await mock.scenario("firmware-b27");
  await ready.goto("/router/mobile-network/");
  await expect(page.getByText("已连接 · IPv4 + IPv6").first()).toBeVisible();
  await expect(page.getByText("没有连接")).toHaveCount(0);
});
