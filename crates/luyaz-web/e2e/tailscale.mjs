// /services/tailscale (batch 5b): the common checks (first load <= 40 KiB gzip without fonts, axe light/dark 0,
// 390 px no sideways scroll, English body without Chinese, no page errors, screenshots) and the page's own:
// status, this node, peers, mesh, the daemon log (polled, Pause stops the polling, Refresh), the
// agent-down / error-reply states (last data stays, greyed). Read-only: no write path. Needs the typed
// mock agent behind the fake agent (AGENT). From source/manager:
//   WEB=http://127.0.0.1:18592 AGENT=http://127.0.0.1:19592 OUT=/tmp/lw-ts crates/luyaz-web/e2e/run.sh tailscale
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18592";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19592";
const OUT = process.env.OUT ?? "/tmp/lw-ts";
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
  m.querySelectorAll("#ts-log, #ts-peers, #ts-node .nd-row__value, #ts-status .nd-status__meta, #ts-mesh .nd-row__label").forEach((e) => e.remove());
  return m.innerText + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, sel, re, ms = 10000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout: ms }).catch(() => {});


async function own(p) {
  const st = async () => txt(p, "#ts-status");
  ok(/运行中/.test(await st()) && /共 \d+ 个 · \d+ 在线/.test(await st()), `status: ${await st()}`);
  ok(/\.ts\.net\./.test(await txt(p, "#ts-status .nd-status__meta")), "tailnet name in the status meta");
  const node = await txt(p, "#ts-node");
  ok(/主机名\s*u60-pro/.test(node) && /IP \(v4\)\s*100\.101\.12\.34/.test(node) && /IP \(v6\)\s*fd7a:/.test(node) && /DERP 中继/.test(node) && /版本\s*1\.88/.test(node) && /子网路由\s*192\.168\.0\.0\/24/.test(node) && /密钥到期/.test(node) && /可作出口节点/.test(node), `node: ${node.slice(0, 200)}`);
  const peers = p.locator("#ts-peers .ts-peer");
  ok(await peers.count() >= 3, `peer rows: ${await peers.count()}`);
  const first = await txt(p, "#ts-peers .ts-peer >> nth=0");
  ok(/home-nas/.test(first) && /100\.88\.14\.2/.test(first) && /↑.*↓/.test(first) && /在线/.test(first), `first peer: ${first}`);
  ok(/离线/.test(await txt(p, "#ts-peers")), "an offline peer");
  ok(/节点\s*\d+/.test(await txt(p, "#ts-mesh")) && /未选择出口节点/.test(await txt(p, "#ts-mesh")) && /正在通信/.test(await txt(p, "#ts-mesh")), `mesh: ${await txt(p, "#ts-mesh")}`);
  // help popover
  await p.click("#ts-node .nd-help >> nth=0");
  ok(await p.locator("#h-ts-state").isVisible(), "help popover opens");
  await p.keyboard.press("Escape");

  // the log: lines shown; polled every 4 s (204 while unchanged); Pause stops it; Refresh asks
  const logs = [];
  p.on("request", (r) => { if (r.url().includes("/services/tailscale/log")) logs.push(r.url()); });
  ok(/logtail started/.test(await txt(p, "#ts-log")) && (await p.locator("#ts-log").getAttribute("data-h")).length > 0, "log lines + hash");
  await p.waitForTimeout(9000);
  ok(logs.length >= 1 && logs.every((u) => /[?&]h=[0-9a-f]+/.test(u)), `polled ${logs.length}x with the hash of what it shows`);
  const pause = p.locator(".ts-logbtn").first();
  ok(await pause.getAttribute("aria-pressed") === "false" && /暂停/.test(await pause.innerText()), "Pause button, not pressed");
  await pause.click();
  ok(await pause.getAttribute("aria-pressed") === "true" && /继续/.test(await pause.innerText()), "paused: pressed, says 继续");
  logs.length = 0;
  await p.waitForTimeout(9000);
  ok(logs.length === 0, `paused: no log requests (${logs.length})`);
  await p.click("button[aria-label='刷新日志']");
  await p.waitForTimeout(800);
  ok(logs.length === 1 && /force=1/.test(logs[0]), "Refresh asks once, forced");
  await pause.click();
  ok(await pause.getAttribute("aria-pressed") === "false", "resumed");
  await p.screenshot({ path: `${OUT}/ts-desk-zh.png`, fullPage: true });
  // the header refresh re-reads
  await p.click("button[aria-label='刷新']");

  // the error reply: the last data stays, greyed, with the reason
  await scenario("fakesuccess");
  await post("/services/tailscale/refresh");
  await until(p, "#ts-status", /Tailscale：/, 15000);
  ok(/Tailscale：failed to connect/.test(await st()) && /数字停在/.test(await st()) && await p.locator("#ts-node.nd-stale, #ts-node .nd-stale").count() >= 1, `error reply: ${await st()}`);
  ok(await p.locator("#ts-status button").count() === 1, "Retry button");
  await p.screenshot({ path: `${OUT}/ts-error.png`, fullPage: true });
  await scenario("normal");
  await p.click("#ts-status button");
  await p.waitForFunction(() => !/数字停在/.test(document.querySelector("#ts-status")?.textContent ?? ""), null, { timeout: 12000 }).catch(() => {});
  ok(/运行中/.test(await st()) && !/数字停在/.test(await st()), "Retry: back to live");

  // agent down
  await scenario("down");
  await post("/services/tailscale/refresh");
  await until(p, "#ts-status", /数字停在/, 15000);
  ok(/数字停在 \d\d:\d\d/.test(await st()) && await p.locator("#ts-node .nd-stale").count() === 1, `agent down: ${await st()}`);
  await scenario("normal");
  await post("/services/tailscale/refresh");
  await p.waitForFunction(() => !/数字停在/.test(document.querySelector("#ts-status")?.textContent ?? ""), null, { timeout: 12000 }).catch(() => {});

  // key about to expire
  await scenario("ts-keysoon");
  await post("/services/tailscale/refresh");
  await until(p, "#ts-status", /密钥 \d+ 天后过期/, 12000);
  ok(/密钥 \d+ 天后过期/.test(await st()) && p.locator("#ts-status .nd-status--warn").count(), `key soon: ${await st()}`);
  await p.screenshot({ path: `${OUT}/ts-keysoon.png`, fullPage: true });
  await scenario("normal");
  await post("/services/tailscale/refresh");

  // no scenario hides the old URL form
  const sl = await fetch(`${WEB}/services/tailscale/`, { redirect: "manual", headers: { accept: "text/html", cookie: `lw_sid=${sid}` } });
  ok(sl.status === 308 && sl.headers.get("location") === "/services/tailscale", `/services/tailscale/ → ${sl.status} ${sl.headers.get("location")}`);
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
  await p.goto(WEB + "/services/tailscale", { waitUntil: "load" });
  await p.waitForSelector("#ts-peers .ts-peer", { timeout: 15000 });
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (<= 40)`);
    ok(await p.evaluate(() => { const a = document.querySelector("#ts-node").getBoundingClientRect(), b = document.querySelector("#ts-mesh").getBoundingClientRect(); return b.left > a.right - 1; }), "desktop: mesh column beside the node column");
  }
  if (lang === "en") {
    ok(/Tailscale/.test(await txt(p, "h1")) && /Running/.test(await txt(p, "#ts-status")), "English status");
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
  await p.screenshot({ path: `${OUT}/ts-${name}-${lang}-${theme}.png`, fullPage: true });
  if (name === "desk" && lang === "zh") await own(p);
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
