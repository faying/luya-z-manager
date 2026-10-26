// /alerts 「事故 · 7 天」 (design 2026-10-24, design review 5A/6A/10A/13A, eng review round 3 D2/D3/D6/D7/D8):
// the mock core's incidents by scenario (/mock/incidents?set=…), its own alerts tagged with incident ids.
// The common checks (axe light/dark 0, 390 px no sideways scroll, no page errors) and the six from the
// round-3 test plan: alerts of a drawn incident leave 「记录」; the 51st ended one keeps its alert in
// 「记录」; incidents unreadable / older core hide nothing; a crash row is one incident line and not in
// 「记录」; an unread alert marks its incident; quiet ones say 「已结束 · N 次」. From source/manager:
//   WEB=http://127.0.0.1:18081 MOCK=http://127.0.0.1:19461 OUT=/tmp/lw-inc crates/luyaz-web/e2e/run.sh incidents
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19461";
const OUT = process.env.OUT ?? "/tmp/lw-inc";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const scenario = async (set) => {
  const r = await fetch(`${MOCK}/mock/incidents?set=${set}`, { method: "POST" });
  if (!r.ok) throw new Error(`mock incidents ${set}: ${r.status}`);
};
await fetch(`${MOCK}/mock/alerts-to-agent?on=false`, { method: "POST" });

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);

const browser = await chromium.launch({ channel: "chrome" });
const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }]);
const p = await ctx.newPage();
const errors = [];
p.on("pageerror", (e) => errors.push(String(e)));
const txt = async (sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const open = async () => { await p.goto(`${WEB}/alerts`); await p.waitForSelector("#al-incidents"); };
const eventsText = () => txt("#al-events");

// ── incidents drawn ──
await scenario("ok");
await open();
ok(/2 个事故还开着/.test(await txt("#al-status")) && /内存超标 · \d\d:\d\d 起 \(\+1\)/.test(await txt("#al-status .nd-status__reason")), `status leads with open incidents: ${await txt("#al-status")}`);
const rows = p.locator("#al-incidents li.al-inc");
ok(await rows.count() === 5, `five rows (${await rows.count()})`);
ok(/内存超标 · 还开着/.test(await rows.nth(0).textContent()) && /出口不通 · 还开着/.test(await rows.nth(1).textContent()), "open ones first, the 8-day-old one too");
ok(/已 2h 10m/.test(await rows.nth(0).textContent()) && /可能相关：core 上机/.test(await rows.nth(0).textContent()), "open: since, how long, the ship");
ok(/数据服务崩溃 · 已结束 · 3 次/.test(await rows.nth(2).textContent()), "quiet: 已结束 · 3 次 (D8)");
ok(/蜂窝断线 · 已恢复/.test(await rows.nth(3).textContent()), "recovered");
ok(await rows.nth(0).locator(".al-unread").count() === 1 && await rows.nth(2).locator(".al-unread").count() === 0, "an unread alert marks its incident (D6)");
const ev = await eventsText();
ok(!/exit 139/.test(ev) && !/anon 100MiB/.test(ev), "alerts of drawn incidents are not in 记录 (D3/D7)");
ok(/Wi-Fi 掉了/.test(ev) && /数据服务重启/.test(ev), "other alerts stay in 记录");
ok(await p.locator("#al-events li").count() === 2, `记录 has the two others (${await p.locator("#al-events li").count()})`);
// keyboard: Tab to the first row, Enter opens it, details load
await p.locator("#al-incidents li.al-inc button").nth(2).focus();
await p.keyboard.press("Enter");
await p.waitForFunction(() => /归入的告警/.test(document.querySelector("#al-it-q3")?.textContent ?? ""), null, { timeout: 8000 }).catch(() => {});
ok(await p.getAttribute("#al-incidents li.al-inc:nth-child(3) button", "aria-expanded") === "true", "Enter opens the row (aria-expanded)");
const d = await txt("#al-it-q3");
ok(/1 小时内没再发生/.test(d) && /同时观察到/.test(d) && /归入的告警/.test(d) && (d.match(/exit 139/g) ?? []).length === 3, `details of the crash row: ${d.slice(0, 160)}`);
// the list refreshes: the open details stay
await p.request.post(`${WEB}/alerts/refresh`, { headers: { cookie: `lw_sid=${sid}`, origin: new URL(WEB).origin } });
await p.waitForTimeout(1500);
ok(/归入的告警/.test(await txt("#al-it-q3")), "a refresh does not overwrite the open details");
// a pruned one
await p.locator("#al-incidents li.al-inc button").nth(4).click();
await p.waitForFunction(() => /保存期限/.test(document.querySelector("#al-it-gone-1")?.textContent ?? ""), null, { timeout: 8000 }).catch(() => {});
ok(/这条详情已超过保存期限/.test(await txt("#al-it-gone-1")), "pruned details say so");
await p.screenshot({ path: `${OUT}/incidents-ok.png`, fullPage: true });
for (const theme of ["light", "dark"]) {
  const c2 = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  await c2.addCookies([{ name: "lw_sid", value: sid, url: WEB }]);
  await c2.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const q = await c2.newPage();
  await q.goto(`${WEB}/alerts`);
  await q.waitForSelector("#al-incidents li.al-inc");
  await q.locator("#al-incidents li.al-inc button").nth(2).click();
  await q.waitForFunction(() => /归入的告警/.test(document.querySelector("#al-it-q3")?.textContent ?? ""), null, { timeout: 8000 }).catch(() => {});
  const a = await new AxeBuilder({ page: q }).withTags(["wcag2a", "wcag2aa"]).analyze();
  ok(a.violations.length === 0, `axe ${theme}: ${a.violations.map((v) => `${v.id} ${v.nodes[0]?.target}`).join(", ") || "0"}`);
  await q.screenshot({ path: `${OUT}/incidents-${theme}.png`, fullPage: true });
  await c2.close();
}
await p.setViewportSize({ width: 390, height: 844 });
ok(await p.evaluate(() => document.documentElement.scrollWidth <= 390), "390 px: no sideways scroll");
await p.screenshot({ path: `${OUT}/incidents-phone.png`, fullPage: true });
await p.setViewportSize({ width: 1280, height: 900 });

// ── more than 50 ended: the 51st's alert stays in 记录 ──
await scenario("many");
await open();
ok(/还有 3 条，可用 MCP 或 tools\/luyaz 查/.test(await txt("#al-incidents")), "past 50: 还有 3 条");
ok(await p.locator("#al-incidents li.al-inc").count() === 51, `the open one and 50 ended (${await p.locator("#al-incidents li.al-inc").count()})`);
ok(/the 53rd/.test(await eventsText()), "the alert of an incident not drawn stays in 记录 (OV3-1)");

// ── cannot read / older core / error / empty: nothing hidden ──
await scenario("down");
await open();
ok(/读不到事故/.test(await txt("#al-incidents")) && /重试/.test(await txt("#al-incidents")), `down: ${await txt("#al-incidents")}`);
ok(await p.locator("#al-events li").count() === 6 && /exit 139/.test(await eventsText()), "down: every alert in 记录 (D2)");
await scenario("off");
await open();
ok(/core 版本太旧，不支持事故/.test(await txt("#al-incidents")), "an older core says so");
ok(await p.locator("#al-events li").count() === 2, "older core: the mock's own alerts, all shown");
await scenario("error");
await open();
ok(/读不到事故：the regress layer has not run yet/.test(await txt("#al-incidents")), "the core's error is said");
await scenario("empty");
await open();
ok(/7 天内没有事故/.test(await txt("#al-incidents")) && /某一类看不见时会另外通知/.test(await txt("#al-incidents")), "empty");
ok(await p.locator("#al-events li").count() === 6, "empty list: every alert in 记录");

// English
await scenario("ok");
await ctx.addCookies([{ name: "u60_lang", value: "en", url: WEB }]);
await open();
ok(/2 incident\(s\) still open/.test(await txt("#al-status")) && /Ended · 3×/.test(await txt("#al-incidents")), `English: ${await txt("#al-incidents")}`.slice(0, 200));

ok(errors.length === 0, `no page errors ${errors.join(" | ")}`);
await scenario("off");
await browser.close();
console.log(failed ? `${failed} failed` : "all ok");
process.exit(failed ? 1 : 0);
