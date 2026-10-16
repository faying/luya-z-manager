// /router/vpn (batch 4): common checks (first load ≤ 40 KiB gzip without fonts, axe light/dark 0,
// 390 px no sideways scroll, English body without Chinese, no page errors) and the page's own:
// B31 state (only SIP reported: 状态不明 + 打开/关掉, each 「设备已接受，读不回来确认」), a
// reported field read back, no readback → 「设备已接受，还没读回」, the read failed, the Tailscale
// dialog (tier 3, a Host without a dot counts as remote). Run from source/manager:
//   WEB=http://127.0.0.1:18212 MOCK=http://127.0.0.1:19412 OUT=/tmp/lw-vpn crates/luyaz-web/e2e/run.sh router-vpn
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mkdirSync } from "node:fs";

const WEB = process.env.WEB ?? "http://127.0.0.1:18212";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19412";
const OUT = process.env.OUT ?? "/tmp/lw-vpn";
mkdirSync(OUT, { recursive: true });
const mock = (path) => fetch(MOCK + path, { method: "POST" });
const ops = () => fetch(`${MOCK}/mock/stats`).then((r) => r.json()).then((s) => s.page_ops.filter((b) => b.params?.method === "router_set_alg_switch"));
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
const PATH = "/router/vpn";

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
const waitText = (p, sel, re, timeout = 10000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout }).catch(() => {});
const ready = (p) => p.waitForSelector("#vpn-rows button:not([disabled])", { timeout: 15000 });

async function own(p) {
  ok(/这版固件不报告 VPN 穿透/.test(await txt(p, "#vpn-status")), `B31: ${await txt(p, "#vpn-status")}`);
  ok(await p.locator("#vpn-rows .nd-switch").count() === 0, "unreported: no switches");
  for (const k of ["l2tp", "pptp", "ipsec"]) ok(/状态不明/.test(await txt(p, `#vpn-${k}`)) && await p.locator(`#vpn-${k} button:has-text('打开')`).count() === 1 && await p.locator(`#vpn-${k} button:has-text('关掉')`).count() === 1, `${k}: 状态不明 + 打开/关掉`);
  ok(/SIP ALG.*这里只读.*开/.test(await txt(p, "#vpn-sip")), `SIP read-only: ${await txt(p, "#vpn-sip")}`);

  // unreported field: 「打开」 → inline confirm, sent once, 「设备已接受，读不回来确认」
  const n0 = (await ops()).length;
  await p.click("#vpn-l2tp button:has-text('打开')");
  await p.waitForSelector("#vpn-sw-confirm", { timeout: 10000 }).catch(() => {});
  ok(/确认：打开 L2TP 穿透/.test(await txt(p, "#vpn-sw-op")) && await p.locator("dialog#vpn-sw-confirm").count() === 0, `tier 2 inline: ${await txt(p, "#vpn-sw-op")}`);
  await p.click("#vpn-sw-confirm button.nd-btn--confirm");
  await waitText(p, "#vpn-sw-op", /设备已接受/);
  ok(/设备已接受，读不回来确认/.test(await txt(p, "#vpn-sw-op")), `unreported on: ${await txt(p, "#vpn-sw-op")}`);
  let o = await ops();
  ok(o.length === n0 + 1 && JSON.stringify(o.at(-1).params.args) === '{"l2tp_passthrough":"1"}', `sent once: ${JSON.stringify(o.at(-1)?.params)}`);
  ok(/状态不明/.test(await txt(p, "#vpn-l2tp")), "still 状态不明 after the send");
  // 「关掉」 on an unreported field
  await p.click("#vpn-ipsec button:has-text('关掉')");
  await p.waitForSelector("#vpn-sw-confirm", { timeout: 10000 }).catch(() => {});
  ok(/确认：关掉 IPSec 穿透/.test(await txt(p, "#vpn-sw-op")), `关掉 asks to turn off: ${await txt(p, "#vpn-sw-op")}`);
  await p.click("#vpn-sw-confirm button.nd-btn--confirm");
  await waitText(p, "#vpn-sw-op", /设备已接受/);
  o = await ops();
  ok(/设备已接受，读不回来确认/.test(await txt(p, "#vpn-sw-op")) && o.length === n0 + 2 && JSON.stringify(o.at(-1).params.args) === '{"ipsec_passthrough":"0"}', `unreported off: ${await txt(p, "#vpn-sw-op")}`);

  // cancel
  await p.click("#vpn-pptp button:has-text('打开')");
  await p.waitForSelector("#vpn-sw-confirm", { timeout: 10000 }).catch(() => {});
  await p.keyboard.press("Escape");
  await p.waitForSelector("#vpn-sw-confirm", { state: "detached", timeout: 10000 }).catch(() => {});
  ok(await p.locator("#vpn-sw-confirm").count() === 0 && (await ops()).length === n0 + 2, "Esc cancels, nothing sent");

  // reported: read back (after the mock's 1.2 s, so the unreported write above can't land on it)
  await new Promise((r) => setTimeout(r, 1500));
  await mock("/mock/router-vpn?reported=true");
  await waitText(p, "#vpn-status", /穿透全部关着/);
  ok(/穿透全部关着/.test(await txt(p, "#vpn-status")) && await p.locator("#vpn-sip").count() === 1 && await p.locator("#vpn-rows .nd-switch").count() === 3 && await p.locator("#vpn-rows .nd-btn").count() === 0, "reported: all off, three switches, no buttons");
  ok(await p.getAttribute("#vpn-l2tp .nd-switch", "aria-labelledby") === "vpn-l2tp-label", "switch named by its label");
  await p.click("#vpn-pptp .nd-switch");
  await p.waitForSelector("#vpn-sw-confirm", { timeout: 10000 }).catch(() => {});
  await p.click("#vpn-sw-confirm button.nd-btn--confirm");
  await waitText(p, "#vpn-sw-op", /已打开 PPTP 穿透/);
  ok(/已打开 PPTP 穿透/.test(await txt(p, "#vpn-sw-op")), `readback: ${await txt(p, "#vpn-sw-op")}`);
  await waitText(p, "#vpn-status", /开着：PPTP/);
  ok(await p.getAttribute("#vpn-pptp .nd-switch", "aria-checked") === "true" && /开着：PPTP/.test(await txt(p, "#vpn-status")), `switch and status follow: ${await p.getAttribute("#vpn-pptp .nd-switch", "aria-checked")} ${await txt(p, "#vpn-status")}`);

  // no readback in time
  await mock("/mock/control?noreadback=true");
  await p.click("#vpn-pptp .nd-switch");
  await p.waitForSelector("#vpn-sw-confirm", { timeout: 10000 }).catch(() => {});
  ok(/确认：关掉 PPTP 穿透/.test(await txt(p, "#vpn-sw-op")), "on → asks to turn off");
  await p.click("#vpn-sw-confirm button.nd-btn--confirm");
  await waitText(p, "#vpn-rows", /正在关掉/, 3000);
  ok(await p.locator("#vpn-rows .nd-switch[disabled]").count() === 3, "switches locked while sending");
  await waitText(p, "#vpn-sw-op", /设备已接受/, 25000);
  ok(/设备已接受，还没读回/.test(await txt(p, "#vpn-sw-op")), `no readback: ${await txt(p, "#vpn-sw-op")}`);
  o = await ops();
  ok(JSON.stringify(o.at(-1).params.args) === '{"pptp_passthrough":"0"}', "sent pptp 0");

  // the read failed
  await mock("/mock/router-vpn?failed=true");
  await waitText(p, "#vpn-status", /读不到/);
  ok(/读不到 VPN 穿透设置/.test(await txt(p, "#vpn-status")) && await p.locator("#vpn-status button:has-text('重试')").count() === 1 && await p.locator("#vpn-rows .nd-skel").count() === 3, "read failed: said, Retry, no switches");
  await mock("/mock/router-vpn?reported=false");
  await waitText(p, "#vpn-status", /这版固件不报告/);
}

const browser = await chromium.launch({ channel: "chrome", args: ["--host-resolver-rules=MAP lwremote 127.0.0.1"] });
await mock("/mock/router-vpn?reported=false");
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
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
    // axe with the inline confirm open too
    await p.click("#vpn-ipsec button:has-text('打开')");
    await p.waitForSelector("#vpn-sw-confirm", { timeout: 10000 }).catch(() => {});
    const ax = await new AxeBuilder({ page: p }).analyze();
    ok(ax.violations.length === 0, `axe with confirm open: ${ax.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
    await p.screenshot({ path: `${OUT}/vpn-confirm.png`, fullPage: true });
    await p.keyboard.press("Escape");
    await p.waitForSelector("#vpn-sw-confirm", { state: "detached", timeout: 10000 }).catch(() => {});
    await own(p);
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
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await p.screenshot({ path: `${OUT}/vpn-${name}-${lang}-${theme}.png`, fullPage: true });
  await ctx.close();
}

// over Tailscale (a Host without a dot): tier 3 dialog with the warning; Cancel sends nothing
{
  const RW = WEB.replace("127.0.0.1", "lwremote");
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: RW }, { name: "u60_lang", value: "zh", url: RW }]);
  const p = await ctx.newPage();
  await p.goto(RW + PATH, { waitUntil: "load" });
  await ready(p).catch(() => ok(false, "remote: live content never showed"));
  const n0 = (await ops()).length;
  await p.click("#vpn-ipsec button:has-text('关掉')");
  await p.waitForSelector("dialog#vpn-sw-confirm[open]", { timeout: 10000 }).catch(() => {});
  ok(await p.locator("dialog#vpn-sw-confirm[open]").count() === 1 && /Tailscale/.test(await txt(p, "#vpn-sw-confirm")) && /怎么恢复/.test(await txt(p, "#vpn-sw-confirm")), `remote: tier 3 dialog with the warning`);
  const ax = await new AxeBuilder({ page: p }).analyze();
  ok(ax.violations.length === 0, `axe dialog: ${ax.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
  await p.screenshot({ path: `${OUT}/vpn-remote-dialog.png` });
  await p.click("#vpn-sw-confirm button:has-text('取消')");
  await p.waitForSelector("#vpn-sw-confirm", { state: "detached", timeout: 10000 }).catch(() => {});
  ok(await p.locator("#vpn-sw-confirm").count() === 0 && (await ops()).length === n0, "dialog cancelled, nothing sent");
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
