// Top-bar status and alert bar (8c-A step 4): icons from u60d blocks + side reads, the bar comes
// and goes by push on a hub page and on /signal, "Got it" goes through, one upstream for two tabs,
// axe with the bar open in both themes.
// Same servers as shell.mjs (fake agent on the Mac + luyaz-web serve/mock in a container), with
// run.sh:
//   WEB=http://127.0.0.1:18081 \
//     MOCK=http://127.0.0.1:19461 OUT=/tmp/lw crates/luyaz-web/e2e/run.sh topbar
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19461";
const OUT = process.env.OUT ?? "/tmp/lw";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const setUnread = (n) => fetch(`${MOCK}/mock/alerts?unread=${n}`, { method: "POST" });
// the mock's own alerts (e2e/alerts.mjs may have left them to the fake agent)
await fetch(`${MOCK}/mock/alerts-to-agent?on=false`, { method: "POST" });

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);

await setUnread(0);
const browser = await chromium.launch({ channel: "chrome" });
const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }]);
const hubPage = await ctx.newPage();
const sigPage = await ctx.newPage();
await hubPage.goto(`${WEB}/charts`);
await sigPage.goto(`${WEB}/signal`);

// the side poll runs on the first tab; give it one round
await hubPage.waitForSelector("a#st-ts", { timeout: 15000 }).catch(() => {});
for (const [p, name] of [[hubPage, "/charts"], [sigPage, "/signal"]]) {
  for (const id of ["st-net", "st-wifi", "st-sms", "st-chill", "st-ts"]) {
    const label = await p.locator(`a#${id}`).getAttribute("aria-label", { timeout: 15000 }).catch(() => null);
    ok(!!label, `${name} ${id}: ${label}`);
  }
  ok(await p.locator("#alert-bar[hidden]").count() === 1, `${name}: no bar with 0 unread`);
}
ok((await sigPage.locator("a#st-sms .t-st__badge").textContent().catch(() => "")) !== "", "SMS badge from the sms block");

const stats = await (await fetch(`${MOCK}/mock/stats`)).json();
ok(stats.connections === 1, `two tabs, one upstream connection (${stats.connections})`);

await setUnread(2);
for (const [p, name] of [[hubPage, "/charts"], [sigPage, "/signal"]]) {
  const t0 = Date.now();
  const shown = await p.waitForSelector("#alert-bar.nd-announce", { timeout: 15000 }).then(() => true, () => false);
  ok(shown, `${name}: bar arrives by push (${((Date.now() - t0) / 1000).toFixed(1)} s)`);
  const title = await p.locator("#alert-bar .font-semibold").textContent().catch(() => "");
  ok(title.includes("2 条新告警") && title.includes("Wi-Fi 掉了"), `${name}: "${title}"`);
}
for (const theme of ["light", "dark"]) {
  await sigPage.emulateMedia({ colorScheme: theme });
  await sigPage.waitForTimeout(200);
  await sigPage.screenshot({ path: `${OUT}/topbar-${theme}.png` });
  const axe = await new AxeBuilder({ page: sigPage }).analyze();
  ok(axe.violations.length === 0, `axe ${theme} with the bar: ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
}

// "Got it": the request goes to luyaz-web, which marks read through the agent and answers with the bar
const reply = hubPage.waitForResponse((res) => res.url().includes("/alerts/read"));
await hubPage.click("#alert-bar button");
const res = await reply;
ok(res.status() === 200, `Got it → ${res.status()}`);
await hubPage.waitForTimeout(500);
ok(await hubPage.locator(".t-announce__err").count() === 0, "no error line after Got it");

// the device's read cursor moves (here: the mock) → the bar leaves every tab by push
await setUnread(0);
for (const [p, name] of [[hubPage, "/charts"], [sigPage, "/signal"]]) {
  const gone = await p.waitForSelector("#alert-bar[hidden]", { state: "attached", timeout: 15000 }).then(() => true, () => false);
  ok(gone, `${name}: bar leaves by push`);
}

// pass line 3 on a page whose stream only carries the frame (quiet for minutes): close it, time
// until the upstream drops (N = 10 s after the last tab, plus however long noticing takes)
await hubPage.close();
await sigPage.close();
await new Promise((r) => setTimeout(r, 30000)); // let the earlier tabs' upstream run out
{
  const p = await ctx.newPage();
  await p.goto(`${WEB}/charts`);
  const until = async (f, ms) => { const end = Date.now() + ms; while (Date.now() < end) { if (await f()) return true; await new Promise((r) => setTimeout(r, 200)); } return false; };
  await until(async () => (await (await fetch(`${MOCK}/mock/stats`)).json()).connections === 1, 10000);
  await p.waitForTimeout(3000);
  await p.close();
  const t0 = Date.now();
  const dropped = await until(async () => (await (await fetch(`${MOCK}/mock/stats`)).json()).connections === 0, 60000);
  const s = (Date.now() - t0) / 1000;
  ok(dropped && s <= 13, `/charts closed → upstream gone in ${s.toFixed(1)} s (≤ 13)`);
}

// phones: back + five icons + search + logout fit in 390 px, no sideways scroll
{
  const phone = await browser.newContext({ viewport: { width: 390, height: 844 } });
  await phone.addCookies([{ name: "lw_sid", value: sid, url: WEB }]);
  const p = await phone.newPage();
  for (const path of ["/signal", "/charts"]) {
    await p.goto(WEB + path);
    await p.waitForSelector("a#st-ts", { timeout: 15000 }).catch(() => {});
    const [sw, iw] = await p.evaluate(() => [document.documentElement.scrollWidth, innerWidth]);
    ok(sw <= iw, `phone ${path}: scrollWidth ${sw} ≤ ${iw}`);
    await p.screenshot({ path: `${OUT}/topbar-phone${path.replace("/", "-")}.png` });
  }
  await phone.close();
}

await browser.close();
console.log(failed ? `${failed} failed` : "all ok");
process.exit(failed ? 1 : 0);
