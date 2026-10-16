// /router/device (batch 5c): the common checks (first load ≤ 40 KiB gzip without fonts, axe light/dark
// 0, 390 px no sideways scroll, English body without Chinese, no page errors, screenshots) and the
// page's own: status, battery details, the charge-limit draft (Apply, Cancel, readback, out-of-range
// refused), stop charging / power-save / fast boot (readback; "accepted, not read back" when the
// device ignores the write; refused when the agent is down), and the three tier-3 dialogs. Reboot,
// power off and factory reset only ever hit the typed mock agent: GET /__lw/device on the fake agent
// counts their POSTs (exactly one each, none for Cancel or a wrong word). Factory reset runs last
// (it resets the mock's state). From source/manager:
//   WEB=http://127.0.0.1:18610 AGENT=http://127.0.0.1:19710 OUT=/tmp/lw-dev \
//     crates/luyaz-web/e2e/run.sh device
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18610";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19710";
const OUT = process.env.OUT ?? "/tmp/lw-dev";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
const scenario = (set) => fetch(`${AGENT}/__mock/scenario?set=${set}`);
const counts = async () => (await fetch(`${AGENT}/__lw/device`)).json();

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


await fetch(`${AGENT}/__lw/device/reset`, { method: "POST" });
const SEL = "#dc-modes .nd-row, #dc-form [role=switch]";
const sw = (p, k) => p.locator(`#dc-modes button[aria-labelledby="dc-m-${k}"]`);
const area = (p, id) => txt(p, `#${id}-op`);

async function own(p) {
  ok(/^●?\s*电量 \d+%/.test(await txt(p, "#dc-status .nd-status__state")), `status: ${await txt(p, "#dc-status .nd-status__state")} | ${await txt(p, "#dc-status .nd-status__reason")}`);
  const rows = await p.locator("#dc-battery .nd-row").count();
  ok(rows >= 9, `battery details: ${rows} rows`);
  const bt = await txt(p, "#dc-battery");
  ok(/预估/.test(bt) && /电压/.test(bt) && /健康度/.test(bt) && /循环次数/.test(bt) && /满充 \/ 设计容量/.test(bt), "battery rows named");
  ok(/当前电量/.test(await txt(p, "#dc-cap")), `capacity row: ${await txt(p, "#dc-cap")}`);

  // ---- the charge-limit draft ----
  ok(await p.locator("#dc-form [role=switch]").getAttribute("aria-checked") === "false", "limit starts off");
  ok(!(await p.locator("#dc-limit").isVisible()), "sliders hidden while off");
  await p.click("#dc-form [role=switch]");
  ok(await p.locator("#dc-limit").isVisible() && await p.locator("#dc-hyst").isVisible(), "sliders shown when on");
  ok(/^50%/.test((await txt(p, "#dc-form .dc-slider__ends")).slice(0, 3)), "slider ends labelled");
  const setRange = (id, v) => p.evaluate(([i, x]) => { const e = document.getElementById(i); e.value = String(x); e.dispatchEvent(new Event("input", { bubbles: true })); e.dispatchEvent(new Event("change", { bubbles: true })); }, [id, v]);
  await setRange("dc-limit", 70);
  await setRange("dc-hyst", 8);
  await p.waitForFunction(() => /70%/.test(document.querySelector("#dc-form .dc-slider__head .nd-mono")?.textContent ?? ""), null, { timeout: 3000 }).catch(() => {});
  ok(/70%/.test(await txt(p, "#dc-form .dc-slider >> nth=0")) && await p.getAttribute("#dc-limit", "aria-valuetext") === "70%", "slider value and aria-valuetext follow");
  ok(await p.locator("#dc-form >> text=还没应用").isVisible(), "「还没应用」 shown for a draft");
  await p.screenshot({ path: `${OUT}/device-draft.png`, fullPage: true });
  const was = await txt(p, "#dc-status .nd-status__reason");
  await p.click("#dc-form .nd-btn--primary");
  await p.waitForSelector("#dc-charge-op .nd-confirm", { timeout: 6000 });
  ok(/70% 停止充电，低于 62% 再开始充/.test(await txt(p, "#dc-charge-op .nd-confirm__text")), `confirm: ${await txt(p, "#dc-charge-op .nd-confirm__text")}`);
  await p.click("#dc-charge-op .nd-btn--secondary");
  await p.waitForFunction(() => !document.querySelector("#dc-charge-op .nd-confirm"), null, { timeout: 4000 });
  await p.waitForTimeout(800);
  ok(was === await txt(p, "#dc-status .nd-status__reason"), "Cancel: nothing changed on the device");
  ok(await p.locator("#dc-limit").inputValue() === "70", "Cancel keeps the draft");
  await p.click("#dc-form .nd-btn--primary");
  await p.waitForSelector("#dc-charge-op .nd-confirm");
  await p.click("#dc-charge-op .nd-btn--confirm");
  await until(p, "#dc-charge-op", /已设置充电上限|设备已接受|没有生效/, 15000);
  ok(/已设置充电上限 70%，低于 62% 再充/.test(await area(p, "dc-charge")), `charge result: ${await area(p, "dc-charge")}`);
  ok(!(await p.locator("#dc-form >> text=还没应用").isVisible()), "no 「还没应用」 after Apply");
  ok(await p.locator("#dc-limit").inputValue() === "70" && await p.locator("#dc-form [role=switch]").getAttribute("aria-checked") === "true", "the form shows what was applied");
  await until(p, "#dc-status", /充电上限 70%|62%/, 12000);
  ok(/充电上限 70%|62%/.test(await txt(p, "#dc-status")), `status follows: ${await txt(p, "#dc-status .nd-status__reason")}`);
  await p.screenshot({ path: `${OUT}/device-applied.png`, fullPage: true });
  // out of range: said, never sent
  const raw = (path, body) => fetch(WEB + path, { method: "POST", headers: { cookie: `lw_sid=${sid}; u60_lang=zh`, origin: new URL(WEB).origin, "content-type": "application/json" }, body }).then((x) => x.text());
  const bad = await raw("/router/device/charge/ask", JSON.stringify({ _cle: true, _cll: 101, _clh: 5 }));
  ok(bad.includes("充电上限要在 50–100%") && !bad.includes("nd-confirm"), "ask out of range: refused");
  const bad2 = await raw("/router/device/charge/set?e=true&l=101&h=5", "{}");
  ok(bad2.includes("数值超出范围"), "set out of range: refused");
  // limit off again
  await p.click("#dc-form [role=switch]");
  await p.click("#dc-form .nd-btn--primary");
  await p.waitForSelector("#dc-charge-op .nd-confirm");
  ok(/取消充电限制/.test(await txt(p, "#dc-charge-op .nd-confirm__text")), "off: confirm says the limit is removed");
  await p.click("#dc-charge-op .nd-btn--confirm");
  await until(p, "#dc-charge-op", /已取消充电限制|设备已接受|没有生效/, 15000);
  ok(/已取消充电限制/.test(await area(p, "dc-charge")), `limit removed: ${await area(p, "dc-charge")}`);

  // ---- the three switches ----
  for (const [k, on, ask, done, off] of [["dps", true, /切断充电输入/, /已停止充电/, /已恢复充电/], ["ps", true, /省电模式/, /已打开省电模式/, /已关闭省电模式/], ["fb", true, /重启后生效/, /已打开快速启动/, /已关闭快速启动/]]) {
    const s = sw(p, k);
    ok(await s.getAttribute("aria-checked") === "false", `${k}: starts off`);
    await s.click();
    await p.waitForSelector("#dc-mode-op .nd-confirm", { timeout: 8000 });
    ok(ask.test(await txt(p, "#dc-mode-op .nd-confirm__text")), `${k}: confirm: ${await txt(p, "#dc-mode-op .nd-confirm__text")}`);
    if (k === "dps") await p.screenshot({ path: `${OUT}/device-confirm.png`, fullPage: true });
    await p.click("#dc-mode-op .nd-btn--secondary");
    await p.waitForFunction(() => !document.querySelector("#dc-mode-op .nd-confirm"), null, { timeout: 4000 });
    ok(await s.getAttribute("aria-checked") === "false", `${k}: Cancel leaves it off (no optimistic flip)`);
    await s.click();
    await p.waitForSelector("#dc-mode-op .nd-confirm");
    await p.click("#dc-mode-op .nd-btn--confirm");
    await until(p, "#dc-mode-op", done, 15000);
    ok(done.test(await area(p, "dc-mode")), `${k}: ${await area(p, "dc-mode")}`);
    await p.waitForFunction((key) => document.querySelector(`#dc-modes button[aria-labelledby="dc-m-${key}"]`)?.getAttribute("aria-checked") === "true", k, { timeout: 12000 }).catch(() => {});
    ok(await s.getAttribute("aria-checked") === "true", `${k}: the switch shows the device value (on)`);
    if (k === "dps") ok(/已停止充电/.test(await txt(p, "#dc-status .nd-status__state")), `dps: status says stopped: ${await txt(p, "#dc-status .nd-status__state")}`);
    await s.click();
    await p.waitForSelector("#dc-mode-op .nd-confirm");
    await p.click("#dc-mode-op .nd-btn--confirm");
    await until(p, "#dc-mode-op", off, 15000);
    ok(off.test(await area(p, "dc-mode")), `${k} back: ${await area(p, "dc-mode")}`);
    await p.waitForFunction((key) => document.querySelector(`#dc-modes button[aria-labelledby="dc-m-${key}"]`)?.getAttribute("aria-checked") === "false", k, { timeout: 12000 }).catch(() => {});
  }
  // the device ignores the write: accepted, not read back, never "成功"
  await scenario("fakesuccess");
  await sw(p, "ps").click();
  await p.waitForSelector("#dc-mode-op .nd-confirm");
  await p.click("#dc-mode-op .nd-btn--confirm");
  await until(p, "#dc-mode-op", /设备已接受|已打开省电模式/, 15000);
  const fs = await area(p, "dc-mode");
  ok(/设备已接受，还没读回/.test(fs) && !/成功|已打开/.test(fs), `fakesuccess: ${fs}`);
  await scenario("normal");
  // the agent is down: refused with the reason, no confirm
  await scenario("down");
  await sw(p, "fb").click();
  await until(p, "#dc-mode-op", /没有生效/, 20000);
  ok(/没有生效/.test(await area(p, "dc-mode")) && await p.locator("#dc-mode-op .nd-confirm").count() === 0, `agent down: ${await area(p, "dc-mode")}`);
  await scenario("normal");

  // ---- tier 3: reboot / power off / factory reset (the typed mock agent only) ----
  const c0 = await counts();
  ok(Object.keys(c0).length === 0, `no reboot / power off / reset yet: ${JSON.stringify(c0)}`);
  const dlg = "#dc-rst-op dialog[open]";
  // reboot
  await p.click("button:has-text('重启设备')");
  await p.waitForSelector(dlg, { timeout: 8000 });
  const rd = await txt(p, dlg);
  ok(/重启设备？/.test(rd) && /约 90 秒/.test(rd) && /怎么恢复/.test(rd), `reboot dialog: ${rd.slice(0, 100)}`);
  await p.screenshot({ path: `${OUT}/device-reboot-dialog.png` });
  await p.click(`${dlg} .nd-btn--secondary`);
  await p.waitForFunction(() => !document.querySelector("#dc-rst-op dialog[open]"), null, { timeout: 4000 });
  ok(Object.keys(await counts()).length === 0, "Cancel: nothing reached the agent");
  await p.click("button:has-text('重启设备')");
  await p.waitForSelector(dlg);
  await p.click(`${dlg} .nd-btn--confirm`);
  await until(p, "#dc-rst-op", /设备已接受重启|连接中断|没有生效/, 20000);
  ok(/设备已接受重启/.test(await area(p, "dc-rst")) && !/成功/.test(await area(p, "dc-rst")), `reboot result: ${(await area(p, "dc-rst")).slice(0, 90)}`);
  ok((await counts())["/api/device/reboot"] === 1, "exactly one POST /api/device/reboot");
  // power off
  await p.click("button.nd-btn--secondary:has-text('关机')");
  await p.waitForSelector(dlg);
  ok(/长按电源键/.test(await txt(p, dlg)), `power-off dialog: ${(await txt(p, dlg)).slice(0, 90)}`);
  await p.click(`${dlg} .nd-btn--confirm`);
  await until(p, "#dc-rst-op", /设备已接受关机|连接中断|没有生效/, 20000);
  ok(/设备已接受关机。只能在设备上按电源键开机。/.test(await area(p, "dc-rst")), `power-off result: ${await area(p, "dc-rst")}`);
  ok((await counts())["/api/device/poweroff"] === 1, "exactly one POST /api/device/poweroff");
  // factory reset: the word gates it, in the page and on the server
  await p.click("button.nd-btn--danger:has-text('恢复出厂设置')");
  await p.waitForSelector(dlg);
  const btn = p.locator(`${dlg} .nd-btn--danger`);
  ok(await btn.isDisabled(), "reset: the button is off until the word is typed");
  await p.fill(`${dlg} input`, "恢复");
  ok(await btn.isDisabled(), "reset: a wrong word keeps it off");
  await p.screenshot({ path: `${OUT}/device-reset-dialog.png` });
  const wrong = await fetch(`${WEB}/router/device/rst/set?k=reset&word=${encodeURIComponent("恢复")}`, { method: "POST", headers: { cookie: `lw_sid=${sid}; u60_lang=zh`, origin: new URL(WEB).origin } }).then((x) => x.text());
  ok(wrong.includes("输入的确认词不对"), "a request with a wrong word is refused by the server");
  const none = await fetch(`${WEB}/router/device/rst/set?k=reset`, { method: "POST", headers: { cookie: `lw_sid=${sid}; u60_lang=zh`, origin: new URL(WEB).origin } }).then((x) => x.text());
  ok(none.includes("输入的确认词不对"), "a request without the word is refused by the server");
  ok((await counts())["/api/device/factory-reset"] === undefined, "no factory-reset POST yet");
  await p.click(`${dlg} .nd-btn--secondary`);
  await p.waitForFunction(() => !document.querySelector("#dc-rst-op dialog[open]"), null, { timeout: 4000 });
  ok((await counts())["/api/device/factory-reset"] === undefined, "Cancel: no factory-reset POST");
  await p.click("button.nd-btn--danger:has-text('恢复出厂设置')");
  await p.waitForSelector(dlg);
  await p.fill(`${dlg} input`, "恢复出厂");
  ok(!(await btn.isDisabled()), "the right word turns it on");
  await p.click(`${dlg} .nd-btn--danger`);
  await until(p, "#dc-rst-op", /设备已接受恢复出厂|连接中断|没有生效/, 20000);
  ok(/设备已接受恢复出厂/.test(await area(p, "dc-rst")) && /重新运行装机包/.test(await area(p, "dc-rst")), `reset result: ${(await area(p, "dc-rst")).slice(0, 100)}`);
  ok((await counts())["/api/device/factory-reset"] === 1, "exactly one POST /api/device/factory-reset");
  await p.screenshot({ path: `${OUT}/device-reset-done.png`, fullPage: true });
}

async function english(p) {
  // the dialogs and results in English, from the pages only (no write)
  const e = await fetch(WEB + "/router/device/rst/ask?k=reset", { method: "POST", headers: { cookie: `lw_sid=${sid}; u60_lang=en`, origin: new URL(WEB).origin } }).then((x) => x.text());
  const left = (e.match(CJK) ?? []);
  ok(left.length === 0 && /Type “factory reset” to confirm/.test(e), `English reset dialog (${[...new Set(left)].join("") || "no Chinese"})`);
}

const browser = await chromium.launch({ channel: "chrome" });
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"], [390, 844, "phone", "en", "dark"]]) {
  const { ctx, p, errs } = await newPage(browser, lang, theme, w, h);
  await p.goto(WEB + "/router/device", { waitUntil: "load" });
  await p.waitForSelector("#dc-form [role=switch]", { timeout: 20000 });
  await p.waitForSelector("#dc-modes .nd-row .nd-switch:not([disabled])", { timeout: 20000 });
  await p.waitForSelector("#dc-battery .nd-row", { timeout: 20000 });
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
  }
  if (lang === "en") {
    ok(/Device Control/.test(await txt(p, "h1")) && /Charge Limit/.test(await txt(p, "main")) && /Factory Reset/.test(await txt(p, "main")), "English page");
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
    if (name === "desk") await english(p);
  }
  if (name === "phone") {
    const sw0 = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw0 <= 390, `${lang}: 390 px no sideways scroll (${sw0})`);
  }
  // axe with the limit on (sliders shown) and a confirm open
  await p.click("#dc-form [role=switch]");
  await p.click("#dc-form .nd-btn--primary");
  await p.waitForSelector("#dc-charge-op .nd-confirm", { timeout: 6000 });
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme} (sliders + confirm open): ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
  if (name === "phone") {
    const sw1 = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw1 <= 390, `${lang}: 390 px no sideways scroll with sliders + confirm (${sw1})`);
  }
  await p.screenshot({ path: `${OUT}/device-${name}-${lang}-${theme}.png`, fullPage: true });
  if (name === "desk" && lang === "zh") {
    await p.reload({ waitUntil: "load" });
    await p.waitForSelector("#dc-modes .nd-row .nd-switch:not([disabled])", { timeout: 20000 });
    await own(p);
  }
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
