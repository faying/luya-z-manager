// /router/stk (batch 5c): the common checks (first load ≤ 40 KiB gzip without fonts, axe light/dark 0,
// 390 px no sideways scroll, English body without Chinese, no page errors, screenshots) and the page's
// own: USSD send (bad characters refused, confirm names the code, Cancel sends nothing, the reply with
// its status in words, the reply box while the session is on, reply -> done -> New query, the network
// ending the session, End session, a reply into a session that is gone -> failure never "ok"), STK
// (load behind a confirm, items, a sub-menu with crumbs and Back, a display text with the raw reply).
// Needs the typed mock agent behind the fake agent (AGENT). From source/manager:
//   WEB=http://127.0.0.1:18630 AGENT=http://127.0.0.1:19730 OUT=/tmp/lw-b5-stk crates/luyaz-web/e2e/run.sh stk
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

const U = "#stk-p-ussd", S = "#stk-p-stk";
const confirmBtn = (p, id) => p.locator(`#${id}-op .nd-confirm .nd-btn--confirm`);
async function ask(p, id, click) {
  await click();
  await p.waitForSelector(`#${id}-op .nd-confirm`, { timeout: 8000 });
}
async function ussdSend(p, code) {
  await p.fill("#ussd-code", code);
  await ask(p, "stk-u", () => p.click(`${U} .stk-form .nd-btn--primary`));
}

async function own(p) {
  // refused characters: said at once, nothing to confirm
  await p.fill("#ussd-code", "*1;0#");
  await p.click(`${U} .stk-form .nd-btn--primary`);
  await p.waitForSelector("#ussd-err p", { timeout: 5000 });
  ok(/只能发送数字和 \* # \+/.test(await txt(p, "#ussd-err")), `bad characters: ${await txt(p, "#ussd-err")}`);
  ok(await p.locator("#stk-u-op .nd-confirm").count() === 0, "no confirm for bad characters");

  // confirm names the code; Cancel sends nothing
  await ussdSend(p, "*100#");
  ok(/\*100#/.test(await txt(p, "#stk-u-op .nd-confirm")), `confirm text: ${await txt(p, "#stk-u-op .nd-confirm")}`);
  ok(await p.locator("#ussd-err p").count() === 0, "the error went away with a good code");
  await p.screenshot({ path: `${OUT}/stk-confirm.png`, fullPage: true });
  await p.click("#stk-u-op .nd-confirm .nd-btn--secondary");
  await p.waitForSelector("#stk-u-op .nd-confirm", { state: "detached" });
  ok(/还没有结果/.test(await txt(p, "#ussd-out")), "Cancel: nothing was sent");

  // send: the reply, status in words, session on -> reply box
  await ussdSend(p, "*100#");
  await confirmBtn(p, "stk-u").click();
  await p.waitForSelector("#ussd-reply", { timeout: 15000 });
  ok(/尊敬的客户/.test(await txt(p, "#ussd-out .stk-text")), "carrier text shown");
  ok(/等你回复/.test(await txt(p, "#ussd-out .stk-meta")) && /会话进行中/.test(await txt(p, "#ussd-out .stk-meta")), `status: ${await txt(p, "#ussd-out .stk-meta")}`);
  ok(await p.locator("#stk-u-op .nd-confirm").count() === 0 && await txt(p, "#stk-u-op") === "", "the area is quiet after a good reply");
  await p.screenshot({ path: `${OUT}/stk-session.png`, fullPage: true });

  // reply 1 -> done, New query
  await p.fill("#ussd-reply", "1");
  await ask(p, "stk-u", () => p.click("#ussd-out .nd-btn--primary"));
  ok(/回复 1/.test(await txt(p, "#stk-u-op .nd-confirm")), "reply confirm names the reply");
  await confirmBtn(p, "stk-u").click();
  await until(p, "#ussd-out", /国际漫游流量已使用/);
  ok(/已完成/.test(await txt(p, "#ussd-out .stk-meta")), `done: ${await txt(p, "#ussd-out .stk-meta")}`);
  ok(await p.locator("#ussd-reply").count() === 0, "no reply box once the session is over");
  await p.click("#ussd-out .nd-btn--secondary");
  await until(p, "#ussd-out", /还没有结果/);
  ok(await p.inputValue("#ussd-code") === "", "New query empties the code");

  // the network ends the session (reply 0)
  await ussdSend(p, "*100#");
  await confirmBtn(p, "stk-u").click();
  await p.waitForSelector("#ussd-reply", { timeout: 15000 });
  await p.fill("#ussd-reply", "0");
  await ask(p, "stk-u", () => p.click("#ussd-out .nd-btn--primary"));
  await confirmBtn(p, "stk-u").click();
  await until(p, "#ussd-out", /再见/);
  ok(/运营商已结束会话/.test(await txt(p, "#ussd-out .stk-meta")), `ended by the network: ${await txt(p, "#ussd-out .stk-meta")}`);
  await p.click("#ussd-out .nd-btn--secondary");

  // a reply into a session that is gone: the agent says 200 + ERROR; here it is a failure
  await ussdSend(p, "*100#");
  await confirmBtn(p, "stk-u").click();
  await p.waitForSelector("#ussd-reply", { timeout: 15000 });
  await post("/router/stk/ussd/end"); // behind the page's back: the modem's session is over
  await p.waitForTimeout(600);
  await p.fill("#ussd-reply", "1");
  await ask(p, "stk-u", () => p.click("#ussd-out .nd-btn--primary"));
  await confirmBtn(p, "stk-u").click();
  await p.waitForSelector("#stk-u-op .t-note--bad", { timeout: 15000 });
  ok(/没有生效：调制解调器回了 ERROR/.test(await txt(p, "#stk-u-op")), `ERROR is a failure: ${await txt(p, "#stk-u-op")}`);
  ok(/尊敬的客户/.test(await txt(p, "#ussd-out .stk-text")), "the earlier reply stays, the session can be ended");
  await p.screenshot({ path: `${OUT}/stk-error.png`, fullPage: true });

  // End session (confirm), reply cleared
  await ask(p, "stk-u", () => p.click("#ussd-out .nd-btn--secondary"));
  ok(/结束 USSD 会话/.test(await txt(p, "#stk-u-op .nd-confirm")), "End asks first");
  await confirmBtn(p, "stk-u").click();
  await until(p, "#stk-u-op", /USSD 会话已结束/);
  ok(/USSD 会话已结束/.test(await txt(p, "#stk-u-op")) && /还没有结果/.test(await txt(p, "#ussd-out")), `ended: ${await txt(p, "#stk-u-op")}`);

  // ---- STK ----
  await p.click("#stk-tab-stk");
  await p.waitForSelector(`${S}`, { state: "visible" });
  ok(/打开 SIM 的 STK 菜单/.test(await txt(p, "#stk-view")), "nothing is read on entry: the load hint");
  await ask(p, "stk-m", () => p.click("#stk-view .nd-btn--primary"));
  ok(/SIM 卡要它的菜单/.test(await txt(p, "#stk-m-op .nd-confirm")), "load asks first (tier 2)");
  await confirmBtn(p, "stk-m").click();
  await p.waitForSelector("#stk-view .stk-item", { timeout: 15000 });
  ok(await p.locator("#stk-view .stk-item").count() === 3 && /中国移动/.test(await txt(p, "#stk-view .stk-title")), `root menu: ${await txt(p, "#stk-view")}`);
  ok(await p.evaluate(() => document.querySelector("[id=stk-sig]")?.getAttribute("data-init")?.includes("中国移动")), "the path is in the tab (signal stk)");
  await p.screenshot({ path: `${OUT}/stk-menu.png`, fullPage: true });

  // a sub-menu: crumbs + Back
  await ask(p, "stk-m", () => p.click("#stk-view .stk-item >> nth=0"));
  ok(/国际漫游/.test(await txt(p, "#stk-m-op .nd-confirm")), "item confirm names the entry");
  await confirmBtn(p, "stk-m").click();
  await until(p, "#stk-view .stk-title", /国际漫游/);
  ok(await p.locator("#stk-view .stk-crumbs").count() === 1 && /中国移动.*国际漫游/.test(await txt(p, "#stk-view .stk-crumbs")), `crumbs: ${await txt(p, "#stk-view .stk-crumbs")}`);
  ok(await p.locator("#stk-view .stk-item").count() === 2, "sub-menu items");
  await p.click("#stk-view .nd-btn--secondary");
  await until(p, "#stk-view .stk-title", /中国移动/);
  ok(await p.locator("#stk-view .stk-item").count() === 3 && await p.locator("#stk-view .stk-crumbs").count() === 0, "Back: the root again");

  // a display text, with the raw reply
  await ask(p, "stk-m", () => p.click("#stk-view .stk-item >> nth=1"));
  await confirmBtn(p, "stk-m").click();
  await p.waitForSelector("#stk-disp .stk-text", { timeout: 15000 });
  ok(/您的话费余额为86\.50元/.test(await txt(p, "#stk-disp .stk-text")) && await p.locator("#stk-disp .stk-raw").count() === 1, `display: ${await txt(p, "#stk-disp")}`);
  await p.screenshot({ path: `${OUT}/stk-display.png`, fullPage: true });
  // an answer with no quoted text shows the raw text
  await ask(p, "stk-m", () => p.click("#stk-view .stk-item >> nth=2"));
  await confirmBtn(p, "stk-m").click();
  await until(p, "#stk-disp", /CUSATE: 0OK/);
  ok(/CUSATE: 0OK/.test(await txt(p, "#stk-disp .stk-text")) && await p.locator("#stk-disp .stk-raw").count() === 0, `unquoted: ${await txt(p, "#stk-disp")}`);
  // Reload: asks, comes back to the root and drops the display
  await ask(p, "stk-m", () => p.click("#stk-view button[aria-label]"));
  await confirmBtn(p, "stk-m").click();
  await until(p, "#stk-disp", /^$/);
  ok(await p.locator("#stk-disp .stk-text").count() === 0 && await p.locator("#stk-view .stk-item").count() === 3, "Reload: the root, no old display");
  await p.click("#stk-tab-ussd");
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
  await p.goto(WEB + "/router/stk", { waitUntil: "load" });
  await p.waitForSelector("#ussd-out", { timeout: 15000 });
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
    await own(p);
  } else {
    // drive a reply and a menu so axe sees the rich state
    await p.fill("#ussd-code", "*100#");
    await p.click(`${U} .stk-form .nd-btn--primary`);
    await p.waitForSelector("#stk-u-op .nd-confirm");
    await confirmBtn(p, "stk-u").click();
    await p.waitForSelector("#ussd-reply", { timeout: 15000 });
    await p.click("#stk-tab-stk");
    await p.click("#stk-view .nd-btn--primary");
    await p.waitForSelector("#stk-m-op .nd-confirm");
    await confirmBtn(p, "stk-m").click();
    await p.waitForSelector("#stk-view .stk-item", { timeout: 15000 });
    await p.click("#stk-view .stk-item >> nth=0");
    await p.waitForSelector("#stk-m-op .nd-confirm");
    await confirmBtn(p, "stk-m").click();
    await until(p, "#stk-view .stk-title", /国际漫游/);
    await p.click("#stk-tab-ussd");
  }
  if (lang === "en") {
    ok(/STK/.test(await txt(p, "h1")), `English title: ${await txt(p, "h1")}`);
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
    await p.click("#stk-tab-stk");
    const left2 = (await mainText(p)).match(CJK) ?? [];
    ok(left2.length === 0, `English STK tab without Chinese (${[...new Set(left2)].join("") || "none"})`);
    await p.click("#stk-tab-ussd");
  }
  if (name === "phone") {
    for (const tab of ["ussd", "stk"]) {
      await p.click(`#stk-tab-${tab}`);
      const sw = await p.evaluate(() => document.documentElement.scrollWidth);
      ok(sw <= 390, `${lang} ${tab}: 390 px no sideways scroll (${sw})`);
    }
    await p.click("#stk-tab-ussd");
  }
  for (const tab of ["ussd", "stk"]) {
    await p.click(`#stk-tab-${tab}`);
    const axe = await new AxeBuilder({ page: p }).analyze();
    ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme} ${tab}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
    await p.screenshot({ path: `${OUT}/stk-${name}-${lang}-${theme}-${tab}.png`, fullPage: true });
  }
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
