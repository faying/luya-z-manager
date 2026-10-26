// /sms/forward (batch 5a): the common checks (first load ≤ 40 KiB gzip without fonts, axe
// light/dark 0, 390 px no sideways scroll, English body without Chinese, no page errors,
// screenshots) and each write path once against the typed mock agent (fixtures/sms.ts), which the
// fake agent serves: settings (refused interval, saved + read back), a new Telegram rule, editing
// it with the token left empty (kept: read from the agent), on/off, test (sent / refused with the
// WAN down), retry the failed, delete, clear, a refused write (cmdfail). And that no secret ever
// reaches the browser: the page HTML, every write reply and the push stream are searched for the
// fixture's Bark key and the token typed here. Mock state is in memory: start a fresh fake agent
// before each run. From source/manager:
//   WEB=http://127.0.0.1:18252 AGENT=http://127.0.0.1:19260 OUT=/tmp/lw-fwd \
//     crates/luyaz-web/e2e/run.sh sms-forward
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import fs from "node:fs";

const WEB = process.env.WEB ?? "http://127.0.0.1:18252";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19260";
const OUT = process.env.OUT ?? "/tmp/lw-fwd";
fs.mkdirSync(OUT, { recursive: true });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
const SECRETS = ["Q7mZr2x9Ke", "777:SECRETtok"];
const scenario = (set) => fetch(`${AGENT}/__mock/scenario?set=${set}`);

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);
// the agent's config as stored (through the proxy, with this session)
const stored = () => fetch(`${AGENT}/__lw/fwd-config`).then((x) => x.json()).then((v) => v.data.config);
ok((await fetch(`${WEB}/api/sms/forward/config`, { headers: { cookie: `lw_sid=${sid}` } })).status === 404, "the agent's config route is not served (404; secrets never raw)");

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
  m.querySelectorAll("[style*='display: none']").forEach((e) => e.remove());
  return m.innerText + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, fn, arg, ms = 15000) => p.waitForFunction(fn, arg, { timeout: ms }).catch(() => {});
const ready = (p) => p.waitForFunction(() => /转发|Forwarding/.test(document.querySelector("#fwd-status")?.textContent ?? "") && !/正在读取|Reading/.test(document.querySelector("#fwd-status")?.textContent ?? ""), null, { timeout: 15000 });

// every finite reply of this page (the write answers) goes into the leak check
const replies = [];
function watch(p) {
  p.on("response", async (res) => {
    const u = res.url();
    if (!u.includes("/sms/forward") || u.includes("/events")) return;
    replies.push(await res.text().catch(() => ""));
  });
}

async function own(p) {
  const done = (id, re) => until(p, ([id, re]) => new RegExp(re).test(document.querySelector(`#${id}-op`)?.textContent ?? ""), [id, re], 25000);
  const inline = (id) => p.locator(`#${id}-confirm.nd-confirm .nd-btn--confirm`);
  const dialog = (id) => p.locator(`dialog#${id}-confirm[open]`);
  const op = (id) => txt(p, `#${id}-op`);

  ok(/转发已开 · 1 条规则启用/.test(await txt(p, "#fwd-status")) && /#106/.test(await txt(p, "#fwd-status")), `status: ${await txt(p, "#fwd-status")}`);
  ok(await p.getAttribute("#fwd-tab-cfg", "aria-selected") === "true", "tabs: settings first");

  // settings: an interval the agent would refuse is stopped here, no confirm
  await p.fill("#fwd-cp", "5");
  await p.click("#fwd-cfgform .nd-btn--primary");
  await done("fwd-cfg", "10");
  ok(/10 到 3600/.test(await op("fwd-cfg")) && await inline("fwd-cfg").count() === 0, `interval 5: ${await op("fwd-cfg")}`);
  await p.fill("#fwd-cp", "45");
  await p.click("button[aria-labelledby=fwd-cm-l]");
  ok(await p.isVisible("#fwd-cfgform button:text-is('放弃改动')"), "a changed draft offers 放弃改动");
  await p.click("#fwd-cfgform .nd-btn--primary");
  await inline("fwd-cfg").waitFor({ timeout: 8000 }).catch(() => {});
  ok(await inline("fwd-cfg").count() === 1 && /每 45 秒/.test(await op("fwd-cfg")), `settings: inline confirm (tier 2): ${await op("fwd-cfg")}`);
  await inline("fwd-cfg").click();
  await done("fwd-cfg", "已保存|没有生效|设备已接受");
  let c = await stored();
  ok(await op("fwd-cfg") === "设置已保存" && c.poll_interval_secs === 45 && c.mark_read_after_forward === true, `saved + read back: ${await op("fwd-cfg")}`);
  await until(p, () => /每 45 秒/.test(document.querySelector("#fwd-status")?.textContent ?? ""));
  ok(/每 45 秒/.test(await txt(p, "#fwd-status")), "status follows");

  // rules: a new Telegram rule
  await p.click("#fwd-tab-rules");
  ok(/1 条规则/.test(await txt(p, "#fwd-rulen")) && /POST push\.example\.net/.test(await txt(p, "#fwd-rules")), `rule row by type + host: ${await txt(p, "#fwd-rules")}`);
  await p.click("button:text-is('添加规则')");
  await p.waitForSelector("input[data-bind=_rn]", { timeout: 8000 }).catch(() => {});
  await p.fill("input[data-bind=_rn]", "TG 测试");
  await p.fill("input[data-bind=_rbt]", "777:SECRETtok");
  await p.click("#fwd-form .nd-btn--primary");
  await done("fwd-rule", "Chat ID 填上");
  ok(/把 Chat ID 填上/.test(await op("fwd-rule")), `missing chat id: ${await op("fwd-rule")}`);
  await p.fill("input[data-bind=_rci]", "-42");
  await p.click("#fwd-form .nd-btn--primary");
  await inline("fwd-rule").waitFor({ timeout: 8000 }).catch(() => {});
  ok(/全部短信转发到 Telegram -42/.test(await op("fwd-rule")), `create: inline confirm: ${await op("fwd-rule")}`);
  await inline("fwd-rule").click();
  await done("fwd-rule", "已创建|没有生效|设备已接受");
  c = await stored();
  const tg = c.rules.find((x) => x.name === "TG 测试");
  ok(await op("fwd-rule") === "规则已创建" && tg?.destination.bot_token === "777:SECRETtok" && tg.destination.chat_id === "-42", `created: ${await op("fwd-rule")}`);
  ok(await p.locator("#fwd-form input").count() === 0, "the form closes");
  await until(p, () => /2 条规则/.test(document.querySelector("#fwd-rulen")?.textContent ?? ""));
  ok(/Telegram -42/.test(await txt(p, "#fwd-rules")), "the new row");

  // edit it: the token is never shown; empty = keep
  await p.click(`#fwd-e-${tg.id}`);
  await p.waitForSelector("input[data-bind=_rbt]", { timeout: 8000 }).catch(() => {});
  ok(await p.inputValue("input[data-bind=_rbt]") === "" && /已设 · 留空不改/.test(await p.getAttribute("input[data-bind=_rbt]", "placeholder")), "edit: token field empty, 「已设 · 留空不改」");
  ok(await p.inputValue("input[data-bind=_rci]") === "-42" && await p.inputValue("input[data-bind=_rn]") === "TG 测试", "edit: the rest prefilled");
  await p.fill("input[data-bind=_rn]", "TG 2");
  await p.click("#fwd-form .nd-btn--primary");
  await inline("fwd-rule").waitFor({ timeout: 8000 }).catch(() => {});
  await inline("fwd-rule").click();
  await done("fwd-rule", "已更新|没有生效|设备已接受");
  c = await stored();
  const tg2 = c.rules.find((x) => x.id === tg.id);
  ok(await op("fwd-rule") === "规则已更新" && tg2?.name === "TG 2" && tg2.destination.bot_token === "777:SECRETtok", `updated, token kept: ${await op("fwd-rule")} ${tg2?.destination.bot_token === "777:SECRETtok"}`);

  // on/off
  await p.click("button[aria-labelledby='fwd-en-1 fwd-n-1']");
  await inline("fwd-rules").waitFor({ timeout: 8000 }).catch(() => {});
  ok(await inline("fwd-rules").count() === 1 && await dialog("fwd-rules").count() === 0, "rule off: inline (tier 2)");
  await inline("fwd-rules").click();
  await done("fwd-rules", "已关掉|没有生效|设备已接受");
  ok(/已关掉/.test(await op("fwd-rules")) && (await stored()).rules.find((x) => x.id === 1).enabled === false, `rule off: ${await op("fwd-rules")}`);
  await until(p, () => document.querySelector("button[aria-labelledby='fwd-en-1 fwd-n-1']")?.getAttribute("aria-checked") === "false");
  ok(await p.getAttribute("button[aria-labelledby='fwd-en-1 fwd-n-1']", "aria-checked") === "false", "the switch follows");

  // a refused write
  await scenario("cmdfail");
  await p.click("button[aria-labelledby='fwd-en-1 fwd-n-1']");
  await inline("fwd-rules").waitFor({ timeout: 8000 }).catch(() => {});
  await inline("fwd-rules").click();
  await done("fwd-rules", "没有生效|设备已接受|已打开");
  ok(/没有生效/.test(await op("fwd-rules")), `refused: ${await op("fwd-rules")}`);
  await scenario("normal");

  // test: really sends; the WAN down → refused, said
  await p.click(`#fwd-t-${tg.id}`);
  await inline("fwd-rules").waitFor({ timeout: 8000 }).catch(() => {});
  ok(/真实的测试消息/.test(await op("fwd-rules")), `test confirm: ${await op("fwd-rules")}`);
  await inline("fwd-rules").click();
  await done("fwd-rules", "测试已发到|没有生效|连接中断");
  ok(await op("fwd-rules") === "测试已发到 Telegram -42", `test sent: ${await op("fwd-rules")}`);
  await scenario("nosignal");
  await p.click(`#fwd-t-${tg.id}`);
  await inline("fwd-rules").waitFor({ timeout: 8000 }).catch(() => {});
  await inline("fwd-rules").click();
  await done("fwd-rules", "测试已发到|没有生效|连接中断");
  ok(/没有生效：telegram/.test(await op("fwd-rules")), `test refused: ${await op("fwd-rules")}`);
  await scenario("normal");

  // delete: tier 3
  await p.click(`#fwd-d-${tg.id}`);
  await dialog("fwd-rules").waitFor({ timeout: 8000 }).catch(() => {});
  ok(await dialog("fwd-rules").count() === 1, "delete: dialog (tier 3)");
  await p.click("dialog#fwd-rules-confirm .nd-btn--danger");
  await done("fwd-rules", "已删除|没有生效|设备已接受");
  ok(/已删除/.test(await op("fwd-rules")) && !(await stored()).rules.some((x) => x.id === tg.id), `deleted: ${await op("fwd-rules")}`);

  // log: retry the failed, clear
  await p.click("#fwd-tab-log");
  ok(/重试失败项 \(2\)/.test(await txt(p, "#fwd-logbtns")), `failed count: ${await txt(p, "#fwd-logbtns")}`);
  ok(/webhook: HTTP 502/.test(await txt(p, "#fwd-log")) && /2026-09-24 08:31/.test(await txt(p, "#fwd-log")), "log rows: error + device time");
  await p.click("#fwd-logbtns button:has-text('重试失败项')");
  await inline("fwd-log").waitFor({ timeout: 8000 }).catch(() => {});
  ok(/2 条失败/.test(await op("fwd-log")), `retry confirm: ${await op("fwd-log")}`);
  await inline("fwd-log").click();
  await done("fwd-log", "重新发出|还有|连接中断|没有生效");
  ok(await op("fwd-log") === "2 条都已重新发出", `retry: ${await op("fwd-log")}`);
  await until(p, () => !/重试失败项 \(/.test(document.querySelector("#fwd-logbtns")?.textContent ?? ""));
  ok(!/\(\d\)/.test(await txt(p, "#fwd-logbtns")), "no failed left");
  await p.click("#fwd-logbtns button:has-text('清空日志')");
  await dialog("fwd-log").waitFor({ timeout: 8000 }).catch(() => {});
  ok(await dialog("fwd-log").count() === 1, "clear: dialog (tier 3)");
  await p.click("dialog#fwd-log-confirm .nd-btn--danger");
  await done("fwd-log", "已清空|没有生效|设备已接受");
  ok(await op("fwd-log") === "日志已清空", `cleared: ${await op("fwd-log")}`);
  await until(p, () => /暂无日志/.test(document.querySelector("#fwd-log")?.textContent ?? ""));
  ok(/暂无日志/.test(await txt(p, "#fwd-log")), "log empty");
}

// the push stream, read for a few seconds, for the leak check
async function streamSample() {
  const ac = new AbortController();
  const res = await fetch(`${WEB}/sms/forward/events?g=0`, { headers: { cookie: `lw_sid=${sid}` }, signal: ac.signal });
  let body = "";
  const rd = res.body.getReader();
  const t = setTimeout(() => ac.abort(), 4000);
  try { for (;;) { const { value, done } = await rd.read(); if (done) break; body += new TextDecoder().decode(value); } } catch { /* aborted */ }
  clearTimeout(t);
  return body;
}

const browser = await chromium.launch({ channel: "chrome" });
const pages = [];
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  watch(p);
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  await p.goto(WEB + "/sms/forward", { waitUntil: "load" });
  await ready(p).catch(() => ok(false, `${name} ${lang}: live content never showed`));
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
    await own(p);
    pages.push(await p.content());
    // the form open, for axe and the screenshot
    await p.click("#fwd-tab-rules");
    await p.click("#fwd-e-1");
    await p.waitForSelector("input[data-bind=_rwu]", { timeout: 8000 }).catch(() => {});
    ok(await p.isVisible("input[data-bind=_rwu]") && /已设 · 留空不改/.test(await p.getAttribute("input[data-bind=_rwu]", "placeholder")), "webhook URL write-only");
    pages.push(await p.content());
  }
  if (lang === "en") {
    let left = [];
    for (const t of ["cfg", "rules", "log"]) {
      await p.click(`#fwd-tab-${t}`);
      await sleep(200);
      left = left.concat((await mainText(p)).match(CJK) ?? []);
    }
    await p.click("#fwd-tab-rules");
    await p.click("button:text-is('Add Rule')");
    await p.waitForSelector("input[data-bind=_rn]", { timeout: 8000 }).catch(() => {});
    left = left.concat((await mainText(p)).match(CJK) ?? []);
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
  }
  if (name === "phone") {
    let worst = 0;
    for (const t of ["cfg", "rules", "log"]) {
      await p.click(`#fwd-tab-${t}`);
      if (t === "rules") {
        await p.click("#fwd-e-1");
        await p.waitForSelector("input[data-bind=_rn]", { timeout: 8000 }).catch(() => {});
      }
      worst = Math.max(worst, await p.evaluate(() => document.documentElement.scrollWidth));
      await p.screenshot({ path: `${OUT}/fwd-phone-${t}.png`, fullPage: true });
    }
    ok(worst <= 390, `390 px no sideways scroll (${worst})`);
    await p.click("#fwd-tab-rules");
  }
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length + (process.env.PROBE ? " " + v.nodes.map((n) => n.html + " " + n.failureSummary).join(" / ") : "")).join(", ") || "0"}`);
  ok(errs.length === 0, `${name}: no page errors ${errs.join("; ")}`);
  pages.push(await p.content());
  await p.screenshot({ path: `${OUT}/fwd-${name}-${lang}-${theme}.png`, fullPage: true });
  await ctx.close();
}
await browser.close();

const all = pages.join("\n") + replies.join("\n") + (await streamSample());
const leaks = SECRETS.filter((s) => all.includes(s));
ok(leaks.length === 0 && replies.length > 10, `no secret in pages, ${replies.length} replies, the stream (${leaks.join(", ") || "none"})`);
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
