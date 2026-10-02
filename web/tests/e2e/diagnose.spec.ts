// Diagnose / 网络诊断 (/tools/diagnose, slow-diagnosis.md §12.3, §12.5, §12.8)
// against the mock's deep_diag (scripts/mock-agent/fixtures/diagnose.ts):
// start → 正在检查… N/M → result; the weak persona's main cause and "+1 more";
// the two-press speed test when roaming (the mock SIM roams in Taiwan);
// feedback once; agent gone → 后台没回应 + 重试 keeping the rows; the
// home page's 查原因 → link; the busy 409 on the speed test page; English.
import { test, expect } from "./support/fixtures";
import type { Page } from "@playwright/test";
import type { Ready } from "./support/fixtures";

test.use({ freshMock: true, viewport: { width: 1280, height: 1000 } });

const posts = (ready: Ready, path: string, since = 0) => ready.writes(since).filter((w) => w.method === "POST" && w.path === path);
const status = (page: Page) => page.locator(".nd-status");
const layers = (page: Page) => page.locator("section", { has: page.locator("#dg-layers") });
const row = (page: Page, name: string) => layers(page).locator(".nd-row", { has: page.locator(".nd-row__label", { hasText: new RegExp(`^${name}$`) }) });

/** Start a run from inside the page (same token as the app). */
async function startViaApi(page: Page, mockUrl: string) {
  await page.evaluate(async (url) => {
    await fetch(`${url}/api/diagnose`, { method: "POST", headers: { Authorization: `Bearer ${localStorage.getItem("u60.token")}` }, body: "{}" });
  }, mockUrl);
}

test.describe("zh", () => {
  test.use({ setup: { authed: true, lang: "zh" } });

  test("opening the page does not start a run; 开始 runs it to a result", async ({ ready, page }) => {
    await ready.goto("/tools/diagnose/");
    await expect(page.locator("h1")).toHaveText("网络诊断");
    await expect(status(page)).toContainText("还没查过");
    expect(posts(ready, "/api/diagnose")).toHaveLength(0);

    const since = ready.requests.length;
    await page.getByRole("button", { name: "开始" }).click();
    await expect(status(page)).toContainText(/正在检查…/);
    await expect(status(page)).toContainText("约 10 秒，会发少量探测包");
    await expect(status(page)).toContainText(/正在检查… \d\/6/);
    expect(posts(ready, "/api/diagnose", since)).toHaveLength(1);

    // Pressing again while it runs says so and starts nothing.
    await page.getByRole("button", { name: "再查一次" }).click();
    await expect(page.getByText("正在查，稍等")).toBeVisible();
    expect(posts(ready, "/api/diagnose", since)).toHaveLength(1);

    await expect(status(page)).toContainText("没查到问题", { timeout: 10_000 });
    await expect(page.getByText("正在查，稍等")).toHaveCount(0);
    await expect(status(page)).toContainText("可能是对方网站慢；也可以加测速度");
    await expect(status(page)).toContainText(/\d\d:\d\d 测/);
    await expect(page.locator(".nd-status--ok")).toBeVisible();
    await expect(row(page, "Wi-Fi")).toContainText("●正常");
    await expect(row(page, "信号")).toContainText("RSRP −95 dBm · SINR 18 dB");
    await expect(row(page, "限速")).toContainText("测不了 · QoS 读不到");
    await expect(row(page, "基站负载")).toContainText("测不了 · 历史不够");
    // The mock persona runs the proxy service: the proxy layer is there.
    await expect(row(page, "代理")).toContainText("正常");
    expect(ready.errors).toEqual([]);
  });

  test("weak signal: red main cause, one action, +1 more; the link layer is a suspect", async ({ ready, page, mock }) => {
    await mock.scenario("weak");
    await ready.goto("/tools/diagnose/");
    await page.getByRole("button", { name: "开始" }).click();
    await expect(status(page)).toContainText("信号弱", { timeout: 10_000 });
    await expect(page.locator(".nd-status--bad")).toBeVisible();
    await expect(status(page)).toContainText("固定位置时用摆放模式；在路上只能等");
    await expect(status(page)).toContainText("另有 1 处疑点");
    await expect(row(page, "信号")).toContainText("■差");
    await expect(row(page, "蜂窝链路")).toContainText("▲疑点");
  });

  test("加测速度 asks twice while roaming, then adds the speed row", async ({ ready, page }) => {
    await ready.goto("/tools/diagnose/");
    await page.getByRole("button", { name: "开始" }).click();
    await expect(status(page)).toContainText("没查到问题", { timeout: 10_000 });

    const since = ready.requests.length;
    await page.getByRole("button", { name: "加测速度 · 约 5 秒、最多 30 MB" }).click();
    const again = page.getByRole("button", { name: "走漫游流量，再按一次" });
    await expect(again).toBeVisible();
    expect(posts(ready, "/api/diagnose/speed", since)).toHaveLength(0);
    await again.click();
    await expect.poll(() => posts(ready, "/api/diagnose/speed", since).length).toBe(1);
    await expect(row(page, "速度")).toContainText("测试中…");
    await expect(row(page, "速度")).toContainText("直连 ↓ 86 Mbps", { timeout: 8_000 });
  });

  test("对 / 不对: answered once, then 已记下，谢谢", async ({ ready, page }) => {
    await ready.goto("/tools/diagnose/");
    await page.getByRole("button", { name: "开始" }).click();
    await expect(status(page)).toContainText("没查到问题", { timeout: 10_000 });
    const since = ready.requests.length;
    await page.getByRole("button", { name: "对", exact: true }).click();
    await expect(page.getByText("已记下，谢谢")).toBeVisible();
    await expect(page.getByRole("button", { name: "不对" })).toHaveCount(0);
    const fb = posts(ready, "/api/diagnose/feedback", since);
    expect(fb).toHaveLength(1);
    expect(JSON.parse(fb[0].body ?? "{}")).toMatchObject({ right: true });
  });

  test("a result from the last 10 minutes shows again on return", async ({ ready, page }) => {
    await ready.goto("/tools/diagnose/");
    await page.getByRole("button", { name: "开始" }).click();
    await expect(status(page)).toContainText("没查到问题", { timeout: 10_000 });
    const since = ready.requests.length;
    await ready.goto("/settings/");
    await ready.goto("/tools/diagnose/");
    await expect(status(page)).toContainText("没查到问题");
    await expect(page.getByRole("button", { name: "再查一次" })).toBeVisible();
    expect(posts(ready, "/api/diagnose", since)).toHaveLength(0);
  });

  test("waiting for another operation", async ({ ready, page, mock }) => {
    await mock.scenario("diag-waiting");
    await ready.goto("/tools/diagnose/");
    await page.getByRole("button", { name: "开始" }).click();
    await expect(status(page)).toContainText("等另一个操作做完…");
    await expect(status(page)).toContainText("正在测速，做完自动开始");
    await expect(row(page, "信号")).toContainText("等待");
    await expect(status(page)).toContainText("没查到问题", { timeout: 12_000 });
  });

  test("agent gone: 后台没回应 + 重试, the rows stay", async ({ ready, page, mock }) => {
    await ready.goto("/tools/diagnose/");
    await page.getByRole("button", { name: "开始" }).click();
    await expect(status(page)).toContainText("没查到问题", { timeout: 10_000 });
    await mock.scenario("down");
    await page.getByRole("button", { name: "再查一次" }).click();
    await expect(status(page)).toContainText("后台没回应", { timeout: 10_000 });
    await expect(page.locator(".nd-status--bad")).toBeVisible();
    await expect(row(page, "信号")).toContainText("正常");
    await mock.scenario("normal");
    await page.getByRole("button", { name: "重试" }).click();
    await expect(status(page)).toContainText(/正在检查…|没查到问题/, { timeout: 10_000 });
    await expect(status(page)).not.toContainText("后台没回应");
  });

  test("an older agent without /api/diagnose: says so instead of reading forever", async ({ ready, page }) => {
    await page.route(/\/api\/diagnose$/, (route) =>
      route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ ok: false, error: "not found" }) }),
    );
    await ready.goto("/tools/diagnose/");
    await expect(status(page)).toContainText("设备上的后台还不能诊断");
    await expect(status(page)).toContainText("not found");
    await expect(status(page)).not.toContainText("正在读取");
  });

  test("home: weak signal offers 查原因 →, which starts a run", async ({ ready, page, mock }) => {
    await mock.scenario("weak");
    await ready.goto("/");
    const link = page.getByRole("link", { name: "查原因 →" });
    await expect(link).toBeVisible();
    // The existing next step stays beside it.
    await expect(page.getByRole("link", { name: "去锁频 ›" })).toBeVisible();
    await link.click();
    await expect(page).toHaveURL(/\/tools\/diagnose\/?\?start=1$/);
    await expect(status(page)).toContainText(/正在检查…|信号弱/);
    await expect.poll(() => posts(ready, "/api/diagnose").length).toBe(1);
  });

  for (const v of ["stall", "crowd"] as const) {
    test(`home: datad verdict ${v} with good signal offers 查原因 →`, async ({ ready, page, mock }) => {
      await mock.scenario(`verdict-${v}`);
      await ready.goto("/");
      await expect(page.getByRole("link", { name: "查原因 →" })).toBeVisible();
    });
  }

  test("home: datad says ok → no link even with weak bars; no verdict (older agent) → falls back to weak signal", async ({ ready, page, mock }) => {
    await mock.scenario("weak");
    let verdict: string | undefined = "ok";
    await page.route(/\/api\/public\/status$/, async (route) => {
      const r = await route.fetch();
      const j = await r.json();
      if (verdict === undefined) delete j.data.network.verdict;
      else j.data.network.verdict = verdict;
      await route.fulfill({ response: r, json: j });
    });
    await ready.goto("/");
    await expect(page.locator(".nd-status")).toContainText("RSRP");
    await expect(page.getByRole("link", { name: "查原因 →" })).toHaveCount(0);
    verdict = undefined;
    await page.reload();
    await expect(page.getByRole("link", { name: "查原因 →" })).toBeVisible();
  });

  test("entering from the Charts page starts a check (no recent result); a hard load does not", async ({ ready, page }) => {
    await ready.goto("/charts/");
    const since = ready.requests.length;
    await page.getByRole("link", { name: /网络诊断/ }).first().click();
    await expect(page).toHaveURL(/\/tools\/diagnose\/?$/);
    await expect.poll(() => posts(ready, "/api/diagnose", since).length).toBe(1);
    await expect(status(page)).toContainText("没查到问题", { timeout: 10_000 });
    // Within 10 minutes: back in-app shows the last result, no new run.
    await page.goBack();
    await page.getByRole("link", { name: /网络诊断/ }).first().click();
    await expect(status(page)).toContainText("没查到问题");
    expect(posts(ready, "/api/diagnose", since)).toHaveLength(1);
  });

  test("home: good signal has no 查原因 link", async ({ ready, page }) => {
    await ready.goto("/");
    await expect(page.locator(".nd-status")).toBeVisible();
    await expect(page.getByRole("link", { name: "查原因 →" })).toHaveCount(0);
  });

  test("speed test while a diagnosis runs: 正在诊断，约 N 秒后再试", async ({ ready, page, mock }) => {
    await mock.scenario("diag-slow");
    await ready.goto("/tools/speedtest/");
    await startViaApi(page, mock.url);
    await page.getByRole("button", { name: "开始", exact: true }).click();
    await page.getByRole("button", { name: /^确认/ }).click();
    await expect(page.locator("p[role=alert]")).toContainText(/正在诊断，约 \d+ 秒后再试/);
    await expect(page.locator("p[role=alert]")).not.toContainText("未生效");
  });

});

test.describe("en", () => {
  test.use({ setup: { authed: true, lang: "en" } });

  test("Diagnose in English: agent wording in English, no stray Chinese in the result", async ({ ready, page, mock }) => {
    await mock.scenario("weak");
    await ready.goto("/tools/diagnose/");
    await expect(page.locator("h1")).toHaveText("Diagnose");
    await expect(status(page)).toContainText("Not checked yet");
    await page.getByRole("button", { name: "Start" }).click();
    await expect(status(page)).toContainText(/Checking…/);
    await expect(status(page)).toContainText("Weak signal", { timeout: 10_000 });
    await expect(status(page)).toContainText("Use Placement if you're staying put; on the move, wait");
    await expect(status(page)).toContainText("+1 more");
    await expect(row(page, "Signal")).toContainText("■Poor");
    await expect(row(page, "Cellular link")).toContainText("▲Suspect");
    await expect(row(page, "Cell load")).toContainText("Can't check · Not enough history");
    await expect(row(page, "Speed cap")).toContainText("Can't check · QoS unavailable");
    await page.getByRole("button", { name: "Add speed test · 5 s, ≤30 MB" }).click();
    await page.getByRole("button", { name: "Uses roaming data; press again" }).click();
    await expect(row(page, "Speed")).toContainText("Direct ↓ 4 Mbps", { timeout: 8_000 });
    await page.getByRole("button", { name: "Wrong" }).click();
    await expect(page.getByText("Noted, thanks")).toBeVisible();
    const text = await page.locator("main").innerText();
    expect(text.match(/[\u3400-\u9fff]+/g) ?? []).toEqual([]);
  });

  test("home link and busy speed test in English", async ({ ready, page, mock }) => {
    await mock.scenario("weak");
    await ready.goto("/");
    await expect(page.getByRole("link", { name: "Diagnose →" })).toBeVisible();
    await mock.scenario("diag-slow");
    await ready.goto("/tools/speedtest/");
    await startViaApi(page, mock.url);
    await page.getByRole("button", { name: "Start", exact: true }).click();
    await page.getByRole("button", { name: /^Confirm/ }).click();
    await expect(page.locator("p[role=alert]")).toContainText(/Diagnosing; try again in about \d+ s/);
  });
});

test.describe("phone", () => {
  test.use({ setup: { authed: true, lang: "en" }, viewport: { width: 375, height: 812 } });

  test("one column, no sideways scroll, 44px targets", async ({ ready, page }) => {
    await ready.goto("/tools/diagnose/");
    await page.getByRole("button", { name: "Start" }).click();
    await expect(status(page)).toContainText("No problem found", { timeout: 10_000 });
    const { sw, iw } = await page.evaluate(() => ({ sw: document.documentElement.scrollWidth, iw: window.innerWidth }));
    expect(sw).toBeLessThanOrEqual(iw);
    for (const name of ["Check again", "Add speed test · 5 s, ≤30 MB", "Right", "Wrong"]) {
      const box = await page.getByRole("button", { name, exact: true }).boundingBox();
      expect(box?.height ?? 0, name).toBeGreaterThanOrEqual(44);
      expect(box?.width ?? 0, name).toBeGreaterThanOrEqual(44);
    }
  });
});
