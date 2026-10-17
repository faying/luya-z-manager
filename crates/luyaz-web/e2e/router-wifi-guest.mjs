// /router/wifi-guest (batch 4): the common checks (first load ≤ 40 KiB gzip without fonts, axe
// light/dark 0, 390 px no sideways scroll, English body without Chinese, no page errors,
// screenshots) and each write path once against the mock u60d (src/mock/wifi_guest.rs).
//   WEB=http://127.0.0.1:18217 MOCK=http://127.0.0.1:19417 OUT=/tmp/lw-wg \
//     crates/luyaz-web/e2e/run.sh router-wifi-guest
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mkdirSync } from "node:fs";

const WEB = process.env.WEB ?? "http://127.0.0.1:18217";
const OUT = process.env.OUT ?? "/tmp/lw-wg";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19417";
mkdirSync(OUT, { recursive: true });
const post = (path, body) => fetch(MOCK + path, { method: "POST", headers: { "content-type": "application/json" }, body: body && JSON.stringify(body) });
const stats = () => fetch(`${MOCK}/mock/stats`).then((r) => r.json());
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);

async function firstLoad(p) {
  const urls = [p.url(), ...(await p.evaluate(() => [...document.querySelectorAll("link[href^='/a/'],script[src^='/a/']")].map((e) => e.href || e.src)))];
  const html = await p.content();
  for (const m of html.matchAll(/(?:url\(|href=")(\/a\/[^")#]+)/g)) urls.push(WEB + m[1].replaceAll("&amp;", "&"));
  let body = 0;
  for (const u of new Set(urls)) {
    if (u.includes(".woff2")) continue;
    const res = await fetch(u, { headers: { cookie: `lw_sid=${sid}`, "accept-encoding": "gzip" } });
    body += Number(res.headers.get("content-length") ?? (await res.arrayBuffer()).byteLength);
  }
  return body;
}
const mainText = (p) => p.evaluate(() => {
  const m = document.querySelector("main").cloneNode(true);
  m.querySelectorAll("[data-data]").forEach((e) => e.remove());
  return m.innerText + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, fn, arg, ms = 15000) => p.waitForFunction(fn, arg, { timeout: ms }).catch(() => {});
const lastOp = async () => (await stats()).page_ops.filter((b) => b.action === "wifi.guest").at(-1);
const nOps = async () => (await stats()).page_ops.filter((b) => b.action === "wifi.guest").length;
const canon = (v) => JSON.stringify(v, (_, x) => (x && typeof x === "object" && !Array.isArray(x) ? Object.fromEntries(Object.entries(x).sort()) : x));
const same = (a, b) => canon(a) === canon(b);
const idleOp = () => post("/mock/op", { rollback_enabled: true, active: null, last: null });

const reset = async () => { await post("/mock/wguest"); await post("/mock/txn?end=confirmed"); await idleOp(); };
const ready = (p) => p.waitForFunction(() => document.querySelectorAll("#wg-rows .nd-row").length >= 8, null, { timeout: 15000 });

async function own(p) {
  const done = (re) => until(p, (re) => new RegExp(re).test(document.querySelector("#wg-save-op")?.textContent ?? ""), re, 30000);
  const dialog = () => p.locator("dialog#wg-save-confirm[open]");
  const open = async () => {
    if ((await p.getAttribute("#wg-btn", "aria-expanded")) !== "true") await p.click("#wg-btn");
    await p.waitForSelector("input[data-bind=_wgssid]", { timeout: 8000 }).catch(() => {});
  };
  const apply = () => p.click("#wg-form button:has-text('应用')");

  ok(/访客 Wi-Fi 已开启/.test(await txt(p, "#wg-status")) && /2\.4 GHz/.test(await txt(p, "#wg-status")), `status: ${await txt(p, "#wg-status")}`);
  ok(/已设密码/.test(await txt(p, "#wg-rows")) && /U60-Guest/.test(await txt(p, "#wg-rows")) && /240 分钟/.test(await txt(p, "#wg-rows")), `rows: ${await txt(p, "#wg-rows")}`);
  const t0 = await txt(p, "#wg-status .nd-status__meta");
  await sleep(2100);
  const t1 = await txt(p, "#wg-status .nd-status__meta");
  ok(/^剩余 2 小时 0\d 分 \d\d 秒$/.test(t1) && t0 !== t1, `countdown runs: ${t0} → ${t1}`);

  // nothing changed: said in the area, nothing sent
  await open();
  const n0 = await nOps();
  await apply();
  await done("还没有改动");
  ok(/还没有改动/.test(await txt(p, "#wg-save-op")) && (await nOps()) === n0, `no change: ${await txt(p, "#wg-save-op")}`);
  // a short password: refused before any send
  await p.fill("input[data-bind=_wgpw]", "short");
  await apply();
  await done("8–63");
  ok(/8–63/.test(await txt(p, "#wg-save-op")) && (await nOps()) === n0, "short password refused");

  // the change: tier 3 dialog; the password is never in the page's HTML; only changed keys go
  await p.click("button[aria-labelledby=wg-on5-l]");
  await p.fill("input[data-bind=_wgssid]", "Cafe e2e");
  await p.fill("input[data-bind=_wgpw]", "secretpw99");
  await apply();
  await dialog().waitFor({ timeout: 8000 }).catch(() => {});
  const dt = await txt(p, "#wg-save-confirm");
  ok((await dialog().count()) === 1 && /主网络/.test(dt) && /「Cafe e2e」/.test(dt) && /重新连上主 Wi-Fi/.test(dt), `dialog: ${dt.slice(0, 160)}`);
  ok(!/Tailscale 远程操作/.test(dt), "no 'someone at the device' warning (Tailscale unaffected)");
  ok(!(await p.content()).includes("secretpw99"), "password not in the page HTML");
  const ax = await new AxeBuilder({ page: p }).include("#wg-save-confirm").analyze();
  ok(ax.violations.length === 0, `dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
  await p.click("#wg-save-confirm .nd-btn--confirm");
  await done("已改成|没有生效|设备已接受|连接中断");
  const b = await lastOp();
  ok(await p.locator("#wg-save-op .t-note--ok").count() === 1, `applied: ${await txt(p, "#wg-save-op")}`);
  ok(b.source === "web" && /^lw-/.test(b.op_id) && same(b.params, { guest_ssid: "Cafe e2e", guest_key: "secretpw99", guest_disabled_5g: "0" }), `body: ${JSON.stringify(b.params)}`);
  await until(p, () => /Cafe e2e/.test(document.querySelector("#wg-rows")?.textContent ?? ""));
  ok(/Cafe e2e/.test(await txt(p, "#wg-rows")) && /2\.4 GHz \+ 5 GHz/.test(await txt(p, "#wg-status")), "rows and status follow");
  ok(await p.locator("input[data-bind=_wgssid]").count() === 0 && (await p.getAttribute("#wg-btn", "aria-expanded")) === "false", "form closed after the readback");

  // one band only = three APs with the mains (luyaz_wifi::layout): refused before any send
  await open();
  const n1 = await nOps();
  await p.click("button[aria-labelledby=wg-on2-l]");
  await apply();
  await done("一起开");
  ok(/两个频段一起开或一起关/.test(await txt(p, "#wg-save-op")) && (await nOps()) === n1, `one band refused: ${await txt(p, "#wg-save-op")}`);
  await p.reload({ waitUntil: "load" });
  await ready(p).catch(() => {});

  // the browser drops during the reload and comes back: the area shows how it ended
  await open();
  await p.click("button[aria-labelledby=wg-hid-l]");
  await apply();
  await dialog().waitFor({ timeout: 8000 }).catch(() => {});
  await p.click("#wg-save-confirm .nd-btn--confirm");
  // Chrome's offline mode doesn't cut a stream already open, so the drop is the event Datastar
  // sends when a send's reply is lost (write.html listens for it); offline makes the lost post
  // itself fail first, as on a Wi-Fi reload, and Datastar retries it until the browser is back
  await until(p, () => /正在/.test(document.querySelector("#wg-save-op")?.textContent ?? ""), null, 8000);
  const lostReply = p.waitForResponse((q) => /\/router\/wifi-guest\/cancel\?t=\w+&lost=true/.test(q.url()) && q.status() === 200, { timeout: 60000 }).catch(() => null);
  await p.context().setOffline(true);
  await p.evaluate(() => {
    const el = document.createElement("button");
    el.dataset.write = "wg-save";
    document.dispatchEvent(new CustomEvent("datastar-fetch", { detail: { type: "retries-failed", el, argsRaw: {} } }));
  });
  await sleep(3000);
  await p.context().setOffline(false);
  const lr = await lostReply;
  // (CDP hands an event-stream body over as Latin-1: check the ASCII parts)
  const lb = lr ? await lr.text().catch(() => "") : "";
  ok(lr !== null && /wg-save-op/.test(lb) && /t-note--ok[^<]*hidden</.test(lb), `lost post after reconnect says how it ended (${lb.length} bytes)`);
  await done("已改成|没有生效|设备已接受|连接中断");
  ok(await p.locator("#wg-save-op .t-note--ok").count() === 1, `after reconnect: ${await txt(p, "#wg-save-op")}`);
  ok((await lastOp()).params.guest_hidden === "1", "hidden sent");
  await p.reload({ waitUntil: "load" });
  await ready(p).catch(() => {});

  // u60d busy with another change (409 + doing): said as is
  await post("/mock/op", { rollback_enabled: true, active: { op_id: "x-1", item: "network.mode", phase: "verifying", action: "network.set_mode", say_zh: "正在换制式", say_en: "Switching network mode" }, last: null });
  await open();
  await p.fill("input[data-bind=_wgssid]", "Busy try");
  await apply();
  await dialog().waitFor({ timeout: 8000 }).catch(() => {});
  await p.click("#wg-save-confirm .nd-btn--confirm");
  await done("没有生效");
  ok(/另一项改动正在进行（正在换制式）/.test(await txt(p, "#wg-save-op")), `409 busy: ${await txt(p, "#wg-save-op")}`);
  await idleOp();
  // Wi-Fi lock held elsewhere (503) and refused (409)
  for (const [next, re] of [["busy503", "Wi-Fi 正在被别处修改"], ["refused", "没有生效：wifi.guest refused by the mock"]]) {
    await post("/mock/wguest", { next });
    await apply();
    await dialog().waitFor({ timeout: 8000 }).catch(() => {});
    await p.click("#wg-save-confirm .nd-btn--confirm");
    await done("没有生效");
    ok(new RegExp(re).test(await txt(p, "#wg-save-op")), `${next}: ${await txt(p, "#wg-save-op")}`);
  }
  // already so on the device (u60d "no changes"): plain
  await apply();
  await dialog().waitFor({ timeout: 8000 }).catch(() => {});
  await post("/mock/wguest", { guest: { ssid: "Busy try" } });
  await sleep(500);
  await p.click("#wg-save-confirm .nd-btn--confirm");
  await done("没有要改的|没有生效");
  ok(/没有要改的/.test(await txt(p, "#wg-save-op")), `no changes: ${await txt(p, "#wg-save-op")}`);

  // a transaction that is not confirmed in time: "accepted", never "applied"
  await post("/mock/txn?end=live");
  await open();
  await p.fill("input[data-bind=_wgtime]", "60");
  await apply();
  await dialog().waitFor({ timeout: 8000 }).catch(() => {});
  await p.click("#wg-save-confirm .nd-btn--confirm");
  await sleep(1500);
  ok(/正在确认|正在改访客/.test(await txt(p, "#wg-save-op")), `live: progress ${await txt(p, "#wg-save-op")}`);
  ok((await lastOp()).params.guest_active_time === "60", "active time sent as a string");
  // a running Wi-Fi change locks the form
  await p.goto(WEB + "/router/wifi-guest", { waitUntil: "load" });
  await ready(p).catch(() => {});
  await p.click("#wg-btn");
  await until(p, () => /Wi-Fi 正在改/.test(document.querySelector("#wg-save-op")?.textContent ?? ""), null, 8000);
  ok(/Wi-Fi 正在改/.test(await txt(p, "#wg-save-op")) && await p.locator("input[data-bind=_wgssid]").count() === 0, `running Wi-Fi txn locks: ${await txt(p, "#wg-save-op")}`);
  await reset();
  await sleep(500);
}

const browser = await chromium.launch({ channel: "chrome" });
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource|ERR_INTERNET_DISCONNECTED/.test(m.text())) errs.push(m.text()); });
  await reset(); await sleep(500);
  await p.goto(WEB + "/router/wifi-guest", { waitUntil: "load" });
  await ready(p).catch(() => ok(false, `${name} ${lang}: live content never showed`));
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
    await own(p);
    errs.length = 0; // the offline step's own network errors
    await p.reload({ waitUntil: "load" });
    await ready(p).catch(() => {});
  }
  await p.click("#wg-btn");
  await p.waitForSelector("input[data-bind=_wgssid]", { timeout: 8000 }).catch(() => {});
  if (lang === "en") {
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
  }
  if (name === "phone") {
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `390 px no sideways scroll (${sw})`);
  }
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length + (process.env.PROBE ? " " + v.nodes.map((n) => n.html + " " + n.failureSummary).join(" / ") : "")).join(", ") || "0"}`);
  ok(errs.length === 0, `${name}: no page errors ${errs.join("; ")}`);
  await p.screenshot({ path: `${OUT}/wg-${name}-${lang}-${theme}.png`, fullPage: true });
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
