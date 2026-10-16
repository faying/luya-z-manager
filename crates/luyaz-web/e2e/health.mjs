// /health (batch 5a): the common checks (first load ≤ 40 KiB gzip without fonts, axe light/dark 0,
// 390 px no sideways scroll, English body without Chinese, no page errors, screenshots) and the
// page's own: summary + checked time, the check list (manifest first), the crash-log rows (open /
// close, the text of a log, a log that cannot be read), 「现在检查」 (confirm, Cancel, run, result,
// one at a time), the plain curl /health still JSON, the stale state. Needs the typed mock agent
// behind the fake agent (AGENT). From source/manager:
//   WEB=http://127.0.0.1:18251 AGENT=http://127.0.0.1:19250 OUT=/tmp/lw-ah \
//     crates/luyaz-web/e2e/run.sh health
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18251";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19250";
const OUT = process.env.OUT ?? "/tmp/lw-ah";
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
  // device-supplied text (check names from an older doctor, crash-log text and status) is not ours
  m.querySelectorAll("[data-data], .hl-pre, .hl-item, .hl-crash .nd-row__sub").forEach((e) => e.remove());
  return m.innerText + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, sel, re, ms = 10000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout: ms }).catch(() => {});

async function own(p) {
  ok(/\d+ 项异常，\d+ 项需要注意。|一切正常。/.test(await txt(p, "#hl-status .nd-status__state")), `summary: ${await txt(p, "#hl-status .nd-status__state")}`);
  ok(/\d\d:\d\d 检查/.test(await txt(p, "#hl-status .nd-status__meta")), `checked time: ${await txt(p, "#hl-status .nd-status__meta")}`);
  const n = await p.locator("#hl-checks .nd-row").count();
  ok(n >= 5, `check rows: ${n}`);
  ok(/正常|需要注意|异常/.test(await txt(p, "#hl-checks")), "level words");
  const first = await txt(p, "#hl-checks .nd-row >> nth=0");
  ok(await p.locator("#hl-checks .nd-row").count() > 0, `first row: ${first.slice(0, 40)}`);

  // crash rows: open one, see its text, close it
  const rows = p.locator("#hl-crashes .hl-crash");
  ok(await rows.count() >= 1, `crash rows: ${await rows.count()}`);
  const btn = rows.nth(0).locator("button");
  ok(await btn.getAttribute("aria-expanded") === "false" && /^查看 .+ 的崩溃日志$/.test(await btn.getAttribute("aria-label")), `closed: ${await btn.getAttribute("aria-label")}`);
  ok(!(await rows.nth(0).locator(".hl-panel").isVisible()), "panel hidden at first");
  await btn.click();
  await rows.nth(0).locator(".hl-pre").waitFor({ timeout: 8000 }).catch(() => {});
  ok(await btn.getAttribute("aria-expanded") === "true" && /^收起 /.test(await btn.getAttribute("aria-label")), "open: aria-expanded true, label says Hide");
  ok(/^program: /.test(await txt(p, ".hl-crash >> nth=0 >> .hl-pre")), `log text: ${(await txt(p, ".hl-crash >> nth=0 >> .hl-pre")).slice(0, 50)}`);
  ok(/收起/.test(await btn.innerText()), "row says 收起");
  await p.screenshot({ path: `${OUT}/health-open.png`, fullPage: true });
  // a push of the list keeps it open (kick = a re-read now)
  await post("/health/refresh");
  await p.waitForTimeout(1200);
  ok(await btn.getAttribute("aria-expanded") === "true" && await rows.nth(0).locator(".hl-pre").count() === 1, "open row survives a re-read");
  await btn.click();
  ok(await btn.getAttribute("aria-expanded") === "false" && !(await rows.nth(0).locator(".hl-panel").isVisible()), "closed again");
  // a name the agent refuses: said, not a crash
  const bad = await (await post("/health/log?program=nope&file=x.log")).text();
  ok(bad.includes("读不到这份日志。"), "a missing log says it could not be loaded");

  // 现在检查: Cancel leaves it alone; confirm runs it
  const before = await txt(p, "#hl-status .nd-status__meta");
  await p.click("#hl-status .nd-status__actions button");
  await p.waitForSelector("#hl-chk-op .nd-confirm");
  ok(/doctor\.sh/.test(await txt(p, "#hl-chk-op .nd-confirm__text")) && /20 秒/.test(await txt(p, "#hl-chk-op .nd-confirm__text")), `confirm: ${await txt(p, "#hl-chk-op .nd-confirm__text")}`);
  await p.screenshot({ path: `${OUT}/health-confirm.png`, fullPage: true });
  await p.click("#hl-chk-op .nd-btn--secondary");
  await p.waitForFunction(() => !document.querySelector("#hl-chk-op .nd-confirm"), null, { timeout: 5000 });
  ok(before === await txt(p, "#hl-status .nd-status__meta"), "Cancel: nothing ran");
  await p.click("#hl-status .nd-status__actions button");
  await p.waitForSelector("#hl-chk-op .nd-confirm");
  await p.click("#hl-chk-op .nd-btn--confirm");
  await until(p, "#hl-chk-op", /正在运行检查/, 3000);
  ok(/正在运行检查/.test(await txt(p, "#hl-chk-op")), "while it runs: 正在运行检查");
  // a second press says one is running
  await p.click("#hl-status .nd-status__actions button");
  await until(p, "#hl-chk-op", /已经有一次检查在运行/, 3000);
  ok(/已经有一次检查在运行/.test(await txt(p, "#hl-chk-op")), `second press: ${await txt(p, "#hl-chk-op")}`);
  await until(p, "#hl-chk-op", /检查完成/, 15000);
  ok(/\d\d:\d\d 检查完成/.test(await txt(p, "#hl-chk-op")), `result: ${await txt(p, "#hl-chk-op")}`);
  await p.screenshot({ path: `${OUT}/health-done.png`, fullPage: true });

  // curl keeps luyaz-web's own JSON
  const j = await fetch(`${WEB}/health`);
  ok((j.headers.get("content-type") ?? "").includes("json"), `plain /health: ${j.headers.get("content-type")}`);
  const h = await fetch(`${WEB}/health`, { headers: { accept: "text/html", cookie: `lw_sid=${sid}` } });
  ok((h.headers.get("content-type") ?? "").includes("text/html"), "browser /health: HTML");
  // the old pages link with a trailing slash: /health/ → /health, the new page
  const sl = await fetch(`${WEB}/health/`, { redirect: "manual", headers: { accept: "text/html", cookie: `lw_sid=${sid}` } });
  ok(sl.status === 308 && sl.headers.get("location") === "/health", `browser /health/ → ${sl.status} ${sl.headers.get("location")}`);

  // agent down: the list stays, greyed and said; Retry later
  await scenario("down");
  await post("/health/refresh");
  await until(p, "#hl-status", /数字停在/, 15000);
  ok(/数字停在 \d\d:\d\d/.test(await txt(p, "#hl-status")) && await p.locator("#hl-checks.nd-stale").count() === 1, `stale: ${await txt(p, "#hl-status .nd-status__meta")}`);
  await p.screenshot({ path: `${OUT}/health-stale.png`, fullPage: true });
  // a check while the agent is down: said, not "成功"
  await p.click("#hl-status .nd-status__actions button");
  await p.waitForSelector("#hl-chk-op .nd-confirm");
  await p.click("#hl-chk-op .nd-btn--confirm");
  await until(p, "#hl-chk-op", /没有生效|连接中断/, 40000);
  ok(/没有生效|连接中断/.test(await txt(p, "#hl-chk-op")) && !/成功/.test(await txt(p, "#hl-chk-op")), `check with the agent down: ${await txt(p, "#hl-chk-op")}`);
  await scenario("normal");
  await post("/health/refresh");
  await p.waitForFunction(() => !/数字停在/.test(document.querySelector("#hl-status")?.textContent ?? ""), null, { timeout: 10000 }).catch(() => {});
  ok(!/数字停在/.test(await txt(p, "#hl-status")), "back to live");
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
  await p.goto(WEB + "/health", { waitUntil: "load" });
  await p.waitForSelector("#hl-checks .nd-row", { timeout: 15000 });
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
  }
  if (lang === "en") {
    ok(/Health/.test(await txt(p, "h1")) && /Check now/.test(await txt(p, "#hl-status")), "English status");
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
  }
  if (name === "phone") {
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `${lang}: 390 px no sideways scroll (${sw})`);
  }
  // open a log before axe: the console band and its pre must pass too
  const open = p.locator("#hl-crashes .hl-crash button").first();
  if (await open.count()) { await open.click(); await p.locator(".hl-pre").first().waitFor({ timeout: 8000 }).catch(() => {}); }
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme} (a log open): ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
  if (name === "phone") {
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `${lang}: 390 px no sideways scroll with a log open (${sw})`);
  }
  await p.screenshot({ path: `${OUT}/health-${name}-${lang}-${theme}.png`, fullPage: true });
  if (await open.count()) await open.click();
  if (name === "desk" && lang === "zh") await own(p);
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
