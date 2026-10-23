// /tools/enable-adb (batch 5c): the common checks (first load ≤ 40 KiB gzip without fonts, axe
// light/dark 0, 390 px no sideways scroll, English body without Chinese, no page errors,
// screenshots) and the page's own: on a firmware that can't set the USB mode (mode_settable false,
// CN B31) the button is disabled in the very first HTML with the reason, and nothing is ever sent
// (also not by a request that skips the page); where it can, the dialog (Cancel, confirm), the
// read-back result and the "Re-enable" label. Needs the typed mock agent behind the fake agent
// (AGENT) and the mock u60d (MOCK). From source/manager:
//   WEB=http://127.0.0.1:18610 MOCK=http://127.0.0.1:19610 AGENT=http://127.0.0.1:19710 OUT=/tmp/lw-dev \
//     crates/luyaz-web/e2e/run.sh enable-adb
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18610";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19610";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19710";
const OUT = process.env.OUT ?? "/tmp/lw-dev";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
const settable = (v) => fetch(`${AGENT}/__lw/usb?settable=${v}`);
const usbOps = async () => (await (await fetch(`${MOCK}/mock/stats`)).json()).usb_ops.length;

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);
const get = (path, lang = "zh") => fetch(WEB + path, { headers: { cookie: `lw_sid=${sid}; u60_lang=${lang}`, accept: "text/html" } }).then((x) => x.text());
const post = (path) => fetch(WEB + path, { method: "POST", headers: { cookie: `lw_sid=${sid}`, origin: new URL(WEB).origin } }).then((x) => x.text());

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
  return m.innerText + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, sel, re, ms = 10000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout: ms }).catch(() => {});

async function newPage(browser, lang, theme, w = 1280, h = 900) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  return { ctx, p, errs };
}

// ---- a firmware that can't set the mode (CN B31) ----
await settable("false");
const ops0 = await usbOps();
{
  const raw = await get("/tools/enable-adb");
  const btn = /<button[^>]*data-on:click="@post\('\/tools\/enable-adb\/ask'\)"[^>]*>/.exec(raw)?.[0] ?? "";
  ok(/\sdisabled[\s>]/.test(btn), `raw HTML: the button is disabled from the first render (${btn.slice(0, 90)})`);
  ok(raw.includes("这版固件不能开 ADB 模式"), "raw HTML: the reason is there");
  const rawEn = await get("/tools/enable-adb", "en");
  ok(rawEn.includes("This firmware can&#39;t turn on ADB mode"), "raw HTML (English): the reason is there");
  // a request that skips the page: said, nothing sent
  const ask = await post("/tools/enable-adb/ask");
  ok(ask.includes("这版固件不能改 USB 模式") && !ask.includes("<dialog"), `ask without the page: ${ask.slice(0, 120).replace(/\n/g, " ")}`);
  const set = await post("/tools/enable-adb/set");
  ok(set.includes("这版固件不能改 USB 模式"), "set without the page: refused with the reason");
  ok(await usbOps() === ops0, `nothing reached usb.set (${await usbOps()} ops)`);
}

const browser = await chromium.launch({ channel: "chrome" });
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"], [390, 844, "phone", "en", "dark"]]) {
  const { ctx, p, errs } = await newPage(browser, lang, theme, w, h);
  await p.goto(WEB + "/tools/enable-adb", { waitUntil: "load" });
  await p.waitForSelector("#adb-btn button", { timeout: 15000 });
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
    ok(await p.locator("#adb-btn button").isDisabled(), "button disabled in the browser");
    ok(await p.getAttribute("#adb-btn button", "aria-describedby") === "adb-why", "the reason is the button's description");
    ok(/不能开 ADB/.test(await txt(p, "#adb-why")), `reason: ${await txt(p, "#adb-why")}`);
    ok(/ADB 未开启|设备没报告|USB 模式/.test(await txt(p, "#adb-status")), `status: ${await txt(p, "#adb-status")}`);
    ok(await p.locator("main a[href='/usb']").count() === 1, "link to the USB mode page");
  }
  if (lang === "en") {
    ok(/Enable ADB/.test(await txt(p, "h1")) && /Turn On ADB over USB/.test(await txt(p, "#adb-btn")), "English page");
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
  }
  if (name === "phone") {
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `${lang}: 390 px no sideways scroll (${sw})`);
  }
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
  await p.screenshot({ path: `${OUT}/adb-${name}-${lang}-${theme}.png`, fullPage: true });
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}

// ---- a firmware that can: dialog, Cancel, confirm, read-back, Re-enable ----
await settable("true");
{
  const { ctx, p, errs } = await newPage(browser, "zh", "light");
  await p.goto(WEB + "/tools/enable-adb", { waitUntil: "load" });
  await p.waitForSelector("#adb-btn button", { timeout: 15000 });
  ok(!(await p.locator("#adb-btn button").isDisabled()) && await p.locator("#adb-why").count() === 0, "settable: the button is on, no reason shown");
  const base = await usbOps();
  await p.click("#adb-btn button");
  await p.waitForSelector("#adb-op dialog[open]", { timeout: 8000 });
  const d = await txt(p, "#adb-op dialog");
  ok(/打开 USB 的 ADB 调试？/.test(d) && /root shell/.test(d) && /断开多久/.test(d) && /怎么恢复/.test(d) && /USB 模式」页面/.test(d), `dialog: ${d.slice(0, 120)}`);
  await p.screenshot({ path: `${OUT}/adb-dialog.png` });
  await p.click("#adb-op dialog .nd-btn--secondary");
  await p.waitForFunction(() => !document.querySelector("#adb-op dialog[open]"), null, { timeout: 5000 });
  ok(await usbOps() === base, "Cancel: nothing sent");
  await p.click("#adb-btn button");
  await p.waitForSelector("#adb-op dialog[open]");
  await p.click("#adb-op dialog .nd-btn--danger");
  await until(p, "#adb-op", /ADB 已打开|设备已接受|没有生效/, 40000);
  const res = await txt(p, "#adb-op");
  ok(/^ADB 已打开。插上 USB 线，运行 adb devices 看看。$/.test(res), `result (read back): ${res}`);
  ok(await usbOps() === base + 1, "exactly one usb.set");
  await until(p, "#adb-status", /ADB 已开启/, 8000);
  ok(/ADB 已开启/.test(await txt(p, "#adb-status")), `status: ${await txt(p, "#adb-status")}`);
  ok(/重新启用 ADB/.test(await txt(p, "#adb-btn")), `button: ${await txt(p, "#adb-btn")}`);
  await p.screenshot({ path: `${OUT}/adb-done.png`, fullPage: true });
  // the agent has gone: the page says so, nothing is sent
  await fetch(`${AGENT}/__mock/scenario?set=down`);
  await p.click("#adb-btn button");
  await until(p, "#adb-op", /没有生效/, 20000);
  ok(/没有生效/.test(await txt(p, "#adb-op")) && await p.locator("#adb-op dialog").count() === 0, `agent down: ${await txt(p, "#adb-op")}`);
  await fetch(`${AGENT}/__mock/scenario?set=normal`);
  ok(await usbOps() === base + 1, "still one usb.set");
  ok(errs.length === 0, `no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await settable("absent");
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
