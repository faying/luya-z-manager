// /router/lan (batch 4): the common checks (first load ≤ 40 KiB gzip without fonts, axe
// light/dark 0, 390 px no sideways scroll, English body without Chinese, no page errors,
// screenshots) and each write path once against the mock u60d (src/mock/router_lan.rs).
//   WEB=http://127.0.0.1:18215 MOCK=http://127.0.0.1:19415 OUT=/tmp/lw-lan \
//     crates/luyaz-web/e2e/run.sh router-lan
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { execSync } from "node:child_process";

const WEB = process.env.WEB ?? "http://127.0.0.1:18215";
const OUT = process.env.OUT ?? "/tmp/lw-lan";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19415";
const post = (path, body) => fetch(MOCK + path, { method: "POST", headers: { "content-type": "application/json" }, body: body && JSON.stringify(body) });
const stats = () => fetch(`${MOCK}/mock/stats`).then((r) => r.json());
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
const until = (p, fn, arg, ms = 15000) => p.waitForFunction(fn, arg, { timeout: ms }).catch(() => {});
const lastOp = async () => (await stats()).page_ops.at(-1);
const canon = (v) => JSON.stringify(v, (_, x) => (x && typeof x === "object" && !Array.isArray(x) ? Object.fromEntries(Object.entries(x).sort()) : x));
const same = (a, b) => canon(a) === canon(b);

const reset = () => post("/mock/lan");
const ready = (p) => p.waitForFunction(() => /192\.168\.0\.149/.test(document.querySelector("#lan-rows")?.textContent ?? ""), null, { timeout: 15000 });
const openForm = async (p) => {
  await p.click("#lan-btn");
  await p.waitForSelector("input[data-bind=_nip]", { timeout: 8000 }).catch(() => {});
};

async function own(p) {
  const done = (re, ms = 20000) => until(p, (re) => new RegExp(re).test(document.querySelector("#lan-ap-op")?.textContent ?? ""), re, ms);
  const inline = () => p.locator("#lan-ap-confirm.nd-confirm .nd-btn--confirm");
  const dialog = () => p.locator("dialog#lan-ap-confirm[open]");
  const apply = () => p.click("#lan-form button:has-text('应用')");

  ok(/DHCP 已开启/.test(await txt(p, "#lan-status")) && /分配 192\.168\.0\.100 – 192\.168\.0\.149/.test(await txt(p, "#lan-status")), `status: ${await txt(p, "#lan-status")}`);
  ok(/12 小时/.test(await txt(p, "#lan-rows")) && /255\.255\.255\.0/.test(await txt(p, "#lan-rows")), "rows: mask, lease");
  ok(await p.locator("input[data-bind=_nip]").count() === 0, "form closed at first");

  // the form opens prefilled; nothing changed → said so
  await openForm(p);
  ok(await p.getAttribute("#lan-btn", "aria-expanded") === "true" && await p.inputValue("input[data-bind=_nst]") === "192.168.0.100", "form open, range as addresses");
  await apply();
  await done("没有改动");
  ok(/没有改动/.test(await txt(p, "#lan-ap-op")), `unchanged: ${await txt(p, "#lan-ap-op")}`);

  // a range off the subnet and one holding the U60 are refused before sending
  await p.fill("input[data-bind=_nst]", "10.0.0.5");
  await apply();
  await done("网段");
  ok(/不在局域网网段（192\.168\.0\.0\/24）里/.test(await txt(p, "#lan-ap-op")), `off-subnet: ${await txt(p, "#lan-ap-op")}`);
  await p.fill("input[data-bind=_nst]", "1");
  await apply();
  await done("自己的地址");
  ok(/U60 自己的地址/.test(await txt(p, "#lan-ap-op")), `own address: ${await txt(p, "#lan-ap-op")}`);
  await p.fill("input[data-bind=_nmask]", "255.0.255.0");
  await p.fill("input[data-bind=_nst]", "100");
  await apply();
  await done("掩码");
  ok(/不是有效的掩码/.test(await txt(p, "#lan-ap-op")), `mask: ${await txt(p, "#lan-ap-op")}`);
  await p.fill("input[data-bind=_nmask]", "255.255.255.0");

  // range + lease: tier 2 inline, read back
  await p.fill("input[data-bind=_nend]", "199");
  await p.fill("input[data-bind=_nlease]", "1d");
  await apply();
  await inline().waitFor({ timeout: 8000 }).catch(() => {});
  ok(await inline().count() === 1 && await dialog().count() === 0, "range: inline confirm (tier 2)");
  ok(/192\.168\.0\.100 – 192\.168\.0\.199，租约 1 天/.test(await txt(p, "#lan-ap-confirm")), `confirm: ${await txt(p, "#lan-ap-confirm")}`);
  await inline().click();
  await done("已生效|没有生效|设备已接受");
  ok(await txt(p, "#lan-ap-op") === "局域网设置已生效", `range: ${await txt(p, "#lan-ap-op")}`);
  ok(same((await lastOp()).params, { ip: "192.168.0.1", netmask: "255.255.255.0", dhcp_disabled: 0, dhcp_start: "192.168.0.100", dhcp_end: "192.168.0.199", lease_seconds: "86400" }), `body ${JSON.stringify((await lastOp()).params)}`);
  ok((await lastOp()).action === "lan.set", "action lan.set");
  await until(p, () => /192\.168\.0\.199/.test(document.querySelector("#lan-rows")?.textContent ?? ""));
  ok(/192\.168\.0\.199/.test(await txt(p, "#lan-rows")) && /1 天/.test(await txt(p, "#lan-rows")) && await p.locator("input[data-bind=_nip]").count() === 0, "rows follow, form closed");
  ok(await txt(p, "#lan-new") === "", "no new-address note without a new address");

  // DHCP off, no readback: "accepted", never "applied"
  await post("/mock/control?noreadback=true");
  await openForm(p);
  await p.click("button[aria-labelledby=lan-on-l]");
  await apply();
  await inline().waitFor({ timeout: 8000 }).catch(() => {});
  ok(/DHCP 关闭/.test(await txt(p, "#lan-ap-confirm")), "DHCP off said");
  await inline().click();
  await done("设备已接受|没有生效|已生效", 60000);
  ok(/^设备已接受/.test(await txt(p, "#lan-ap-op")) && (await lastOp()).params.dhcp_disabled === 1, `DHCP off, no readback: ${await txt(p, "#lan-ap-op")}`);
  await p.click("#lan-btn"); // close the form
  await until(p, () => !document.querySelector("input[data-bind=_nip]"), null, 5000);

  // a new address: tier 3 even on the LAN; the dialog and the pending note give the new link
  await reset();
  await until(p, () => /192\.168\.0\.149/.test(document.querySelector("#lan-rows")?.textContent ?? ""));
  await openForm(p);
  await p.fill("input[data-bind=_nip]", "192.168.8.1");
  await apply();
  await done("网段");
  ok(/不在局域网网段（192\.168\.8\.0\/24）里/.test(await txt(p, "#lan-ap-op")), "old full range refused in the new subnet");
  await p.fill("input[data-bind=_nst]", "100");
  await p.fill("input[data-bind=_nend]", "149");
  await apply();
  await dialog().waitFor({ timeout: 8000 }).catch(() => {});
  ok(await dialog().count() === 1, "new address: dialog (tier 3)");
  ok(/http:\/\/192\.168\.8\.1:9090/.test(await txt(p, "#lan-ap-confirm")) && /重新登录/.test(await txt(p, "#lan-ap-confirm")), `dialog names the new address: ${await txt(p, "#lan-ap-confirm")}`);
  const ax = await new AxeBuilder({ page: p }).include("#lan-ap-confirm").analyze();
  ok(ax.violations.length === 0, `dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
  await p.click("#lan-ap-confirm .nd-btn--confirm");
  await until(p, () => /如果这个页面不再回应/.test(document.querySelector("#lan-new")?.textContent ?? ""), null, 5000);
  ok(await p.getAttribute("#lan-new a", "href").catch(() => null) === "http://192.168.8.1:9090", `pending note links the new address: ${await txt(p, "#lan-new")}`);
  await done("已生效|没有生效|设备已接受");
  ok(await txt(p, "#lan-ap-op") === "局域网设置已生效：U60 现在的地址是 192.168.8.1", `new address: ${await txt(p, "#lan-ap-op")}`);
  // this tab came by 127.0.0.1, not the old LAN address: it survives, LAN devices move
  ok(/U60 现在的地址是 192\.168\.8\.1。局域网里的设备之后用 http:\/\/192\.168\.8\.1:9090 打开；这个页面照常可用/.test(await txt(p, "#lan-new")), `note after readback (other address): ${await txt(p, "#lan-new")}`);
  ok(same((await lastOp()).params, { ip: "192.168.8.1", netmask: "255.255.255.0", dhcp_disabled: 0, dhcp_start: "192.168.8.100", dhcp_end: "192.168.8.149", lease_seconds: "43200" }), "new address body");

  // the tab came by the old LAN address (the mock's LAN is 127.0.0.0/24 here) and the reply came
  // back there with no readback: not confirmed, still answering here
  await post("/mock/lan", { ip: "127.0.0.1", netmask: "255.255.255.0", start: "100", limit: "50", dhcp_enabled: true });
  await until(p, () => /127\.0\.0\.149/.test(document.querySelector("#lan-rows")?.textContent ?? ""));
  await post("/mock/control?noreadback=true");
  await openForm(p);
  await p.fill("input[data-bind=_nip]", "192.168.9.1");
  await p.fill("input[data-bind=_nst]", "100");
  await p.fill("input[data-bind=_nend]", "149");
  await apply();
  await dialog().waitFor({ timeout: 8000 }).catch(() => {});
  await p.click("#lan-ap-confirm .nd-btn--confirm");
  await done("设备已接受|没有生效|已生效", 60000);
  ok(/^设备已接受/.test(await txt(p, "#lan-ap-op")) && /还在这个地址回应，新地址没有确认/.test(await txt(p, "#lan-new")), `no readback after a new address: ${await txt(p, "#lan-new")}`);

  // a lost reply after a new address: expected, open the new address (what the area posts)
  const lost = await fetch(`${WEB}/router/lan/cancel?ip=192.168.9.1&lost=true`, { method: "POST", headers: { cookie: `lw_sid=${sid}; u60_lang=zh`, origin: new URL(WEB).origin } }).then((x) => x.text());
  ok(/连接中断，还没确认新地址是否生效/.test(lost) && /http:\/\/192\.168\.9\.1:9090/.test(lost), "lost reply: names the new address");

  // stale: the change is refused
  if (await p.getAttribute("#lan-btn", "aria-expanded") === "true") await p.click("#lan-btn");
  await until(p, () => !document.querySelector("input[data-bind=_nip]"), null, 5000);
  await reset();
  await post("/mock/lan", { stale: true });
  await until(p, () => /不是最新/.test(document.querySelector("#lan-status")?.textContent ?? ""));
  await p.click("#lan-btn");
  await done("还没读到");
  ok(/不是最新/.test(await txt(p, "#lan-status")) && /还没读到最新的局域网设置/.test(await txt(p, "#lan-ap-op")) && await p.locator("input[data-bind=_nip]").count() === 0, `stale: ${await txt(p, "#lan-ap-op")}`);
  await reset();
}

// The server goes away mid-write (what a new LAN address does to a LAN tab): the page stays,
// with the link to the new address. Last: the restart drops the session and the mock's state.
async function dies(browser) {
  const C = process.env.CONTAINER ?? "lw-b4-lan";
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: "zh", url: WEB }]);
  const p = await ctx.newPage();
  const errs = []; // Datastar throws / logs its own failed fetches (FetchFailed): expected here
  p.on("pageerror", (e) => { if (!/FetchFailed/.test(e.message)) errs.push(e.message); });
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource|ERR_|net::|FetchFailed/.test(m.text())) errs.push(m.text()); });
  await post("/mock/lan", { ip: "127.0.0.1", netmask: "255.255.255.0", start: "100", limit: "50", dhcp_enabled: true });
  await p.goto(WEB + "/router/lan", { waitUntil: "load" });
  await p.waitForFunction(() => /127\.0\.0\.149/.test(document.querySelector("#lan-rows")?.textContent ?? ""), null, { timeout: 15000 }).catch(() => {});
  await post("/mock/control?noreadback=true");
  await openForm(p);
  await p.fill("input[data-bind=_nip]", "192.168.7.1");
  await p.fill("input[data-bind=_nst]", "100");
  await p.fill("input[data-bind=_nend]", "149");
  await p.click("#lan-form button:has-text('应用')");
  await p.locator("dialog#lan-ap-confirm[open]").waitFor({ timeout: 8000 }).catch(() => {});
  await p.click("#lan-ap-confirm .nd-btn--confirm");
  await p.waitForSelector("#lan-new a[href]", { timeout: 8000 }).catch(() => {});
  execSync(`docker stop -t 1 ${C}`);
  await sleep(30000);
  ok(new URL(p.url()).pathname === "/router/lan", `server gone: still on the page (${p.url()})`);
  ok(await p.getAttribute("#lan-new a", "href").catch(() => null) === "http://192.168.7.1:9090", `server gone: the new link stays: ${await txt(p, "#lan-new")}`);
  ok(errs.length === 0, `server gone: only failed loads ${errs.join("; ")}`);
  await p.screenshot({ path: `${OUT}/lan-server-gone.png`, fullPage: true });
  execSync(`docker start ${C}`);
  await ctx.close();
}

const browser = await chromium.launch({ channel: "chrome" });
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  await reset(); await sleep(500);
  await p.goto(WEB + "/router/lan", { waitUntil: "load" });
  await ready(p).catch(() => ok(false, `${name} ${lang}: live content never showed`));
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
    await own(p);
    await p.reload({ waitUntil: "load" });
    await ready(p).catch(() => {});
  }
  await openForm(p);
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
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length + (process.env.PROBE ? " " + v.nodes.map((n) => n.html + " " + n.failureSummary).join(" / ") : "")).join(", ") || "0"}`);
  ok(errs.length === 0, `${name}: no page errors ${errs.join("; ")}`);
  await p.screenshot({ path: `${OUT}/lan-${name}-${lang}-${theme}.png`, fullPage: true });
  await ctx.close();
}
await dies(browser);
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
