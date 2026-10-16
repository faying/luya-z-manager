// The op bar (8c-A step 5, first item): a running change arrives by push on a hub page and on
// /signal and counts down by itself, freezes and says so when u60d is stuck, ends in a 3-second
// result that leaves, a sticky result stays, the one-time notice's "Got it" reaches u60d, axe in
// both themes, no sideways scroll at 390 px.
// Same servers as topbar.mjs; run from source/manager with e2e/run.sh:
//   WEB=http://127.0.0.1:18081 \
//     MOCK=http://127.0.0.1:19461 OUT=/tmp/lw crates/luyaz-web/e2e/run.sh opbar
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19461";
const OUT = process.env.OUT ?? "/tmp/lw";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const setOp = (op) => fetch(`${MOCK}/mock/op`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(op) });
const stuck = (age) => fetch(`${MOCK}/mock/stuck${age === undefined ? "" : `?age=${age}`}`, { method: "POST" });
const stats = async () => (await fetch(`${MOCK}/mock/stats`)).json();

const live = (id, ms) => ({
  op_id: id, action: "network.set_mode", item: "network.mode", source: "web", phase: "waiting",
  say_zh: "正在改网络模式", say_en: "Changing network mode", what_zh: "网络模式", what_en: "Network mode",
  old_zh: "自动", old_en: "Auto", target_zh: "只用 4G", target_en: "4G only", source_zh: "网页", source_en: "Web",
  remaining_ms: ms, mark: null, stay: "live",
});
const done = (id, stay, mark, say) => ({
  op_id: id, action: "network.set_mode", item: "network.mode", source: "web", phase: "confirmed",
  say_zh: say, say_en: say, remaining_ms: null, mark, stay, acked: false, needs_ack: stay !== "brief",
});

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);

await stuck();
await setOp({ rollback_enabled: false, active: null, last: null });
const browser = await chromium.launch({ channel: "chrome" });
const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }]);
const hubPage = await ctx.newPage();
const sigPage = await ctx.newPage();
await hubPage.goto(`${WEB}/charts`);
await sigPage.goto(`${WEB}/signal`);
const pages = [[hubPage, "/charts"], [sigPage, "/signal"]];
for (const [p, name] of pages) ok(await p.locator("#op-bar[hidden]").count() === 1, `${name}: no op bar when nothing runs`);

// a change starts: the bar arrives by push and counts down by itself
await setOp({ rollback_enabled: true, active: live("t1", 95000), last: null });
for (const [p, name] of pages) {
  const shown = await p.waitForSelector("#op-bar.nd-announce", { timeout: 10000 }).then(() => true, () => false);
  const title = await p.locator("#op-bar .font-semibold").textContent().catch(() => "");
  ok(shown && title.includes("正在改网络模式"), `${name}: bar arrives by push: "${title}"`);
  ok(await p.locator('#op-bar a[href="/router/network-mode"]').count() === 1, `${name}: Details links to the item's page`);
}
const c1 = await sigPage.locator("#op-bar-clock").textContent();
await sigPage.waitForTimeout(2200);
const c2 = await sigPage.locator("#op-bar-clock").textContent();
ok(c1 !== c2 && /\d:\d\d/.test(c2), `countdown moves: "${c1}" → "${c2}"`);
ok(await sigPage.locator("#op-bar-clock[aria-hidden=true]").count() === 1, "countdown is aria-hidden");
for (const theme of ["light", "dark"]) {
  await hubPage.emulateMedia({ colorScheme: theme });
  await hubPage.waitForTimeout(200);
  await hubPage.screenshot({ path: `${OUT}/opbar-live-${theme}.png` });
  const axe = await new AxeBuilder({ page: hubPage }).analyze();
  ok(axe.violations.length === 0, `axe ${theme} with the op bar: ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
}
await hubPage.emulateMedia({ colorScheme: "light" });

// u60d's executor stuck: the bar says so; back: the countdown again
await stuck(25000);
const stuckShown = await hubPage.waitForFunction(() => document.querySelector("#op-bar")?.textContent.includes("数据服务没响应"), null, { timeout: 10000 }).then(() => true, () => false);
ok(stuckShown, "stuck: 数据服务没响应");
await hubPage.screenshot({ path: `${OUT}/opbar-stuck.png` });
await stuck();
const back = await hubPage.waitForFunction(() => document.querySelector("#op-bar-clock:not([hidden])"), null, { timeout: 10000 }).then(() => true, () => false);
ok(back, "unstuck: the countdown is back");

// it ends with a 3-second result: shown, then gone by itself
await setOp({ rollback_enabled: true, active: null, last: done("t1", "brief", "ok", "现在是只用 4G") });
for (const [p, name] of pages) {
  const brief = await p.waitForFunction(() => document.querySelector("#op-bar")?.textContent.includes("现在是只用 4G"), null, { timeout: 5000 }).then(() => true, () => false);
  ok(brief, `${name}: the brief result shows`);
}
for (const [p, name] of pages) {
  const t0 = Date.now();
  const gone = await p.waitForSelector("#op-bar[hidden]", { state: "attached", timeout: 8000 }).then(() => true, () => false);
  ok(gone, `${name}: the brief result leaves by itself (${((Date.now() - t0) / 1000).toFixed(1)} s)`);
}

// a result that waits for "Got it" stays, also on a page opened after it ended
await setOp({ rollback_enabled: true, active: null, last: done("t2", "sticky", "warn", "没核对到 · 已保持只用 4G") });
await hubPage.waitForTimeout(1500);
const fresh = await ctx.newPage();
await fresh.goto(`${WEB}/system`);
for (const [p, name] of [[hubPage, "/charts"], [fresh, "/system (opened after)"]]) {
  const sym = await p.locator("#op-bar .nd-announce__sym").textContent().catch(() => "");
  ok(sym === "▲" && (await p.locator("#op-bar").textContent()).includes("没核对到"), `${name}: sticky result stays (${sym})`);
}
await fresh.waitForTimeout(4000);
ok(await fresh.locator("#op-bar.nd-announce").count() === 1, "sticky result still there after 4 s");
await fresh.close();

// nothing to show but the one-time notice: "Got it" reaches u60d, the bar leaves
await setOp({ rollback_enabled: true, active: null, last: { ...done("t2", "sticky", "warn", "x"), acked: true, needs_ack: false }, notice: "rollback_on" });
const noticed = await hubPage.waitForFunction(() => document.querySelector("#op-bar")?.textContent.includes("自动退回已开"), null, { timeout: 10000 }).then(() => true, () => false);
ok(noticed, "the notice shows");
const before = (await stats()).notice_acks;
const reply = hubPage.waitForResponse((res) => res.url().includes("/ops/notice"));
await hubPage.click("#op-bar button");
const res = await reply;
ok(res.status() === 200, `Got it → ${res.status()}`);
const gone = await hubPage.waitForSelector("#op-bar[hidden]", { state: "attached", timeout: 5000 }).then(() => true, () => false);
ok(gone && (await stats()).notice_acks === before + 1, `notice gone, u60d got op.notice_ack (${(await stats()).notice_acks - before})`);
ok(await sigPage.waitForSelector("#op-bar[hidden]", { state: "attached", timeout: 5000 }).then(() => true, () => false), "/signal: notice gone by push");

// lost u60d during a change: 断开了, then the live bar again after the reconnect
await setOp({ rollback_enabled: true, active: live("t4", 90000), last: null });
await hubPage.waitForFunction(() => document.querySelector("#op-bar")?.textContent.includes("正在改网络模式"), null, { timeout: 10000 }).catch(() => {});
await fetch(`${MOCK}/mock/drop`, { method: "POST" });
const lost = await hubPage.waitForFunction(() => document.querySelector("#op-bar")?.textContent.includes("断开了"), null, { timeout: 40000 }).then(() => true, () => false);
ok(lost, "dropped upstream during a change: 和设备断开了 · 操作结果未知");
await hubPage.screenshot({ path: `${OUT}/opbar-lost.png` });
const again = await hubPage.waitForFunction(() => document.querySelector("#op-bar")?.textContent.includes("正在改网络模式"), null, { timeout: 15000 }).then(() => true, () => false);
ok(again, "reconnected: the running change again");

// a first visit after the idle drop never says 断开了 from the last connection's leftovers
await hubPage.close();
await sigPage.close();
{
  const until = async (f, ms) => { const end = Date.now() + ms; while (Date.now() < end) { if (await f()) return true; await new Promise((r) => setTimeout(r, 200)); } return false; };
  ok(await until(async () => (await stats()).connections === 0, 20000), "all tabs closed: upstream gone");
  const p = await ctx.newPage();
  const seen = new Set();
  p.on("framenavigated", () => {});
  await p.goto(`${WEB}/charts`);
  const end = Date.now() + 3000;
  while (Date.now() < end) {
    seen.add((await p.locator("#op-bar").textContent().catch(() => "")) || "(hidden)");
    await p.waitForTimeout(100);
  }
  const texts = [...seen];
  ok(!texts.some((t) => t.includes("断开")), `first visit after idle: never 断开了 (${texts.length} states: ${texts.map((t) => t.slice(0, 12)).join(" | ")})`);
  await p.close();
}

// phones: the running bar fits in 390 px
await setOp({ rollback_enabled: true, active: live("t3", 60000), last: null });
{
  const phone = await browser.newContext({ viewport: { width: 390, height: 844 } });
  await phone.addCookies([{ name: "lw_sid", value: sid, url: WEB }]);
  const p = await phone.newPage();
  for (const path of ["/signal", "/charts"]) {
    await p.goto(WEB + path);
    await p.waitForSelector("#op-bar.nd-announce", { timeout: 10000 }).catch(() => {});
    const [sw, iw] = await p.evaluate(() => [document.documentElement.scrollWidth, innerWidth]);
    ok(sw <= iw, `phone ${path}: scrollWidth ${sw} ≤ ${iw}`);
    await p.screenshot({ path: `${OUT}/opbar-phone${path.replace("/", "-")}.png` });
  }
  await phone.close();
}
await setOp({ rollback_enabled: false, active: null, last: null });

await browser.close();
console.log(failed ? `${failed} failed` : "all ok");
process.exit(failed ? 1 : 0);
