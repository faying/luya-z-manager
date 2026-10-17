// /router/scenario (batch 5b): the common checks (first load ≤ 40 KiB gzip without fonts, axe
// light/dark 0, 390 px no sideways scroll, English body without Chinese, no page errors,
// screenshots) and every write against the typed mock agent (fixtures/scenario.ts), each proven by
// the mock's stored state (GET /__lw/scene), because the old page's writes were double-encoded and
// looked fine on screen: the engine off (enabled === false, not true), a pin (pin === "home", not
// null), a config PUT (200 and changed): a home network removed / added from a scan (a SSID with
// quote and angle bracket too), a scenario's Wi-Fi, a CHILL node (and Cancel puts the select
// back), the six timing numbers
// defaults on an empty config. A write the agent refuses is said. Mock state is in memory: start a
// fresh fake agent before each run. From source/manager:
//   WEB=http://127.0.0.1:18594 AGENT=http://127.0.0.1:19594 OUT=/tmp/lw-scene \
//     crates/luyaz-web/e2e/run.sh scenario
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import fs from "node:fs";

const WEB = process.env.WEB ?? "http://127.0.0.1:18594";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19594";
const OUT = process.env.OUT ?? "/tmp/lw-scene";
fs.mkdirSync(OUT, { recursive: true });
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
const scenario = (set) => fetch(`${AGENT}/__mock/scenario?set=${set}`);

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);
const post = (path) => fetch(WEB + path, { method: "POST", headers: { cookie: `lw_sid=${sid}`, origin: new URL(WEB).origin } });
// the engine state as stored in the mock agent, and a config put behind the page's back
const stored = async () => (await (await fetch(`${AGENT}/__lw/scene`)).json()).data;
const putCfg = (cfg) => fetch(`${AGENT}/__lw/scene`, { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify(cfg) });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

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
const until = (p, sel, re, ms = 12000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout: ms }).catch(() => {});
const ready = (p) => p.waitForFunction(() => document.querySelector("#sc-now .nd-group, #sc-setup .nd-group"), null, { timeout: 20000 });

async function own(p) {
  const confirm = (id) => p.locator(`#${id}-op .nd-confirm .nd-btn--confirm`);
  const cancel = (id) => p.locator(`#${id}-op .nd-confirm .nd-btn--secondary`);
  const op = (id) => txt(p, `#${id}-op`);
  // the result, not the confirm that is still there while the write is sent
  const done = async (id, re, ms = 20000) => {
    await p.waitForFunction((i) => !document.querySelector(`#${i}-op .nd-confirm, #${i}-op dialog`), id, { timeout: ms }).catch(() => {});
    await until(p, `#${id}-op`, re, ms);
  };
  const ask = async (id) => { await p.locator(`#${id}-op .nd-confirm`).waitFor({ timeout: 10000 }).catch(() => {}); };

  ok(/国外/.test(await txt(p, "#sc-status")), `status: ${await txt(p, "#sc-status .nd-status__state")}`);
  ok(/466/.test(await txt(p, "#sc-now")) && /\d{4}-\d\d-\d\d \d\d:\d\d/.test(await txt(p, "#sc-now")), "right now: SIM country code and the times");
  await p.screenshot({ path: `${OUT}/scene-desk-zh.png`, fullPage: true });
  ok((await stored()).enabled === true, "engine starts on");

  // ---- engine: Cancel leaves it; confirm turns it OFF (the old page turned it on) ----
  await p.click("#sc-now button[role=switch]");
  await ask("sc-eng");
  ok(/关闭引擎/.test(await op("sc-eng")) || /不再切换情景/.test(await op("sc-eng")), `engine off: inline confirm: ${await op("sc-eng")}`);
  await cancel("sc-eng").click();
  await p.waitForFunction(() => !document.querySelector("#sc-eng-op .nd-confirm"), null, { timeout: 5000 });
  ok((await stored()).enabled === true, "Cancel: still on");
  await p.click("#sc-now button[role=switch]");
  await ask("sc-eng");
  await confirm("sc-eng").click();
  await done("sc-eng", /引擎已关闭|没有生效|设备已接受|连接中断/, 60000);
  ok(await op("sc-eng") === "引擎已关闭" && (await stored()).enabled === false, `engine off, read back: ${await op("sc-eng")} / enabled=${(await stored()).enabled}`);
  await until(p, "#sc-status", /引擎已关闭/, 15000);
  ok(await p.locator("#sc-now button[role=switch]").getAttribute("aria-checked") === "false", "the switch follows");
  await p.click("#sc-now button[role=switch]");
  await ask("sc-eng");
  await confirm("sc-eng").click();
  await done("sc-eng", /引擎已开启|没有生效|设备已接受/, 30000);
  ok(await op("sc-eng") === "引擎已开启" && (await stored()).enabled === true, `engine on again: ${await op("sc-eng")}`);

  // ---- pin: "home" pins home (the old page always unpinned) ----
  await p.click("#sc-pin button[role=radio]:has-text('在家')");
  await ask("sc-pin");
  ok(/设备现在切到「在家」/.test(await op("sc-pin")), `pin: ${await op("sc-pin")}`);
  await confirm("sc-pin").click();
  await done("sc-pin", /已固定|没有生效|设备已接受/);
  ok(await op("sc-pin") === "已固定为「在家」" && (await stored()).pin === "home", `pinned, read back: ${await op("sc-pin")} / pin=${(await stored()).pin}`);
  await until(p, "#sc-pin", /固定为\s*在家/, 15000);
  ok(await p.locator("#sc-pin button[role=radio][aria-checked=true]").innerText() === "在家", "the segment follows");
  await p.click("#sc-pin button:has-text('取消固定')");
  await ask("sc-pin");
  await confirm("sc-pin").click();
  await done("sc-pin", /已取消固定|没有生效|设备已接受/);
  ok(await op("sc-pin") === "已取消固定" && (await stored()).pin === null, `unpinned: ${await op("sc-pin")}`);
  // a pin the agent refuses (the scenario is removed between ask and confirm): said, not "成功"
  await p.click("#sc-pin button[role=radio]:has-text('国外')");
  await ask("sc-pin");
  const before = await stored();
  await putCfg({ ...before.config, scenarios: before.config.scenarios.filter((s) => s.id !== "abroad") });
  await confirm("sc-pin").click();
  await done("sc-pin", /没有生效|设备已接受|连接中断/);
  ok(/没有生效：.*abroad/.test(await op("sc-pin")) && (await stored()).pin === null, `refused pin: ${await op("sc-pin")}`);
  await putCfg(before.config);
  await post("/router/scenario/refresh");

  // ---- timing: bad input stops here; a draft survives a push; save reads back ----
  const num = (sig) => p.locator(`input[data-bind=${sig}]`);
  await num("_te").fill("0");
  await p.click("#sc-timing .nd-btn--primary");
  await done("sc-timing", /至少为 1/, 8000);
  ok(/至少为 1/.test(await op("sc-timing")) && await p.locator("#sc-timing-op .nd-confirm").count() === 0, `0 confirmations: ${await op("sc-timing")}`);
  await num("_te").fill("abc");
  await p.click("#sc-timing .nd-btn--primary");
  await done("sc-timing", /必须是数字/, 8000);
  ok(/必须是数字/.test(await op("sc-timing")), `abc: ${await op("sc-timing")}`);
  await num("_te").fill("3");
  await num("_tc").fill("90");
  await num("_tr").fill("-65");
  await post("/router/scenario/refresh");
  await sleep(1800);
  ok(await num("_tc").inputValue() === "90" && await num("_te").inputValue() === "3", "typing survives a push");
  ok(await p.isVisible("#sc-timing button:text-is('放弃改动')"), "a changed draft offers 放弃改动");
  await p.click("#sc-timing .nd-btn--primary");
  await ask("sc-timing");
  await confirm("sc-timing").click();
  await done("sc-timing", /反应快慢已保存|没有生效|设备已接受/);
  let c = (await stored()).config.params;
  ok(await op("sc-timing") === "反应快慢已保存" && c.scan_interval_away_charging_secs === 90 && c.enter_hits === 3 && c.min_rssi_dbm === -65, `timing saved, read back: ${await op("sc-timing")} ${JSON.stringify(c)}`);
  ok(!await p.isVisible("#sc-timing button:text-is('放弃改动')"), "saved: nothing to discard");

  // ---- home networks: remove one, then add from a scan ----
  ok(/Home-5G/.test(await txt(p, "#sc-home")) && /任何使用这个名称的热点/.test(await txt(p, "#sc-home")) && /1 条只按名称匹配/.test(await txt(p, "#sc-home")), `home list + name-only warning: ${(await txt(p, "#sc-home")).slice(0, 120)}`);
  await p.click("#sc-home button[aria-label='移除 Home-2.4G']");
  await ask("sc-home");
  ok(/不再算作在家/.test(await op("sc-home")), `remove: ${await op("sc-home")}`);
  await confirm("sc-home").click();
  await done("sc-home", /已把|没有生效|设备已接受/);
  let home = (await stored()).config.scenarios.find((s) => s.id === "home").detect.entries;
  ok(/已把「Home-2\.4G」从在家的网络里移除/.test(await op("sc-home")) && home.length === 1 && home[0].ssid === "Home-5G", `removed, read back: ${await op("sc-home")} ${JSON.stringify(home)}`);
  await until(p, "#sc-home", /^(?!.*1 条只按名称匹配)/);

  await p.click("#sc-nearby button");
  await ask("sc-scan");
  ok(/5–30 秒/.test(await op("sc-scan")), `scan: inline confirm: ${await op("sc-scan")}`);
  await confirm("sc-scan").click();
  await until(p, "#sc-scan-op", /扫描中/, 3000);
  ok(/扫描中/.test(await op("sc-scan")), "while it scans: 扫描中");
  await done("sc-scan", /找到 \d+ 个网络|什么都没找到|没有生效/, 30000);
  ok(/找到 8 个网络/.test(await op("sc-scan")), `scan result: ${await op("sc-scan")}`);
  await until(p, "#sc-scanlist", /U60-Travel-5G/, 8000);
  ok(/U60-Travel-5G/.test(await txt(p, "#sc-scanlist")) && /-31 dBm/.test(await txt(p, "#sc-scanlist")) && /隐藏的网络/.test(await txt(p, "#sc-scanlist")), "the list: names, dBm, a hidden network");
  ok(await p.locator("#sc-scanlist button[aria-label^='添加']").count() === 7, "a hidden network has no add button (7 of 8)");
  await p.screenshot({ path: `${OUT}/scene-scan.png`, fullPage: true });
  await p.click("#sc-scanlist button[aria-label='添加 U60-Travel-5G']");
  await ask("sc-scan");
  ok(/看到「U60-Travel-5G」时就算作在家/.test(await op("sc-scan")), `add: ${await op("sc-scan")}`);
  await confirm("sc-scan").click();
  await done("sc-scan", /已把「|没有生效|设备已接受/);
  home = (await stored()).config.scenarios.find((s) => s.id === "home").detect.entries;
  ok(home.some((e) => e.ssid === "U60-Travel-5G" && e.bssid === "02:00:00:00:00:21"), `added with its hardware address: ${JSON.stringify(home)}`);
  await until(p, "#sc-home", /U60-Travel-5G/, 8000);
  await p.waitForFunction(() => !document.querySelector("#sc-scanlist")?.textContent.includes("U60-Travel-5G"), null, { timeout: 15000 }).catch(() => {});
  ok(/U60-Travel-5G/.test(await txt(p, "#sc-home")) && !/U60-Travel-5G/.test(await txt(p, "#sc-scanlist")), "listed now: shown in the home list, hidden from the scan");
  // a name with quote, quote and angle brackets: the buttons still work, nothing is injected
  const weird = `O'Brien "x" <b>\\`;
  const cur = await stored();
  await putCfg({ ...cur.config, scenarios: cur.config.scenarios.map((s) => s.id === "home" ? { ...s, detect: { ...s.detect, entries: [...s.detect.entries, { ssid: weird, bssid: "02:00:00:00:00:a1" }] } } : s) });
  await post("/router/scenario/refresh");
  const rm = p.getByRole("button", { name: `移除 ${weird}`, exact: true });
  await rm.waitFor({ timeout: 10000 });
  ok(await p.locator("#sc-home b").count() === 0, "a SSID with <b> is text");
  await rm.click();
  await ask("sc-home");
  await confirm("sc-home").click();
  await done("sc-home", /已把|没有生效|设备已接受/);
  home = (await stored()).config.scenarios.find((s) => s.id === "home").detect.entries;
  ok(!home.some((e) => e.ssid === weird), `the odd name removed too: ${await op("sc-home")}`);

  // ---- what each scenario does: Wi-Fi ----
  const wifi = p.locator("#sc-does button[role=switch][aria-label='「在家」的 Wi-Fi']");
  ok(await wifi.getAttribute("aria-checked") === "false" && await p.locator("#sc-does button[role=switch]").count() === 2, "home: Wi-Fi off; away has no switch (始终开)");
  ok(/始终开/.test(await txt(p, "#sc-does")), "away: 始终开");
  await wifi.click();
  await ask("sc-does");
  ok(/进入「在家」时设备会打开 Wi-Fi/.test(await op("sc-does")), `wifi on: ${await op("sc-does")}`);
  await confirm("sc-does").click();
  await done("sc-does", /会开 Wi-Fi|没有生效|设备已接受/);
  let h = (await stored()).config.scenarios.find((s) => s.id === "home");
  ok(h.actions[0].path === "/api/wifi/radio" && h.actions[0].body.ap_2g === true && h.actions[0].snapshot.length === 2 && /会开 Wi-Fi/.test(await op("sc-does")), `wifi on, read back: ${await op("sc-does")}`);
  await until(p, "#sc-does", /./, 1000);
  await p.waitForFunction(() => document.querySelector("#sc-does button[role=switch]")?.getAttribute("aria-checked") === "true", null, { timeout: 15000 }).catch(() => {});
  await wifi.click();
  const remote = await Promise.race([
    p.locator("#sc-does-op .nd-confirm").waitFor({ timeout: 8000 }).then(() => "inline"),
    p.locator("#sc-does-op dialog[open]").waitFor({ timeout: 8000 }).then(() => "dialog"),
  ]).catch(() => "none");
  console.log(`info wifi off confirm is ${remote}`);
  ok(remote !== "none", "wifi off: a confirm (tier 2 inline; tier 3 dialog when remote)");
  if (remote === "dialog") {
    ok(/断开多久|How long/.test(await txt(p, "#sc-does-op dialog")), "the dialog says how long and how to recover");
    await p.screenshot({ path: `${OUT}/scene-dialog.png` });
    await p.locator("#sc-does-op dialog .nd-btn--confirm").click();
  } else {
    ok(/连着它的设备都会断开/.test(await op("sc-does")), `wifi off: ${await op("sc-does")}`);
    await confirm("sc-does").click();
  }
  await done("sc-does", /会关 Wi-Fi|没有生效|设备已接受/);
  h = (await stored()).config.scenarios.find((s) => s.id === "home");
  ok(h.actions[0].body.ap_2g === false && /会关 Wi-Fi/.test(await op("sc-does")), `wifi off, read back: ${await op("sc-does")}`);

  // ---- CHILL node: pick, cancel puts the select back, confirm reads back ----
  const sel = p.locator("#sc-does select[aria-label='「在家」的 CHILL 节点']");
  ok((await sel.inputValue()) === "", "home: node 不动");
  await sel.selectOption({ label: "🇯🇵 日本" });
  await ask("sc-does");
  ok(/CHILL 主节点组切到 🇯🇵 日本/.test(await op("sc-does")), `node: ${await op("sc-does")}`);
  await cancel("sc-does").click();
  await p.waitForFunction(() => document.querySelector("#sc-does select")?.value === "", null, { timeout: 8000 }).catch(() => {});
  ok((await p.locator("#sc-does select[aria-label='「在家」的 CHILL 节点']").inputValue()) === "", "Cancel: the select is back on 不动");
  ok((await stored()).config.scenarios.find((s) => s.id === "home").actions.length === 1, "Cancel: nothing was sent");
  await p.locator("#sc-does select[aria-label='「在家」的 CHILL 节点']").selectOption({ label: "🇯🇵 日本" });
  await ask("sc-does");
  await confirm("sc-does").click();
  await done("sc-does", /CHILL 节点已设为|没有生效|设备已接受/);
  h = (await stored()).config.scenarios.find((s) => s.id === "home");
  const reg = h.actions.find((a) => a.path === "/api/services/chill/regions");
  ok(reg?.body.member === "🇯🇵 日本" && reg.best_effort === true && reg.restore_on_exit?.field === "member" && reg.verify_field?.equals === "🇯🇵 日本" && h.actions[0].path === "/api/wifi/radio", `node saved, read back, the Wi-Fi action kept: ${await op("sc-does")}`);
  await p.locator("#sc-does select[aria-label='「在家」的 CHILL 节点']").selectOption({ label: "不动" });
  await ask("sc-does");
  await confirm("sc-does").click();
  await done("sc-does", /不再改动|没有生效|设备已接受/);
  ok(!(await stored()).config.scenarios.find((s) => s.id === "home").actions.some((a) => a.path === "/api/services/chill/regions"), `node cleared: ${await op("sc-does")}`);

  // ---- abroad: gone from the config → "添加国外情景" puts back only that ----
  const base = await stored();
  await putCfg({ ...base.config, scenarios: base.config.scenarios.filter((s) => s.id !== "abroad") });
  await post("/router/scenario/refresh");
  await p.locator("#sc-abroad button:has-text('添加国外情景')").waitFor({ timeout: 10000 });
  ok(/未设置/.test(await txt(p, "#sc-abroad")), "no abroad scenario: said, with the add button");
  await p.click("#sc-abroad button:has-text('添加国外情景')");
  await ask("sc-abroad");
  await confirm("sc-abroad").click();
  await done("sc-abroad", /已添加国外情景|没有生效|设备已接受/);
  c = (await stored()).config;
  ok(c.scenarios.some((s) => s.detect.type === "abroad") && c.scenarios.length === 3 && c.scenarios.find((s) => s.id === "home").detect.entries.length >= 2, `abroad added, the home networks kept: ${await op("sc-abroad")}`);
  await until(p, "#sc-abroad", /所有外国 SIM/, 15000);
  ok(/所有外国 SIM/.test(await txt(p, "#sc-abroad")) && !(await p.locator("#sc-abroad button:has-text('添加国外情景')").count()), "abroad row shown, add button gone");
  await p.screenshot({ path: `${OUT}/scene-abroad.png`, fullPage: true });

  // ---- the agent can't be read: said, controls off ----
  await scenario("down");
  await post("/router/scenario/refresh");
  await until(p, "#sc-status", /设置停在/, 20000);
  ok(/设置停在 \d\d:\d\d/.test(await txt(p, "#sc-status")) && await p.locator("#sc-now button[role=switch]").isDisabled(), `stale: ${await txt(p, "#sc-status .nd-status__meta")}; controls off`);
  await p.screenshot({ path: `${OUT}/scene-stale.png`, fullPage: true });
  await scenario("normal");
  await post("/router/scenario/refresh");
  await p.waitForFunction(() => !/设置停在/.test(document.querySelector("#sc-status")?.textContent ?? ""), null, { timeout: 20000 }).catch(() => {});
  ok(!/设置停在/.test(await txt(p, "#sc-status")), "back to live");

  // ---- the log ----
  ok(/configuration replaced/.test(await txt(p, "#sc-log")) && /pinned to/.test(await txt(p, "#sc-log")), "the log shows what the writes did");

  // ---- nothing set up: only the card; defaults put the two/three back ----
  const full = await stored();
  await putCfg({ ...full.config, scenarios: [] });
  await post("/router/scenario/refresh");
  await p.locator("#sc-setup button").waitFor({ timeout: 10000 });
  ok(/还没有情景/.test(await txt(p, "#sc-status")) && await p.locator("#sc-now .nd-group").count() === 0, "no scenarios: the card, the rest is hidden");
  await p.screenshot({ path: `${OUT}/scene-empty.png`, fullPage: true });
  await p.click("#sc-setup button");
  await ask("sc-setup");
  await confirm("sc-setup").click();
  await done("sc-setup", /已创建默认情景|没有生效|设备已接受/);
  c = (await stored()).config;
  ok(c.scenarios.length >= 2 && c.scenarios.some((s) => s.id === "home") && /已创建默认情景/.test(await op("sc-setup")), `defaults created, read back: ${await op("sc-setup")}`);
  await p.locator("#sc-now .nd-group").waitFor({ timeout: 15000 });
  ok(await p.locator("#sc-setup button").count() === 0, "the card is gone");
}

async function remote(browser) {
  // over Tailscale (a single-label MagicDNS name): a scenario's Wi-Fi set to off is a tier-3 dialog
  const port = new URL(WEB).port;
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: `http://u60:${port}` }, { name: "u60_lang", value: "zh", url: `http://u60:${port}` }]);
  const p = await ctx.newPage();
  await p.goto(`http://u60:${port}/router/scenario`, { waitUntil: "load" });
  await ready(p);
  const sw = p.locator("#sc-does button[role=switch][aria-label='「在家」的 Wi-Fi']");
  const wifiNow = async () => (await stored()).config.scenarios.find((s) => s.id === "home").actions.find((a) => a.path === "/api/wifi/radio").body.ap_2g;
  if (await sw.getAttribute("aria-checked") === "false") {
    await sw.click();
    await p.locator("#sc-does-op .nd-confirm").waitFor({ timeout: 8000 });
    ok(true, "remote: Wi-Fi on stays inline (tier 2)");
    await p.locator("#sc-does-op .nd-confirm .nd-btn--confirm").click();
    await p.waitForFunction(() => document.querySelector("#sc-does button[role=switch]")?.getAttribute("aria-checked") === "true", null, { timeout: 20000 });
  }
  await sw.click();
  await p.locator("#sc-does-op dialog[open]").waitFor({ timeout: 8000 }).catch(() => {});
  const d = await txt(p, "#sc-does-op dialog");
  ok(/Tailscale/.test(d) && /断开多久/.test(d) && /怎么恢复/.test(d), `remote Wi-Fi off: dialog: ${d.slice(0, 160)}`);
  const ax = await new AxeBuilder({ page: p }).analyze();
  ok(ax.violations.length === 0, `remote dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
  await p.screenshot({ path: `${OUT}/scene-remote-dialog.png` });
  await p.locator("#sc-does-op dialog .nd-btn--secondary").click();
  await sleep(600);
  ok(await wifiNow() === true, "remote: Cancel sends nothing");
  await sw.click();
  await p.locator("#sc-does-op dialog[open] .nd-btn--confirm").click();
  await p.waitForFunction(() => /会关 Wi-Fi|没有生效|设备已接受/.test(document.querySelector("#sc-does-op")?.textContent ?? ""), null, { timeout: 20000 }).catch(() => {});
  ok(await wifiNow() === false && /会关 Wi-Fi/.test(await txt(p, "#sc-does-op")), `remote: confirmed, read back: ${await txt(p, "#sc-does-op")}`);
  await ctx.close();
}

const browser = await chromium.launch({ channel: "chrome", args: ["--host-resolver-rules=MAP u60 127.0.0.1"] });
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"], [390, 844, "phone", "en", "dark"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  await p.goto(WEB + "/router/scenario", { waitUntil: "load" });
  await ready(p);
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
  }
  if (lang === "en") {
    ok(/Scenarios/.test(await txt(p, "h1")) && /Right now|Not set up/.test(await txt(p, "main")), "English page");
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
  }
  if (name === "phone") {
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `${lang}: 390 px no sideways scroll (${sw})`);
  }
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length + " " + v.nodes.map((n) => n.target.join(" ")).slice(0, 3).join("|")).join(", ") || "0"}`);
  await p.screenshot({ path: `${OUT}/scene-${name}-${lang}-${theme}.png`, fullPage: true });
  if (name === "desk" && lang === "zh") await own(p);
  if (name === "desk" && lang === "en") {
    // the English page with a confirm open and a scan list shown
    await p.click("#sc-nearby button");
    await p.locator("#sc-scan-op .nd-confirm").waitFor({ timeout: 8000 });
    const left2 = (await mainText(p)).match(CJK) ?? [];
    ok(left2.length === 0, `English body with a confirm open (${[...new Set(left2)].join("") || "none"})`);
    const axe2 = await new AxeBuilder({ page: p }).analyze();
    ok(axe2.violations.length === 0, `axe en dark with a confirm open: ${axe2.violations.map((v) => v.id).join(", ") || "0"}`);
  }
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await remote(browser);
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
