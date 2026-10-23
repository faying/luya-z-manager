// The 8c-A batch 2 pages. Step 2-1: device info, QCI / bearers, CPU & memory; 2-2: SMS inbox and
// compose (needs MOCK: the mock u60d's made-up inbox, never the recording's); 2-3: USB (needs MOCK
// and AGENT: the fake agent's /__lw/usb hook for mode_settable); 2-4: change log. Later steps add
// their pages to PAGES below. For each page the common checks (first load ≤ 40 KiB gzip without
// fonts, axe light/dark 0, 390 px no sideways scroll, English page body without Chinese, no page
// errors, screenshots) and the page's own. Same servers as step5.mjs; run from source/manager with e2e/run.sh:
//   WEB=http://127.0.0.1:18081 MOCK=http://127.0.0.1:19461 \
//     AGENT=http://127.0.0.1:19090 OUT=/tmp/lw PAGES=cpu crates/luyaz-web/e2e/run.sh batch2
// PAGES = comma list of keys below (default: all).
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19090";
const OUT = process.env.OUT ?? "/tmp/lw";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19461";
const mock = (path) => fetch(MOCK + path, { method: "POST" });
const stats = () => fetch(`${MOCK}/mock/stats`).then((r) => r.json());
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
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
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();

// page key → { path, ready(p) waits for the live content, own(p, ctx) page checks (desktop zh light) }
const PAGES = {
  "device-info": {
    path: "/device-info",
    ready: (p) => p.waitForFunction(() => /\d/.test(document.querySelector("#di-imei .nd-row__value")?.textContent ?? ""), null, { timeout: 15000 }),
    async own(p, ctx) {
      ok(await p.locator("main .nd-group").count() === 4, "four groups (identity, network, system, versions)");
      ok(await txt(p, "#di-ver-core .nd-row__value") === "1.0.0" && /已是最新/.test(await txt(p, "#di-ver-core")), "versions: core 1.0.0, up to date");
      ok(/有新版：1\.1\.0/.test(await txt(p, "#di-ver-touch")), `versions: touch has a newer one: ${await txt(p, "#di-ver-touch")}`);
      ok(await txt(p, "#di-ver-scripts .nd-row__value") === "未编号", "versions: scripts from before 1.0.0 say so");
      ok(/(\d+ (分钟|小时|天)前|刚刚)$/.test(await txt(p, "#di-ver-checked .nd-row__value")), `versions: last check: ${await txt(p, "#di-ver-checked .nd-row__value")}`);
      await p.click("#di-versions-err button");
      await p.waitForFunction(() => /刚刚/.test(document.querySelector("#di-ver-checked")?.textContent ?? ""), null, { timeout: 15000 }).catch(() => {});
      ok(/刚刚/.test(await txt(p, "#di-ver-checked .nd-row__value")), "versions: 检查更新 asks u60d and the row follows");
      ok(await p.locator("main .nd-skel").count() === 0, "no skeleton left");
      for (const id of ["imei", "imsi", "iccid", "wan4", "gw", "dns", "wan6", "lan", "host", "kernel", "uptime", "memtotal", "memfree", "memavail"]) {
        const v = await txt(p, `#di-${id} .nd-row__value`);
        ok(v && v !== "—", `${id}: ${id.match(/imei|imsi|iccid|wan|gw|dns|lan/) ? "(set)" : v}`);
      }
      ok(/^(就绪|需要|没有 SIM)/.test(await txt(p, "#di-simstate .nd-row__value")), `SIM state worded: ${await txt(p, "#di-simstate .nd-row__value")}`);
      ok(await txt(p, "#di-nettype .nd-row__value") !== "—" && await txt(p, "#di-operator .nd-row__value") !== "—", "operator and network type");
      ok(await txt(p, "#di-kernel .nd-row__value") === "5.4.210" && /^Linux version 5\.4\.210/.test(await txt(p, "#di-kernel .nd-row__sub")), "kernel release + full line (agent /api/device)");
      ok(/\d+ (天|小时|分钟)/.test(await txt(p, "#di-uptime .nd-row__value")), `uptime: ${await txt(p, "#di-uptime .nd-row__value")}`);
      ok(/^\d+ MB$/.test(await txt(p, "#di-memtotal .nd-row__value")), "memory in MB");
      ok(await p.locator("[id$=-err] [role=alert]").count() === 0, "nothing listed as unread");
      // live: a push changes something (memory free / uptime follow the live block)
      const before = await txt(p, "#di-memfree .nd-row__value") + await txt(p, "#di-uptime .nd-row__value");
      await p.waitForFunction((b) => (document.querySelector("#di-memfree .nd-row__value")?.textContent ?? "") + (document.querySelector("#di-uptime .nd-row__value")?.textContent ?? "") !== b, before, { timeout: 30000 }).catch(() => {});
      ok(before !== await txt(p, "#di-memfree .nd-row__value") + await txt(p, "#di-uptime .nd-row__value"), "values move with pushes");
      // agent down at load: the kernel row says so, Retry recovers it
      await scenario("down");
      const q = await ctx.newPage();
      await q.goto(WEB + "/device-info", { waitUntil: "load" });
      await q.waitForSelector("#di-system-err [role=alert]", { timeout: 15000 }).catch(() => {});
      // the fake agent's front answers 502 when the mock drops the socket; the real one is refused
      ok(/读取失败：内核（(连不上后台|HTTP 502)）/.test(await txt(q, "#di-system-err")), `agent down: ${await txt(q, "#di-system-err")}`);
      ok(/\d/.test(await txt(q, "#di-imei .nd-row__value")), "agent down: the u60d rows still show");
      await scenario("normal");
      await q.click("#di-system-err button");
      await q.waitForFunction(() => /5\.4\.210/.test(document.querySelector("#di-kernel")?.textContent ?? ""), null, { timeout: 8000 }).catch(() => {});
      ok(await txt(q, "#di-kernel .nd-row__value") === "5.4.210" && await q.locator("#di-system-err [role=alert]").count() === 0, "Retry reads the kernel line, the error goes");
      await q.close();
    },
  },
  qci: {
    path: "/router/qci",
    // the table is an AT read: only on a press (an open tab alone never reads)
    ready: async (p) => {
      await p.waitForSelector("#qc-bearers button", { timeout: 20000 });
      await p.click("#qc-bearers button");
      await p.waitForSelector("#qc-bearers tbody tr", { timeout: 20000 });
    },
    async own(p, ctx) {
      ok(/\d+ 个承载在用/.test(await txt(p, "#qc-status")), `status: ${await txt(p, "#qc-status .nd-status__state")}`);
      const rows = await p.locator("#qc-bearers tbody tr").count();
      ok(rows >= 1, `bearer rows: ${rows}`);
      ok(/基带报告|实测|根据 APN 推断/.test(await txt(p, "#qc-bearers tbody")), "source column worded");
      ok(/IMS 信令|基于 TCP/.test(await txt(p, "#qc-bearers tbody")), "QCI labels");
      ok(/(?:QCI|5QI) \d+ · ↓ [\d.]+ ↑ [\d.—]+ Mbps/.test(await txt(p, "#qc-now")), `u60d summary: ${await txt(p, "#qc-now .nd-row__value")}`);
      ok(/和设备「关于」页显示的是同一个值/.test(await txt(p, "#qc-bearers")), "the note");
      // raw toggle
      const btn = p.locator("button[aria-controls=qc-raw-out]");
      ok(await btn.getAttribute("aria-expanded") === "false" && !(await p.isVisible("#qc-raw-out")), "raw output hidden at first");
      await btn.click();
      ok(await btn.getAttribute("aria-expanded") === "true" && /\+CGCONTRDP:/.test(await txt(p, "#qc-raw")), "toggle shows the raw AT+CGCONTRDP");
      ok(/隐藏原始/.test(await btn.innerText()), "toggle reads Hide");
      // a second tab uses the same read: it shows the table without its own 12 s wait
      const q = await ctx.newPage();
      const t0 = Date.now();
      await q.goto(WEB + "/router/qci", { waitUntil: "load" });
      const shared = await q.locator("#qc-bearers tbody tr").count();
      ok(shared === rows, `second tab: table in its first render (${shared} rows, ${Date.now() - t0} ms)`);
      await q.close();
      // agent down: the next (pressed) read fails, the table stays greyed with the failure said
      await scenario("down");
      await p.waitForSelector("#qc-bearers button", { timeout: 20000 });
      await p.click("#qc-bearers button");
      await p.waitForFunction(() => /最近一次刷新失败/.test(document.querySelector("#qc-bearers")?.textContent ?? ""), null, { timeout: 40000 }).catch(() => {});
      ok(/最近一次刷新失败：(连不上后台|HTTP 502)。显示的是上一次的读数。/.test(await txt(p, "#qc-bearers")), `agent down: ${await txt(p, "#qc-bearers p[role=alert]")}`);
      ok(await p.locator("#qc-bearers tbody tr").count() === rows && /数字停在 \d\d:\d\d/.test(await txt(p, "#qc-status")), "table kept, numbers stopped at hh:mm");
      ok(await btn.getAttribute("aria-expanded") === "true", "raw toggle survives the pushes");
      await scenario("normal");
      await p.click("#qc-status .nd-status__actions button");
      await p.waitForFunction(() => !/最近一次刷新失败/.test(document.querySelector("#qc-bearers")?.textContent ?? ""), null, { timeout: 10000 }).catch(() => {});
      ok(!/刷新失败/.test(await txt(p, "#qc-bearers")) && /承载在用/.test(await txt(p, "#qc-status")), `Retry: ${await txt(p, "#qc-status .nd-status__state")}`);
    },
  },
  cpu: {
    path: "/tools/cpu",
    ready: (p) => p.waitForFunction(() => /CPU \d+\.\d%/.test(document.querySelector("#cpu-status")?.textContent ?? ""), null, { timeout: 15000 }),
    async own(p) {
      ok(/内存已用 \d+ MiB \/ \d+ MiB（\d+%）/.test(await txt(p, "#cpu-status")), `status: ${await txt(p, "#cpu-status")}`);
      const cores = await p.locator("#cpu-cores .nd-row").count();
      ok(cores >= 1, `core rows: ${cores}`);
      ok(/CPU 0 ?[\d.]+% · \d+ \/ \d+ MHz/.test(await txt(p, "#cpu-cores")), `core line: ${await txt(p, "#cpu-cores .nd-row >> nth=0")}`);
      ok(await p.locator("#cpu-mem .nd-row").count() === 4 && /已用.*可用.*缓存.*总量/.test(await txt(p, "#cpu-mem")), "memory: used, available, cache, total");
      await p.waitForSelector("#cpu-line polyline", { state: "attached", timeout: 20000 }).catch(() => {});
      const a = await p.getAttribute("#cpu-line polyline", "points").catch(() => null);
      ok(a && a.split(" ").length >= 2, `trend drawn (${a?.split(" ").length ?? 0} points)`);
      const s1 = await txt(p, "#cpu-status .nd-status__state");
      await p.waitForFunction((s) => document.querySelector("#cpu-status .nd-status__state")?.textContent?.trim() !== s, s1, { timeout: 30000 }).catch(() => {});
      ok(s1 !== await txt(p, "#cpu-status .nd-status__state"), `overall moves with pushes (${s1} → ${await txt(p, "#cpu-status .nd-status__state")})`);
    },
  },
  // before sms: its session-expiry check logs in again and the mock agent honours only the newest
  // token, so later pages read the agent as signed out
  usb: {
    path: "/usb",
    // the u60d rows (battery block) have arrived
    ready: (p) => p.waitForFunction(() => (document.querySelector("#usb-chgconn .nd-row__value")?.textContent ?? "—") !== "—" && /USB/.test(document.querySelector("#usb-status")?.textContent ?? ""), null, { timeout: 15000 }),
    async own(p, ctx) {
      const lw = (q) => fetch(`${AGENT}/__lw/usb?${q}`, { method: "POST" });
      const radios = p.locator("#usb-modes [role=radio]");
      const usbOps = async () => (await stats()).usb_ops ?? [];
      // what the B31 recording says: mode "user", nothing plugged in, fast charging off
      ok(/没插 USB 线 · 模式 USER/.test(await txt(p, "#usb-status")), `status: ${await txt(p, "#usb-status .nd-status__state")}`);
      ok(/当前模式：user/.test(await txt(p, "#usb-modes")) && await p.locator("#usb-modes [aria-checked=true]").count() === 0, "mode from u60d qos: user, no radio selected");
      ok(await radios.count() === 3 && await radios.nth(0).isEnabled() && !/这版固件/.test(await txt(p, "#usb-modes")), "mode_settable absent: like the old page");
      ok(await txt(p, "#usb-cable .nd-row__value") === "已断开" && await txt(p, "#usb-cc .nd-row__value") === "cc1" && await txt(p, "#usb-rj45 .nd-row__value") === "没有", "port rows (agent)");
      ok(await txt(p, "#usb-moderow .nd-row__value") === "USER", "mode row from u60d");
      ok(/^(是|否)$/.test(await txt(p, "#usb-chgconn .nd-row__value")) && /^\d+$/.test(await txt(p, "#usb-chgtype .nd-row__value")) && /^\d+$/.test(await txt(p, "#usb-chgstat .nd-row__value")), "charger rows (battery block)");
      ok(/disable/.test(await txt(p, "#usb-direct")) && /允许充电/.test(await txt(p, "#usb-direct")), "direct power + meaning (charger block)");
      const sw = p.locator("#usb-pbrow [role=switch]");
      ok(await sw.isDisabled() && /先在 USB-C 口插上手机/.test(await txt(p, "#usb-pbrow")), "nothing plugged in: fast charging off-limits, said why");

      // tier 3 mode: the dialog; Cancel and Esc send nothing
      let n0 = (await usbOps()).length;
      await radios.nth(2).click();
      await p.waitForSelector("dialog#usb-mode-confirm[open]", { timeout: 5000 }).catch(() => {});
      const dlg = await txt(p, "#usb-mode-confirm");
      ok(/把 USB 切换到 RNDIS？/.test(dlg) && /会发生什么/.test(dlg) && /断开多久.*USB 连接会断开，几秒后恢复/.test(dlg) && /怎么恢复.*拔下线再插上.*（USER）/.test(dlg), "tier-3 dialog: what, how long, how to recover");
      ok(await p.evaluate(() => document.activeElement?.textContent?.trim()) === "取消", "focus on Cancel");
      await p.screenshot({ path: `${OUT}/b2-usb-dialog.png` });
      const ax = await new AxeBuilder({ page: p }).analyze();
      ok(ax.violations.length === 0, `mode dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
      await p.click("#usb-mode-confirm button:has-text('取消')");
      await sleep(600);
      await radios.nth(0).click();
      await p.waitForSelector("dialog#usb-mode-confirm[open]", { timeout: 5000 }).catch(() => {});
      ok(await p.locator("#usb-mode-confirm .nd-btn--danger").count() === 1 && /root shell/.test(await txt(p, "#usb-mode-confirm")), "DEBUG: danger button, root shell warning");
      await p.keyboard.press("Escape");
      await sleep(600);
      ok(await p.locator("dialog#usb-mode-confirm[open]").count() === 0 && (await usbOps()).length === n0, "Cancel and Esc: closed, nothing sent");
      // confirm: one usb.set, read back from qos.usb_mode
      await radios.nth(1).click();
      await p.waitForSelector("dialog#usb-mode-confirm[open]", { timeout: 5000 }).catch(() => {});
      await p.click("#usb-mode-confirm button:has-text('切换到 MTP')");
      await p.waitForFunction(() => /USB 已切到 MTP/.test(document.querySelector("#usb-mode-op")?.textContent ?? ""), null, { timeout: 20000 }).catch(() => {});
      ok(/USB 已切到 MTP/.test(await txt(p, "#usb-mode-op")), `read back: ${await txt(p, "#usb-mode-op")}`);
      ok(await radios.nth(1).getAttribute("aria-checked") === "true" && !/当前模式/.test(await txt(p, "#usb-modes")), "MTP selected, the other-mode line gone");
      let ops = (await usbOps()).slice(n0);
      ok(ops.length === 1 && ops[0].action === "usb.set" && ops[0].params.mode === "mtp" && ops[0].source === "web" && /^lw-/.test(ops[0].op_id), `one usb.set {mode: mtp}: ${JSON.stringify(ops.map((o) => o.params))}`);

      // the reply drops mid-write (a computer on USB loses its link): one send, then the area asks
      // again with the target and reads the mode before saying 「连接中断」
      // (in a tab of its own: Datastar logs the dropped reply as a FetchFailed page error, write.rs)
      n0 = (await usbOps()).length;
      const q = await ctx.newPage();
      await q.goto(WEB + "/usb", { waitUntil: "load" });
      await q.waitForSelector("#usb-modes [role=radio][aria-checked=true]", { timeout: 15000 }).catch(() => {});
      await q.route("**/usb/mode/set?*", async (route) => { await route.fetch().catch(() => {}); await route.abort(); });
      await q.locator("#usb-modes [role=radio]").nth(2).click();
      await q.waitForSelector("dialog#usb-mode-confirm[open]", { timeout: 5000 }).catch(() => {});
      await q.click("#usb-mode-confirm button:has-text('切换到 RNDIS')");
      await q.waitForFunction(() => /USB 已切到 RNDIS|连接中断/.test(document.querySelector("#usb-mode-op")?.textContent ?? ""), null, { timeout: 40000 }).catch(() => {});
      ok(/USB 已切到 RNDIS/.test(await txt(q, "#usb-mode-op")), `reply dropped, mode read on the lost post: ${await txt(q, "#usb-mode-op")}`);
      await q.close();
      ops = (await usbOps()).slice(n0);
      ok(ops.length === 1 && ops[0].params.mode === "rndis", `reply dropped: sent once (${ops.length})`);

      // mode_settable false: read-only before any press; the server never sends it either
      await lw("settable=false");
      await p.reload({ waitUntil: "load" });
      await sleep(1500);
      ok(/这版固件不能改 USB 模式/.test(await txt(p, "#usb-modes")), "false: 「这版固件不能改 USB 模式」 on load");
      ok(await p.locator("#usb-modes [role=radio]:disabled").count() === 3, "false: all three disabled");
      await p.screenshot({ path: `${OUT}/b2-usb-fixed.png` });
      n0 = (await usbOps()).length;
      const ds = { cookie: `lw_sid=${sid}`, origin: new URL(WEB).origin, "datastar-request": "true" };
      for (const u of ["/usb/mode/ask?mode=rndis", "/usb/mode/set?mode=rndis"]) {
        const body = await (await fetch(WEB + u, { method: "POST", headers: ds })).text();
        ok(/这版固件不能改 USB 模式/.test(body), `direct POST ${u}: refused with the firmware line`);
      }
      ok((await usbOps()).length === n0, "false: no usb.set reached u60d");
      await lw("settable=true");
      await p.reload({ waitUntil: "load" });
      await sleep(1500);
      ok(!/这版固件/.test(await txt(p, "#usb-modes")) && await p.locator("#usb-modes [role=radio]:disabled").count() === 0, "true: switchable, no firmware line");
      await lw("settable=absent");

      // fast charging, tier 2: plug in, inline confirm, Cancel sends nothing, confirm reads back
      await mock("/mock/usb?cc=1");
      await p.waitForFunction(() => !document.querySelector("#usb-pbrow [role=switch]")?.disabled, null, { timeout: 10000 }).catch(() => {});
      ok(await sw.isEnabled() && /18W 快充/.test(await txt(p, "#usb-pbrow")), "plugged in: the switch works, the hint says what it does");
      n0 = (await usbOps()).length;
      await sw.click();
      await p.waitForSelector("#usb-pb-confirm", { timeout: 5000 }).catch(() => {});
      ok(/USB 口上的手机改用 U60 的电池快充（18W），这根线不再给它上网。/.test(await txt(p, "#usb-pb-confirm")), "tier-2 confirm: the consequence");
      ok(await p.evaluate(() => document.activeElement?.textContent?.trim()) === "取消", "focus on Cancel");
      await p.click("#usb-pb-confirm button:has-text('取消')");
      await sleep(600);
      ok(await p.locator("#usb-pb-confirm").count() === 0 && (await usbOps()).length === n0, "Cancel: nothing sent");
      await sw.click();
      await p.waitForSelector("#usb-pb-confirm", { timeout: 5000 }).catch(() => {});
      await p.click("#usb-pb-confirm button:has-text('确认：打开快速充电宝')");
      await p.waitForFunction(() => /已打开快速充电宝/.test(document.querySelector("#usb-pb-op")?.textContent ?? ""), null, { timeout: 20000 }).catch(() => {});
      ok(/已打开快速充电宝/.test(await txt(p, "#usb-pb-op")), `read back: ${await txt(p, "#usb-pb-op")}`);
      await p.waitForFunction(() => document.querySelector("#usb-pbrow [role=switch]")?.getAttribute("aria-checked") === "true", null, { timeout: 5000 }).catch(() => {});
      ok(await sw.getAttribute("aria-checked") === "true" && /快速充电宝开着/.test(await txt(p, "#usb-status")), "switch on, the status says so");
      ops = (await usbOps()).slice(n0);
      ok(ops.length === 1 && ops[0].action === "vendor.call" && ops[0].params.object === "zwrt_bsp.powerbank" && ops[0].params.method === "set" && ops[0].params.args.state === 1, `one vendor.call powerbank set {state: 1}: ${JSON.stringify(ops.map((o) => o.params))}`);
      // u60d takes it, the block never shows it: 设备已接受, not success
      await mock("/mock/control?noreadback=true");
      await sw.click();
      await p.waitForSelector("#usb-pb-confirm", { timeout: 5000 }).catch(() => {});
      await p.click("#usb-pb-confirm button:has-text('确认：关掉快速充电宝')");
      await p.waitForFunction(() => /设备已接受/.test(document.querySelector("#usb-pb-op")?.textContent ?? ""), null, { timeout: 25000 }).catch(() => {});
      const acc = await txt(p, "#usb-pb-op");
      ok(/设备已接受，还没读回/.test(acc) && !/成功/.test(acc), `accepted, not read back: ${acc}`);
      const ids = (await usbOps()).map((o) => o.op_id).filter((i) => i); // the run's own resets have none
      ok(new Set(ids).size === ids.length, `every write its own op_id (${ids.length})`);
      // unplugged: off-limits again, and a direct POST is not sent
      await mock("/mock/usb?cc=0");
      await p.waitForFunction(() => document.querySelector("#usb-pbrow [role=switch]")?.disabled, null, { timeout: 10000 }).catch(() => {});
      ok(await sw.isDisabled(), "unplugged: switch disabled");
      n0 = (await usbOps()).length;
      const body = await (await fetch(WEB + "/usb/pb/set?on=true", { method: "POST", headers: ds })).text();
      ok(/先在 USB-C 口插上手机/.test(body) && (await usbOps()).length === n0, "direct POST while unplugged: said why, nothing sent");
    },
  },
  sms: {
    path: "/sms",
    ready: (p) => p.waitForSelector("#sms-list .sm-row", { timeout: 20000 }),
    async own(p, ctx) {
      // (the made-up inbox was reset before this page loaded: see the loop)
      const rows = () => p.locator("#sms-list .sm-row:visible").count();
      ok(await rows() === 6, `six rows (${await rows()})`);
      ok(/2 条未读$/.test(await txt(p, "#sms-status .nd-status__state")), `status: ${await txt(p, "#sms-status .nd-status__state")}`);
      ok(/SIM 已用 \d+ \/ \d+ · 设备已用 \d+ \/ \d+ · 总已用 \d+ \/ \d+ · 未读 2/.test(await txt(p, "#sms-status .nd-status__reason")), "capacity line (agent) + unread");
      for (const w of ["未读", "已发送", "发送失败"]) ok(await p.locator(`#sms-list .sm-word:has-text("${w}")`).count() > 0, `word ${w}`);
      ok(await txt(p, "#sms-r27 .sm-from") === "CMHK", "a sender in UCS-2 hex is decoded");
      ok((await txt(p, "#sms-r27 .sm-meta")).endsWith("SIM") && (await txt(p, "#sms-r29 .sm-meta")).endsWith("设备"), "rows say their store");
      // filter: client-side, clears the selection
      const radios = p.locator("[role=radiogroup][aria-label=存储] [role=radio]");
      ok(await radios.count() === 3 && await p.locator("[role=radiogroup]").isVisible(), "store filter shown (every row knows its store)");
      await p.check("#sms-r29 input[name=id]");
      await radios.nth(1).click();
      ok(await rows() === 2 && await p.getAttribute("[role=radio]:nth-child(2)", "aria-checked") === "true", `SIM: two rows (${await rows()})`);
      ok(await p.locator("#sms-list input[name=id]:checked").count() === 0, "a filter change clears the selection");
      await radios.nth(2).click();
      ok(await rows() === 4, `device: four rows (${await rows()})`);
      await radios.nth(0).click();

      // tier 1 that u60d takes but the list never shows: 设备已接受, not success
      await mock("/mock/control?noreadback=true");
      await p.check("#sms-r31 input[name=id]");
      ok(/标为已读 \(1\)/.test(await txt(p, ".sm-acts")) && /删除 \(1\)/.test(await txt(p, ".sm-acts")), `selection counted: ${await txt(p, ".sm-acts")}`);
      await p.click(".sm-acts button[data-write=sms-read]");
      await p.waitForFunction(() => /设备已接受/.test(document.querySelector("#sms-read-op")?.textContent ?? ""), null, { timeout: 25000 }).catch(() => {});
      const acc = await txt(p, "#sms-read-op");
      ok(/设备已接受，还没读回/.test(acc) && !/成功/.test(acc), `accepted, not read back: ${acc}`);
      ok(await p.locator("#sms-list input[name=id]:checked").count() === 0 && !(await p.locator(".sm-acts").isVisible()), "selection cleared after the write");

      // open an unread row: the dialog, then its mark follows the write (tier 1)
      let s0 = await stats();
      await p.click("#sms-r31 .sm-open");
      await p.waitForSelector("dialog#sms-view[open]", { timeout: 5000 }).catch(() => {});
      ok(await p.locator("dialog#sms-view[open]").count() === 1, "the message opens in a dialog");
      ok(/验证码/.test(await txt(p, "#sms-view .sm-body")), "whole text in the dialog");
      await p.screenshot({ path: `${OUT}/b2-sms-open.png` });
      await p.waitForFunction(() => document.querySelector("#sms-view-mark span:last-child")?.textContent === "已读", null, { timeout: 20000 }).catch(() => {});
      ok(await txt(p, "#sms-view-mark span:last-child") === "已读", `opened unread → 已读 (${await txt(p, "#sms-view-mark")})`);
      let s1 = await stats();
      ok(s1.sms_ops.slice(s0.sms_ops.length).filter((a) => a === "sms.mark_read").length === 1, "one mark_read for the open");
      const focusIn = await p.evaluate(() => document.activeElement?.closest("dialog")?.id);
      ok(focusIn === "sms-view", "focus inside the dialog");
      await p.keyboard.press("Escape");
      ok(await p.locator("dialog#sms-view[open]").count() === 0, "Esc closes it");
      await p.waitForFunction(() => /1 条未读$/.test(document.querySelector("#sms-status .nd-status__state")?.textContent?.trim() ?? ""), null, { timeout: 10000 }).catch(() => {});
      ok(/1 条未读$/.test(await txt(p, "#sms-status .nd-status__state")), `status follows: ${await txt(p, "#sms-status .nd-status__state")}`);
      // a two-line message keeps its line break
      await p.click("#sms-r30 .sm-open");
      await p.waitForSelector("dialog#sms-view[open]", { timeout: 5000 }).catch(() => {});
      ok(await p.locator("#sms-view .sm-body br").count() === 1, "line break kept");
      await p.keyboard.press("Escape");
      await p.waitForFunction(() => /没有未读短信/.test(document.querySelector("#sms-status")?.textContent ?? ""), null, { timeout: 15000 }).catch(() => {});

      // 标为已读 (tier 1) read back: none unread now, so reset one: open marks happen above; use a fresh inbox
      await mock("/mock/sms?count=0");
      await p.waitForFunction(() => /2 条未读$/.test(document.querySelector("#sms-status .nd-status__state")?.textContent?.trim() ?? ""), null, { timeout: 15000 }).catch(() => {});
      await p.check("#sms-r31 input[name=id]");
      await p.check("#sms-r30 input[name=id]");
      await p.check("#sms-r29 input[name=id]");
      ok(/标为已读 \(2\)/.test(await txt(p, ".sm-acts")) && /删除 \(3\)/.test(await txt(p, ".sm-acts")), `unread counted apart: ${await txt(p, ".sm-acts")}`);
      s0 = await stats();
      await p.click(".sm-acts button[data-write=sms-read]");
      await p.waitForFunction(() => /已把 2 条标为已读/.test(document.querySelector("#sms-read-op")?.textContent ?? ""), null, { timeout: 20000 }).catch(() => {});
      ok(/已把 2 条标为已读/.test(await txt(p, "#sms-read-op")), `mark read read back: ${await txt(p, "#sms-read-op")}`);
      s1 = await stats();
      ok(s1.sms_ops.slice(s0.sms_ops.length).filter((a) => a === "sms.mark_read").length === 1, "one write for the selection");
      await p.waitForFunction(() => /没有未读短信/.test(document.querySelector("#sms-status")?.textContent ?? ""), null, { timeout: 15000 }).catch(() => {});
      ok(/没有未读短信/.test(await txt(p, "#sms-status .nd-status__state")), "status: no unread");

      // 删除 (tier 3): a dialog; Cancel sends nothing; confirm deletes (NV by sms.delete, SIM by db_delete)
      await p.check("#sms-r29 input[name=id]");
      await p.check("#sms-r27 input[name=id]");
      s0 = await stats();
      await p.click(".sm-acts .nd-btn--danger");
      await p.waitForSelector("dialog#sms-del-confirm[open]", { timeout: 5000 }).catch(() => {});
      const dlg = await txt(p, "#sms-del-confirm");
      ok(/删除 2 条短信？/.test(dlg) && /的 2 条短信会被删除，删除后没法恢复。/.test(dlg), "tier-3 dialog: how many, can't be undone");
      ok(await p.evaluate(() => document.activeElement?.textContent?.trim()) === "取消", "focus on Cancel");
      await p.screenshot({ path: `${OUT}/b2-sms-delete.png` });
      const ax = await new AxeBuilder({ page: p }).analyze();
      ok(ax.violations.length === 0, `delete dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
      await p.click("#sms-del-confirm button:has-text('取消')");
      await sleep(800);
      ok(await p.locator("dialog#sms-del-confirm[open]").count() === 0, "Cancel closes it");
      s1 = await stats();
      ok(!s1.sms_ops.slice(s0.sms_ops.length).some((a) => a !== "sms.list_after"), "Cancel: nothing written");
      ok(await p.locator("#sms-list input[name=id]:checked").count() === 2, "selection kept after Cancel");
      await p.click(".sm-acts .nd-btn--danger");
      await p.waitForSelector("dialog#sms-del-confirm[open]", { timeout: 5000 }).catch(() => {});
      await p.click("#sms-del-confirm .nd-btn--danger");
      await p.waitForFunction(() => /已删除 2 条短信/.test(document.querySelector("#sms-del-op")?.textContent ?? ""), null, { timeout: 25000 }).catch(() => {});
      ok(/已删除 2 条短信/.test(await txt(p, "#sms-del-op")), `delete read back: ${await txt(p, "#sms-del-op")}`);
      s1 = await stats();
      const w = s1.sms_ops.slice(s0.sms_ops.length).filter((a) => a !== "sms.list_after");
      ok(JSON.stringify(w) === JSON.stringify(["sms.delete", "sms.db_delete"]), `sms.delete, then db_delete for the SIM survivor (${w.join(", ")})`);
      ok(JSON.stringify(s1.sms_ids) === "[31,30,28,26]", "rows gone in the mock");
      await p.waitForFunction(() => !document.querySelector("#sms-r29") && !document.querySelector("#sms-r27"), null, { timeout: 10000 }).catch(() => {});
      ok(await p.locator("#sms-r29, #sms-r27").count() === 0, "rows gone from the page");
      const ids = s1.op_ids.filter((x) => x.startsWith("lw-"));
      ok(new Set(ids).size === ids.length, "every write its own op_id, none resent");

      // session expired: the stream says so, then a button does too, and nothing is written
      const r2 = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
      const sid2 = /lw_sid=([0-9a-f]+)/.exec(r2.headers.get("set-cookie") ?? "")?.[1];
      const c2 = await p.context().browser().newContext({ viewport: { width: 1280, height: 900 } });
      await c2.addCookies([{ name: "lw_sid", value: sid2, url: WEB }]);
      const q = await c2.newPage();
      await q.goto(WEB + "/sms", { waitUntil: "load" });
      await q.waitForSelector("#sms-list .sm-row", { timeout: 15000 }).catch(() => {});
      await fetch(`${WEB}/logout`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, cookie: `lw_sid=${sid2}` } });
      await q.waitForSelector("#lw-expired dialog[open]", { timeout: 8000 }).catch(() => {});
      ok(/登录已过期/.test(await txt(q, "#lw-expired")), "expired: the open stream says so");
      await q.keyboard.press("Escape");
      s0 = await stats();
      await q.check("#sms-r31 input[name=id]");
      await q.click(".sm-acts .nd-btn--danger");
      await q.waitForSelector("#lw-expired dialog[open]", { timeout: 5000 }).catch(() => {});
      ok(/登录已过期/.test(await txt(q, "#lw-expired")) && /next=%2Fsms|next=\/sms/.test(await q.getAttribute("#lw-expired a", "href")), "expired: a button says so, with the way back");
      s1 = await stats();
      ok(s1.sms_ops.length === s0.sms_ops.length, "expired: nothing sent to u60d");
      await c2.close();

      // a big inbox: the first screen does not carry the rows
      await mock("/mock/sms?count=200");
      const q2 = await ctx.newPage();
      await q2.goto(WEB + "/sms", { waitUntil: "load" });
      await q2.waitForFunction(() => document.querySelectorAll("#sms-list .sm-row").length === 200, null, { timeout: 20000 }).catch(() => {});
      const kb = (await firstLoad(q2)) / 1024;
      ok(kb <= 40 && await q2.locator("#sms-list .sm-row").count() === 200, `200 messages: first load ${kb.toFixed(1)} KiB, rows come with the push`);
      await q2.close();
      await mock("/mock/sms?count=0");
    },
  },
  compose: {
    path: "/sms/compose",
    ready: (p) => p.waitForSelector("#smsc-body", { timeout: 10000 }),
    async own(p) {
      const send = p.locator("button:has-text('发送')").first();
      ok(await send.isDisabled(), "Send off while empty");
      await p.fill("#smsc-to", "12a");
      await p.fill("#smsc-body", "hi");
      await send.click();
      await p.waitForSelector("#smsc-err [role=alert]", { timeout: 5000 }).catch(() => {});
      ok(await txt(p, "#smsc-err") === "号码只能是数字，可以用 + 开头，3 到 20 位。", "a bad number: the old sentence");
      await p.fill("#smsc-to", "+886 912-000-451");
      await p.fill("#smsc-body", "测试 e2e");
      ok(/^6 个字符 · 1 条/.test(await txt(p, "#smsc-count")), `counter: ${await txt(p, "#smsc-count")}`);
      await p.fill("#smsc-body", "字".repeat(71));
      ok(/^71 个字符 · 2 条/.test(await txt(p, "#smsc-count")), "UCS-2 parts: 71 → 2");
      await p.fill("#smsc-body", "测试 e2e");
      let s0 = await stats();
      await send.click();
      await p.waitForSelector("#smsc-confirm", { timeout: 5000 }).catch(() => {});
      ok(/把 6 个字符发给 \+886912000451，分 1 条短信经移动网络发出，运营商可能按条收费。发出后没法撤回。/.test(await txt(p, "#smsc-confirm")), `tier 2: ${await txt(p, "#smsc-confirm")}`);
      ok(await txt(p, "#smsc-err") === "", "the error line went");
      await sleep(400);
      await p.screenshot({ path: `${OUT}/b2-compose-confirm.png` });
      ok(await p.evaluate(() => document.activeElement?.textContent?.trim()) === "取消", "focus on Cancel");
      await p.click("#smsc-confirm button:has-text('取消')");
      await sleep(600);
      ok(await p.locator("#smsc-confirm").count() === 0, "Cancel closes the confirm");
      // typing closes an open confirm; a send whose text changed since it was confirmed is refused
      await send.click();
      await p.waitForSelector("#smsc-confirm", { timeout: 5000 }).catch(() => {});
      await p.type("#smsc-body", "!");
      ok(await p.locator("#smsc-confirm").count() === 0, "typing closes the confirm");
      const h = /h=([0-9a-f]+)/.exec(await p.evaluate(() => document.querySelector("#smsc-op")?.innerHTML ?? "") ?? "")?.[1];
      const bad = await fetch(`${WEB}/sms/compose/send?h=0000000000000000`, { method: "POST", headers: { origin: new URL(WEB).origin, cookie: `lw_sid=${sid}`, "datastar-request": "true", "content-type": "application/json" }, body: JSON.stringify({ to: "+886912000451", body: "测试 e2e" }) }).then((r) => r.text());
      ok(/没有发出：确认之后号码或内容又改了/.test(bad) && h === undefined, "a stale confirm is not sent");
      let s1 = await stats();
      ok(s1.sms_sends.length === s0.sms_sends.length, "Cancel / stale: nothing sent");
      await p.fill("#smsc-body", "测试 e2e");
      await send.click();
      await p.waitForSelector("#smsc-confirm", { timeout: 5000 }).catch(() => {});
      await p.click("#smsc-confirm .nd-btn--confirm");
      await p.waitForFunction(() => /设备已接受/.test(document.querySelector("#smsc-op")?.textContent ?? ""), null, { timeout: 20000 }).catch(() => {});
      const res = await txt(p, "#smsc-op");
      ok(res === "设备已接受：发给 +886912000451 的短信", `accepted, never success (${res.includes("成功") ? "says 成功" : "ok"})`);
      s1 = await stats();
      const sent = s1.sms_sends.slice(s0.sms_sends.length);
      ok(sent.length === 1, `one send (${sent.length})`);
      ok(sent[0]?.number === "+886912000451" && sent[0]?.message_hex === "6D4B8BD50020006500320065" && /^\d\d(;\d\d){5};[+-]?\d+$/.test(sent[0]?.sms_time ?? ""), "sent: number, UCS-2 hex, sms_time format");
      ok(await p.inputValue("#smsc-to") === "" && await p.inputValue("#smsc-body") === "", "the form empties after the send");
      const ids = s1.op_ids.filter((x) => x.startsWith("lw-"));
      ok(new Set(ids).size === ids.length, "own op_id, not resent");
    },
  },
  changes: {
    path: "/changes",
    ready: (p) => p.waitForSelector("#ch-main a.nd-row", { timeout: 20000 }),
    async own(p, ctx) {
      const post = (path, body) => fetch(MOCK + path, { method: "POST", headers: { "content-type": "application/json" }, body: body && JSON.stringify(body) });
      const idleOp = () => post("/mock/op", { rollback_enabled: true, active: null, last: null });
      const rows = p.locator("#ch-main a.nd-row");
      ok(await rows.count() === 5, `five rows, the hidden op.ack not listed (${await rows.count()})`);
      ok(await txt(p, "#ch-main a.nd-row >> nth=0 >> .nd-row__label") === "●制式 · 自动 → 只用 4G", `line 1: ${await txt(p, "#ch-main a.nd-row >> nth=0 >> .nd-row__label")}`);
      ok(await txt(p, "#ch-main a.nd-row >> nth=0 >> .nd-row__sub") === "10-03 14:32 · 网页 · 已切到只用 4G", "line 2: time · source · result");
      ok(await txt(p, "#ch-main a.nd-row >> nth=1 >> .nd-mark__sym") === "■" && await txt(p, "#ch-main a.nd-row >> nth=2 >> .nd-mark__sym") === "▲", "marks ■ / ▲");
      ok(await txt(p, "#ch-main a.nd-row >> nth=2 >> .nd-row__sub") === "10-02 22:10 · 情景跳过 ×5（你手动改过）", "the source is not said twice");
      ok(/只显示最近 50 条/.test(await txt(p, "#ch-main")), "footer: latest 50 only");
      // a row without an op_id opens too
      await rows.nth(1).click();
      await p.waitForSelector("#ch-main .nd-status", { timeout: 8000 }).catch(() => {});
      ok(/[?&]row=[0-9a-f]{16}$/.test(p.url()) && /没改成/.test(await txt(p, "#ch-main")) && await p.locator("#ch-main button").count() === 0, `row without op_id: detail, no undo (${p.url()})`);
      await p.click("#ch-main a:has-text('改动记录')");
      await p.waitForSelector("#ch-main a.nd-row", { timeout: 8000 });

      // the detail: result, what · change, time · source, 撤销
      await rows.nth(0).click();
      await p.waitForSelector("#ch-main button", { timeout: 8000 }).catch(() => {});
      ok(p.url().endsWith("/changes?op=web-20"), `?op= in the address (${p.url()})`);
      ok(/已切到只用 4G/.test(await txt(p, "#ch-main .nd-status__state")) && await txt(p, "#ch-main .nd-status__reason") === "制式 · 自动 → 只用 4G" && await txt(p, "#ch-main .nd-status__meta") === "10-03 14:32 · 网页", "detail: result, what · change, time · source");
      const undoBtn = p.locator("#ch-main button:has-text('撤销')");
      ok(await undoBtn.isEnabled(), "撤销 is on");

      // tier 3 (cuts the mobile link): dialog; Cancel sends nothing
      let s0 = await stats();
      await undoBtn.click();
      await p.waitForSelector("dialog#ch-undo-confirm[open]", { timeout: 8000 }).catch(() => {});
      const dlg = await txt(p, "#ch-undo-confirm");
      ok(/撤销：制式？/.test(dlg) && /撤回这次改动：自动 → 只用 4G/.test(dlg), `tier-3 dialog: ${dlg.slice(0, 60)}`);
      ok(/调制解调器重新附着，移动网络断开约 30 秒。/.test(dlg) && /在「网络模式」页改回来/.test(dlg), "how long offline, how to recover");
      ok(await p.evaluate(() => document.activeElement?.textContent?.trim()) === "取消", "focus on Cancel");
      await p.screenshot({ path: `${OUT}/b2-changes-dialog.png` });
      const ax = await new AxeBuilder({ page: p }).analyze();
      ok(ax.violations.length === 0, `undo dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
      await p.click("#ch-undo-confirm button:has-text('取消')");
      await sleep(800);
      ok(await p.locator("dialog#ch-undo-confirm[open]").count() === 0, "Cancel closes it");
      await p.keyboard.press("Escape");
      await undoBtn.click();
      await p.waitForSelector("dialog#ch-undo-confirm[open]", { timeout: 8000 }).catch(() => {});
      await p.keyboard.press("Escape");
      await sleep(800);
      ok(await p.locator("dialog#ch-undo-confirm[open]").count() === 0, "Esc closes it");
      let s1 = await stats();
      ok(s1.txns.length === s0.txns.length && s1.op_ids.length === s0.op_ids.length, "Cancel / Esc: nothing written");

      // confirm, the change rolls back: u60d's own words with its tone, read from the op block phase
      await post("/mock/txn?end=rolled_back");
      await undoBtn.click();
      await p.waitForSelector("dialog#ch-undo-confirm[open]", { timeout: 8000 }).catch(() => {});
      await p.click("#ch-undo-confirm .nd-btn--confirm");
      await p.waitForFunction(() => /没通/.test(document.querySelector("#ch-undo-op")?.textContent ?? ""), null, { timeout: 25000 }).catch(() => {});
      ok(await txt(p, "#ch-undo-op") === "没通 · 已退回只用 4G" && await p.locator("#ch-undo-op .t-note--warn[role=alert]").count() === 1, `rolled back: ${await txt(p, "#ch-undo-op")}`);
      s1 = await stats();
      let sent = s1.txns.slice(s0.txns.length);
      ok(sent.length === 1, `one write (${sent.length})`);
      ok(sent[0]?.undo === true && sent[0]?.source === "web" && sent[0]?.params?.mode === "WL_AND_5G", "undo: true, source web, the row's request (back to Auto)");
      ok(/^lw-/.test(sent[0]?.op_id ?? "") && sent[0]?.op_id !== "web-20", `a new op_id, not the row's (${sent[0]?.op_id})`);
      await p.waitForFunction(() => /之后又改过/.test(document.querySelector("#ch-main")?.textContent ?? ""), null, { timeout: 20000 }).catch(() => {});
      ok(/之后又改过/.test(await txt(p, "#ch-main")) && await undoBtn.isDisabled(), "the row can't be undone any more: off, u60d's reason");

      // again on a fresh log, the change holds: applied
      await post("/mock/journal?mode=full");
      await post("/mock/txn?end=confirmed");
      await p.reload({ waitUntil: "load" });
      await p.waitForSelector("#ch-main button:has-text('撤销'):not([disabled])", { timeout: 20000 }).catch(() => {});
      await undoBtn.click();
      await p.waitForSelector("dialog#ch-undo-confirm[open]", { timeout: 8000 }).catch(() => {});
      await p.click("#ch-undo-confirm .nd-btn--confirm");
      await p.waitForSelector("#op-bar .nd-announce", { timeout: 5000 }).catch(() => {});
      await p.waitForFunction(() => /已切到/.test(document.querySelector("#ch-undo-op")?.textContent ?? ""), null, { timeout: 25000 }).catch(() => {});
      ok(await txt(p, "#ch-undo-op") === "已切到自动" && await p.locator("#ch-undo-op .t-note--ok").count() === 1, `confirmed: ${await txt(p, "#ch-undo-op")}`);
      await p.goto(WEB + "/changes", { waitUntil: "load" });
      await p.waitForSelector("#ch-main a.nd-row", { timeout: 20000 }).catch(() => {});
      ok(/只用 4G → 自动/.test(await txt(p, "#ch-main a.nd-row >> nth=0")) && /已切到自动/.test(await txt(p, "#ch-main a.nd-row >> nth=0")), "the undo is the newest row");
      await rows.nth(0).click();
      await p.waitForSelector("#ch-main button", { timeout: 8000 }).catch(() => {});
      ok(await p.locator("#ch-main button:has-text('重做')").isEnabled(), "its detail offers 重做");

      // still checking at the deadline: accepted, the bar follows (never 成功)
      await post("/mock/txn?end=live");
      await p.click("#ch-main button:has-text('重做')");
      await p.waitForSelector("dialog#ch-undo-confirm[open]", { timeout: 8000 }).catch(() => {});
      ok(/重做：制式？/.test(await txt(p, "#ch-undo-confirm")), "redo: the same dialog, labelled 重做");
      await p.click("#ch-undo-confirm .nd-btn--confirm");
      await p.waitForFunction(() => /设备已接受/.test(document.querySelector("#ch-undo-op")?.textContent ?? ""), null, { timeout: 30000 }).catch(() => {});
      const live = await txt(p, "#ch-undo-op");
      ok(live === "设备已接受 · 正在确认，结果会显示在顶部横条" && !/成功/.test(live), `still checking: ${live}`);
      ok(/正在确认/.test(await txt(p, "#op-bar")), `the op bar follows it: ${await txt(p, "#op-bar")}`);
      s1 = await stats();
      const ids = s1.txns.map((x) => x.op_id);
      ok(ids.length >= 3 && new Set(ids).size === ids.length && !ids.includes("web-20"), `every undo its own op_id, none resent (${ids.join(", ")})`);
      await idleOp();

      // u60d stuck: the button goes off with the reason
      await post("/mock/journal?mode=full");
      await p.goto(WEB + "/changes?op=web-20", { waitUntil: "load" });
      await p.waitForSelector("#ch-main button:has-text('撤销'):not([disabled])", { timeout: 20000 }).catch(() => {});
      await post("/mock/stuck?age=30000");
      await p.waitForFunction(() => /暂时不能改设置/.test(document.querySelector("#ch-main")?.textContent ?? ""), null, { timeout: 10000 }).catch(() => {});
      ok(/数据服务没响应 · 暂时不能改设置/.test(await txt(p, "#ch-main")) && await undoBtn.isDisabled(), "stuck: off, said why");
      await post("/mock/stuck");
      await p.waitForSelector("#ch-main button:has-text('撤销'):not([disabled])", { timeout: 10000 }).catch(() => {});
      ok(await undoBtn.isEnabled(), "back on when u60d moves again");

      // the journal unreadable: the list stays, greyed, 「数字停在」; 重试 brings it back; empty
      await p.goto(WEB + "/changes", { waitUntil: "load" });
      await p.waitForSelector("#ch-main a.nd-row", { timeout: 20000 });
      await post("/mock/journal?mode=down");
      await p.waitForFunction(() => /读不到改动记录/.test(document.querySelector("#ch-main")?.textContent ?? ""), null, { timeout: 25000 }).catch(() => {});
      ok(/读不到改动记录 · 数据服务没响应/.test(await txt(p, "#ch-main")) && /数字停在 \d\d:\d\d/.test(await txt(p, "#ch-main")) && await p.locator("#ch-main .nd-group.nd-stale a.nd-row").count() === 5, "unreadable: kept, greyed, stopped at hh:mm");
      await post("/mock/journal?mode=empty");
      await p.click("#ch-main button:has-text('重试')");
      await p.waitForFunction(() => /还没有改动/.test(document.querySelector("#ch-main")?.textContent ?? ""), null, { timeout: 10000 }).catch(() => {});
      ok(/还没有改动/.test(await txt(p, "#ch-main")) && /触屏、网页、情景、定时任务改的设置都会记在这里。/.test(await txt(p, "#ch-main")), "重试; empty: no changes yet");
      await post("/mock/journal?mode=full");

      // the network-mode page's "last changed" link (luyaz-web since batch 3) lands on that row's detail
      await p.goto(WEB + "/router/network-mode", { waitUntil: "load" });
      const link = p.locator("#nm-now a[href^='/changes?op=']");
      await link.waitFor({ timeout: 20000 }).catch(() => {});
      ok(/10-03 14:32 · 网页/.test(await link.textContent().catch(() => "")), `settings page link: ${await link.textContent().catch(() => "none")}`);
      await link.click().catch(() => {});
      await p.waitForURL(/\/changes\?op=web-20$/, { timeout: 15000 }).catch(() => {});
      await p.waitForSelector("#ch-main button:has-text('撤销')", { timeout: 15000 }).catch(() => {});
      ok(p.url().endsWith("/changes?op=web-20") && /已切到只用 4G/.test(await txt(p, "#ch-main")), `link opens the row's detail (${p.url()})`);
      await fetch(`${AGENT}/__mock/ops?set=off`);
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
    if (key === "sms") await mock("/mock/sms?count=0");
    if (key === "changes") await mock("/mock/journal?mode=full");
    if (key === "usb") { await fetch(`${AGENT}/__lw/usb?settable=absent`, { method: "POST" }); await mock("/mock/usb?cc=0"); await fetch(MOCK + "/control", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ action: "vendor.call", params: { object: "zwrt_bsp.powerbank", method: "set", args: { state: 0 } } }) }); await fetch(MOCK + "/control", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ action: "usb.set", params: { mode: "user" } }) }); await sleep(1500); }
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
    await p.screenshot({ path: `${OUT}/b2-${key}-${name}-${lang}-${theme}.png`, fullPage: true });
    await ctx.close();
  }
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
