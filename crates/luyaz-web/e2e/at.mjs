// /tools/at (batch 5c): the common checks (first load ≤ 40 KiB gzip without fonts, axe light/dark 0,
// 390 px no sideways scroll, English body without Chinese, no page errors, screenshots) and the page's
// own: the port line found on its own after load, a command not starting with AT refused, a normal
// command sent at once (transcript, history button, last time, box emptied), ↑ / ↓ recall, a history
// button puts the command back, a risky command asks in a dialog (Cancel sends nothing, confirm
// sends), the widened list, the history stays in the tab (a second tab / session sees none of it),
// and no CFUN anywhere on the page. Needs the typed mock agent behind the fake agent (AGENT).
//   WEB=http://127.0.0.1:18630 AGENT=http://127.0.0.1:19730 OUT=/tmp/lw-b5-stk crates/luyaz-web/e2e/run.sh at
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18630";
const OUT = process.env.OUT ?? "/tmp/lw-b5-stk";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);
const post = (path) => fetch(WEB + path, { method: "POST", headers: { cookie: `lw_sid=${sid}`, origin: new URL(WEB).origin } });

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
// device-supplied text (carrier replies, SIM menu labels, port names, modem answers) is not ours
const mainText = (p) => p.evaluate(() => {
  const m = document.querySelector("main").cloneNode(true);
  m.querySelectorAll("[data-data]").forEach((e) => e.remove());
  return m.innerText + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, sel, re, ms = 12000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout: ms }).catch(() => {});

const sendBtn = (p) => p.locator("button[data-write='at']");
async function typeSend(p, cmd, via = "button") {
  await p.fill("#at-cmd", cmd);
  if (via === "enter") await p.press("#at-cmd", "Enter"); else await sendBtn(p).click();
}

async function own(p, ctx) {
  await until(p, "#at-port", /AT 端口可用/, 15000);
  ok(/AT 端口可用/.test(await txt(p, "#at-port")) && /\/dev\//.test(await txt(p, "#at-port")), `port found on its own: ${await txt(p, "#at-port")}`);
  ok(await sendBtn(p).isDisabled(), "Send is off while the box is empty");
  ok(/AT\+CGMR/.test(await p.getAttribute("#at-cmd", "placeholder")), "placeholder is the revision read");
  ok(!(await p.content()).toUpperCase().includes("CFUN"), "no CFUN anywhere on the page");

  // not AT: refused, nothing sent
  await typeSend(p, "hello");
  await p.waitForSelector("#at-err p", { timeout: 5000 });
  ok(/命令必须以 AT 开头/.test(await txt(p, "#at-err")), `must start with AT: ${await txt(p, "#at-err")}`);
  ok(await p.locator(".at-entry").count() === 0, "nothing in the log");

  // a normal command: tier 1, runs at once, no confirm
  await typeSend(p, "at+cgmr");
  await p.waitForSelector(".at-entry", { timeout: 15000 });
  ok(await p.locator("#at-op .nd-confirm, #at-op dialog").count() === 0, "a normal command asks nothing");
  ok(/> at\+cgmr/.test(await txt(p, ".at-cmdline")) && /OK/.test(await txt(p, ".at-resp")), `transcript: ${await txt(p, ".at-entry")}`);
  ok(/\d+ ms/.test(await txt(p, ".at-ms")) && /上次响应耗时 \d+ ms/.test(await txt(p, "#at-last")), `time: ${await txt(p, "#at-last")}`);
  ok(await p.inputValue("#at-cmd") === "", "the box is emptied");
  ok(await p.locator("#at-log-empty").count() === 0 && await p.locator("#at-hist-empty").count() === 0, "the empty notes go away");
  ok(await p.locator("#at-hist-list li").count() === 1, "history has the command");
  ok(await txt(p, "#at-op") === "", "the area is quiet after a good send");

  // Enter sends too; a second command; history is newest first
  await typeSend(p, "AT+CSQ", "enter");
  await until(p, "#at-hist-list", /AT\+CSQ/);
  ok(await p.locator(".at-entry").count() === 2 && /AT\+CSQ/.test(await txt(p, "#at-hist-list li >> nth=0")), "Enter sends; newest first");
  ok(/\+CSQ: /.test(await txt(p, ".at-entry >> nth=1")), "second reply");

  // ↑ / ↓ recall
  await p.focus("#at-cmd");
  await p.keyboard.press("ArrowUp");
  ok(await p.inputValue("#at-cmd") === "AT+CSQ", `up: ${await p.inputValue("#at-cmd")}`);
  await p.keyboard.press("ArrowUp");
  ok(await p.inputValue("#at-cmd") === "at+cgmr", `up again: ${await p.inputValue("#at-cmd")}`);
  await p.keyboard.press("ArrowDown");
  ok(await p.inputValue("#at-cmd") === "AT+CSQ", "down");
  await p.keyboard.press("ArrowDown");
  ok(await p.inputValue("#at-cmd") === "", "down past the newest empties the box");

  // a history button puts it back
  await p.click("#at-hist-list li >> nth=1 >> button");
  ok(await p.inputValue("#at-cmd") === "at+cgmr", "history button");
  await p.fill("#at-cmd", "");

  // a risky command: a dialog, Cancel sends nothing
  const n0 = await p.locator(".at-entry").count();
  await typeSend(p, "AT+COPS=0");
  await p.waitForSelector("#at-op dialog[open]", { timeout: 8000 });
  ok(/\+COPS= 会改变网络运营商注册/.test(await txt(p, "#at-op dialog")) && /AT\+COPS=0/.test(await txt(p, "#at-op dialog")), `dialog: ${(await txt(p, "#at-op dialog")).slice(0, 120)}`);
  ok(/怎么恢复/.test(await txt(p, "#at-op dialog")) && /断开多久/.test(await txt(p, "#at-op dialog")), "what / how long / how to recover");
  await p.screenshot({ path: `${OUT}/at-dialog.png`, fullPage: true });
  await p.click("#at-op dialog .nd-btn--secondary");
  await p.waitForSelector("#at-op dialog", { state: "detached" });
  ok(await p.locator(".at-entry").count() === n0, "Cancel: nothing was sent");
  ok(await p.inputValue("#at-cmd") === "AT+COPS=0", "the command stays in the box");
  // Esc cancels too
  await sendBtn(p).click();
  await p.waitForSelector("#at-op dialog[open]");
  await p.keyboard.press("Escape");
  await p.waitForSelector("#at-op dialog", { state: "detached" });
  ok(await p.locator(".at-entry").count() === n0, "Esc: nothing was sent");
  // confirm sends it
  await sendBtn(p).click();
  await p.waitForSelector("#at-op dialog[open]");
  await p.click("#at-op dialog .nd-btn--danger");
  await p.waitForFunction((n) => document.querySelectorAll(".at-entry").length > n, n0, { timeout: 15000 });
  ok(/AT\+COPS=0/.test(await txt(p, ".at-entry >> nth=2")), "confirmed: sent and logged");
  // what the filter would hide is still judged (AT&F is sent as ATF)
  await typeSend(p, "AT&F");
  await p.waitForSelector("#at-op dialog[open]", { timeout: 8000 });
  ok(/&F 会把调制解调器设置恢复出厂/.test(await txt(p, "#at-op dialog")), "AT&F asks although the agent would strip the &");
  await p.click("#at-op dialog .nd-btn--secondary");
  await p.waitForSelector("#at-op dialog", { state: "detached" });
  // a +CFUN= set is refused outright (owner 10-13: low power needs a restart), no dialog, not sent
  const n1 = await p.locator(".at-entry").count();
  await typeSend(p, "AT+CFUN=4");
  await until(p, "#at-err", /AT\+CFUN/);
  ok(await p.locator("#at-op dialog").count() === 0, "a +CFUN= set is refused without a dialog");
  ok(await p.locator(".at-entry").count() === n1, "a +CFUN= set is not sent");
  // a read form of the same command does not ask
  await typeSend(p, "AT+COPS?");
  await p.waitForFunction(() => document.querySelectorAll(".at-entry").length >= 4, null, { timeout: 15000 });
  ok(await p.locator("#at-op dialog").count() === 0, "a read does not ask");
  await p.screenshot({ path: `${OUT}/at-log.png`, fullPage: true });

  // Check again re-reads the port
  await p.click("#at-port button");
  await until(p, "#at-port", /AT 端口可用/);
  ok(/AT 端口可用/.test(await txt(p, "#at-port")), "Check again");

  // history stays in this tab: a second tab of the same session starts empty
  const p2 = await ctx.newPage();
  await p2.goto(WEB + "/tools/at", { waitUntil: "load" });
  ok(await p2.locator(".at-entry").count() === 0 && await p2.locator("#at-hist-list li").count() === 0, "another tab of the session sees no history");
  await p2.close();
}

const browser = await chromium.launch({ channel: "chrome" });
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"], [390, 844, "phone", "en", "dark"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  await p.goto(WEB + "/tools/at", { waitUntil: "load" });
  await p.waitForSelector("#at-port", { timeout: 15000 });
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
    await own(p, ctx);
  } else {
    await p.waitForFunction(() => /AT port available|AT 端口可用|No AT port/.test(document.querySelector("#at-port")?.textContent ?? ""), null, { timeout: 15000 });
    await p.fill("#at-cmd", "AT+CGMR");
    await sendBtn(p).click();
    await p.waitForSelector(".at-entry", { timeout: 15000 });
  }
  if (lang === "en") {
    ok(/AT Terminal/.test(await txt(p, "h1")) && /AT port available/.test(await txt(p, "#at-port")), `English: ${await txt(p, "h1")} / ${await txt(p, "#at-port")}`);
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
    // the dialog too
    await p.fill("#at-cmd", "AT+CRESET");
    await sendBtn(p).click();
    await p.waitForSelector("#at-op dialog[open]", { timeout: 8000 });
    const dl = (await txt(p, "#at-op dialog")).match(CJK) ?? [];
    ok(dl.length === 0, "English dialog without Chinese");
    const axeD = await new AxeBuilder({ page: p }).analyze();
    ok(axeD.violations.length === 0, `axe ${name} ${lang} ${theme} dialog open: ${axeD.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
    await p.screenshot({ path: `${OUT}/at-${name}-${lang}-${theme}-dialog.png`, fullPage: true });
    await p.click("#at-op dialog .nd-btn--secondary");
    await p.waitForSelector("#at-op dialog", { state: "detached" });
  }
  if (name === "phone") {
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `${lang}: 390 px no sideways scroll (${sw})`);
  }
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
  await p.screenshot({ path: `${OUT}/at-${name}-${lang}-${theme}.png`, fullPage: true });
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
