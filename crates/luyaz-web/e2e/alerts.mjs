// /alerts (batch 5a): the common checks (first load ≤ 40 KiB gzip without fonts, axe light/dark 0,
// 390 px no sideways scroll, English body without Chinese, no page errors, screenshots) and the
// page's own: the list, the SMS part, each write once (number save / off, abroad switch, mark all
// as read) with its confirm and readback, a refused write, the stale state. Needs the typed mock
// agent behind the fake agent (AGENT) and the mock u60d (MOCK), told to say "not mine" for
// GET /v2/api/alerts (/mock/alerts-to-agent; its own alerts have no SMS part). The mock agent keeps
// its state: restart the fake agent for a fresh run. From source/manager:
//   WEB=http://127.0.0.1:18251 MOCK=http://127.0.0.1:19451 AGENT=http://127.0.0.1:19250 OUT=/tmp/lw-ah \
//     crates/luyaz-web/e2e/run.sh alerts
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18251";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19250";
const OUT = process.env.OUT ?? "/tmp/lw-ah";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19451";
{
  const res = await fetch(`${MOCK}/mock/alerts-to-agent?on=true`, { method: "POST" });
  if (!res.ok) throw new Error(`mock alerts-to-agent: ${res.status}`);
}
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
const scenario = (set) => fetch(`${AGENT}/__mock/scenario?set=${set}`);

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
const mainText = (p) => p.evaluate(() => {
  const m = document.querySelector("main").cloneNode(true);
  m.querySelectorAll("[data-data]").forEach((e) => e.remove());
  return m.innerText + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, sel, re, ms = 10000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout: ms }).catch(() => {});

async function own(p, ctx) {
  // the list and the SMS part
  ok(/1 条新告警/.test(await txt(p, "#al-status")) && /触屏界面闪退/.test(await txt(p, "#al-status .nd-status__reason")), `status: ${await txt(p, "#al-status .nd-status__state")}`);
  ok(await p.locator("#al-events li").count() === 4, "four events");
  ok(await p.locator("#al-events li .al-unread").count() === 1, "one unread mark (▲)");
  ok(/开机后 \d+ 分钟/.test(await txt(p, "#al-events")) && /\d\d-\d\d \d\d:\d\d/.test(await txt(p, "#al-events")), "times: after boot / MM-DD HH:mm");
  ok(/已开启：用本机 SIM 发到 \+8612300000000/.test(await txt(p, "#al-smsstate")), `sms state: ${await txt(p, "#al-smsstate")}`);
  ok(await p.inputValue("#al-n") === "+8612300000000", "number field starts at the saved number");
  ok(await p.locator("#al-numform button[type=submit]").isDisabled(), "Save disabled while unchanged");
  ok(await p.getAttribute("#al-abroad button", "aria-checked") === "false" && /国外情景/.test(await txt(p, "#al-abroad")), "abroad off + 'abroad now' note");
  ok(await p.locator("#al-recent li").count() === 4 && /24 小时内已发 \d+ 条/.test(await txt(p, "#al-recent")), `recent SMS list + sent in 24 h (${await txt(p, "#al-recent-t")})`);
  ok(/已发送/.test(await txt(p, "#al-recent")) && /在国外，未发/.test(await txt(p, "#al-recent")) && /超出限额/.test(await txt(p, "#al-recent")), "SMS results worded");
  ok(/管理网页意外退出/.test(await txt(p, "#al-recent")), "SMS rows carry the kind label");

  // number: a bad draft is said and cannot be saved
  await p.fill("#al-n", "12");
  ok(await p.locator("#al-n-hint").isVisible() && await p.locator("#al-numform button[type=submit]").isDisabled(), "bad draft: hint shown, Save disabled");
  await p.screenshot({ path: `${OUT}/alerts-baddraft.png`, fullPage: true });
  // a good draft: confirm first, Cancel leaves it alone
  await p.fill("#al-n", " +886 912-345-678 ");
  ok(await p.locator("#al-n-hint").isHidden() && await p.locator("#al-numform button[type=submit]").isEnabled(), "good draft: Save enabled");
  await p.click("#al-numform button[type=submit]");
  await p.waitForSelector("#al-num-op .nd-confirm", { timeout: 5000 });
  ok(/\+886912345678/.test(await txt(p, "#al-num-op .nd-confirm__text")), `confirm: ${await txt(p, "#al-num-op .nd-confirm__text")}`);
  await p.screenshot({ path: `${OUT}/alerts-confirm.png`, fullPage: true });
  await p.click("#al-num-op .nd-btn--secondary");
  await p.waitForFunction(() => !document.querySelector("#al-num-op .nd-confirm"), null, { timeout: 5000 });
  ok(/\+8612300000000/.test(await txt(p, "#al-smsstate")), "Cancel: nothing changed");
  // save
  await p.click("#al-numform button[type=submit]");
  await p.waitForSelector("#al-num-op .nd-confirm");
  await p.click("#al-num-op .nd-btn--confirm");
  await until(p, "#al-num-op", /号码已保存/);
  ok(/号码已保存/.test(await txt(p, "#al-num-op")), `saved: ${await txt(p, "#al-num-op")}`);
  await until(p, "#al-smsstate", /\+886912345678/);
  ok(/发到 \+886912345678/.test(await txt(p, "#al-smsstate")), "the state line follows the readback");
  ok(await p.inputValue("#al-n") === " +886 912-345-678 " || /\+886/.test(await p.inputValue("#al-n")), "the field keeps what was typed");
  // turn off: empty + save
  await p.fill("#al-n", "");
  await p.click("#al-numform button[type=submit]");
  await p.waitForSelector("#al-num-op .nd-confirm");
  ok(/不再发告警短信/.test(await txt(p, "#al-num-op .nd-confirm__text")), "off confirm says what stops");
  await p.click("#al-num-op .nd-btn--confirm");
  await until(p, "#al-num-op", /短信告警已关闭/);
  ok(/短信告警已关闭/.test(await txt(p, "#al-num-op")), `off: ${await txt(p, "#al-num-op")}`);
  await until(p, "#al-smsstate", /未配置/);
  ok(/未配置/.test(await txt(p, "#al-smsstate")), "state: not set up");
  // the server checks again: a bad number sent anyway is refused with the reason
  const bad = await post("/alerts/number/set?n=12");
  ok((await bad.text()).includes("只能是数字"), "server refuses a bad number");

  // abroad switch
  await p.click("#al-abroad button");
  await p.waitForSelector("#al-abroad-op .nd-confirm");
  ok(/漫游费/.test(await txt(p, "#al-abroad-op .nd-confirm__text")), "abroad confirm warns about roaming fees");
  await p.click("#al-abroad-op .nd-btn--confirm");
  await until(p, "#al-abroad-op", /在国外也会发送/);
  ok(/在国外也会发送/.test(await txt(p, "#al-abroad-op")), `abroad on: ${await txt(p, "#al-abroad-op")}`);
  await p.waitForFunction(() => document.querySelector("#al-abroad button")?.getAttribute("aria-checked") === "true", null, { timeout: 8000 }).catch(() => {});
  ok(await p.getAttribute("#al-abroad button", "aria-checked") === "true" && !/国外情景/.test(await txt(p, "#al-abroad")), "switch on, the 'abroad now' note gone");
  await p.click("#al-abroad button");
  await p.waitForSelector("#al-abroad-op .nd-confirm");
  await p.click("#al-abroad-op .nd-btn--confirm");
  await until(p, "#al-abroad-op", /在国外不发送/);
  ok(/在国外不发送/.test(await txt(p, "#al-abroad-op")), "abroad off again");

  // mark all as read (tier 1: no confirm)
  ok(await p.locator("#al-status button").count() === 1, "Mark all as read is offered");
  await p.click("#al-status button");
  await until(p, "#al-mark-op", /已全部标为已读/);
  ok(/已全部标为已读/.test(await txt(p, "#al-mark-op")), `mark: ${await txt(p, "#al-mark-op")}`);
  await until(p, "#al-status", /没有新告警/);
  ok(/没有新告警/.test(await txt(p, "#al-status")) && await p.locator("#al-status button").count() === 0 && await p.locator("#al-events li .al-unread").count() === 0, "status: no new alerts, button gone, marks gone");
  const pub = await (await fetch(`${AGENT}/api/public/status`)).json();
  ok(pub?.data?.alerts?.unread === 0 || pub?.alerts?.unread === 0, "public status unread = 0 (the top bar's source)");
  await p.screenshot({ path: `${OUT}/alerts-after.png`, fullPage: true });

  // agent down: the list stays, greyed and said; nothing can be changed; back after Retry
  await scenario("down");
  await post("/alerts/refresh");
  await until(p, "#al-status", /列表停在/, 15000);
  ok(/列表停在 \d\d:\d\d/.test(await txt(p, "#al-status")) && await p.locator("#al-events.nd-stale, #al-events .nd-stale").count() >= 1, `stale: ${await txt(p, "#al-status .nd-status__meta")}`);
  ok(/刷新失败/.test(await txt(p, "#al-events")), "stale: refresh failure said in the list");
  ok(await p.locator("#al-numform button[type=submit]").isDisabled() && await p.locator("#al-abroad button").isDisabled(), "stale: save and switch disabled");
  await p.screenshot({ path: `${OUT}/alerts-stale.png`, fullPage: true });
  await scenario("normal");
  await p.click("#al-events .al-note button");
  await p.waitForFunction(() => !/列表停在/.test(document.querySelector("#al-status")?.textContent ?? ""), null, { timeout: 10000 }).catch(() => {});
  ok(!/列表停在/.test(await txt(p, "#al-status")) && await p.locator("#al-abroad button").isEnabled(), `back to live after Retry (${await txt(p, "#al-status")})`);
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
  await p.goto(WEB + "/alerts", { waitUntil: "load" });
  await p.waitForSelector("#al-events li", { timeout: 15000 });
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
  }
  if (lang === "en") {
    ok(/Alerts/.test(await txt(p, "h1")) && /new alert/.test(await txt(p, "#al-status")), "English status");
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
  await p.screenshot({ path: `${OUT}/alerts-${name}-${lang}-${theme}.png`, fullPage: true });
  if (name === "desk" && lang === "zh") await own(p, ctx);
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await browser.close();
await fetch(`${MOCK}/mock/alerts-to-agent?on=false`, { method: "POST" });
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
