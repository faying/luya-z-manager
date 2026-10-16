// 8c-A 2-0: a page left open past its session. The session is ended on the server (POST /logout
// with the same cookie from here, which is what expiry looks like to the page: the browser still
// sends a cookie the server no longer knows). Then the page's open push stream and its buttons must
// show the "session expired" dialog with a way back, instead of stopping silently.
// Same servers as trial.mjs; run from source/manager with e2e/run.sh:
//   WEB=http://127.0.0.1:18081 \
//     MOCK=http://127.0.0.1:19461 OUT=/tmp/lw crates/luyaz-web/e2e/run.sh session
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19461";
const OUT = process.env.OUT ?? "/tmp/lw";
const ORIGIN = new URL(WEB).origin;
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };

async function login() {
  const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: ORIGIN, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
  const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
  if (!sid) throw new Error(`login failed: ${r.status}`);
  return sid;
}
const logout = (sid) => fetch(`${WEB}/logout`, { method: "POST", redirect: "manual", headers: { origin: ORIGIN, cookie: `lw_sid=${sid}` } });
const cookies = (sid, lang) => [
  { name: "lw_sid", value: sid, url: WEB },
  { name: "u60_lang", value: lang, url: WEB },
];
const dialogOpen = (p) => p.evaluate(() => !!document.querySelector("#lw-expired dialog")?.open);
const controls = async () => (await (await fetch(`${MOCK}/mock/stats`)).json()).op_ids.length;

const browser = await chromium.launch({ channel: "chrome" });

// ---- /signal, Chinese: u60d stream + the tier-2 roaming switch ----
{
  const sid = await login();
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale: "zh-CN" });
  await ctx.addCookies(cookies(sid, "zh"));
  const p = await ctx.newPage();
  const errors = [];
  p.on("pageerror", (e) => errors.push(e.message));
  await p.goto(`${WEB}/signal`);
  await p.waitForSelector("#sig-readouts .nd-readout__value span", { timeout: 20000 });
  ok(!(await dialogOpen(p)), "no notice while the session lives");
  const streams = [];
  p.on("request", (r) => { if (r.url().includes("/signal/events")) streams.push(Date.now()); });
  const t0 = Date.now();
  ok((await logout(sid)).status === 200, "session ended on the server");
  await p.waitForSelector("#lw-expired dialog[open]", { timeout: 10000 }).catch(() => {});
  ok(await dialogOpen(p), `the open stream shows the notice (${((Date.now() - t0) / 1000).toFixed(1)} s)`);
  ok((await p.textContent("#lw-expired h2")).trim() === "登录已过期", "wording: 登录已过期");
  ok((await p.getAttribute("#lw-expired a", "href")) === "/login?next=/signal", `way back: ${await p.getAttribute("#lw-expired a", "href")}`);
  ok(await p.evaluate(() => document.activeElement?.closest("#lw-expired") !== null), "focus in the dialog");
  await p.screenshot({ path: `${OUT}/session-expired-zh.png` });
  const ax = await new AxeBuilder({ page: p }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "best-practice"]).analyze();
  ok(ax.violations.length === 0, `axe with the notice open: ${ax.violations.map((v) => `${v.id} ${v.nodes[0]?.target}`).join("; ") || "0"}`);
  const s0 = streams.length;
  await p.waitForTimeout(4000);
  ok(streams.length === s0, `the stream does not reconnect after the notice (${streams.length - s0} requests)`);

  await p.keyboard.press("Escape");
  ok(!(await dialogOpen(p)), "Esc closes it");
  const c0 = await controls();
  await p.click("#roam [role=switch]");
  await p.waitForSelector("#lw-expired dialog[open]", { timeout: 5000 }).catch(() => {});
  ok(await dialogOpen(p), "a button shows the notice again");
  ok(!(await p.isVisible("#roam-confirm")), "and does nothing else");
  ok((await controls()) === c0, "no write reached u60d");
  ok(errors.length === 0, `no page errors ${errors.join("; ")}`);

  // the same answers without a browser
  const ds = { cookie: `lw_sid=${sid}`, "datastar-request": "true", referer: `${WEB}/signal`, origin: ORIGIN };
  let r = await fetch(`${WEB}/signal/events?g=0`, { headers: ds });
  let body = await r.text();
  ok(r.status === 200 && r.headers.get("content-type").startsWith("text/event-stream") && body.includes("mode replace") && body.includes("lw-expired"), `stream reconnect: ${r.status} patch`);
  r = await fetch(`${WEB}/roam/set?on=true`, { method: "POST", headers: ds });
  body = await r.text();
  ok(r.status === 200 && body.includes("登录已过期"), `write POST: ${r.status} patch`);
  ok((await controls()) === c0, "still no write");
  r = await fetch(`${WEB}/signal`, { redirect: "manual", headers: { cookie: `lw_sid=${sid}` } });
  ok(r.status === 303 && r.headers.get("location") === "/login?next=/signal", `plain page load still redirects (${r.status} ${r.headers.get("location")})`);
  r = await fetch(`${WEB}/roam/set?on=true`, { method: "POST", headers: { cookie: `lw_sid=${sid}`, origin: ORIGIN } });
  ok(r.status === 401, `a non-Datastar POST still gets 401 (${r.status})`);
  await ctx.close();
}

// ---- /clients, English, phone: an agent-backed page (its own stream and a button) ----
{
  const sid = await login();
  const ctx = await browser.newContext({ viewport: { width: 390, height: 844 }, locale: "en-US", colorScheme: "dark" });
  await ctx.addCookies(cookies(sid, "en"));
  const p = await ctx.newPage();
  await p.goto(`${WEB}/clients`);
  await p.waitForSelector("#cl-list .nd-row", { timeout: 15000 });
  await logout(sid);
  await p.waitForSelector("#lw-expired dialog[open]", { timeout: 10000 }).catch(() => {});
  ok(await dialogOpen(p), "/clients: the stream shows the notice");
  const text = await p.textContent("#lw-expired dialog");
  ok(/Session expired/.test(text) && !/[一-鿿]/.test(text), `English notice: ${text.replace(/\s+/g, " ").trim()}`);
  ok((await p.getAttribute("#lw-expired a", "href")) === "/login?next=/clients", "way back to /clients");
  ok(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "390 px: no sideways scroll");
  await p.screenshot({ path: `${OUT}/session-expired-en-phone-dark.png` });
  const ax = await new AxeBuilder({ page: p }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "best-practice"]).analyze();
  ok(ax.violations.length === 0, `axe dark: ${ax.violations.map((v) => `${v.id} ${v.nodes[0]?.target}`).join("; ") || "0"}`);
  await p.keyboard.press("Escape");
  await p.click("button[aria-label='Refresh']");
  await p.waitForSelector("#lw-expired dialog[open]", { timeout: 5000 }).catch(() => {});
  ok(await dialogOpen(p), "/clients: the Refresh button shows the notice");
  await p.click("#lw-expired a");
  await p.waitForURL(/\/login/, { timeout: 5000 });
  ok(new URL(p.url()).searchParams.get("next") === "/clients", "the link opens the login page for /clients");
  await ctx.close();
}

await browser.close();
console.log(failed ? `${failed} FAILED` : "all passed");
process.exit(failed ? 1 : 0);
