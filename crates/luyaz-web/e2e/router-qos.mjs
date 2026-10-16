// /router/qos (batch 4, read-only): the common checks (first load ≤ 40 KiB gzip without fonts, axe
// light/dark 0, 390 px no sideways scroll, English body without Chinese, no page errors,
// screenshots) and the page's own: the mode and xdpi from u60d's `router.qos`, live pushes, an
// unread part shown as "—" (not "off"), the stale state. No writes on this page. Needs MOCK (the
// /mock/qos hook). From source/manager, servers as in batch2.mjs:
//   WEB=http://127.0.0.1:18211 MOCK=http://127.0.0.1:19411 OUT=/tmp/lw-qos \
//     crates/luyaz-web/e2e/run.sh router-qos
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18211";
const OUT = process.env.OUT ?? "/tmp/lw-qos";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19411";
const mock = (path) => fetch(MOCK + path, { method: "POST" }).then((r) => { if (!r.ok) throw new Error(`${path}: ${r.status}`); });
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
const until = (p, sel, re) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout: 10000 }).catch(() => {});

async function own(p) {
  ok(/带宽分配：极速（不区分应用）/.test(await txt(p, "#qs-status")), `status: ${await txt(p, "#qs-status .nd-status__state")}`);
  ok(await txt(p, "#qs-mode .nd-row__value") === "极速（不区分应用）", "mode row");
  ok(await txt(p, "#qs-xdpi .nd-row__value") === "已关", "xdpi row: 已关");
  ok(await p.locator("main button").count() === 0, "read-only: no buttons");
  // live: a push changes the page without reload
  await mock("/mock/qos?mode=Game_mode&xdpi=on");
  await until(p, "#qs-mode .nd-row__value", /游戏/);
  ok(await txt(p, "#qs-mode .nd-row__value") === "游戏" && await txt(p, "#qs-xdpi .nd-row__value") === "已开", "push: 游戏, xdpi 已开");
  ok(/现在是开着的/.test(await txt(p, "#qs-status .nd-status__reason")), "xdpi on: the reason says so");
  // unread parts: "—", not "off"
  await mock("/mock/qos?mode=&xdpi=null");
  await until(p, "#qs-status", /读不到带宽分配模式/);
  ok(await txt(p, "#qs-mode .nd-row__value") === "—" && await txt(p, "#qs-xdpi .nd-row__value") === "—", "unread → — —");
  await mock("/mock/qos?gone=true");
  await until(p, "#qs-status", /一分钟内/);
  ok(/读不到带宽分配模式/.test(await txt(p, "#qs-status")) && await txt(p, "#qs-xdpi .nd-row__value") === "—", "vendor read failed (qos null): warn, —");
  // stale
  await mock("/mock/qos?mode=Nomal_mode&xdpi=off");
  await mock("/mock/qos?stale=true");
  await until(p, "#qs-status", /没有在更新/);
  ok(await p.locator("#qs-status .nd-status--stale").count() === 1 && await p.locator("#qs-rows.nd-stale").count() === 1, "stale: greyed + said");
  await p.screenshot({ path: `${OUT}/qos-stale.png`, fullPage: true });
  await mock("/mock/qos?stale=false");
  await until(p, "#qs-status", /极速/);
  ok(await p.locator(".nd-stale").count() === 0, "back to live");
}

await mock("/mock/qos?mode=Nomal_mode&xdpi=off");
await mock("/mock/qos?stale=false");
const browser = await chromium.launch({ channel: "chrome" });
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"], [390, 844, "phone", "en", "dark"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  await p.goto(WEB + "/router/qos", { waitUntil: "load" });
  await until(p, "#qs-mode .nd-row__value", /\S/);
  ok(await p.locator("#qs-mode .nd-skel").count() === 0, `${name} ${lang}: mode shown`);
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
  }
  if (lang === "en") {
    ok(await txt(p, "#qs-mode .nd-row__value") === "Fastest (no per-app sorting)", "English mode name");
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
  await p.screenshot({ path: `${OUT}/qos-${name}-${lang}-${theme}.png`, fullPage: true });
  if (name === "desk" && lang === "zh") await own(p);
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
