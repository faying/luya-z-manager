// /router/telemetry (batch 4): the domain filter. Common checks (first load ≤ 40 KiB gzip without
// fonts, axe light/dark 0, 390 px no sideways scroll, English body without Chinese, no page errors,
// screenshots) and the page's own: add (read back), validation, the panel's own host refused before
// sending, remove, quick block with a failed step then "send the rest", no readback → 「设备已接受」,
// unreadable list locks changes, tier 3 dialog over Tailscale. Needs MOCK (the mock u60d's
// /mock/filter, /mock/control, /mock/stats). Run from source/manager:
//   WEB=http://127.0.0.1:18213 MOCK=http://127.0.0.1:19413 OUT=/tmp/b4f crates/luyaz-web/e2e/run.sh router-filter
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18213";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19413";
const OUT = process.env.OUT ?? "/tmp/b4f";
const PATH = "/router/telemetry";
const port = new URL(WEB).port;
const mock = (path) => fetch(MOCK + path, { method: "POST" });
const ops = async () => (await fetch(`${MOCK}/mock/stats`).then((r) => r.json())).page_ops ?? [];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
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
const ready = (p) => p.waitForFunction(() => document.querySelectorAll("#tf-list .tf-d").length > 0 || /还没有|No domains/.test(document.querySelector("#tf-list")?.textContent ?? ""), null, { timeout: 15000 });
const note = (p, id, re) => p.waitForFunction(([id, re]) => new RegExp(re).test(document.querySelector(`#${id}-op .t-note`)?.textContent ?? ""), [id, typeof re === "string" ? re : re.source], { timeout: 20000 });
const rows = (p) => p.$$eval("#tf-list .tf-d", (e) => e.map((x) => x.textContent.trim()));

async function add(p, d) {
  await p.fill("#tf-dom", d);
  await p.click("#tf-addbtn");
}

async function own(p, ctx) {
  ok(/已知遥测域名拦截了 1 \/ 4 个/.test(await txt(p, "#tf-status")), `status: ${await txt(p, "#tf-status")}`);
  ok(JSON.stringify(await rows(p)) === JSON.stringify(["iot.zte.com.cn", "ads.example.com"]), `rows ${await rows(p)}`);
  ok(/已知 ZTE 遥测/.test(await txt(p, "#tf-list .nd-row:first-child")), "known tag on the known row");
  ok(await p.getAttribute("#tf-list button[aria-label='移除 ads.example.com']", "aria-label") !== null, "remove button named with the domain");

  // validation: nothing sent
  let n = (await ops()).length;
  await add(p, "nope");
  await note(p, "tf-add", "不像域名").catch(() => {});
  ok(/这不像域名/.test(await txt(p, "#tf-add-op")), `bad domain: ${await txt(p, "#tf-add-op")}`);
  await add(p, "ADS.example.com");
  await note(p, "tf-add", "已经在").catch(() => {});
  ok(/ads\.example\.com 已经在拦截列表里/.test(await txt(p, "#tf-add-op")), `already: ${await txt(p, "#tf-add-op")}`);
  ok((await ops()).length === n, "nothing sent for invalid input");

  // add, tier 2 inline confirm, read back, input cleared
  await add(p, " Tracker.Example.NET ");
  await p.waitForSelector("#tf-add-confirm", { timeout: 10000 });
  ok(/U60 后面的设备将无法访问 tracker\.example\.net/.test(await txt(p, "#tf-add-confirm")), `inline confirm: ${await txt(p, "#tf-add-confirm")}`);
  const ax = await new AxeBuilder({ page: p }).analyze();
  ok(ax.violations.length === 0, `inline confirm axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
  await p.click("#tf-add-confirm .nd-btn--confirm");
  await note(p, "tf-add", "已拦截").catch(() => {});
  ok(/已拦截 tracker\.example\.net/.test(await txt(p, "#tf-add-op")), `add result: ${await txt(p, "#tf-add-op")}`);
  ok((await rows(p)).includes("tracker.example.net"), "row pushed");
  ok((await p.inputValue("#tf-dom")) === "", "input cleared");
  let o = (await ops()).slice(n);
  ok(o.length === 1 && o[0].action === "vendor.call" && o[0].params.object === "zwrt_router.api" && o[0].params.method === "router_set_domain_filter" && JSON.stringify(o[0].params.args) === JSON.stringify({ action: "add", domain: "tracker.example.net" }), `one add sent: ${JSON.stringify(o.map((x) => x.params))}`);

  // remove
  n = (await ops()).length;
  await p.click("#tf-list button[aria-label='移除 ads.example.com']");
  await p.waitForSelector("#tf-del-confirm", { timeout: 10000 });
  ok(/又能访问 ads\.example\.com/.test(await txt(p, "#tf-del-confirm")), "remove confirm says the consequence");
  await p.click("#tf-del-confirm .nd-btn--confirm");
  await note(p, "tf-del", "已解除").catch(() => {});
  ok(/已解除拦截 ads\.example\.com/.test(await txt(p, "#tf-del-op")), `remove result: ${await txt(p, "#tf-del-op")}`);
  ok(!(await rows(p)).includes("ads.example.com"), "row gone");
  o = (await ops()).slice(n);
  ok(o.length === 1 && o[0].params.args.action === "delete" && o[0].params.args.domain === "ads.example.com", "one delete sent");

  // no readback → 「设备已接受」
  n = (await ops()).length;
  await mock("/mock/control?noreadback=true");
  await add(p, "quiet.example.org");
  await p.waitForSelector("#tf-add-confirm", { timeout: 10000 });
  await p.click("#tf-add-confirm .nd-btn--confirm");
  await note(p, "tf-add", "设备已接受").catch(() => {});
  ok(/设备已接受，还没读回/.test(await txt(p, "#tf-add-op")), `no readback: ${await txt(p, "#tf-add-op")}`);
  ok((await ops()).length === n + 1, "sent once");

  // quick block: one step fails, the result names it, pressing again sends only the rest
  await mock("/mock/filter?list=&fail=zte.com.cn");
  await p.waitForFunction(() => /0 \/ 4/.test(document.querySelector("#tf-status")?.textContent ?? ""), null, { timeout: 10000 });
  n = (await ops()).length;
  await p.click("#tf-quickbtn button");
  await p.waitForSelector("#tf-quick-confirm", { timeout: 10000 });
  ok(/iot\.zte\.com\.cn、ztems\.com、zte\.com\.cn、ztemt\.com\.cn/.test(await txt(p, "#tf-quick-confirm")), `quick confirm lists all 4: ${await txt(p, "#tf-quick-confirm")}`);
  await p.click("#tf-quick-confirm .nd-btn--confirm");
  await note(p, "tf-quick", "没拦截").catch(() => {});
  const qr = await txt(p, "#tf-quick-op");
  ok(/已拦截：iot\.zte\.com\.cn、ztems\.com、ztemt\.com\.cn。/.test(qr) && /没拦截：zte\.com\.cn（zwrt_router\.api router_set_domain_filter: timeout）/.test(qr), `partial: ${qr}`);
  ok(await p.getAttribute("#tf-quick-op .t-note", "role") === "alert", "partial result is an alert");
  o = (await ops()).slice(n);
  ok(o.length === 4 && new Set(o.map((x) => x.op_id)).size === 4, `4 steps, each once with its own op_id (${o.length})`);
  await mock("/mock/filter?list=iot.zte.com.cn,ztems.com,ztemt.com.cn&fail=");
  await p.waitForFunction(() => /3 \/ 4/.test(document.querySelector("#tf-status")?.textContent ?? ""), null, { timeout: 10000 });
  n = (await ops()).length;
  await p.click("#tf-quickbtn button");
  await p.waitForSelector("#tf-quick-confirm", { timeout: 10000 });
  ok(/确认：拦截 1 个遥测域名/.test(await txt(p, "#tf-quick-confirm")), `second press: only the rest (${await txt(p, "#tf-quick-confirm button.nd-btn--confirm")})`);
  await p.click("#tf-quick-confirm .nd-btn--confirm");
  await note(p, "tf-quick", "已拦截 1").catch(() => {});
  ok(/已拦截 1 个遥测域名/.test(await txt(p, "#tf-quick-op")), `rest: ${await txt(p, "#tf-quick-op")}`);
  o = (await ops()).slice(n);
  ok(o.length === 1 && o[0].params.args.domain === "zte.com.cn", "only zte.com.cn sent");
  await p.waitForFunction(() => /ZTE 遥测已拦截/.test(document.querySelector("#tf-status")?.textContent ?? ""), null, { timeout: 10000 }).catch(() => {});
  ok(/ZTE 遥测已拦截/.test(await txt(p, "#tf-status")) && await p.isDisabled("#tf-quickbtn button"), "all blocked: status ok, button off");

  // unreadable: locked, never "nothing blocked"
  await mock("/mock/filter?list=null");
  await p.waitForFunction(() => /读不出来/.test(document.querySelector("#tf-status")?.textContent ?? ""), null, { timeout: 10000 }).catch(() => {});
  ok(/拦截列表读不出来/.test(await txt(p, "#tf-status")), `unreadable: ${await txt(p, "#tf-status")}`);
  ok(await p.isDisabled("#tf-dom") && await p.isDisabled("#tf-quickbtn button") && !/还没有拦截/.test(await txt(p, "#tf-list")), "unreadable locks changes, not shown as empty");
  await p.screenshot({ path: `${OUT}/tf-unreadable.png`, fullPage: true });
  await mock("/mock/filter");

  // the panel's own host: refused before sending (Chrome sends *.localhost to loopback)
  const hp = await ctx.newPage();
  await hp.goto(`http://u60.panel.localhost:${port}${PATH}`, { waitUntil: "load" });
  await ready(hp).catch(() => ok(false, "panel.localhost: page never showed"));
  n = (await ops()).length;
  await add(hp, "panel.localhost");
  await note(hp, "tf-add", "u60\\.panel\\.localhost").catch(() => {});
  ok(/没有发送：拦截 panel\.localhost 也会拦住 u60\.panel\.localhost/.test(await txt(hp, "#tf-add-op")), `own host refused: ${await txt(hp, "#tf-add-op")}`);
  ok(await hp.getAttribute("#tf-add-op .t-note", "role") === "alert", "refusal is an alert");
  // keyword: the rule text inside the host (not a suffix) is refused too
  await add(hp, "panel.local");
  await note(hp, "tf-add", "拦截 panel\\.local 也会").catch(() => {});
  ok(/没有发送：拦截 panel\.local 也会拦住 u60\.panel\.localhost/.test(await txt(hp, "#tf-add-op")), `keyword refused: ${await txt(hp, "#tf-add-op")}`);
  // a sibling name is fine (asked, not sent here)
  await add(hp, "other.localhost");
  await hp.waitForSelector("#tf-add-confirm", { timeout: 10000 }).catch(() => {});
  ok(await hp.$("#tf-add-confirm") !== null, "a name the panel doesn't sit under is asked as usual");
  await hp.click("#tf-add-confirm .nd-btn--secondary");
  await sleep(500);
  ok((await ops()).length === n, "nothing sent from the panel-host checks");
  await hp.screenshot({ path: `${OUT}/tf-ownhost.png`, fullPage: true });
  await hp.close();
}

async function remote(browser) {
  // over Tailscale (a single-label MagicDNS name): tier 3 dialog with the warning
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: `http://u60:${port}` }, { name: "u60_lang", value: "zh", url: `http://u60:${port}` }]);
  const p = await ctx.newPage();
  await p.goto(`http://u60:${port}${PATH}`, { waitUntil: "load" });
  await ready(p).catch(() => ok(false, "remote: page never showed"));
  const n = (await ops()).length;
  await p.click("#tf-list button[aria-label='移除 ads.example.com']");
  await p.waitForSelector("dialog#tf-del-confirm[open]", { timeout: 10000 }).catch(() => {});
  const d = await txt(p, "#tf-del-confirm");
  ok(/Tailscale/.test(d) && /不断网/.test(d) && /连着 U60 Wi-Fi/.test(d), `remote dialog: ${d}`);
  const ax = await new AxeBuilder({ page: p }).analyze();
  ok(ax.violations.length === 0, `remote dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
  await p.screenshot({ path: `${OUT}/tf-remote-dialog.png` });
  await p.click("#tf-del-confirm .nd-btn--secondary");
  await sleep(500);
  ok((await ops()).length === n, "cancel sends nothing");
  await ctx.close();
}

await mock("/mock/filter");
const browser = await chromium.launch({ channel: "chrome", args: ["--host-resolver-rules=MAP u60 127.0.0.1"] });
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  for (const url of [WEB, `http://u60.panel.localhost:${port}`]) await ctx.addCookies([{ name: "lw_sid", value: sid, url }, { name: "u60_lang", value: lang, url }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  await p.goto(WEB + PATH, { waitUntil: "load" });
  await ready(p).catch(() => ok(false, `${name} ${lang}: live content never showed`));
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
  }
  if (lang === "en") {
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
  }
  if (name === "phone") {
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `390 px no sideways scroll (${sw})`);
  }
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
  await p.screenshot({ path: `${OUT}/tf-${name}-${lang}-${theme}.png`, fullPage: true });
  if (name === "desk" && lang === "zh") {
    await own(p, ctx);
    await mock("/mock/filter");
  }
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await remote(browser);
await mock("/mock/filter");
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
