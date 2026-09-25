// Flow specs against the mock agent, located by role + Chinese accessible
// name. "Nothing written before confirm" is asserted on the requests the page
// actually sent (ready.requests), and every negative check in a spec is
// paired with the positive path in the same spec, so a broken request
// capture can't make it pass.
import AxeBuilder from "@axe-core/playwright";
import type { Page } from "@playwright/test";
import { test, expect, type Ready } from "./support/fixtures";

/** axe with an inline confirm open (the axe spec only sees initial states). */
async function axeSerious(page: Page): Promise<string[]> {
  const res = await new AxeBuilder({ page }).analyze();
  return res.violations
    .filter((v) => v.impact === "serious" || v.impact === "critical")
    .flatMap((v) => v.nodes.map((n) => `${v.impact} ${v.id} · ${n.target.join(" ")}`));
}

// Writes change mock state: give every flow its own mock.
test.use({ freshMock: true, viewport: { width: 1440, height: 1000 } });

/** Requests that use POST but only read (not writes). */
const READ_POSTS = new Set(["/api/sms/list", "/api/auth/login"]);

function writesSince(ready: Ready, since: number) {
  return ready.writes(since).filter((w) => !READ_POSTS.has(w.path));
}

/** Give a (wrongly) fired request time to leave the page before asserting none did. */
async function settle(ready: Ready) {
  await ready.page.waitForTimeout(800);
}

test.describe("(a) login", () => {
  test.use({ setup: { authed: false, lang: "zh" } });

  test("wrong password shows an error, right one signs in", async ({ ready, page }) => {
    await ready.goto("/login/");
    await expect(page.getByRole("heading", { level: 1, name: "U60 Pro · MU5250" })).toBeVisible();
    const form = page.getByRole("form", { name: "登录" });
    const pw = form.getByLabel("Agent 密码");
    await expect(pw).toBeVisible();

    // The mock rejects an empty password with 401 (like a wrong one).
    await form.getByRole("button", { name: "登录", exact: true }).click();
    await expect(page.getByRole("alert").filter({ hasText: "密码不对。" })).toBeVisible();
    await expect(pw).toHaveAttribute("aria-invalid", "true");
    await expect(page).toHaveURL(/\/login\/$/);
    expect(await page.evaluate(() => localStorage.getItem("u60.token"))).toBeNull();

    // Advanced shows the agent URL (pointing at this test's mock).
    await form.getByRole("button", { name: "高级" }).click();
    await expect(form.getByLabel("Agent 地址")).toHaveValue(ready.mock.url);
    await expect(form.getByRole("button", { name: "隐藏" })).toBeVisible();

    await pw.fill("anything");
    await form.getByRole("button", { name: "登录", exact: true }).click();
    await expect(page).toHaveURL(/\/$/);
    await expect(page.getByRole("heading", { level: 1, name: "U60 Pro · MU5250" })).toBeVisible();
    await expect(page.getByRole("button", { name: "退出登录" }).first()).toBeAttached();
    expect(await page.evaluate(() => localStorage.getItem("u60.token"))).toMatch(/^mock-token-/);
    expect(ready.errors.filter((e) => !/401/.test(e))).toEqual([]);
  });
});

test("(b) band lock: dialog, cancel sends nothing, confirm sends both NR POSTs", async ({ ready, page }) => {
  await ready.goto("/bandlock/");
  const nr = page.getByRole("region", { name: "NR (5G) 频段" });
  const apply = nr.getByRole("button", { name: "应用 NR 锁定" });
  await expect(apply).toBeDisabled();
  await nr.getByRole("toolbar", { name: "NR (5G) 频段" }).getByRole("button", { name: "n78", exact: true }).click();
  await expect(apply).toBeEnabled();

  let since = ready.requests.length;
  await apply.click();
  const dialog = page.getByRole("dialog", { name: "把 NR 锁定到 n78？" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "取消" }).click();
  await expect(dialog).toBeHidden();
  await settle(ready);
  expect(writesSince(ready, since), "cancel must not write").toEqual([]);

  since = ready.requests.length;
  await apply.click();
  await expect(dialog).toBeVisible();
  // Default focus is Cancel (the safe choice).
  await expect(dialog.getByRole("button", { name: "取消" })).toBeFocused();
  await dialog.getByRole("button", { name: "锁定 NR 频段" }).click();
  await expect
    .poll(() => writesSince(ready, since).filter((w) => w.path === "/api/cell/band/nr").length, { timeout: 10_000 })
    .toBe(2);
  const bodies = writesSince(ready, since)
    .filter((w) => w.path === "/api/cell/band/nr")
    .map((w) => JSON.parse(w.body ?? "{}"));
  expect(bodies).toEqual([
    { nr5g_type: "nsa", nr5g_band: "78" },
    { nr5g_type: "sa", nr5g_band: "78" },
  ]);
  expect(writesSince(ready, since).every((w) => w.method === "POST")).toBe(true);
});

test("(c) DNS Apply asks inline first; no PUT before confirm", async ({ ready, page }) => {
  await ready.goto("/router/dns/");
  const mode = page.getByRole("region", { name: "DNS 模式" });
  const apply = mode.getByRole("button", { name: "应用", exact: true });

  let since = ready.requests.length;
  await apply.click();
  const confirm = page.getByRole("button", { name: "确认：应用" });
  await expect(confirm).toBeVisible();
  await expect(apply).toHaveAttribute("aria-expanded", "true");
  await expect(apply).toHaveAttribute("aria-controls", /.+/);
  expect(await axeSerious(page), "axe with the inline confirm open").toEqual([]);
  await settle(ready);
  expect(writesSince(ready, since), "first press only asks").toEqual([]);

  // Cancel closes it without writing.
  await page.getByRole("group", { name: "应用" }).getByRole("button", { name: "取消" }).click();
  await expect(confirm).toBeHidden();
  await expect(apply).not.toHaveAttribute("aria-expanded", "true");
  await settle(ready);
  expect(writesSince(ready, since)).toEqual([]);

  since = ready.requests.length;
  await apply.click();
  await page.getByRole("button", { name: "确认：应用" }).click();
  await expect
    .poll(() => writesSince(ready, since).map((w) => `${w.method} ${w.path}`), { timeout: 10_000 })
    .toEqual(["PUT /api/router/dns", "POST /api/doh/disable"]);
});

test("(c2) inline confirm behind a wrapper span: SMS compose 发送 asks first", async ({ ready, page }) => {
  await ready.goto("/sms/compose/");
  await page.getByRole("textbox", { name: "收件人（电话号码）" }).fill("+886912345678");
  await page.getByRole("textbox", { name: "内容" }).fill("e2e");
  const send = page.getByRole("button", { name: "发送", exact: true });
  await expect(send).not.toHaveAttribute("aria-expanded", "true");
  let since = ready.requests.length;
  await send.click();
  // The trigger props sit on a <span> around the button; the button itself
  // gets aria-expanded, the span carries no ARIA state.
  await expect(send).toHaveAttribute("aria-expanded", "true");
  await expect(page.locator("span[aria-expanded]")).toHaveCount(0);
  const confirm = page.getByRole("button", { name: /^确认：/ });
  await expect(confirm).toBeVisible();
  expect(await axeSerious(page), "axe with the inline confirm open").toEqual([]);
  await settle(ready);
  expect(writesSince(ready, since), "first press only asks").toEqual([]);

  // The inline confirm's own Cancel (the page's 「取消」 leaves for the inbox).
  await page.getByRole("group").filter({ has: confirm }).getByRole("button", { name: "取消" }).click();
  await expect(confirm).toBeHidden();
  await expect(page).toHaveURL(/\/sms\/compose\/$/);
  await expect(send).toHaveAttribute("aria-expanded", "false");
  await settle(ready);
  expect(writesSince(ready, since)).toEqual([]);

  since = ready.requests.length;
  await send.click();
  await page.getByRole("button", { name: /^确认：/ }).click();
  await expect.poll(() => writesSince(ready, since).map((w) => `${w.method} ${w.path}`)).toContain("POST /api/sms/send");
});

test("(d) factory reset: disabled until 「恢复出厂」 is typed; cancel sends nothing", async ({ ready, page }) => {
  await ready.goto("/router/device/");
  const since = ready.requests.length;
  await page.getByRole("button", { name: "恢复出厂设置" }).click();
  const dialog = page.getByRole("dialog", { name: "清除全部内容，恢复出厂设置？" });
  await expect(dialog).toBeVisible();
  const action = dialog.getByRole("button", { name: "立即重置" });
  await expect(action).toBeDisabled();
  const word = dialog.getByRole("textbox");
  await word.fill("恢复");
  await expect(action).toBeDisabled();
  await word.fill("恢复出厂");
  await expect(action).toBeEnabled();
  await dialog.getByRole("button", { name: "取消" }).click();
  await expect(dialog).toBeHidden();
  await settle(ready);
  expect(writesSince(ready, since), "cancel must not write").toEqual([]);

  // Reopening starts from an empty word again.
  await page.getByRole("button", { name: "恢复出厂设置" }).click();
  await expect(dialog.getByRole("textbox")).toHaveValue("");
  await expect(dialog.getByRole("button", { name: "立即重置" })).toBeDisabled();
  await dialog.getByRole("button", { name: "取消" }).click();
  await settle(ready);
  expect(ready.requests.some((r) => r.url().includes("/api/device/factory-reset"))).toBe(false);
});

test("(e) SMS delete opens a dialog; cancel deletes nothing, confirm deletes", async ({ ready, page }) => {
  await ready.goto("/sms/");
  const list = page.getByRole("region", { name: "短信列表" });
  await list.getByRole("checkbox", { name: "选择来自 +886912000451 的短信" }).check();
  const del = page.getByRole("button", { name: "删除 (1)" });
  await expect(del).toBeVisible();

  let since = ready.requests.length;
  await del.click();
  const dialog = page.getByRole("dialog", { name: "删除 1 条短信？" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "取消" }).click();
  await expect(dialog).toBeHidden();
  await settle(ready);
  expect(writesSince(ready, since).filter((w) => w.path === "/api/sms/delete")).toEqual([]);

  since = ready.requests.length;
  await del.click();
  await dialog.getByRole("button", { name: "删除 (1)" }).click();
  await expect.poll(() => writesSince(ready, since).filter((w) => w.path === "/api/sms/delete").length).toBe(1);
  await expect(list.getByRole("checkbox", { name: "选择来自 +886912000451 的短信" })).toHaveCount(0, { timeout: 10_000 });
});

test("(f2) tier 1: alerts 知道了 marks read at once", async ({ ready, page }) => {
  await ready.goto("/");
  const since = ready.requests.length;
  await page.getByRole("button", { name: "知道了" }).click();
  await expect
    .poll(() => writesSince(ready, since).map((w) => `${w.method} ${w.path}`))
    .toEqual(["POST /api/alerts/read"]);
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("(g) expired token: in-place login dialog, page stays", async ({ ready, page }) => {
  await ready.goto("/router/dns/");
  const url = page.url();
  // Something else logs in: the page's token is now stale (mock keeps only the newest).
  await ready.mock.login();
  const dialog = page.getByRole("dialog", { name: "登录已过期" });
  await expect(dialog).toBeVisible({ timeout: 20_000 });
  expect(page.url()).toBe(url);
  await expect(page.getByRole("heading", { level: 1, name: "DNS 设置" })).toBeAttached();

  // Sign in stays disabled until something is typed.
  await expect(dialog.getByRole("button", { name: "登录", exact: true })).toBeDisabled();
  await dialog.getByLabel("Agent 密码").fill("x");
  await dialog.getByRole("button", { name: "登录", exact: true }).click();
  await expect(dialog).toBeHidden();
  expect(page.url()).toBe(url);
  await expect(page.getByRole("heading", { level: 1, name: "DNS 设置" })).toBeVisible();
  expect(await page.evaluate(() => localStorage.getItem("u60.token"))).toMatch(/^mock-token-/);
});

test.describe("(h) theme", () => {
  // No stored theme: start from the system default (light in this browser).
  test.use({ setup: { authed: true, lang: "zh" }, colorScheme: "light" });

  test("switching in /settings applies at once and survives a reload", async ({ ready, page }) => {
    await ready.goto("/settings/");
    const theme = page.getByRole("radiogroup", { name: "外观" });
    await expect(theme.getByRole("radio", { name: "跟系统" })).toBeChecked();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
    const since = ready.requests.length;
    await theme.getByRole("radio", { name: "深色" }).click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    expect(await page.evaluate(() => localStorage.getItem("u60.theme"))).toBe("dark");
    await settle(ready);
    expect(writesSince(ready, since), "a UI setting writes nothing to the device").toEqual([]);

    await page.reload();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    await expect(page.getByRole("radiogroup", { name: "外观" }).getByRole("radio", { name: "深色" })).toBeChecked();

    await page.getByRole("radiogroup", { name: "外观" }).getByRole("radio", { name: "跟系统" }).click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
    expect(await page.evaluate(() => localStorage.getItem("u60.theme"))).toBeNull();
  });
});
