// /router/dualsim (global edition, dual-sim-global.md): the common checks (first load <= 40 KiB gzip
// without fonts, axe light/dark 0, 390 px no sideways scroll, English body without Chinese, no page
// errors, screenshots) and the page's own: single-slot device says so and offers nothing, the SIM
// page links here only on a dual device, the slots as u60d reports them, tier-3 dialog with the
// roaming note (D4), Esc sends nothing, one sim.dual transaction with the right params, read back,
// busy while another change runs. From source/manager (setup: memory luyaz-web-e2e-run):
//   WEB=http://127.0.0.1:18081 MOCK=http://127.0.0.1:19461 OUT=/tmp/lw crates/luyaz-web/e2e/run.sh dualsim
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const OUT = process.env.OUT ?? "/tmp/lw";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19461";
const post = (path, body) => fetch(MOCK + path, { method: "POST", headers: { "content-type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body) });
const stats = () => fetch(`${MOCK}/mock/stats`).then((r) => r.json());
const idleOp = () => post("/mock/op", { rollback_enabled: true, active: null, last: null });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;

// u60g 10-20: single SIM on slot 1, the built-in card off (no MCC). Identifiers are made up.
const SINGLE1 = { active_slot: 1, mode: "single", smart: false, switching: false, slots: [
  { slot: 1, provisioned: true, present: true, ready: true, operator: "CUCC", mcc: "460", mnc: "01", iccid_tail: "1111" },
  { slot: 2, provisioned: false, present: true, ready: null, operator: "CMCC", mcc: "", mnc: "", iccid_tail: "2222" }] };

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
  m.querySelectorAll("[hidden], dialog:not([open]), [style*='display: none']").forEach((e) => e.remove());
  return m.textContent;
});
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, fn, arg, ms = 15000) => p.waitForFunction(fn, arg, { timeout: ms }).catch(() => {});
const ready = (p) => p.waitForFunction(() => (document.querySelector("#ds-mode")?.textContent ?? "—") !== "—", null, { timeout: 15000 });

async function own(p) {
  const btn = p.locator("section[aria-labelledby=ds-set-t] button.nd-btn--primary");
  const done = () => until(p, () => /已改成|已切换|没有生效|设备已接受/.test(document.querySelector("#ds-op")?.textContent ?? ""), null, 20000);

  // the slots as u60d reports them
  ok(await txt(p, "#ds-mode") === "单卡" && await txt(p, "#ds-main") === "卡槽 1（插拔卡）", `now: ${await txt(p, "#ds-mode")} / ${await txt(p, "#ds-main")}`);
  const now = await txt(p, "#ds-now");
  ok(/已启用 · 就绪/.test(now) && /未启用/.test(now) && /CUCC · 460-01/.test(now) && /…2222/.test(now), `slots: ${now.slice(0, 120)}`);
  ok(await btn.isEnabled(), "Switch on");

  // the form: dual shows smart; smart hides the main card
  await p.check("input[name=dm][value='dual']");
  ok(await p.locator("input[type=checkbox]").isVisible(), "dual: smart switch shown");
  await p.check("input[type=checkbox]");
  ok(!(await p.locator("input[name=ds][value='2']").isVisible()), "smart: main card hidden");
  await p.uncheck("input[type=checkbox]");
  await p.check("input[name=ds][value='2']");

  // tier-3 dialog with the roaming note (slot 2 is off: no MCC), Esc sends nothing
  let s0 = await stats();
  await btn.click();
  await p.waitForSelector("dialog#ds-confirm[open]", { timeout: 8000 }).catch(() => {});
  const dlg = await txt(p, "#ds-confirm");
  ok(/切换 SIM 卡？/.test(dlg) && /双卡，主卡卡槽 2（内置卡）/.test(dlg) && /看不出这张卡在这里算不算漫游/.test(dlg) && /约 10 秒/.test(dlg) && /2 分钟/.test(dlg), `dialog: ${dlg.slice(0, 140)}`);
  ok(await p.evaluate(() => document.activeElement?.textContent?.trim()) === "取消", "focus on Cancel");
  const ax = await new AxeBuilder({ page: p }).analyze();
  ok(ax.violations.length === 0, `dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
  await p.screenshot({ path: `${OUT}/ds-dialog.png` });
  await p.keyboard.press("Escape");
  await sleep(800);
  ok(await p.locator("dialog#ds-confirm[open]").count() === 0 && (await stats()).txns.length === s0.txns.length, "Esc: closed, nothing written");

  // confirm: progress, one sim.dual, read back in the rows
  await btn.click();
  await p.waitForSelector("dialog#ds-confirm[open]", { timeout: 8000 }).catch(() => {});
  await p.click("#ds-confirm .nd-btn--confirm");
  await until(p, () => /正在/.test(document.querySelector("#ds-op")?.textContent ?? ""), null, 5000);
  ok(/正在/.test(await txt(p, "#ds-op")), `progress: ${await txt(p, "#ds-op")}`);
  await done();
  ok(await txt(p, "#ds-op") === "已改成双卡 · 主卡 卡槽 2" && await p.locator("#ds-op .t-note--ok").count() === 1, `done: ${await txt(p, "#ds-op")}`);
  const sent = (await stats()).txns.slice(s0.txns.length);
  ok(sent.length === 1 && sent[0].action === "sim.dual" && JSON.stringify(sent[0].params) === '{"active_slot":2,"mode":"dual","smart":false}' && /^lw-/.test(sent[0].op_id) && sent[0].source === "web", `one sim.dual (${JSON.stringify(sent)})`);
  await until(p, () => document.querySelector("#ds-mode")?.textContent === "双卡");
  ok(await txt(p, "#ds-mode") === "双卡" && await txt(p, "#ds-main") === "卡槽 2（内置卡）", "read back in the rows");

  // the same again: refused before a dialog
  await btn.click();
  await until(p, () => /已经是这样/.test(document.querySelector("#ds-op")?.textContent ?? ""), null, 5000);
  ok(/已经是这样了/.test(await txt(p, "#ds-op")), `no change: ${await txt(p, "#ds-op")}`);

  // another change running: off, its words
  await post("/mock/op", { rollback_enabled: true, active: { op_id: "screen-9", item: "sim.dual", phase: "verifying", say_zh: "正在确认", say_en: "Checking", next_zh: null, remaining_ms: null }, last: null });
  await until(p, () => /正在确认/.test(document.querySelector("#ds-now")?.textContent ?? ""), null, 8000);
  ok(await btn.isDisabled(), "a SIM change running: off");
  await idleOp();
}

const browser = await chromium.launch({ channel: "chrome" });
// single-slot device: the page says so, the SIM page has no link
{
  await post("/mock/dual", null);
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: "zh", url: WEB }]);
  const p = await ctx.newPage();
  await p.goto(WEB + "/router/dualsim", { waitUntil: "load" });
  await until(p, () => /只有一个卡槽/.test(document.querySelector("#ds-now")?.textContent ?? ""));
  ok(/这台设备只有一个卡槽/.test(await txt(p, "#ds-now")) && !(await p.locator("section[aria-labelledby=ds-set-t]").isVisible()), "single slot: said, no form");
  await p.goto(WEB + "/router/sim", { waitUntil: "load" });
  ok(await p.locator("main a[href='/router/dualsim']").count() === 0, "single slot: no link on the SIM page");
  await post("/mock/dual", SINGLE1);
  await p.goto(WEB + "/router/dualsim", { waitUntil: "load" });
  await ready(p);
  await p.goto(WEB + "/router/sim", { waitUntil: "load" });
  ok(await p.locator("main a[href='/router/dualsim']").count() === 1, "dual: the SIM page links here");
  await ctx.close();
}
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  await post("/mock/dual", SINGLE1); await post("/mock/txn?end=confirmed"); await post("/mock/stuck"); await idleOp(); await sleep(500);
  await p.goto(WEB + "/router/dualsim", { waitUntil: "load" });
  await ready(p).catch(() => ok(false, `${name} ${lang}: live content never showed`));
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
    await own(p);
  }
  if (lang === "en") {
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
  }
  if (name === "phone") {
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `390 px no sideways scroll (${sw})`);
  }
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length + (process.env.PROBE ? " " + v.nodes.map((n) => n.html + " " + n.failureSummary).join(" / ") : "")).join(", ") || "0"}`);
  ok(errs.length === 0, `${name}: no page errors ${errs.join("; ")}`);
  await p.screenshot({ path: `${OUT}/ds-${name}-${lang}-${theme}.png`, fullPage: true });
  await ctx.close();
}
await post("/mock/dual", null);
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
