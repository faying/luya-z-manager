// /router/firewall (batch 4): the common checks (first load ≤ 40 KiB gzip without fonts, axe
// light/dark 0, 390 px no sideways scroll, English body without Chinese, no page errors,
// screenshots) and each write path once against the mock u60d (src/mock/router_firewall.rs).
//   WEB=http://127.0.0.1:18214 MOCK=http://127.0.0.1:19414 OUT=/tmp/lw-fw \
//     crates/luyaz-web/e2e/run.sh router-firewall
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18214";
const OUT = process.env.OUT ?? "/tmp/lw-fw";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19414";
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
// key order is serde_json's (sorted), not the page's
const canon = (v) => JSON.stringify(v, (_, x) => (x && typeof x === "object" && !Array.isArray(x) ? Object.fromEntries(Object.entries(x).sort()) : x));
const same = (a, b) => canon(a) === canon(b);

const reset = async () => { await post("/mock/fw"); await post("/mock/fw", { dhcp: { ip: "192.168.0.1", netmask: "255.255.255.0" } }); };
const ready = (p) => p.waitForFunction(() => document.querySelectorAll("#fw-sws [role=switch]").length === 4, null, { timeout: 15000 });

async function own(p) {
  const done = (id, re) => until(p, ([id, re]) => new RegExp(re).test(document.querySelector(`#${id}-op`)?.textContent ?? ""), [id, re], 20000);
  const sw = (k) => p.locator(`button[aria-labelledby=fw-${k}-l]`);
  const inline = (id) => p.locator(`#${id}-confirm.nd-confirm .nd-btn--confirm`);
  const dialog = (id) => p.locator(`dialog#${id}-confirm[open]`);

  ok(/防火墙已开启/.test(await txt(p, "#fw-status")) && /级别：中/.test(await txt(p, "#fw-status")), `status: ${await txt(p, "#fw-status")}`);
  ok(await p.locator("#fw-sws [role=radio][aria-checked=true]").textContent() === "中", "level medium selected");
  ok(/1 条规则/.test(await txt(p, "#fw-pfn")) && /NAS HTTPS/.test(await txt(p, "#fw-pf")), "the rule list");

  // firewall off: tier 2 inline, read back
  await sw("fw").click();
  await inline("fw-sw").waitFor({ timeout: 8000 }).catch(() => {});
  ok(await inline("fw-sw").count() === 1 && await dialog("fw-sw").count() === 0, "firewall off: inline confirm (tier 2)");
  await inline("fw-sw").click();
  await done("fw-sw", "已关掉防火墙|没有生效|设备已接受");
  ok(await txt(p, "#fw-sw-op") === "已关掉防火墙", `firewall off: ${await txt(p, "#fw-sw-op")}`);
  ok(same((await lastOp()).params, { object: "zwrt_router.api", method: "router_set_firewall_switch", args: { firewall_switch: "0" } }), "firewall body");
  await until(p, () => /防火墙已关闭/.test(document.querySelector("#fw-status")?.textContent ?? ""));
  ok(/防火墙已关闭/.test(await txt(p, "#fw-status")), "status follows");

  // level high
  await p.locator("#fw-sws [role=radio]:text-is('高')").click();
  await inline("fw-sw").waitFor({ timeout: 8000 }).catch(() => {});
  await inline("fw-sw").click();
  await done("fw-sw", "级别已设为|没有生效|设备已接受");
  ok(await txt(p, "#fw-sw-op") === "防火墙级别已设为高" && (await lastOp()).params.args.firewall_level === "high", `level: ${await txt(p, "#fw-sw-op")}`);

  // NAT off: tier 3 even on the LAN; the dialog says devices may lose internet
  await sw("nat").click();
  await dialog("fw-sw").waitFor({ timeout: 8000 }).catch(() => {});
  ok(await dialog("fw-sw").count() === 1 && /断网/.test(await txt(p, "#fw-sw-confirm")) && /192\.168\.0\.1/.test(await txt(p, "#fw-sw-confirm")), "NAT off: dialog, may lose internet, recovery names the LAN address");
  const ax = await new AxeBuilder({ page: p }).include("#fw-sw-confirm").analyze();
  ok(ax.violations.length === 0, `dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
  await p.click("#fw-sw-confirm .nd-btn--confirm");
  await done("fw-sw", "已关掉NAT|没有生效|设备已接受");
  ok(await txt(p, "#fw-sw-op") === "已关掉NAT" && (await lastOp()).params.args.nat_switch === "0", `NAT off: ${await txt(p, "#fw-sw-op")}`);
  await until(p, () => document.querySelector("button[aria-labelledby=fw-nat-l]")?.getAttribute("aria-checked") === "false");
  // NAT on: tier 2
  await sw("nat").click();
  await inline("fw-sw").waitFor({ timeout: 8000 }).catch(() => {});
  ok(await inline("fw-sw").count() === 1, "NAT on: inline");
  await p.click("#fw-sw-confirm button:has-text('取消')");
  await until(p, () => !document.querySelector("#fw-sw-confirm"), null, 5000);
  ok(await p.locator("#fw-sw-confirm").count() === 0, "cancel closes the confirm");

  // UPnP off: the numbers go back as read
  await sw("upnp").click();
  await inline("fw-sw").waitFor({ timeout: 8000 }).catch(() => {});
  await inline("fw-sw").click();
  await done("fw-sw", "已关掉UPnP|没有生效|设备已接受");
  ok(await txt(p, "#fw-sw-op") === "已关掉UPnP" && same((await lastOp()).params.args, { enable_upnp: 0, notify_interval: 30, ttl: 4, natpmp: 1 }), `UPnP: ${await txt(p, "#fw-sw-op")} ${JSON.stringify((await lastOp()).params.args)}`);

  // port forwarding off, no readback: "accepted", never "success"
  await post("/mock/control?noreadback=true");
  await sw("pf").click();
  await inline("fw-sw").waitFor({ timeout: 8000 }).catch(() => {});
  await inline("fw-sw").click();
  await done("fw-sw", "设备已接受|没有生效|已关掉");
  ok(/^设备已接受/.test(await txt(p, "#fw-sw-op")) && (await lastOp()).params.args.port_forward_switch === "0", `pf switch, no readback: ${await txt(p, "#fw-sw-op")}`);

  // DMZ: the form, an address off the LAN is refused, then a dialog (tier 3)
  await p.click("#fw-dmzbtn");
  await p.waitForSelector("input[data-bind=_dip]", { timeout: 8000 }).catch(() => {});
  ok(await p.getAttribute("#fw-dmzbtn", "aria-expanded") === "true" && await p.getAttribute("input[data-bind=_dip]", "placeholder") === "192.168.0.x", "DMZ form open, LAN placeholder");
  await p.click("button[aria-labelledby=fw-don-l]");
  await p.fill("input[data-bind=_dip]", "10.0.0.5");
  await p.click("#fw-dmzform button:has-text('应用')");
  await done("fw-dmz", "局域网");
  ok(/不在 U60 的局域网里（192\.168\.0\.0\/24）/.test(await txt(p, "#fw-dmz-op")), `off-LAN refused: ${await txt(p, "#fw-dmz-op")}`);
  await p.fill("input[data-bind=_dip]", "192.168.0.50");
  await p.click("#fw-dmzform button:has-text('应用')");
  await dialog("fw-dmz").waitFor({ timeout: 8000 }).catch(() => {});
  ok(await dialog("fw-dmz").count() === 1 && /断网/.test(await txt(p, "#fw-dmz-confirm")), "DMZ: dialog, may lose internet");
  await p.click("#fw-dmz-confirm .nd-btn--confirm");
  await done("fw-dmz", "DMZ：|没有生效|设备已接受");
  ok(await txt(p, "#fw-dmz-op") === "已启用 DMZ：192.168.0.50" && same((await lastOp()).params.args, { dmz_enabled: "1", dmz_ip: "192.168.0.50" }), `DMZ: ${await txt(p, "#fw-dmz-op")}`);
  ok(await p.locator("input[data-bind=_dip]").count() === 0 && await p.getAttribute("#fw-dmzbtn", "aria-expanded") === "false", "form closed after the readback");
  await until(p, () => /192\.168\.0\.50/.test(document.querySelector("#fw-dmz")?.textContent ?? ""));
  ok(/已启用 · 192\.168\.0\.50/.test(await txt(p, "#fw-dmz")), `DMZ row follows: ${await txt(p, "#fw-dmz")}`);

  // add a rule
  await p.click("#fw-pfbtn");
  await p.waitForSelector("input[data-bind=_pn]", { timeout: 8000 }).catch(() => {});
  await p.fill("input[data-bind=_pn]", "Game");
  await p.selectOption("select[data-bind=_pp]", "UDP");
  await p.fill("input[data-bind=_pw]", "3074");
  await p.fill("input[data-bind=_pi]", "192.168.0.30");
  await p.fill("input[data-bind=_pl]", "3074");
  await p.click("#fw-pfform button:has-text('保存规则')");
  await inline("fw-pf").waitFor({ timeout: 8000 }).catch(() => {});
  ok(/UDP 端口 3074 的流量会转给 192\.168\.0\.30:3074/.test(await txt(p, "#fw-pf-confirm")), `add confirm: ${await txt(p, "#fw-pf-confirm")}`);
  await inline("fw-pf").click();
  await done("fw-pf", "已添加|没有生效|设备已接受");
  ok(await txt(p, "#fw-pf-op") === "已添加规则「Game」", `add: ${await txt(p, "#fw-pf-op")}`);
  ok(same((await lastOp()).params.args, { action: "add", name: "Game", protocol: "UDP", wan_port: "3074", lan_ip: "192.168.0.30", lan_port: "3074", enabled: "1" }), "add body");
  ok(/2 条规则/.test(await txt(p, "#fw-pfn")) && await p.locator("input[data-bind=_pn]").count() === 0, "listed, form closed");

  // delete the first rule
  await p.click("button[aria-label='删除规则「NAS HTTPS」']");
  await inline("fw-del").waitFor({ timeout: 8000 }).catch(() => {});
  await inline("fw-del").click();
  await done("fw-del", "已删除|没有生效|设备已接受");
  ok(await txt(p, "#fw-del-op") === "已删除规则「NAS HTTPS」" && same((await lastOp()).params.args, { action: "delete", id: "1" }), `delete: ${await txt(p, "#fw-del-op")}`);
  ok(!/NAS HTTPS/.test(await txt(p, "#fw-pf")), "row gone");

  // no netmask: DMZ host and forward target refused, nothing sent (user decision 10-13)
  await post("/mock/fw", { dhcp: { netmask: "" } });
  await sleep(800);
  const n0 = (await stats()).page_ops.length;
  await p.click("#fw-dmzbtn");
  await p.waitForSelector("input[data-bind=_dip]", { timeout: 8000 }).catch(() => {});
  await until(p, () => document.querySelector("button[aria-labelledby=fw-don-l]")?.getAttribute("aria-checked") === "true", null, 3000);
  await p.fill("input[data-bind=_dip]", "192.168.0.60");
  await p.click("#fw-dmzform button:has-text('应用')");
  await done("fw-dmz", "网段");
  ok(await txt(p, "#fw-dmz-op") === "还没读到局域网网段，稍后再试", `no netmask, DMZ: ${await txt(p, "#fw-dmz-op")}`);
  await p.click("#fw-dmzbtn");
  await p.click("#fw-pfbtn");
  await p.waitForSelector("input[data-bind=_pn]", { timeout: 8000 }).catch(() => {});
  await p.fill("input[data-bind=_pn]", "X");
  await p.fill("input[data-bind=_pw]", "81");
  await p.fill("input[data-bind=_pi]", "192.168.0.31");
  await p.fill("input[data-bind=_pl]", "81");
  await p.click("#fw-pfform button:has-text('保存规则')");
  await done("fw-pf", "网段");
  ok(await txt(p, "#fw-pf-op") === "还没读到局域网网段，稍后再试" && (await stats()).page_ops.length === n0, `no netmask, add: ${await txt(p, "#fw-pf-op")}, nothing sent`);
  await p.click("#fw-pfbtn");
  await post("/mock/fw", { dhcp: { netmask: "255.255.255.0" } });

  // B31: no level, UPnP not read → "—", level off
  await post("/mock/fw", { firewall: { enabled: true, level: "", nat: true, dmz: false, dmz_ip: "", port_forward: true, wan_ping: null, remote_web: null }, upnp: null });
  await until(p, () => !document.querySelector("button[aria-labelledby=fw-upnp-l]"));
  ok(await p.locator("button[aria-labelledby=fw-upnp-l]").count() === 0 && /读不到 UPnP/.test(await txt(p, "#fw-sws")), "UPnP unread: — and why");
  ok(await p.locator("#fw-sws [role=radio][disabled]").count() === 3, "B31 level \"\": level buttons off");
  await reset();
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
  await p.goto(WEB + "/router/firewall", { waitUntil: "load" });
  await ready(p).catch(() => ok(false, `${name} ${lang}: live content never showed`));
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
    await own(p);
    await p.reload({ waitUntil: "load" });
    await ready(p).catch(() => {});
    // open both forms for the axe and screenshot
    await p.click("#fw-pfbtn");
    await p.waitForSelector("input[data-bind=_pn]", { timeout: 8000 }).catch(() => {});
  }
  if (lang === "en") {
    await p.click("#fw-dmzbtn");
    await p.waitForSelector("input[data-bind=_dip]", { timeout: 8000 }).catch(() => {});
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
  }
  if (name === "phone") {
    await p.click("#fw-pfbtn");
    await p.waitForSelector("input[data-bind=_pn]", { timeout: 8000 }).catch(() => {});
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `390 px no sideways scroll (${sw})`);
  }
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length + (process.env.PROBE ? " " + v.nodes.map((n) => n.html + " " + n.failureSummary).join(" / ") : "")).join(", ") || "0"}`);
  ok(errs.length === 0, `${name}: no page errors ${errs.join("; ")}`);
  await p.screenshot({ path: `${OUT}/fw-${name}-${lang}-${theme}.png`, fullPage: true });
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
