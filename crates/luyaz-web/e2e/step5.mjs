// The 8c-A step 5 pages (trends, clients, speed test, signal detect, diagnose): for each page the
// common checks (first load ≤ 40 KiB gzip without fonts, axe light/dark 0, 390 px no sideways
// scroll, English page body without Chinese, screenshots) and the page's own.
// Same servers as topbar.mjs; run from source/manager with e2e/run.sh:
//   WEB=http://127.0.0.1:18081 \
//     MOCK=http://127.0.0.1:19461 AGENT=http://127.0.0.1:19090 OUT=/tmp/lw PAGES=trends crates/luyaz-web/e2e/run.sh step5
// PAGES = comma list of keys below (default: all).
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19090";
const OUT = process.env.OUT ?? "/tmp/lw";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
const scenario = (set) => fetch(`${AGENT}/__mock/scenario?set=${set}`);

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
  m.querySelectorAll("[data-data]").forEach((e) => e.remove()); // device-supplied text (names, operators)
  return m.innerText + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});

// page key → { path, ready(p) waits for the live content, own(p, ok) page checks (desktop zh light) }
const PAGES = {
  trends: {
    path: "/trends",
    ready: (p) => p.waitForSelector("#tr-cpu-line polyline", { state: "attached", timeout: 20000 }),
    async own(p) {
      ok(await p.locator(".tr-card").count() === 5, "5 chart cards");
      ok(await p.locator("#tr-speed-line polyline").count() === 2, "speed: down and up lines");
      const a = await p.getAttribute("#tr-cpu-line polyline", "points");
      await p.waitForTimeout(4000);
      const b = await p.getAttribute("#tr-cpu-line polyline", "points");
      ok(a !== b && b.split(" ").length >= a.split(" ").length, `CPU line moves with pushes (${a.split(" ").length} → ${b.split(" ").length} points)`);
      ok(/%/.test(await p.textContent("#tr-cpu-now")), "CPU now value");
      ok(/↓ [\d.]+ ↑ [\d.]+ Mbps/.test(await p.textContent("#tr-speed-now")), `speed now: ${await p.textContent("#tr-speed-now")}`);
    },
  },
  clients: {
    path: "/clients",
    ready: (p) => p.waitForSelector("#cl-list .nd-row .nd-row__label[data-data]", { timeout: 15000 }),
    async own(p) {
      ok(/共 5 台 · 4 个有效租约/.test(await p.textContent("#cl-status")), `summary: ${(await p.textContent("#cl-status .nd-status__state")).trim()}`);
      ok(await p.locator("#cl-list .nd-row:visible").count() === 4, "4 active rows shown, the expired one hidden");
      ok(/5 GHz · Wi-Fi 6 · 2402 Mbps · 信号很好/.test(await p.textContent("#cl-list")), "Wi-Fi line");
      ok(/没连 Wi-Fi/.test(await p.textContent("#cl-list")), "not-on-Wi-Fi line");
      await p.click("#cl-list button[aria-expanded]:not(.cl-more)");
      ok(await p.locator("#cl-list .nd-row:visible").count() === 5 && await p.getAttribute("#cl-list button[aria-expanded]:not(.cl-more)", "aria-expanded") === "true", "toggle shows the expired lease");
      ok(/隐藏已过期 \(1\)/.test(await p.locator("#cl-list button[aria-expanded]:not(.cl-more)").innerText()), "toggle reads Hide");
      // a push keeps the toggle (signal in the page, not server state)
      await p.click("button[aria-label='刷新']");
      await p.waitForTimeout(1500);
      ok(await p.locator("#cl-list .nd-row:visible").count() === 5, "after a refresh push the expired lease is still shown");
      await scenario("down");
      await p.waitForSelector("#cl-status .nd-status--stale", { timeout: 12000 }).catch(() => {});
      const st = await p.textContent("#cl-status");
      ok(/重连中/.test(st) && /列表停在 \d\d:\d\d/.test(st), `agent down: stale, list stopped at (${st.replace(/\s+/g, " ").trim().slice(0, 80)})`);
      ok(await p.locator("#cl-list .nd-row:visible").count() === 5, "agent down: the list stays");
      await scenario("normal");
      await p.click("#cl-status button");
      await p.waitForSelector("#cl-status .nd-status--ok", { timeout: 8000 }).catch(() => {});
      ok(/实时/.test(await p.textContent("#cl-status")), "Retry: live again");
      // identification (lan-client-id.md T5): winning name, kind · vendor, private tag, details
      const list = () => p.textContent("#cl-list");
      ok(/Sam's iPhone/.test(await list()) && /手机 · Apple/.test(await list()), "mDNS name wins, kind · vendor line");
      ok(/Kid's iPad/.test(await list()), "an alias wins over the lease name");
      ok(await p.locator("#cl-list .cl-row:has(#cl-n-020000000011) .cl-tag").count() === 1, "random MAC: 私有地址 tag");
      ok(await p.locator("#cl-list .cl-row:has(#cl-n-a483e7000012) .cl-tag").count() === 0, "real MAC: no tag");
      const mb = "#cl-b-a483e7000012", det = "#cl-d-a483e7000012";
      await p.click(mb);
      ok(await p.isVisible(det) && await p.getAttribute(mb, "aria-expanded") === "true", "details open");
      const srcs = await p.locator(`${det} .cl-src > div`).allInnerTexts();
      ok(srcs.length === 5 && /DHCP/.test(srcs[0]) && /NetBIOS/.test(srcs[1]) && /OUI/.test(srcs[2]) && /SSDP/.test(srcs[3]) && /笔记本电脑/.test(srcs[4]), `every source listed: ${srcs.map((x) => x.replace(/\s+/g, " ")).join(" | ")}`);
      await p.fill(`${det} input`, "工作电脑");
      await p.click(`${det} .cl-acts .nd-btn--secondary`);
      await p.waitForSelector("#cl-alias-confirm", { timeout: 5000 }).catch(() => {});
      ok(/「MacBook-Air」/.test(await p.textContent("#cl-alias-confirm")) && /「工作电脑」/.test(await p.textContent("#cl-alias-confirm")), "inline confirm says what changes");
      await p.waitForTimeout(6000);
      ok(await p.isVisible("#cl-alias-confirm") && await p.isVisible(det), "confirm and details survive a push");
      await p.click("#cl-alias-confirm .nd-btn--confirm");
      await p.waitForFunction(() => /别名已保存|设备已接受/.test(document.querySelector("#cl-alias-op")?.textContent ?? ""), null, { timeout: 15000 }).catch(() => {});
      ok(/别名已保存/.test(await p.textContent("#cl-alias-op")), `alias read back: ${(await p.textContent("#cl-alias-op")).trim()}`);
      await p.waitForFunction(() => document.querySelector("#cl-n-a483e7000012")?.textContent === "工作电脑", null, { timeout: 8000 }).catch(() => {});
      ok(await p.textContent("#cl-n-a483e7000012") === "工作电脑", "row shows the alias");
      ok(/你设的别名/.test(await p.textContent(det)), "details list the alias as a source");
      await p.fill(`${det} input`, "电".repeat(22));
      await p.click(`${det} .cl-acts .nd-btn--secondary`);
      await p.waitForFunction(() => /64 字节/.test(document.querySelector("#cl-alias-op")?.textContent ?? ""), null, { timeout: 5000 }).catch(() => {});
      ok(/64 字节/.test(await p.textContent("#cl-alias-op")), "a too-long alias is refused with the reason");
      await p.click(`${det} .cl-acts .nd-btn--ghost`);
      await p.waitForSelector("#cl-alias-confirm", { timeout: 5000 }).catch(() => {});
      ok(/删除别名/.test(await p.textContent("#cl-alias-confirm")), "remove asks first");
      await p.click("#cl-alias-confirm .nd-btn--confirm");
      await p.waitForFunction(() => document.querySelector("#cl-n-a483e7000012")?.textContent === "MacBook-Air", null, { timeout: 15000 }).catch(() => {});
      ok(/别名已删除/.test(await p.textContent("#cl-alias-op")) && await p.textContent("#cl-n-a483e7000012") === "MacBook-Air", "alias removed, the DHCP name is back");
      await p.click(mb);
      ok(!(await p.isVisible(det)), "details close");
      // phone: targets ≥ 44 px
      await p.setViewportSize({ width: 390, height: 844 });
      await p.click(mb);
      const boxes = await p.evaluate((d) => [document.querySelector("#cl-b-a483e7000012"), ...document.querySelectorAll(`${d} button, ${d} input`)].map((e) => Math.round(e.getBoundingClientRect().height)), det);
      ok(boxes.every((h) => h >= 44), `phone targets ≥ 44 px (${boxes.join(",")})`);
      ok(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "phone: details open, no sideways scroll");
      await p.click(mb);
      await p.setViewportSize({ width: 1280, height: 900 });
    },
  },
  speedtest: {
    path: "/tools/speedtest",
    ready: (p) => p.waitForSelector("#st-server-select option[value=\"ookla:27981\"]", { state: "attached", timeout: 15000 }),
    async own(p) {
      const state = async () => (await p.textContent("#st-status .nd-status__state")).trim();
      const live = async () => (await p.textContent("#st-status [aria-live]")).trim();
      const ready = () => p.waitForSelector("#st-server-select option[value=\"ookla:27981\"]", { state: "attached", timeout: 15000 });
      const done = (re, ms) => p.waitForFunction((src) => new RegExp(src).test(document.querySelector("#st-status .nd-status__state")?.textContent ?? ""), re.source, { timeout: ms }).catch(() => {});
      ok(/还没有结果|完成|已取消/.test(await state()), `first state: ${await state()}`);
      // roaming (the mock's default): the core only runs coarse; the cost says so, CHILL can't be added
      await p.click("#st-status .nd-btn--primary");
      ok(/漫游中：只粗测 5 秒、最多 30 MB/.test(await p.textContent("#st-start-confirm")), `roaming cost: ${(await p.textContent("#st-start-confirm")).trim()}`);
      ok(await p.isDisabled("#st-chill [role=switch]") && /漫游中只粗测/.test(await p.textContent("#st-chill .nd-row__text")), "CHILL switch off while roaming");
      await p.click("#st-start-confirm .nd-btn--confirm");
      await done(/完成/, 12000);
      ok(/粗测 · 漫游中只测 5 秒/.test(await live()), `rough result: ${await live()}`);
      ok(!(await p.isVisible("#st-layers section")), "rough result: no layers");
      // R11: still roaming, 「仍要完整测速」 takes a second confirm with the roaming cost, then runs full
      await p.click("#st-status .nd-btn--primary");
      await p.click("#st-start-confirm button:has-text(\"仍要完整测速\")");
      ok(!(await p.isVisible("#st-start-confirm")) && await p.isVisible("#st-full-confirm"), "full anyway: its own confirm");
      ok(/走漫游流量，约 30 秒 · 最多 750 MB/.test(await p.textContent("#st-full-confirm")), `roaming full cost: ${(await p.textContent("#st-full-confirm")).trim()}`);
      await p.click("#st-full-confirm .nd-btn--confirm");
      await p.waitForFunction(() => /\d\/5 · /.test(document.querySelector("#st-status [aria-live]")?.textContent ?? ""), null, { timeout: 8000 }).catch(() => {});
      ok(/\d\/5 · /.test(await live()), `roaming full run: ${await live()}`);
      ok(await p.evaluate(() => /漫游中完整测速/.test(document.body.textContent)), "roaming full run is marked");
      await p.click("#st-status .nd-btn--secondary");
      await p.click("#st-stop-confirm .nd-btn--confirm");
      await done(/已取消/, 20000);
      // at home: the full v2 run
      await scenario("home-network");
      await p.reload();
      await ready();
      await p.click("#st-status .nd-btn--primary");
      ok(await p.isVisible("#st-start-confirm") && /移动数据：约 30 秒 · 最多 750 MB/.test(await p.textContent("#st-start-confirm")), "Start opens the inline confirm with the data cost");
      ok(await p.getAttribute("#st-status .nd-btn--primary", "aria-expanded") === "true", "trigger aria-expanded");
      await p.selectOption("#st-server-select", "ookla:27981");
      await p.click("#st-start-confirm .nd-btn--confirm");
      await p.waitForSelector("#st-status [role=progressbar]", { timeout: 5000 }).catch(() => {});
      ok(/· \d+%/.test(await state()), `running: ${await state()}`);
      await p.waitForFunction(() => /\d\/5 · /.test(document.querySelector("#st-status [aria-live]")?.textContent ?? ""), null, { timeout: 5000 }).catch(() => {});
      ok(/^\S*\d\/5 · \S+$/.test(await live()) && !/%/.test(await live()), `step announced, percent not: ${await live()}`);
      ok(await p.isDisabled("#st-server-select"), "server choice disabled while running");
      ok(/设备已接受/.test(await p.textContent("#st-op")), "start accepted");
      await p.waitForFunction(() => /实时/.test(document.querySelector("#st-wall .nd-readout--hero")?.textContent ?? ""), null, { timeout: 8000 }).catch(() => {});
      ok(/实时/.test(await p.textContent("#st-wall .nd-readout--hero")), `download readout live: ${(await p.textContent("#ro-dl")).trim()}`);
      // a push must keep an open confirm
      await p.click("#st-status .nd-btn--secondary");
      await p.waitForTimeout(2500);
      ok(await p.isVisible("#st-stop-confirm"), "stop confirm stays open across pushes");
      await p.click("#st-stop-confirm .nd-btn--confirm");
      await p.waitForFunction(() => /已生效|没有生效/.test(document.querySelector("#st-op")?.textContent ?? ""), null, { timeout: 20000 }).catch(() => {});
      ok(/已生效/.test(await p.textContent("#st-op")), `stop read back: ${(await p.textContent("#st-op")).trim()}`);
      await p.waitForTimeout(1500);
      ok(/已取消/.test(await state()) && !(await p.isDisabled("#st-server-select")), `after stop: ${await state()}, server choice enabled`);
      // 「也测经 CHILL」 for one run
      await p.click("#st-chill [role=switch]");
      ok(await p.getAttribute("#st-chill [role=switch]", "aria-checked") === "true" && /1\.15 GB/.test(await p.textContent("#st-cost")), "CHILL on: the cost grows");
      await p.click("#st-status .nd-btn--primary");
      await p.click("#st-start-confirm .nd-btn--confirm");
      await p.waitForFunction(() => /设备已接受/.test(document.querySelector("#st-op")?.textContent ?? ""), null, { timeout: 5000 }).catch(() => {});
      ok(await p.getAttribute("#st-chill [role=switch]", "aria-checked") === "false", "CHILL back to off after the start");
      await p.waitForFunction(() => /^\s*●?完成/.test(document.querySelector("#st-status .nd-status__state")?.textContent ?? ""), null, { timeout: 25000 }).catch(() => {});
      ok(/完成/.test(await state()), `second run completes: ${await state()}`);
      ok(/完成 · ▲慢在单连接、下载时卡/.test((await live()).replace(/\s+/g, " ")), `conclusion announced: ${await live()}`);
      ok(/所用服务器/.test(await p.textContent("#st-details")) && /MB/.test(await p.textContent("#st-details")), "details: server used, data used");
      ok(/全程均值/.test(await p.textContent("#st-details")) && /内核调参\s*关/.test(await p.textContent("#st-details")) && /连接数\s*6/.test(await p.textContent("#st-details")), "details: whole-run average, connections, kernel tuning");
      ok(/\d/.test(await p.textContent("#ro-ul")) && /\d/.test(await p.textContent("#ro-ping")) && /\d/.test(await p.textContent("#ro-loaded")), "upload, ping and ping while downloading filled");
      ok(await p.$("#ro-jitter") === null, "jitter moved to the details");
      const rows = await p.$$eval("#st-layers li", (l) => l.map((e) => e.textContent.trim()));
      ok(rows.length === 5 && /^单连接/.test(rows[0]) && /经 CHILL/.test(rows[1]) && /^基站、Wi-Fi/.test(rows[4]), `layers: ${rows.join(" | ")}`);
      ok(/注意/.test(rows[0]) && /没测/.test(rows[4]), "layer states have text equivalents");
      // an old core (before 1.48.0): the four numbers as before, no conclusion, no layers, no CHILL switch
      await scenario("home-network,speed-v1");
      await p.reload();
      await ready();
      ok(/512 MB/.test(await p.textContent("#st-cost")) && !(await p.$("#st-chill [role=switch]")), "old core: old cost line, no CHILL switch");
      await p.click("#st-status .nd-btn--primary");
      await p.click("#st-start-confirm .nd-btn--confirm");
      await done(/完成/, 25000);
      ok(/^●?完成$/.test(await live()) && !(await p.isVisible("#st-layers section")) && await p.$("#ro-jitter") !== null, `old core result: ${await live()}`);
      // a diagnosis holds the device: the agent's 409 busy sentence is shown, not swallowed
      await scenario("diag-slow");
      await fetch(`${WEB}/tools/diagnose/start`, { method: "POST", headers: { cookie: `lw_sid=${sid}`, origin: new URL(WEB).origin } });
      await p.click("#st-status .nd-btn--primary");
      await p.click("#st-start-confirm .nd-btn--confirm");
      await p.waitForFunction(() => /正在诊断/.test(document.querySelector("#st-op")?.textContent ?? ""), null, { timeout: 6000 }).catch(() => {});
      ok(/▲/.test(await p.textContent("#st-op")) && /正在诊断/.test(await p.textContent("#st-op")), `busy 409: ${(await p.textContent("#st-op")).trim()}`);
      await scenario("normal");
    },
  },
  sigdetect: {
    path: "/router/signal-detect",
    ready: (p) => p.waitForSelector("#sd-status .nd-status__state", { timeout: 10000 }),
    async own(p) {
      const state = async () => (await p.textContent("#sd-status .nd-status__state")).trim();
      ok(/没在检测|检测完成|检测已停止/.test(await state()), `first state: ${await state()}`);
      await p.click("#sd-status .nd-btn--primary");
      ok(await p.isVisible("#sd-start-confirm"), "Start opens the inline confirm");
      await p.click("#sd-start-confirm .nd-btn--confirm");
      await p.waitForFunction(() => /正在检测 · \d+%/.test(document.querySelector("#sd-status")?.textContent ?? ""), null, { timeout: 8000 }).catch(() => {});
      ok(/正在检测 · \d+%/.test(await state()), `running: ${await state()}`);
      await p.waitForFunction(() => /已生效|没有生效/.test(document.querySelector("#sd-op")?.textContent ?? ""), null, { timeout: 25000 }).catch(() => {});
      ok(/已生效/.test(await p.textContent("#sd-op")), `start read back: ${(await p.textContent("#sd-op")).trim()}`);
      // a second tab sees the same run (one state in luyaz-web)
      const p2 = await p.context().newPage();
      await p2.goto(WEB + "/router/signal-detect");
      ok(/正在检测|检测完成/.test(await p2.textContent("#sd-status")), "a second tab shows the run");
      await p2.close();
      await p.waitForFunction(() => /检测完成/.test(document.querySelector("#sd-status")?.textContent ?? ""), null, { timeout: 20000 }).catch(() => {});
      ok(/检测完成/.test(await state()), `finishes: ${await state()}`);
      await p.waitForSelector("#sd-results table", { timeout: 6000 }).catch(() => {});
      ok(await p.locator("#sd-results tbody tr").count() > 0, `results table: ${await p.locator("#sd-results tbody tr").count()} rows`);
      // start again, then stop: two steps
      await p.click("#sd-status .nd-btn--primary");
      await p.click("#sd-start-confirm .nd-btn--confirm");
      await p.waitForSelector("#sd-status .nd-btn--secondary", { timeout: 8000 }).catch(() => {});
      await p.click("#sd-status .nd-btn--secondary");
      ok(await p.isVisible("#sd-stop-confirm"), "Stop opens its confirm");
      await p.click("#sd-stop-confirm .nd-btn--confirm");
      await p.waitForFunction(() => /检测已停止/.test(document.querySelector("#sd-status")?.textContent ?? ""), null, { timeout: 10000 }).catch(() => {});
      ok(/检测已停止/.test(await state()), `stopped: ${await state()}`);
      await p.waitForFunction(() => /已生效|部分生效/.test(document.querySelector("#sd-op")?.textContent ?? ""), null, { timeout: 10000 }).catch(() => {});
      ok(/已生效/.test(await p.textContent("#sd-op")), `stop: ${(await p.textContent("#sd-op")).trim()}`);
    },
  },
  diagnose: {
    path: "/tools/diagnose",
    ready: (p) => p.waitForSelector("#dg-status .nd-status__state", { timeout: 10000 }),
    async own(p) {
      const state = async () => (await p.textContent("#dg-status .nd-status__state")).trim();
      const s0 = await state();
      await p.waitForTimeout(2500);
      ok(/还没查过/.test(s0) ? /还没查过/.test(await state()) : true, `a hard load does not start a run (${s0} → ${await state()})`);
      await p.goto(WEB + "/tools/diagnose?start=1");
      await p.waitForFunction(() => /正在检查|没查到问题|代理|信号|链路/.test(document.querySelector("#dg-status")?.textContent ?? ""), null, { timeout: 6000 }).catch(() => {});
      ok(!/还没查过/.test(await state()), `?start=1 starts a run: ${await state()}`);
      await p.waitForSelector("#dg-next", { timeout: 15000 }).catch(() => {});
      ok(/ 测$|Checked/.test((await p.textContent("#dg-status .nd-status__meta").catch(() => "")).trim()), `finished: ${await state()} · ${(await p.textContent("#dg-status .nd-status__meta").catch(() => "")).trim()}`);
      ok(await p.locator("#dg-body .dg-row").count() >= 5, `layer rows: ${await p.locator("#dg-body .dg-row").count()}`);
      ok(await p.isVisible("#dg-again button"), "「再查一次」 in the title row");
      // roaming (mock): two presses
      const sp = p.locator("#dg-body button[aria-describedby=dg-speed-note]");
      await sp.click();
      ok(/走漫游流量，再按一次/.test(await sp.innerText()), `first press arms: ${(await sp.innerText()).trim()}`);
      await p.waitForTimeout(5500);
      ok(/加测速度/.test(await sp.innerText()), `armed press expires after 5 s: ${(await sp.innerText()).trim()}`);
      const rowsBefore = await p.locator("#dg-body .dg-row").count();
      await sp.click();
      await p.waitForTimeout(1500);
      ok(/走漫游流量，再按一次/.test(await sp.innerText()) && await p.locator("#dg-body .dg-row").count() === rowsBefore, "after expiry one press only arms again (no test sent)");
      await sp.click();
      await p.waitForFunction(() => /速度/.test(document.querySelector("#dg-body")?.textContent ?? ""), null, { timeout: 6000 }).catch(() => {});
      ok(/速度/.test(await p.textContent("#dg-body")), "second press: speed row appears");
      await p.waitForFunction(() => /直连 ↓/.test(document.querySelector("#dg-body")?.textContent ?? ""), null, { timeout: 15000 }).catch(() => {});
      ok(/直连 ↓/.test(await p.textContent("#dg-body")), "speed row result");
      await p.click("#dg-body .dg-fb >> nth=0");
      await p.waitForFunction(() => /已记下/.test(document.querySelector("#dg-body")?.textContent ?? ""), null, { timeout: 6000 }).catch(() => {});
      ok(/已记下，谢谢/.test(await p.textContent("#dg-body")), "feedback: noted");
      await scenario("down");
      await p.waitForFunction(() => /后台没回应/.test(document.querySelector("#dg-status")?.textContent ?? ""), null, { timeout: 35000 }).catch(() => {});
      ok(/后台没回应/.test(await state()) && await p.locator("#dg-body .nd-stale").count() === 1, `agent down: ${await state()}, rows greyed`);
      await scenario("normal");
      await p.click("#dg-status .nd-status__actions button");
      await p.waitForFunction(() => !/后台没回应/.test(document.querySelector("#dg-status")?.textContent ?? ""), null, { timeout: 6000 }).catch(() => {});
      ok(!/后台没回应/.test(await state()), `Retry: ${await state()}`);
      await p.click("#dg-again button");
      await p.waitForFunction(() => /正在检查/.test(document.querySelector("#dg-status")?.textContent ?? ""), null, { timeout: 5000 }).catch(() => {});
      ok(/正在检查… \d+\/\d+/.test(await state()) && await p.locator("#dg-status [role=progressbar]").count() === 1, `check again: ${await state()}`);
      // v2: a bad bearer blocks what needs it (grey 「没测」, not counted); a full eSIM is a suspect
      await scenario("diag-bearer-bad");
      // the run started above must end first (a start during a run joins it)
      await p.waitForFunction(() => !/正在检查/.test(document.querySelector("#dg-status")?.textContent ?? ""), null, { timeout: 20000 }).catch(() => {});
      await p.click("#dg-again button");
      await p.waitForFunction(() => /数据连接没建立/.test(document.querySelector("#dg-status")?.textContent ?? ""), null, { timeout: 20000 }).catch(() => {});
      const bb = (await p.textContent("#dg-body")) ?? "";
      ok(/数据连接没建立/.test(await state()) && (bb.match(/没测/g) ?? []).length >= 5 && /前面的数据连接不通/.test(bb), `bearer bad: ${await state()} · ${(bb.match(/没测/g) ?? []).length} blocked`);
      ok(!/另有/.test((await p.textContent("#dg-status")) ?? ""), "blocked rows are not 「另有 N 处」");
      await scenario("diag-sim-warn");
      await p.click("#dg-again button");
      await p.waitForFunction(() => /eSIM 通道占满/.test(document.querySelector("#dg-status")?.textContent ?? ""), null, { timeout: 20000 }).catch(() => {});
      ok(/eSIM 通道占满/.test(await state()), `sim warn: ${await state()}`);
      // T6: the core's open finding — evidence here, 「忽略」 closes it (the readback is it gone)
      await scenario("finding");
      await p.waitForSelector("#dg-finding section", { timeout: 35000 }).catch(() => {});
      const fd = (await p.textContent("#dg-finding").catch(() => "")) ?? "";
      ok(/数据连接没建立/.test(fd) && /确定/.test(fd) && /…1234/.test(fd) && await p.locator("#dg-finding .dg-row").count() === 4, `finding evidence: ${fd.replace(/\s+/g, " ").slice(0, 120)}`);
      // §4.10: the proposal under the finding; 「重拨」 arms, the second press approves it
      ok(/建议：重拨数据连接/.test(fd), `proposal shown: ${fd.replace(/\s+/g, " ").slice(0, 120)}`);
      await p.click("#dg-finding .dg-approve");
      ok(/会断网约 15 秒/.test((await p.textContent("#dg-finding .dg-approve")) ?? ""), "redial arms first");
      await p.click("#dg-finding .dg-approve");
      await p.waitForFunction(() => /正在重拨/.test(document.querySelector("#dg-op")?.textContent ?? ""), null, { timeout: 6000 }).catch(() => {});
      ok(/正在重拨/.test((await p.textContent("#dg-op")) ?? ""), "redial approved");
      await p.click("#dg-finding .dg-dismiss");
      await p.waitForFunction(() => !document.querySelector("#dg-finding section"), null, { timeout: 6000 }).catch(() => {});
      ok(await p.locator("#dg-finding section").count() === 0, "ignore: the finding is gone");
      // left on (back after the mock's 5 s) so the common checks below see the section
      await p.waitForTimeout(5500);
    },
  },
};

const want = (process.env.PAGES ?? Object.keys(PAGES).join(",")).split(",");
const browser = await chromium.launch({ channel: "chrome" });
for (const key of want) {
  const pg = PAGES[key];
  for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"]]) {
    const ctx = await browser.newContext({ viewport: { width: w, height: h } });
    await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
    await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
    const p = await ctx.newPage();
    const errs = [];
    p.on("pageerror", (e) => errs.push(e.message));
    await p.goto(WEB + pg.path, { waitUntil: "load" });
    await pg.ready(p).catch(() => ok(false, `${key} ${name} ${lang}: live content never showed`));
    if (name === "desk" && lang === "zh") {
      const kb = (await firstLoad(p)) / 1024;
      ok(kb <= 40, `${key}: first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
      await pg.own(p, ctx);
    }
    if (lang === "en") {
      const left = (await mainText(p)).match(CJK) ?? [];
      ok(left.length === 0, `${key}: English body without Chinese (${[...new Set(left)].join("") || "none"})`);
      ok(await p.getAttribute("html", "lang") === "en", `${key}: html lang=en`);
    }
    if (name === "phone") {
      const sw = await p.evaluate(() => document.documentElement.scrollWidth);
      ok(sw <= 390, `${key}: 390 px no sideways scroll (${sw})`);
    }
    const axe = await new AxeBuilder({ page: p }).analyze();
    ok(axe.violations.length === 0, `${key} axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
    ok(errs.length === 0, `${key} ${name}: no page errors ${errs.join("; ")}`);
    await p.screenshot({ path: `${OUT}/s5-${key}-${name}-${lang}-${theme}.png`, fullPage: true });
    await ctx.close();
  }
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
