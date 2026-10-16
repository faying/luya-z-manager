// /router/stc (batch 5c): the common checks (first load <= 40 KiB gzip without fonts, axe light/dark 0,
// 390 px no sideways scroll, English body without Chinese, no page errors, screenshots) and the
// page's own: the switch (dialog, Cancel, confirm, readback), the parameters (inline confirm,
// validation, draft kept through a re-read, readback), the reset (dialog, accepted never "reset"),
// cmdfail, fakesuccess, service down, stale, B27 (unsupported: everything locked). From source/manager:
//   WEB=http://127.0.0.1:18620 AGENT=http://127.0.0.1:19720 OUT=/tmp/lw-stc crates/luyaz-web/e2e/run.sh router-stc
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18620";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19720";
const OUT = process.env.OUT ?? "/tmp/lw-stc";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
const scenario = (set) => fetch(`${AGENT}/__mock/scenario?set=${set}`);

await scenario("normal");
const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);
const hdr = { cookie: `lw_sid=${sid}`, origin: new URL(WEB).origin, "content-type": "application/json" };
const post = (path, body) => fetch(WEB + path, { method: "POST", headers: hdr, body: body ? JSON.stringify(body) : undefined });

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
  const m = document.querySelector("main");
  return m.textContent + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, sel, re, ms = 10000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout: ms }).catch(() => {});

const confirmBtn = (p, id) => p.click(`#${id}-confirm .nd-btn--confirm, #${id}-confirm .nd-btn--danger`);
const result = async (p, id, re, ms = 20000) => { await until(p, `#${id}-op`, re, ms); return txt(p, `#${id}-op`); };
const sw = (p) => p.locator("#stc-switch button[role=switch]");
const openSwitch = async (p) => { await sw(p).click(); await p.waitForSelector("#stc-tg-confirm", { state: "visible" }); };

async function own(p) {
  const st = () => txt(p, "#stc-status .nd-status__state");
  ok(/已禁用/.test(await st()), `status: ${await st()}`);
  ok(await sw(p).getAttribute("aria-checked") === "false", "switch off");
  ok(await p.inputValue("#stc-f-stlt") === "60" && await p.inputValue("#stc-f-stnm") === "8", "parameters from the device");

  // switch on: Cancel sends nothing, confirm applies
  await openSwitch(p);
  ok(/开启 STC/.test(await txt(p, "#stc-tg-confirm")) && /30 秒/.test(await txt(p, "#stc-tg-confirm")), `dialog: ${(await txt(p, "#stc-tg-confirm")).slice(0, 120)}`);
  await p.screenshot({ path: `${OUT}/stc-dialog.png`, fullPage: true });
  await p.click("#stc-tg-confirm .nd-btn--secondary");
  await p.waitForFunction(() => !document.querySelector("#stc-tg-confirm"), null, { timeout: 5000 });
  ok(await sw(p).getAttribute("aria-checked") === "false" && /已禁用/.test(await st()), "Cancel: still off");
  await openSwitch(p);
  await confirmBtn(p, "stc-tg");
  let t = await result(p, "stc-tg", /STC 已开启|没有生效|设备已接受/);
  ok(/STC 已开启/.test(t) && /t-note--ok/.test(await p.getAttribute("#stc-tg-op .t-note", "class")), `on: ${t}`);
  await until(p, "#stc-status", /已启用/);
  ok(/已启用/.test(await st()) && await sw(p).getAttribute("aria-checked") === "true", `status follows: ${await st()}`);
  await openSwitch(p);
  await confirmBtn(p, "stc-tg");
  t = await result(p, "stc-tg", /STC 已关闭/);
  ok(/STC 已关闭/.test(t), `off: ${t}`);

  // parameters
  await p.fill("#stc-f-stlt", "");
  await p.click('#stc-params button:has-text("应用参数")');
  await until(p, "#stc-pa-op", /要填整数/);
  ok(/LTE 采集定时器要填整数/.test(await txt(p, "#stc-pa-op")), `empty field: ${await txt(p, "#stc-pa-op")}`);
  await p.fill("#stc-f-stlt", "9x0");
  ok(await p.inputValue("#stc-f-stlt") === "90", "non-digits are dropped as typed");
  await p.click('#stc-params button:has-text("应用参数")');
  await p.waitForSelector("#stc-pa-confirm");
  ok(/LTE 90 秒 \/ 8 个小区，NR-SA 60 秒 \/ 8 个小区/.test(await txt(p, "#stc-pa-confirm")), `confirm names the values: ${await txt(p, "#stc-pa-confirm")}`);
  ok(await p.locator("dialog#stc-pa-confirm").count() === 0, "tier 2: inline, not a dialog");
  await p.screenshot({ path: `${OUT}/stc-params.png`, fullPage: true });
  await p.click("#stc-pa-confirm .nd-btn--secondary");
  await p.waitForFunction(() => !document.querySelector("#stc-pa-confirm"), null, { timeout: 5000 });
  ok(await p.inputValue("#stc-f-stlt") === "90", "Cancel keeps the draft");
  await post("/router/stc/refresh");
  await p.waitForTimeout(1500);
  ok(await p.inputValue("#stc-f-stlt") === "90", "draft survives a re-read");
  await p.click('#stc-params button:has-text("应用参数")');
  await p.waitForSelector("#stc-pa-confirm");
  await confirmBtn(p, "stc-pa");
  t = await result(p, "stc-pa", /参数已应用|没有生效|设备已接受/);
  ok(/参数已应用/.test(t), `applied: ${t}`);
  await p.click('#stc-params button:has-text("应用参数")');
  await until(p, "#stc-pa-op", /设备上已经是这些值/, 5000);
  ok(/设备上已经是这些值/.test(await txt(p, "#stc-pa-op")), `same values: ${await txt(p, "#stc-pa-op")}`);

  // reset
  await p.click("#stc-resetbtn button");
  await p.waitForSelector("#stc-rs-confirm", { state: "visible" });
  ok(/nd-btn--danger/.test(await p.getAttribute("#stc-rs-confirm button[data-write]", "class")) && /无法撤销/.test(await txt(p, "#stc-rs-confirm")), "reset dialog: danger, cannot be undone");
  await p.click("#stc-rs-confirm .nd-btn--secondary");
  await p.waitForFunction(() => !document.querySelector("#stc-rs-confirm"), null, { timeout: 5000 });
  ok(!/设备已接受/.test(await txt(p, "#stc-rs-op")), "Cancel: nothing sent");
  await p.click("#stc-resetbtn button");
  await p.waitForSelector("#stc-rs-confirm", { state: "visible" });
  await confirmBtn(p, "stc-rs");
  t = await result(p, "stc-rs", /设备已接受|没有生效/);
  ok(/设备已接受：重置白名单/.test(t) && !/已重置|成功/.test(t), `reset: ${t}`);

  // cmdfail: refused with the reason
  await scenario("cmdfail");
  await openSwitch(p);
  await confirmBtn(p, "stc-tg");
  t = await result(p, "stc-tg", /没有生效/);
  ok(/没有生效：mock failure/.test(t), `cmdfail: ${t}`);
  await scenario("normal");

  // fakesuccess: agent says ok, nothing changed -> accepted after the wait, never applied
  await scenario("fakesuccess");
  await openSwitch(p);
  await confirmBtn(p, "stc-tg");
  const second = await (await post("/router/stc/reset/set")).text();
  ok(/另一项 STC 改动正在进行/.test(second), "a second write meanwhile is refused");
  t = await result(p, "stc-tg", /设备已接受/, 50000);
  ok(/设备已接受，还没读回/.test(t) && !/STC 已开启|成功/.test(t), `fakesuccess: ${t}`);
  await scenario("normal");

  // stale: greyed, said, locked
  await scenario("down");
  await post("/router/stc/refresh");
  await until(p, "#stc-status", /停在/, 15000);
  ok(/设置停在 \d\d:\d\d/.test(await txt(p, "#stc-status")) && await p.locator("#stc-switch.nd-stale").count() === 1, `stale: ${await txt(p, "#stc-status")}`);
  ok(await sw(p).isDisabled() && await p.locator("#stc-f-stlt").isDisabled() && await p.locator('#stc-params button:has-text("应用参数")').isDisabled(), "switch and parameters locked");
  await p.screenshot({ path: `${OUT}/stc-stale.png`, fullPage: true });
  // a fresh page while the agent is down: service unavailable, reset stays
  const q = await p.context().newPage();
  await q.goto(WEB + "/router/stc", { waitUntil: "load" });
  await q.waitForSelector("#stc-status");
  ok(/STC 服务不可用/.test(await txt(q, "#stc-status")) && /重试/.test(await txt(q, "#stc-status")), `down: ${await txt(q, "#stc-status")}`);
  ok(!(await q.locator("#stc-resetbtn button").isDisabled()), "reset stays available");
  await q.screenshot({ path: `${OUT}/stc-down.png`, fullPage: true });
  await q.close();
  await scenario("normal");
  await post("/router/stc/refresh");
  await p.waitForFunction(() => !/停在/.test(document.querySelector("#stc-status")?.textContent ?? ""), null, { timeout: 12000 }).catch(() => {});
  ok(!/停在/.test(await txt(p, "#stc-status")), "back to live");

  // B27: no STC at all
  await scenario("firmware-b27");
  const b = await p.context().newPage();
  await b.goto(WEB + "/router/stc", { waitUntil: "load" });
  await b.waitForSelector("#stc-status");
  ok(/这个固件没有此功能/.test(await txt(b, "#stc-status")) && !/重试/.test(await txt(b, "#stc-status")), `B27: ${await txt(b, "#stc-status .nd-status__state")}`);
  ok(await b.locator("#stc-switch button[role=switch]").count() === 0 && await b.locator("#stc-resetbtn button").isDisabled() && await b.locator("#stc-f-stlt").isDisabled(), "B27: switch gone, reset and parameters locked");
  await b.screenshot({ path: `${OUT}/stc-b27.png`, fullPage: true });
  const ax = await new AxeBuilder({ page: b }).analyze();
  ok(ax.violations.length === 0, `axe B27: ${ax.violations.map((v) => v.id).join(",") || "0"}`);
  await b.close();
  await scenario("normal");
}

const browser = await chromium.launch({ channel: "chrome" });
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"], [390, 844, "phone", "en", "dark"], [1280, 900, "desk", "zh", "light"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  await p.goto(WEB + "/router/stc", { waitUntil: "load" });
  await p.waitForSelector("#stc-switch .nd-row", { timeout: 15000 });
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (<= 40)`);
  }
  if (lang === "en") {
    ok(/STC/.test(await txt(p, "h1")) && /Apply Parameters/.test(await txt(p, "#stc-params")), "English status");
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
  }
  if (name === "phone") {
    const w2 = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(w2 <= 390, `${lang}: 390 px no sideways scroll (${w2})`);
  }
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "x" + v.nodes.length).join(", ") || "0"}`);
  await p.screenshot({ path: `${OUT}/stc-${name}-${lang}-${theme}.png`, fullPage: true });
  if (name === "desk" && lang === "zh") await own(p);
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
