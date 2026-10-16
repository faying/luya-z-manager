// The 8c-A batch 3 pages (network family). Step 3-1: band lock (/bandlock), writes only through
// u60d transactions (the mock's band.set_lte / band.set_nr / band.reset). Later steps add their pages
// to PAGES below. Common checks per page as batch2.mjs (first load ≤ 40 KiB gzip without fonts, axe
// light/dark 0, 390 px no sideways scroll, English body without Chinese, no page errors, screenshots).
//   WEB=http://127.0.0.1:18081 MOCK=http://127.0.0.1:19461 \
//     AGENT=http://127.0.0.1:19090 OUT=/tmp/lw PAGES=bandlock crates/luyaz-web/e2e/run.sh batch3
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const OUT = process.env.OUT ?? "/tmp/lw";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19461";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19090";
const scenario = (set) => fetch(`${AGENT}/__mock/scenario?set=${set}`);
const post = (path, body) => fetch(MOCK + path, { method: "POST", headers: { "content-type": "application/json" }, body: body && JSON.stringify(body) });
const stats = () => fetch(`${MOCK}/mock/stats`).then((r) => r.json());
// the typed mock agent keeps the candidate pick across scenarios: back to Auto (POST {id: "auto"})
async function unpick() {
  const r = await fetch(`${AGENT}/api/auth/login`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ password: "x" }) }).then((r) => r.json()).catch(() => null);
  const t = r?.data?.token;
  if (t) await fetch(`${AGENT}/api/netinfo/apn`, { method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${t}` }, body: JSON.stringify({ id: "auto" }) }).catch(() => {});
}
const idleOp = () => post("/mock/op", { rollback_enabled: true, active: null, last: null });
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
const opText = (id) => (document.querySelector(`#${id}-op`)?.textContent ?? "");

const PAGES = {
  bandlock: {
    path: "/bandlock",
    reset: async () => { await post("/mock/bands"); await post("/mock/cells"); },
    ready: (p) => p.waitForFunction(() => (document.querySelector("#bl-locked")?.textContent ?? "—") !== "—" && document.querySelectorAll("#bl-ltechips .nd-chip").length > 0, null, { timeout: 15000 }),
    async own(p) {
      const lteApply = p.locator("button:has-text('应用 LTE 锁定')");
      const nrApply = p.locator("button:has-text('应用 NR 锁定')");
      const resetBtn = p.locator("section[aria-labelledby=bl-reset-t] button:has-text('重置 / 全部解锁')");
      const chip = (id, b) => p.locator(`#${id} .nd-chip:text-is('${b}')`);
      const pressed = async (id) => p.locator(`#${id} .nd-chip[aria-pressed=true]`).allTextContents();

      // not locked: everything preselected, Apply off (nothing to change), the in-use bands
      ok(await txt(p, "#bl-locked") === "没锁（全部频段）", `locked: ${await txt(p, "#bl-locked")}`);
      ok(await p.locator("#bl-ltechips .nd-chip").count() === 24 && await p.locator("#bl-nrchips .nd-chip").count() === 21, "choices from the supported lists (24 LTE, 21 NR)");
      ok((await pressed("bl-ltechips")).length === 24 && (await pressed("bl-nrchips")).length === 21, "the current lock (all) is preselected");
      ok(await lteApply.isDisabled() && await nrApply.isDisabled() && await resetBtn.isEnabled(), "Apply off while the selection is the lock; Reset on");
      await until(p, () => /已选择/.test(document.querySelector("#bl-ltechips span.nd-aux")?.textContent ?? ""), null, 5000);
      ok(/^已选择 24 个$/.test(await txt(p, "#bl-ltechips span.nd-aux")), `count: ${await txt(p, "#bl-ltechips span.nd-aux")}`);
      ok(/^(n|B)\d+/.test(await txt(p, "#bl-now a.nd-row .nd-row__value")), `in use: ${await txt(p, "#bl-now a.nd-row .nd-row__value")}`);

      // the touch screen locks bands meanwhile: the row follows, a half-made draft is kept
      await chip("bl-ltechips", "B2").click();
      await post("/mock/bands?lte=1,3&sa=78&nsa=41,78");
      await until(p, () => /LTE B1 B3/.test(document.querySelector("#bl-locked")?.textContent ?? ""));
      ok(await txt(p, "#bl-locked") === "SA n78 · NSA n41 n78 · LTE B1 B3", `pushed: ${await txt(p, "#bl-locked")}`);
      ok((await pressed("bl-ltechips")).length === 23 && await chip("bl-ltechips", "B2").getAttribute("aria-pressed") === "false", "the draft survived the push");
      ok(/SA 和 NSA 现在锁的频段不一样/.test(await txt(p, "#bl-nrchips")), "SA ≠ NSA: said");
      await p.reload({ waitUntil: "load" });
      await PAGES.bandlock.ready(p);
      ok(JSON.stringify(await pressed("bl-ltechips")) === '["B1","B3"]' && JSON.stringify(await pressed("bl-nrchips")) === '["n78"]', "reload: the locks preselected (NR from SA)");
      ok(await lteApply.isDisabled() && await nrApply.isEnabled(), "LTE equal: off; NR: SA ≠ NSA, on");

      // LTE: pick B7, tier-3 dialog, Cancel / Esc send nothing
      await chip("bl-ltechips", "B7").click();
      ok(await chip("bl-ltechips", "B7").getAttribute("aria-pressed") === "true" && await chip("bl-ltechips", "B7").getAttribute("data-selected") !== null, "chip pressed + selected style");
      ok(await lteApply.isEnabled() && await txt(p, "#bl-ltechips span.nd-aux") === "已选择 3 个", "Apply on, 3 selected");
      let s0 = await stats();
      await lteApply.click();
      await p.waitForSelector("dialog#bl-lte-confirm[open]", { timeout: 8000 }).catch(() => {});
      const dlg = await txt(p, "#bl-lte-confirm");
      ok(/把 LTE 锁定到 B1 B3 B7？/.test(dlg) && /4G 只用这些频段/.test(dlg) && /断开约 30 秒/.test(dlg) && /重置 \/ 全部解锁/.test(dlg), `dialog: ${dlg.slice(0, 80)}`);
      ok(await p.evaluate(() => document.activeElement?.textContent?.trim()) === "取消", "focus on Cancel");
      ok(/retryMaxCount: 0/.test(await p.getAttribute("#bl-lte-confirm .nd-btn--confirm", "data-on:click")), "the confirm is never retried");
      const ax = await new AxeBuilder({ page: p }).analyze();
      ok(ax.violations.length === 0, `dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
      await p.screenshot({ path: `${OUT}/b3-bandlock-dialog.png` });
      await p.keyboard.press("Escape");
      await sleep(800);
      ok(await p.locator("dialog#bl-lte-confirm[open]").count() === 0, "Esc closes it");
      ok((await stats()).txns.length === s0.txns.length, "Esc: nothing written");

      // confirm: u60d's progress, then its words; one write, a new op_id, the canonical list
      await post("/mock/txn?end=confirmed");
      await lteApply.click();
      await p.waitForSelector("dialog#bl-lte-confirm[open]", { timeout: 8000 }).catch(() => {});
      await p.click("#bl-lte-confirm .nd-btn--confirm");
      await until(p, (id) => /正在/.test(document.querySelector(`#${id}-op`)?.textContent ?? ""), "bl-lte", 5000);
      ok(/正在/.test(await txt(p, "#bl-lte-op")), `progress: ${await txt(p, "#bl-lte-op")}`);
      ok(await p.locator("#op-bar .nd-announce, #op-bar a").count() === 0, "the op bar is not shown on its own page");
      await until(p, (id) => /已改成|没有生效|设备已接受/.test(document.querySelector(`#${id}-op`)?.textContent ?? ""), "bl-lte", 20000);
      ok(await txt(p, "#bl-lte-op") === "已改成B1 B3 B7" && await p.locator("#bl-lte-op .t-note--ok").count() === 1, `done: ${await txt(p, "#bl-lte-op")}`);
      let s1 = await stats();
      let sent = s1.txns.slice(s0.txns.length);
      ok(sent.length === 1 && sent[0].action === "band.set_lte" && sent[0].params.bands === "1,3,7" && sent[0].source === "web", `one band.set_lte {bands: 1,3,7} (${JSON.stringify(sent)})`);
      ok(/^lw-/.test(sent[0]?.op_id ?? ""), `fresh op_id ${sent[0]?.op_id}`);
      await until(p, () => /LTE B1 B3 B7/.test(document.querySelector("#bl-locked")?.textContent ?? ""));
      ok(/LTE B1 B3 B7$/.test(await txt(p, "#bl-locked")) && await lteApply.isDisabled(), "read back in the row; Apply off again");

      // NR: one choice, both SA and NSA
      s0 = await stats();
      await nrApply.click();
      await p.waitForSelector("dialog#bl-nr-confirm[open]", { timeout: 8000 }).catch(() => {});
      ok(/把 NR 锁定到 n78？/.test(await txt(p, "#bl-nr-confirm")) && /SA 和 NSA 都是/.test(await txt(p, "#bl-nr-confirm")), "NR dialog: both SA and NSA");
      await p.click("#bl-nr-confirm .nd-btn--confirm");
      await until(p, (id) => /已改成|没有生效|设备已接受/.test(document.querySelector(`#${id}-op`)?.textContent ?? ""), "bl-nr", 20000);
      sent = (await stats()).txns.slice(s0.txns.length);
      ok(sent.length === 1 && sent[0].action === "band.set_nr" && sent[0].params.sa === "78" && sent[0].params.nsa === "78", `band.set_nr {sa: 78, nsa: 78} (${JSON.stringify(sent[0]?.params)})`);
      ok(/^已改成SA n78 · NSA n78$/.test(await txt(p, "#bl-nr-op")), `NR done: ${await txt(p, "#bl-nr-op")}`);
      await until(p, () => /^SA n78 · NSA n78/.test(document.querySelector("#bl-locked")?.textContent ?? ""));
      ok(await txt(p, "#bl-locked") === "SA n78 · NSA n78 · LTE B1 B3 B7" && !/不一样/.test(await txt(p, "#bl-nrchips")), "both locked alike; the note is gone");

      // busy: another change holds the device → 409, said plainly, nothing changes
      await post("/mock/op", { rollback_enabled: true, active: { op_id: "screen-9", item: "network.mode", phase: "verifying", say_zh: "正在确认", say_en: "Checking" }, last: null });
      await sleep(500);
      s0 = await stats();
      await resetBtn.click();
      await p.waitForSelector("dialog#bl-reset-confirm[open]", { timeout: 8000 }).catch(() => {});
      const rd = await txt(p, "#bl-reset-confirm");
      ok(/重置所有频段锁定？/.test(rd) && /频段和小区锁一起恢复/.test(rd), `reset dialog: ${rd.slice(0, 60)}`);
      await p.click("#bl-reset-confirm .nd-btn--confirm");
      await until(p, (id) => /没有生效/.test(document.querySelector(`#${id}-op`)?.textContent ?? ""), "bl-reset", 10000);
      ok(await txt(p, "#bl-reset-op") === "没有生效：另一项改动正在进行，等它结束再试", `busy: ${await txt(p, "#bl-reset-op")}`);
      ok((await stats()).txns.length === s0.txns.length, "busy: the mock wrote nothing");

      // a band change running (e.g. from the touch screen): buttons off, u60d's words shown
      await post("/mock/op", { rollback_enabled: true, active: { op_id: "screen-10", item: "band.lte", phase: "verifying", say_zh: "正在确认", say_en: "Checking", next_zh: null, remaining_ms: null }, last: null });
      await until(p, () => /正在确认/.test(document.querySelector("#bl-now")?.textContent ?? ""), null, 8000);
      ok(/正在确认/.test(await txt(p, "#bl-now")) && await resetBtn.isDisabled() && await chip("bl-ltechips", "B1").isDisabled(), "band change running: off, said");
      await idleOp();
      await until(p, () => !document.querySelector("section[aria-labelledby=bl-reset-t] button")?.disabled, null, 8000);

      // stuck executor: off, the reason
      await post("/mock/stuck?age=30000");
      await until(p, () => /暂时不能改设置/.test(document.querySelector("#bl-now")?.textContent ?? ""), null, 8000);
      ok(/数据服务没响应 · 暂时不能改设置/.test(await txt(p, "#bl-now")) && await resetBtn.isDisabled(), "stuck: off, said why");
      await post("/mock/stuck");
      await until(p, () => !document.querySelector("section[aria-labelledby=bl-reset-t] button")?.disabled, null, 8000);
      ok(await resetBtn.isEnabled(), "back on");

      // a change that rolls back: u60d's words, warn
      await post("/mock/txn?end=rolled_back");
      await chip("bl-ltechips", "B3").click();
      await lteApply.click();
      await p.waitForSelector("dialog#bl-lte-confirm[open]", { timeout: 8000 }).catch(() => {});
      await p.click("#bl-lte-confirm .nd-btn--confirm");
      await until(p, (id) => /没通|已改成/.test(document.querySelector(`#${id}-op`)?.textContent ?? ""), "bl-lte", 20000);
      ok(/^没通 · 已退回 B1 B3 B7$/.test(await txt(p, "#bl-lte-op")) && await p.locator("#bl-lte-op .t-note--warn[role=alert]").count() === 1, `rolled back: ${await txt(p, "#bl-lte-op")}`);
      ok(JSON.stringify(await pressed("bl-ltechips")) === '["B1","B3","B7"]', "the draft back to what the device reads");

      // reset: everything back, the chips with it
      await post("/mock/txn?end=confirmed");
      await resetBtn.click();
      await p.waitForSelector("dialog#bl-reset-confirm[open]", { timeout: 8000 }).catch(() => {});
      s0 = await stats();
      await p.click("#bl-reset-confirm .nd-btn--confirm");
      await until(p, (id) => /已改成|没有生效|设备已接受/.test(document.querySelector(`#${id}-op`)?.textContent ?? ""), "bl-reset", 20000);
      sent = (await stats()).txns.slice(s0.txns.length);
      ok(sent.length === 1 && sent[0].action === "band.reset" && JSON.stringify(sent[0].params) === "{}", "band.reset {}");
      ok(/^已改成/.test(await txt(p, "#bl-reset-op")) && await p.locator("#bl-reset-op .t-note--ok").count() === 1, `reset: ${await txt(p, "#bl-reset-op")}`);
      await until(p, () => /没锁/.test(document.querySelector("#bl-locked")?.textContent ?? ""));
      ok(await txt(p, "#bl-locked") === "没锁（全部频段）" && (await pressed("bl-ltechips")).length === 24 && (await pressed("bl-nrchips")).length === 21, "unlocked, all chips back");
      const ids = (await stats()).txns.map((x) => x.op_id);
      ok(new Set(ids).size === ids.length, "every send its own op_id");

      // the change log lists it with its undo
      await p.goto(WEB + "/changes", { waitUntil: "load" });
      await p.waitForSelector("#ch-main a.nd-row", { timeout: 20000 }).catch(() => {});
      ok(/频段和小区锁/.test(await txt(p, "#ch-main a.nd-row >> nth=0")), `newest row: ${await txt(p, "#ch-main a.nd-row >> nth=0")}`);
      await post("/mock/bands");
      await p.goto(WEB + "/bandlock", { waitUntil: "load" });
      await PAGES.bandlock.ready(p);
    },
  },
  celllock: {
    path: "/router/celllock",
    reset: async () => { await post("/mock/bands"); await post("/mock/cells"); },
    ready: (p) => p.waitForFunction(() => (document.querySelector("#cl-nrlock")?.textContent ?? "—") !== "—", null, { timeout: 15000 }),
    async own(p) {
      const nrBtn = p.locator("button:has-text('锁定 NR')");
      const lteBtn = p.locator("button:has-text('锁定 LTE')");
      const resetBtn = p.locator("section[aria-labelledby=cl-reset-t] button");
      const field = (sec, i) => p.locator(`section[aria-labelledby=${sec}] input >> nth=${i}`);
      const done = (id) => until(p, (id) => /已改成|没有生效|设备已接受/.test(document.querySelector(`#${id}-op`)?.textContent ?? ""), id, 20000);

      // not locked; the serving cell; scan off with the reason
      ok(await txt(p, "#cl-nrlock") === "没锁" && await txt(p, "#cl-ltelock") === "没锁", `locks: ${await txt(p, "#cl-nrlock")} / ${await txt(p, "#cl-ltelock")}`);
      ok(/^(NR|LTE)$/.test(await txt(p, "#cl-now a.nd-row .nd-row__value")), `network: ${await txt(p, "#cl-now a.nd-row .nd-row__value")}`);
      ok(await p.locator("section[aria-labelledby=cl-nbr-t] button").isDisabled() && /邻区扫描已停用/.test(await txt(p, "#cl-nbr-off")), "neighbour scan off, said why");
      ok(await lteBtn.isDisabled() && await resetBtn.isEnabled(), "Lock LTE off until filled; Unlock all on");

      // the touch screen locks a cell meanwhile: the rows follow, typing is kept
      await field("cl-lte-t", 0).fill("12");
      await post("/mock/cells?nr=604,627264,78&lte=");
      await until(p, () => /PCI 604/.test(document.querySelector("#cl-nrlock")?.textContent ?? ""));
      ok(await txt(p, "#cl-nrlock") === "PCI 604 · ARFCN 627264 · n78", `pushed: ${await txt(p, "#cl-nrlock")}`);
      ok(await field("cl-lte-t", 0).inputValue() === "12", "typing survived the push");
      const note = await txt(p, "#cl-now p.t-note");
      ok(/调制解调器(还在|已在)/.test(note) || !/^NR$/.test(await txt(p, "#cl-now a.nd-row .nd-row__value")), `on-cell note: ${note}`);

      // NR: fill, tier-3 dialog, Esc sends nothing
      await field("cl-nr-t", 0).fill("603");
      await field("cl-nr-t", 1).fill("627264");
      await field("cl-nr-t", 2).fill("n78");
      ok(await nrBtn.isEnabled(), "Lock NR on once filled");
      let s0 = await stats();
      await nrBtn.click();
      await p.waitForSelector("dialog#cl-nr-confirm[open]", { timeout: 8000 }).catch(() => {});
      const dlg = await txt(p, "#cl-nr-confirm");
      ok(/把 NR 锁定到 PCI 603？/.test(dlg) && /ARFCN 627264（n78）/.test(dlg) && /断开约 30 秒/.test(dlg) && /全部解锁/.test(dlg), `dialog: ${dlg.slice(0, 90)}`);
      ok(await p.evaluate(() => document.activeElement?.textContent?.trim()) === "取消", "focus on Cancel");
      const ax = await new AxeBuilder({ page: p }).analyze();
      ok(ax.violations.length === 0, `dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
      await p.screenshot({ path: `${OUT}/b3-celllock-dialog.png` });
      await p.keyboard.press("Escape");
      await sleep(800);
      ok(await p.locator("dialog#cl-nr-confirm[open]").count() === 0 && (await stats()).txns.length === s0.txns.length, "Esc: closed, nothing written");

      // confirm: progress, u60d's words, one cell.lock_nr {pci, arfcn, band}, read back
      await nrBtn.click();
      await p.waitForSelector("dialog#cl-nr-confirm[open]", { timeout: 8000 }).catch(() => {});
      await p.click("#cl-nr-confirm .nd-btn--confirm");
      await until(p, () => /正在/.test(document.querySelector("#cl-nr-op")?.textContent ?? ""), null, 5000);
      ok(/正在/.test(await txt(p, "#cl-nr-op")), `progress: ${await txt(p, "#cl-nr-op")}`);
      ok(await p.locator("#op-bar .nd-announce, #op-bar a").count() === 0, "the op bar is not shown on its own page");
      await done("cl-nr");
      ok(await txt(p, "#cl-nr-op") === "已改成PCI 603 · ARFCN 627264 · n78" && await p.locator("#cl-nr-op .t-note--ok").count() === 1, `done: ${await txt(p, "#cl-nr-op")}`);
      let sent = (await stats()).txns.slice(s0.txns.length);
      ok(sent.length === 1 && sent[0].action === "cell.lock_nr" && sent[0].params.pci === "603" && sent[0].params.arfcn === "627264" && sent[0].params.band === "78" && Object.keys(sent[0].params).length === 3 && /^lw-/.test(sent[0].op_id), `one cell.lock_nr (${JSON.stringify(sent)})`);
      await until(p, () => /PCI 603/.test(document.querySelector("#cl-nrlock")?.textContent ?? ""));
      ok(await txt(p, "#cl-nrlock") === "PCI 603 · ARFCN 627264 · n78", "read back in the row");

      // LTE: {pci, earfcn}
      await field("cl-lte-t", 1).fill("1300");
      s0 = await stats();
      await lteBtn.click();
      await p.waitForSelector("dialog#cl-lte-confirm[open]", { timeout: 8000 }).catch(() => {});
      ok(/把 LTE 锁定到 PCI 12？/.test(await txt(p, "#cl-lte-confirm")), "LTE dialog");
      await p.click("#cl-lte-confirm .nd-btn--confirm");
      await done("cl-lte");
      sent = (await stats()).txns.slice(s0.txns.length);
      ok(sent.length === 1 && sent[0].action === "cell.lock_lte" && sent[0].params.pci === "12" && sent[0].params.earfcn === "1300" && Object.keys(sent[0].params).length === 2, `cell.lock_lte (${JSON.stringify(sent[0]?.params)})`);
      await until(p, () => /PCI 12/.test(document.querySelector("#cl-ltelock")?.textContent ?? ""));
      ok(await txt(p, "#cl-ltelock") === "PCI 12 · EARFCN 1300", `LTE row: ${await txt(p, "#cl-ltelock")}`);

      // busy: another change holds the device
      await post("/mock/op", { rollback_enabled: true, active: { op_id: "screen-9", item: "network.mode", phase: "verifying", say_zh: "正在确认", say_en: "Checking" }, last: null });
      await sleep(500);
      s0 = await stats();
      await lteBtn.click();
      await p.waitForSelector("dialog#cl-lte-confirm[open]", { timeout: 8000 }).catch(() => {});
      await p.click("#cl-lte-confirm .nd-btn--confirm");
      await until(p, () => /没有生效/.test(document.querySelector("#cl-lte-op")?.textContent ?? ""), null, 10000);
      ok(await txt(p, "#cl-lte-op") === "没有生效：另一项改动正在进行，等它结束再试" && (await stats()).txns.length === s0.txns.length, `busy: ${await txt(p, "#cl-lte-op")}`);

      // a band change running: off, u60d's words
      await post("/mock/op", { rollback_enabled: true, active: { op_id: "screen-10", item: "band.lte", phase: "verifying", say_zh: "正在确认", say_en: "Checking", next_zh: null, remaining_ms: null }, last: null });
      await until(p, () => /正在确认/.test(document.querySelector("#cl-now")?.textContent ?? ""), null, 8000);
      ok(await resetBtn.isDisabled() && await nrBtn.isDisabled() && await field("cl-nr-t", 0).isDisabled(), "band change running: off");
      await idleOp();
      await until(p, () => !document.querySelector("section[aria-labelledby=cl-reset-t] button")?.disabled, null, 8000);

      // unlock all = band.reset {}: both locks go; the op bar stays off here too
      s0 = await stats();
      await resetBtn.click();
      await p.waitForSelector("dialog#cl-reset-confirm[open]", { timeout: 8000 }).catch(() => {});
      ok(/频段和小区锁一起恢复/.test(await txt(p, "#cl-reset-confirm")), "reset dialog says both");
      await p.click("#cl-reset-confirm .nd-btn--confirm");
      await until(p, () => /正在/.test(document.querySelector("#cl-reset-op")?.textContent ?? ""), null, 5000);
      ok(await p.locator("#op-bar .nd-announce, #op-bar a").count() === 0, "reset: no op bar on this page");
      await done("cl-reset");
      sent = (await stats()).txns.slice(s0.txns.length);
      ok(sent.length === 1 && sent[0].action === "band.reset" && JSON.stringify(sent[0].params) === "{}", "band.reset {}");
      await until(p, () => document.querySelector("#cl-nrlock")?.textContent === "没锁" && document.querySelector("#cl-ltelock")?.textContent === "没锁");
      ok(await txt(p, "#cl-nrlock") === "没锁" && await txt(p, "#cl-ltelock") === "没锁", "both unlocked");

      // the change log: a cell row, its undo dialog points here
      await p.goto(WEB + "/changes", { waitUntil: "load" });
      await p.waitForSelector("#ch-main a.nd-row", { timeout: 20000 }).catch(() => {});
      ok(/4G 小区锁/.test(await txt(p, "#ch-main")), "change log lists the cell lock");
      await PAGES.celllock.reset();
      await p.goto(WEB + "/router/celllock", { waitUntil: "load" });
      await PAGES.celllock.ready(p);
    },
  },
  apn: {
    path: "/router/apn",
    reset: async () => { await post("/mock/apn"); await scenario("apn-candidates"); await unpick(); },
    ready: (p) => p.waitForFunction(() => document.querySelectorAll("#ap-manual .nd-row").length > 0 && /PDP|读取 WAN|WAN IPv6/.test(document.querySelector("#ap-v6")?.textContent ?? ""), null, { timeout: 15000 }),
    async own(p) {
      const PW = "pw-e2e-7Q4zK";
      const n0 = (await stats()).apn_ops.length;
      const seen = [];
      p.on("request", (r) => seen.push({ url: r.url(), body: r.postData() ?? "" }));
      const modeSw = p.locator("#ap-mode button[role=switch]");
      const rowBtn = (name, label) => p.locator(`#ap-manual button[aria-label='${label} ${name}']`);
      const done = (id) => until(p, (id) => /已改成|没有生效|设备已接受|APN 已|没通/.test(document.querySelector(`#${id}-op`)?.textContent ?? ""), id, 20000);
      const confirmDlg = async (id) => { await p.waitForSelector(`dialog#${id}-confirm[open]`, { timeout: 8000 }).catch(() => {}); };

      // auto mode: the carrier's profile dials; marks from the block and the agent; empty slot hidden
      ok(await txt(p, "#ap-status strong") === "数据连接用的是「China Telecom」", `status: ${await txt(p, "#ap-status strong")}`);
      ok(await p.locator("#ap-manual .nd-row").count() === 2, "two manual profiles (the empty slot is hidden)");
      ok(/China Mobile · 手动模式时用这条/.test(await txt(p, "#ap-manual .nd-row >> nth=0")) && /corp\.example · IPv4 · CHAP/.test(await txt(p, "#ap-manual .nd-row >> nth=1")), `manual rows: ${await txt(p, "#ap-manual")}`);
      ok(/China Telecom · 使用中 · 物联网/.test(await txt(p, "#ap-auto .nd-row >> nth=0")), `auto rows: ${await txt(p, "#ap-auto .nd-row >> nth=0")}`);
      ok(await modeSw.getAttribute("aria-checked") === "false", "mode switch: off (auto)");
      ok(/PDP/.test(await txt(p, "#ap-v6")), `v6 row: ${await txt(p, "#ap-v6")}`);

      // switch to manual: tier-3, a transaction, u60d's words; the status follows
      let s0 = await stats();
      await modeSw.click();
      await confirmDlg("ap-mode");
      ok(/改用手动 APN？/.test(await txt(p, "#ap-mode-confirm")) && /断开约 30 秒/.test(await txt(p, "#ap-mode-confirm")), `mode dialog: ${(await txt(p, "#ap-mode-confirm")).slice(0, 60)}`);
      const ax = await new AxeBuilder({ page: p }).analyze();
      ok(ax.violations.length === 0, `dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
      await p.screenshot({ path: `${OUT}/b3-apn-dialog.png` });
      await p.click("#ap-mode-confirm .nd-btn--confirm");
      await done("ap-mode");
      ok(await txt(p, "#ap-mode-op") === "已改成手动", `mode done: ${await txt(p, "#ap-mode-op")}`);
      let sent = (await stats()).apn_ops.slice(s0.apn_ops.length);
      ok(sent.length === 1 && sent[0].action === "apn.set_mode" && sent[0].params.mode === 1 && /^lw-/.test(sent[0].op_id), `apn.set_mode {mode: 1} (${JSON.stringify(sent)})`);
      await until(p, () => /China Mobile/.test(document.querySelector("#ap-status strong")?.textContent ?? ""));
      ok(await txt(p, "#ap-status strong") === "数据连接用的是「China Mobile」" && await rowBtn("China Mobile", "删除").count() === 0, "manual: China Mobile in use, can't be deleted");
      ok(await p.locator("#ap-manual button[aria-label='无法删除正在使用的 APN']").isDisabled(), "delete of the one in use: off, said why");

      // activate Work (manual mode: a transaction)
      s0 = await stats();
      await rowBtn("Work", "启用").click();
      await confirmDlg("ap-row");
      ok(/切换到「Work」？/.test(await txt(p, "#ap-row-confirm")), "activate dialog");
      await p.click("#ap-row-confirm .nd-btn--confirm");
      await done("ap-row");
      ok(await txt(p, "#ap-row-op") === "已改成手动 · manu2", `activate: ${await txt(p, "#ap-row-op")}`);
      sent = (await stats()).apn_ops.slice(s0.apn_ops.length);
      ok(sent.length === 1 && sent[0].action === "apn.enable" && sent[0].params.profile_id === "manu2", "apn.enable manu2");
      await until(p, () => /Work/.test(document.querySelector("#ap-status strong")?.textContent ?? ""));

      // edit Work (in use): the form comes as a patch, password write-only; one modify with it
      await rowBtn("Work", "编辑").click();
      await p.waitForSelector("#ap-form input", { timeout: 8000 }).catch(() => {});
      const f = (i) => p.locator(`#ap-form input >> nth=${i}`);
      ok(await f(0).inputValue() === "Work" && await f(1).inputValue() === "corp.example" && await f(2).inputValue() === "u1" && await f(3).inputValue() === "", "form prefilled; password empty");
      ok(await f(3).getAttribute("placeholder") === "已设置 · 留空不改", "password: set, empty keeps it");
      await f(1).fill("corp2.example");
      await f(3).fill(PW);
      s0 = await stats();
      await p.click("#ap-form button:has-text('保存修改')");
      await confirmDlg("ap-save");
      const dlg = await p.innerHTML("#ap-save-op");
      ok(/保存对「Work」的修改？/.test(await txt(p, "#ap-save-confirm")) && /corp2\.example 重新拨号/.test(await txt(p, "#ap-save-confirm")), "save dialog: redials");
      ok(!dlg.includes(PW), "the dialog's HTML has no password");
      await p.click("#ap-save-confirm .nd-btn--confirm");
      await done("ap-save");
      ok(/^已改成/.test(await txt(p, "#ap-save-op")), `save: ${await txt(p, "#ap-save-op")}`);
      await until(p, () => !document.querySelector("#ap-form input"), null, 5000);
      ok(await p.locator("#ap-form input").count() === 0, "form closed after it read back");
      sent = (await stats()).apn_ops.slice(s0.apn_ops.length);
      ok(sent.length === 1 && sent[0].action === "apn.modify" && sent[0].params.password === PW && sent[0].params.apn === "corp2.example" && sent[0].params.profile_id === "manu2", "one apn.modify with the password");
      const all = (await stats()).apn_ops.slice(n0).filter((b) => JSON.stringify(b).includes(PW));
      ok(all.length === 1, `the password reached u60d once (${all.length})`);
      const carry = seen.filter((r) => r.url.includes(PW) || r.body.includes(PW));
      ok(carry.length === 1 && /\/router\/apn\/save\/ask$/.test(carry[0].url) && !seen.some((r) => r.url.includes(PW)), `browser: only the save ask's body carries it (${carry.map((r) => r.url).join(", ")})`);
      ok(!(await p.content()).includes(PW), "not in the page");

      // add: duplicate name refused in the page, then a plain add
      await p.click("section[aria-labelledby=ap-man-t] button:has-text('添加配置')");
      await p.waitForSelector("#ap-form input", { timeout: 8000 }).catch(() => {});
      ok(await f(0).inputValue() === "" && await f(3).getAttribute("placeholder") === "(可选)", "add form empty");
      await f(0).fill("China Mobile");
      await f(1).fill("e2e.apn");
      s0 = await stats();
      await p.click("#ap-form button:has-text('保存配置')");
      await until(p, () => /同名/.test(document.querySelector("#ap-save-op")?.textContent ?? ""), null, 5000);
      ok(await txt(p, "#ap-save-op") === "已存在同名 APN" && (await stats()).apn_ops.length === s0.apn_ops.length, "duplicate: said, nothing sent");
      await f(0).fill("E2E");
      await p.click("#ap-form button:has-text('保存配置')");
      await confirmDlg("ap-save");
      ok(/添加「E2E」？/.test(await txt(p, "#ap-save-confirm")), "add dialog");
      await p.click("#ap-save-confirm .nd-btn--confirm");
      await done("ap-save");
      ok(await txt(p, "#ap-save-op") === "APN 已添加", `add: ${await txt(p, "#ap-save-op")}`);
      sent = (await stats()).apn_ops.slice(s0.apn_ops.length);
      ok(sent.length === 1 && sent[0].action === "apn.add" && sent[0].params.name === "E2E" && !("password" in sent[0].params), "apn.add without a password");
      await until(p, () => /E2E/.test(document.querySelector("#ap-manual")?.textContent ?? ""));

      // delete it (plain)
      s0 = await stats();
      await rowBtn("E2E", "删除").click();
      await confirmDlg("ap-row");
      ok(/删除「E2E」？/.test(await txt(p, "#ap-row-confirm")), "delete dialog");
      await p.click("#ap-row-confirm .nd-btn--confirm");
      await done("ap-row");
      ok(await txt(p, "#ap-row-op") === "APN 已删除", `delete: ${await txt(p, "#ap-row-op")}`);
      ok((await stats()).apn_ops.slice(s0.apn_ops.length)[0]?.action === "apn.delete", "apn.delete");

      // operator IPv6: a plain u60d write, read back from the agent
      s0 = await stats();
      await p.click("#ap-v6 button[role=switch]");
      await confirmDlg("ap-v6");
      ok(/IPv6/.test(await txt(p, "#ap-v6-confirm")) && /IPv4 不中断/.test(await txt(p, "#ap-v6-confirm")), "v6 dialog");
      await p.click("#ap-v6-confirm .nd-btn--confirm");
      await until(p, () => /已|设备已接受|没有生效/.test(document.querySelector("#ap-v6-op")?.textContent ?? ""), null, 25000);
      sent = (await stats()).apn_ops.slice(s0.apn_ops.length);
      ok(sent.length === 1 && sent[0].action === "apn.set_pdp_type" && typeof sent[0].params.ipv6 === "boolean", `apn.set_pdp_type (${JSON.stringify(sent[0]?.params)}) → ${await txt(p, "#ap-v6-op")}`);

      // use a carrier candidate for this SIM only (POST /api/netinfo/apn → u60d, here the typed mock
      // agent behind the front): 202, then read back from netinfo (manual, dialling ctnet)
      const pickBtn = p.locator("#ap-auto button[aria-label='只给这张卡用 ctnet']");
      await pickBtn.waitFor({ timeout: 10000 }).catch(() => {});
      ok(await pickBtn.count() === 1 && await p.locator("#ap-auto button").count() === 2 && /好几条 APN/.test(await txt(p, "#ap-auto")), `pick buttons on both carrier rows: ${await txt(p, "#ap-auto")}`);
      await p.locator("#ap-auto .nd-row >> nth=1").screenshot({ path: `${OUT}/b3-apn-pick-row.png` });
      await pickBtn.click();
      await confirmDlg("ap-pick");
      ok(/只给这张卡用 ctnet？/.test(await txt(p, "#ap-pick-confirm")) && /只有这张卡用它/.test(await txt(p, "#ap-pick-confirm")) && /断开约 30 秒/.test(await txt(p, "#ap-pick-confirm")), `pick dialog: ${(await txt(p, "#ap-pick-confirm")).slice(0, 60)}`);
      await p.click("#ap-pick-confirm .nd-btn--confirm");
      await until(p, () => /已只给|设备已接受|没有生效|连接中断/.test(document.querySelector("#ap-pick-op")?.textContent ?? ""), null, 30000);
      ok(await txt(p, "#ap-pick-op") === "已只给这张卡用", `pick: ${await txt(p, "#ap-pick-op")}`);
      await until(p, () => /本卡在用/.test(document.querySelector("#ap-auto")?.textContent ?? ""), null, 10000);
      ok(/China Telecom 4G · 本卡在用/.test(await txt(p, "#ap-auto .nd-row >> nth=1")) && await pickBtn.count() === 0, `picked row marked, no button: ${await txt(p, "#ap-auto .nd-row >> nth=1")}`);
      await p.locator("#ap-auto").screenshot({ path: `${OUT}/b3-apn-picked.png` });
      await unpick();
      // busy: another change holds the device
      await post("/mock/op", { rollback_enabled: true, active: { op_id: "screen-9", item: "network.mode", phase: "verifying", say_zh: "正在确认", say_en: "Checking" }, last: null });
      await sleep(500);
      await modeSw.click();
      await confirmDlg("ap-mode");
      await p.click("#ap-mode-confirm .nd-btn--confirm");
      await until(p, () => /没有生效/.test(document.querySelector("#ap-mode-op")?.textContent ?? ""), null, 10000);
      ok(await txt(p, "#ap-mode-op") === "没有生效：另一项改动正在进行，等它结束再试", `busy: ${await txt(p, "#ap-mode-op")}`);
      // an APN change running: off
      await post("/mock/op", { rollback_enabled: true, active: { op_id: "screen-10", item: "apn", phase: "verifying", say_zh: "正在确认", say_en: "Checking" }, last: null });
      await until(p, () => document.querySelector("#ap-mode button[role=switch]")?.disabled, null, 8000);
      ok(await modeSw.isDisabled() && /正在确认/.test(await txt(p, "#ap-status")), "APN change running: off, said");
      await idleOp();
      await PAGES.apn.reset();
      await p.goto(WEB + "/router/apn", { waitUntil: "load" });
      await PAGES.apn.ready(p);
    },
  },
  netmode: {
    path: "/router/network-mode",
    reset: async () => { await post("/mock/net"); },
    ready: (p) => p.waitForFunction(() => !["", "—"].includes(document.querySelector("#nm-now-v")?.textContent ?? ""), null, { timeout: 15000 }),
    async own(p) {
      const apply = p.locator("button:has-text('应用')");
      const radio = (v) => p.locator(`input[name=nm][value=${v}]`);
      ok(await txt(p, "#nm-now-v") === "自动（全部制式）", `now: ${await txt(p, "#nm-now-v")}`);
      ok(await radio("WL_AND_5G").isChecked() && await apply.isDisabled(), "Auto checked (TCHGWL_5G counts as Auto); Apply off");
      ok(await p.locator("label:has(input[value=WL_AND_5G]) span[data-show]").isVisible() && !(await p.locator("label:has(input[value=Only_LTE]) span[data-show]").isVisible()), "「使用中」 on Auto only");
      ok(await p.locator("input[name=nm]").count() === 6, "six choices");

      // SA only: the warning in the dialog; Esc sends nothing
      let s0 = await stats();
      await radio("Only_5G").check();
      ok(await apply.isEnabled(), "Apply on after a change");
      await apply.click();
      await p.waitForSelector("dialog#nm-confirm[open]", { timeout: 8000 }).catch(() => {});
      const d = await txt(p, "#nm-confirm");
      ok(/切换到「只用 5G SA」？/.test(d) && /很多国家还没有 5G SA/.test(d) && !/自动退回没开/.test(d) && /断开约 30 秒/.test(d), `dialog: ${d.slice(0, 80)}`);
      const ax = await new AxeBuilder({ page: p }).analyze();
      ok(ax.violations.length === 0, `dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
      await p.screenshot({ path: `${OUT}/b3-netmode-dialog.png` });
      await p.keyboard.press("Escape");
      await sleep(800);
      ok((await stats()).txns.length === s0.txns.length, "Esc: nothing written");

      // automatic revert off: said; confirm → u60d's words, one network.set_mode, read back
      await post("/mock/op", { rollback_enabled: false, active: null, last: null });
      await sleep(300);
      await radio("Only_LTE").check();
      await apply.click();
      await p.waitForSelector("dialog#nm-confirm[open]", { timeout: 8000 }).catch(() => {});
      ok(/自动退回没开：没通也会保持。/.test(await txt(p, "#nm-confirm")), "rollback off: said");
      await p.click("#nm-confirm .nd-btn--confirm");
      await until(p, () => /正在/.test(document.querySelector("#nm-op")?.textContent ?? ""), null, 5000);
      ok(/正在/.test(await txt(p, "#nm-op")), `progress: ${await txt(p, "#nm-op")}`);
      ok(await p.locator("#op-bar .nd-announce, #op-bar a").count() === 0, "the op bar is not shown on its own page");
      await until(p, () => /已改成|没有生效|设备已接受|网络模式已/.test(document.querySelector("#nm-op")?.textContent ?? ""), null, 20000);
      ok(/^已(改成|切到)/.test(await txt(p, "#nm-op")) && await p.locator("#nm-op .t-note--ok").count() === 1, `done: ${await txt(p, "#nm-op")}`);
      const sent = (await stats()).txns.slice(s0.txns.length);
      ok(sent.length === 1 && sent[0].action === "network.set_mode" && sent[0].params.mode === "Only_LTE" && /^lw-/.test(sent[0].op_id), `one network.set_mode Only_LTE (${JSON.stringify(sent)})`);
      await until(p, () => document.querySelector("#nm-now-v")?.textContent === "只用 4G");
      ok(await txt(p, "#nm-now-v") === "只用 4G" && await apply.isDisabled() && await radio("Only_LTE").isChecked(), "read back; Apply off; draft = device");
      await until(p, () => !!document.querySelector("#nm-now a[href^='/changes?op=']"), null, 20000);
      ok(await p.locator("#nm-now a[href^='/changes?op=']").count() === 1, `last change link: ${await txt(p, "#nm-now a")}`);

      // busy
      await post("/mock/op", { rollback_enabled: true, active: { op_id: "screen-9", item: "band.lte", phase: "verifying", say_zh: "正在确认", say_en: "Checking" }, last: null });
      await sleep(500);
      await radio("WL_AND_5G").check();
      await apply.click();
      await p.waitForSelector("dialog#nm-confirm[open]", { timeout: 8000 }).catch(() => {});
      await p.click("#nm-confirm .nd-btn--confirm");
      await until(p, () => /没有生效/.test(document.querySelector("#nm-op")?.textContent ?? ""), null, 10000);
      ok(await txt(p, "#nm-op") === "没有生效：另一项改动正在进行，等它结束再试", `busy: ${await txt(p, "#nm-op")}`);
      // a mode change running: off
      await post("/mock/op", { rollback_enabled: true, active: { op_id: "screen-10", item: "network.mode", phase: "verifying", say_zh: "正在确认", say_en: "Checking" }, last: null });
      await until(p, () => /正在确认/.test(document.querySelector("#nm-now")?.textContent ?? ""), null, 8000);
      ok(await radio("Only_5G").isDisabled() && await apply.isDisabled(), "mode change running: off");
      await idleOp();
      await post("/mock/net");
    },
  },
  mobilenet: {
    path: "/router/mobile-network",
    reset: async () => { await post("/mock/net"); await scenario("normal"); },
    ready: (p) => p.waitForFunction(() => !["", "—"].includes(document.querySelector("#mn-conn-v")?.textContent ?? "") && !document.querySelector("#mn-conn button[role=switch]")?.disabled, null, { timeout: 15000 }),
    async own(p) {
      const sw = (id) => p.locator(`#mn-conn button[aria-labelledby=${id}-l]`);
      const done = (id, re) => until(p, ([id, re]) => new RegExp(re).test(document.querySelector(`#${id}-op`)?.textContent ?? ""), [id, re], 25000);
      ok(await txt(p, "#mn-status strong") === "已连接" && /IPv4 \+ IPv6 · 允许数据漫游。/.test(await txt(p, "#mn-status")), `status: ${await txt(p, "#mn-status")}`);
      ok(await sw("mn-data").getAttribute("aria-checked") === "true" && await sw("mn-roam").getAttribute("aria-checked") === "true", "data and roaming on");

      // mobile data off (tier 2 here, not remote) and back on
      let s0 = await stats();
      await sw("mn-data").click();
      await p.waitForSelector("#mn-data-confirm", { timeout: 8000 }).catch(() => {});
      ok(/连着 U60 的设备会断网/.test(await txt(p, "#mn-data-confirm")), "data off: inline confirm with the consequence");
      await p.click("#mn-data-confirm .nd-btn--confirm");
      await done("mn-data", "已|设备已接受|没有生效");
      ok(await txt(p, "#mn-data-op") === "已关闭移动数据", `data off: ${await txt(p, "#mn-data-op")}`);
      let sent = (await stats()).apn_ops.slice(s0.apn_ops.length);
      ok(sent.length === 1 && sent[0].action === "cellular.set" && sent[0].params.enabled === 0, `cellular.set {enabled: 0} (${JSON.stringify(sent[0]?.params)})`);
      await until(p, () => /移动数据已关/.test(document.querySelector("#mn-status")?.textContent ?? ""));
      ok(await txt(p, "#mn-status strong") === "移动数据已关", "status: data off");
      s0 = await stats();
      await sw("mn-data").click();
      await p.waitForSelector("#mn-data-confirm", { timeout: 8000 }).catch(() => {});
      await p.click("#mn-data-confirm .nd-btn--confirm");
      await done("mn-data", "已打开|设备已接受|没有生效");
      sent = (await stats()).apn_ops.slice(s0.apn_ops.length);
      ok(await txt(p, "#mn-data-op") === "已打开移动数据" && sent[0]?.params.enabled === 1 && sent[0]?.params.connect_mode === "1", `data on: ${await txt(p, "#mn-data-op")} ${JSON.stringify(sent[0]?.params)}`);

      // roaming off
      await sw("mn-roam").click();
      await p.waitForSelector("#mn-roam-confirm", { timeout: 8000 }).catch(() => {});
      await p.click("#mn-roam-confirm .nd-btn--confirm");
      await done("mn-roam", "已|设备已接受|没有生效");
      ok(await txt(p, "#mn-roam-op") === "已关掉数据漫游", `roam off: ${await txt(p, "#mn-roam-op")}`);

      // operator scan (the agent's job): tier 3, progress, the list
      await p.click("button:has-text('扫描运营商')");
      await p.waitForSelector("dialog#mn-scan-confirm[open]", { timeout: 8000 }).catch(() => {});
      ok(/扫描运营商？/.test(await txt(p, "#mn-scan-confirm")) && /实测约 110 秒/.test(await txt(p, "#mn-scan-confirm")), "scan: tier-3 dialog with the downtime");
      await p.screenshot({ path: `${OUT}/b3-mobilenet-dialog.png` });
      await p.click("#mn-scan-confirm .nd-btn--confirm");
      await until(p, () => /会断开…$/.test(document.querySelector("#mn-scan-op")?.textContent ?? ""), null, 8000);
      ok(/^约 1–3 分钟，搜索期间移动数据会断开…$/.test(await txt(p, "#mn-scan-op")), `scan progress: ${await txt(p, "#mn-scan-op")}`);
      ok(await p.locator("button:has-text('扫描运营商')").isDisabled(), "scan running: button off");
      await done("mn-scan", "找到|没有|超时|失败");
      ok(/^找到 \d+ 个运营商$/.test(await txt(p, "#mn-scan-op")), `scan: ${await txt(p, "#mn-scan-op")}`);
      await until(p, () => document.querySelectorAll("#mn-sel .nd-row").length > 0, null, 10000);
      ok(await p.locator("#mn-sel .nd-row").count() >= 2 && /46692 · 5G · 当前/.test(await txt(p, "#mn-sel .nd-row >> nth=0")), `results: ${await txt(p, "#mn-sel .nd-row >> nth=0")}`);

      // register to the first: tier 3, the guard's result
      await p.click("#mn-sel .nd-row >> nth=2 >> button");
      await p.waitForSelector("dialog#mn-reg-confirm[open]", { timeout: 8000 }).catch(() => {});
      ok(/注册到 Taiwan Mobile？/.test(await txt(p, "#mn-reg-confirm")) && /（46697）/.test(await txt(p, "#mn-reg-confirm")), `register dialog: ${(await txt(p, "#mn-reg-confirm")).slice(0, 50)}`);
      await p.click("#mn-reg-confirm .nd-btn--confirm");
      await done("mn-reg", "已注册|没注册上|超时|没有生效");
      ok(await txt(p, "#mn-reg-op") === "已注册到 Taiwan Mobile", `register: ${await txt(p, "#mn-reg-op")}`);
      await until(p, () => document.querySelectorAll("#mn-sel .nd-row").length === 0, null, 10000);
      ok(await p.locator("#mn-sel .nd-row").count() === 0, "the stale list is cleared after the register");

      // back to automatic (tier 2)
      await p.click("button:has-text('回到自动')");
      await p.waitForSelector("#mn-auto-confirm", { timeout: 8000 }).catch(() => {});
      await p.click("#mn-auto-confirm .nd-btn--confirm");
      await done("mn-auto", "已回到|设备已接受|没有生效");
      ok(await txt(p, "#mn-auto-op") === "已回到自动选网", `auto: ${await txt(p, "#mn-auto-op")}`);

      // u60d's search session (e.g. the touch screen): selection off
      await post("/mock/op", { rollback_enabled: true, active: { op_id: "s-1", item: "netselect.session", phase: "applying", say_zh: "正在搜网", say_en: "Searching" }, last: null });
      await until(p, () => document.querySelector("button[data-on\\:click*='scan/ask']")?.disabled, null, 8000);
      ok(await p.locator("button:has-text('扫描运营商')").isDisabled() && /正在搜网/.test(await txt(p, "#mn-sel")), "search session: scan off, said");
      await idleOp();

      // reboot: u60d device.reboot, accepted
      const r0 = (await stats()).reboots;
      await p.click("button:has-text('重启') >> nth=-1");
      await p.waitForSelector("dialog#mn-boot-confirm[open]", { timeout: 8000 }).catch(() => {});
      ok(/重启 U60？/.test(await txt(p, "#mn-boot-confirm")) && /约 90 秒/.test(await txt(p, "#mn-boot-confirm")), "reboot dialog");
      await p.click("#mn-boot-confirm .nd-btn--confirm");
      await done("mn-boot", "设备已接受|没有生效");
      ok(/^设备已接受 · 正在重启/.test(await txt(p, "#mn-boot-op")) && (await stats()).reboots === r0 + 1, `reboot: ${await txt(p, "#mn-boot-op")}`);
      await PAGES.mobilenet.reset();
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
    p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
    await pg.reset?.(); await post("/mock/txn?end=confirmed"); await post("/mock/stuck"); await idleOp(); await sleep(500);
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
    ok(axe.violations.length === 0, `${key} axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length + (process.env.PROBE ? " " + v.nodes.map((n) => n.html + " " + n.failureSummary).join(" / ") : "")).join(", ") || "0"}`);
    ok(errs.length === 0, `${key} ${name}: no page errors ${errs.join("; ")}`);
    await p.screenshot({ path: `${OUT}/b3-${key}-${name}-${lang}-${theme}.png`, fullPage: true });
    await ctx.close();
  }
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
