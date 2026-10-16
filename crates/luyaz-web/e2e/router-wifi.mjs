// /router/wifi (batch 4): the common checks (first load ≤ 40 KiB gzip without fonts, axe
// light/dark 0, 390 px no sideways scroll, English body without Chinese, no page errors,
// screenshots) and each write path once against the mock u60d (src/mock/wifi_main.rs) and the
// fake agent (its /api/wifi/status carries plain-text passwords: they must never reach the page).
//   WEB=http://127.0.0.1:18216 MOCK=http://127.0.0.1:19416 OUT=/tmp/lw-wifi \
//     crates/luyaz-web/e2e/run.sh router-wifi
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18216";
const OUT = process.env.OUT ?? "/tmp/lw-wifi";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19416";
const AGENT_KEY = "u60pro-Tw2026"; // the typed mock agent's key_2g / key_5g
const post = (path, body) => fetch(MOCK + path, { method: "POST", headers: { "content-type": "application/json" }, body: body && JSON.stringify(body) });
const stats = () => fetch(`${MOCK}/mock/stats`).then((r) => r.json());
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failed = 0;
let dropping = false; // the simulated dropped reply logs Datastar's FetchFailed: expected then
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
  m.querySelectorAll("option").forEach((e) => e.remove()); // region names are in English anyway; channel lists are digits
  return m.innerText + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, fn, arg, ms = 15000) => p.waitForFunction(fn, arg, { timeout: ms }).catch(() => {});
const ops = async () => (await stats()).page_ops;
const lastOp = async () => (await ops()).at(-1);
const canon = (v) => JSON.stringify(v, (_, x) => (x && typeof x === "object" && !Array.isArray(x) ? Object.fromEntries(Object.entries(x).sort()) : x));
const same = (a, b) => canon(a) === canon(b);

const reset = () => post("/mock/wifi", { reset: true });
const ready = (p) => p.waitForFunction(() => document.querySelectorAll("#wf-global .nd-row").length >= 3 && /信道|channel/i.test(document.querySelector("#wf-status")?.textContent ?? ""), null, { timeout: 15000 });

async function own(p) {
  const done = (re) => until(p, (re) => new RegExp(re).test(document.querySelector("#wf-main-op")?.textContent ?? ""), re, 25000);
  const inline = () => p.locator("#wf-main-confirm.nd-confirm .nd-btn--confirm");
  const dialog = () => p.locator("dialog#wf-main-confirm[open]");
  const open = async () => {
    await p.click("#wf-editbtn");
    await p.waitForSelector("input[data-bind=_fs2]", { timeout: 8000 });
  };
  const apply = () => p.click("#wf-form button:has-text('应用')");
  const noKey = async (what) => ok(!(await p.content()).includes(AGENT_KEY), `${what}: the agent's password is not in the page`);

  const st = await txt(p, "#wf-status");
  ok(/Wi-Fi 已开启/.test(st) && /2\.4 GHz：信道 \d+/.test(st) && /台设备连着/.test(st), `status: ${st}`);
  ok(/访客 Wi-Fi/.test(await txt(p, "#wf-guest")) || (await txt(p, "#wf-guest")) === "", "guest row (when the block has it)");
  const bands = await txt(p, "#wf-bands");
  ok(/U60-Home/.test(bands) && /已设密码/.test(bands) && /WPA2\/WPA3 混合/.test(bands) && /149/.test(bands), `band rows: ${bands.slice(0, 160)}`);
  await noKey("first load");

  // NFC: tier 1, read back from the nfc block
  const nfc = p.locator("button[aria-labelledby=wf-nfc-l]");
  const was = (await nfc.getAttribute("aria-checked")) === "true";
  await nfc.click();
  await until(p, () => /NFC|没有生效|设备已接受/.test(document.querySelector("#wf-nfc-op")?.textContent ?? ""));
  ok(new RegExp(`已${was ? "关掉" : "打开"}NFC 碰一碰`).test(await txt(p, "#wf-nfc-op")), `NFC: ${await txt(p, "#wf-nfc-op")}`);
  ok(same((await lastOp()).params, { enabled: was ? 0 : 1, flag: 2 }) && (await lastOp()).action === "nfc.set", "NFC body");
  // power save: tier 1; the agent (not the mock u60d) holds the state, so no readback here
  const psm = p.locator("button[aria-labelledby=wf-psm-l]");
  if (await psm.count()) {
    await psm.click();
    await until(p, () => /节能|没有生效|设备已接受/.test(document.querySelector("#wf-psm-op")?.textContent ?? ""), null, 25000);
    ok(/Wi-Fi 节能|设备已接受/.test(await txt(p, "#wf-psm-op")) && (await lastOp()).action === "wifi.power_save", `power save: ${await txt(p, "#wf-psm-op")}`);
  } else ok(false, "power save row (fake agent answers /api/wifi/power-save)");

  // the form: read-only groups hide while it is open
  await open();
  ok(!(await p.isVisible("#wf-global")), "settings rows hidden while the form is open");
  // Apply with nothing changed: not sent
  let n = (await ops()).length;
  await apply();
  await done("还没有改动");
  ok(/还没有改动/.test(await txt(p, "#wf-main-op")) && (await ops()).length === n, "no change: said, not sent");

  // a new 2.4 GHz name: tier 3 dialog, only that key sent, followed to u60d's result
  await p.fill("input[data-bind=_fs2]", "U60-New");
  await apply();
  await dialog().waitFor({ timeout: 8000 }).catch(() => {});
  const dlg = await txt(p, "dialog#wf-main-confirm");
  ok(await dialog().count() === 1 && /10–60 秒/.test(dlg) && /「U60-New」/.test(dlg) && !/你正在经 Tailscale/.test(dlg), `rename: dialog (tier 3) ${dlg.slice(0, 200)}`);
  await p.click("dialog#wf-main-confirm .nd-btn--confirm");
  await done("设置已生效|没有生效|设备已接受|已退回");
  ok(await txt(p, "#wf-main-op") === "Wi-Fi 设置已生效", `rename: ${await txt(p, "#wf-main-op")}`);
  let b = await lastOp();
  ok(b.action === "wifi.settings" && b.source === "web" && /^lw-/.test(b.op_id) && same(b.params, { ssid_2g: "U60-New" }), `rename body ${JSON.stringify(b.params)}`);
  await until(p, () => /U60-New/.test(document.querySelector("#wf-bands")?.textContent ?? ""));
  ok(/U60-New/.test(await txt(p, "#wf-bands")) && await p.locator("input[data-bind=_fs2]").count() === 0, "read back, form closed");
  ok(/上次改动：Wi-Fi 设置已生效/.test(await txt(p, "#wf-status")), "status: last change");

  // transmit power alone: tier 2 inline
  await open();
  await p.selectOption("select[data-bind=_ft5]", "50");
  await apply();
  await inline().waitFor({ timeout: 8000 }).catch(() => {});
  ok(await inline().count() === 1 && await dialog().count() === 0 && /不重启/.test(await txt(p, "#wf-main-op")), "txpower: inline confirm (tier 2)");
  await inline().click();
  await done("设置已生效|没有生效|设备已接受");
  ok(await txt(p, "#wf-main-op") === "Wi-Fi 设置已生效" && same((await lastOp()).params, { txpower_5g: "50" }), `txpower: ${await txt(p, "#wf-main-op")}`);

  // a new 5 GHz password: sent once in the body, never in the page
  await open();
  await p.fill("input[data-bind=_fk5]", "newpass-123");
  await apply();
  await dialog().waitFor({ timeout: 8000 }).catch(() => {});
  ok(/用新密码/.test(await txt(p, "dialog#wf-main-confirm")) && !(await p.content()).includes("newpass-123"), "password: dialog says rejoin with the new password, without it");
  await p.click("dialog#wf-main-confirm .nd-btn--confirm");
  await done("设置已生效|没有生效|设备已接受");
  ok(same((await lastOp()).params, { key_5g: "newpass-123" }), "password body: key_5g only");
  ok(!(await p.content()).includes("newpass-123"), "the new password is not in the page");

  // Wi-Fi 6: visible, refused before sending
  await open();
  n = (await ops()).length;
  await p.click("button[aria-labelledby=wf-fw6-l]");
  ok(/应用时会被拒绝/.test(await txt(p, "#wf-form")), "Wi-Fi 6: says it will be refused");
  await apply();
  await done("Wi-Fi 6");
  ok(/Wi-Fi 6 在这里改不了/.test(await txt(p, "#wf-main-op")) && (await ops()).length === n, `Wi-Fi 6: ${await txt(p, "#wf-main-op")}`);
  await p.click("button[aria-labelledby=wf-fw6-l]");

  // busy: 409 with doing, then 503 for the Wi-Fi lock — shown, not hidden
  for (const [next, re] of [["doing", "另一项改动正在进行 · 正在改访客 Wi-Fi"], ["lock", "Wi-Fi 正被别的程序修改"], ["refused", "设备上没有留着旧的 Wi-Fi 密码"]]) {
    await post("/mock/wifi", { next });
    await p.click("button[aria-labelledby=wf-fx2-l]");
    await apply();
    await dialog().waitFor({ timeout: 8000 }).catch(() => {});
    await p.click("dialog#wf-main-confirm .nd-btn--confirm");
    await done("没有生效");
    ok(new RegExp(re).test(await txt(p, "#wf-main-op")), `${next}: ${await txt(p, "#wf-main-op")}`);
    await p.click("button[aria-labelledby=wf-fx2-l]");
  }

  // u60d: "no changes" (the device already has what the form sends)
  await post("/mock/wifi", { set: { band_5g: { ap_on: true, radio_on: true, ssid: "Same", encryption: "psk2+psk3+ccmp", has_password: true, hidden: false, channel: "149", txpower: "50", htmode: "VHT80" } } });
  await p.fill("input[data-bind=_fs5]", "Same");
  await apply();
  await dialog().waitFor({ timeout: 8000 }).catch(() => {});
  await p.click("dialog#wf-main-confirm .nd-btn--confirm");
  await done("不用改|没有生效");
  ok(/设备上已经是这些设置/.test(await txt(p, "#wf-main-op")), `no changes: ${await txt(p, "#wf-main-op")}`);

  // rolled back: u60d's words, warn
  await post("/mock/wifi", { end: "rolled_back" });
  await open();
  await p.selectOption("select[data-bind=_fc2]", "6");
  await apply();
  await dialog().waitFor({ timeout: 8000 }).catch(() => {});
  await p.click("dialog#wf-main-confirm .nd-btn--confirm");
  await done("已退回|没有生效|设备已接受");
  ok(/已退回原来的设置/.test(await txt(p, "#wf-main-op")) && await p.locator("#wf-main-op .t-note--warn").count() === 1, `rolled back: ${await txt(p, "#wf-main-op")}`);
  await post("/mock/wifi", { end: "confirmed" });

  // this browser on the reloading Wi-Fi: the set's reply drops after the send (the request reaches
  // the server, the browser's side is cut), the link is down for a while, then back: the area's
  // lost-reply post follows the same op_id and shows u60d's result
  await post("/mock/wifi", { ms: 7000 });
  await p.click("#wf-editbtn"); // discard the rolled-back draft
  await p.waitForSelector("input[data-bind=_fs2]", { state: "detached", timeout: 8000 }).catch(() => {});
  await open();
  await p.selectOption("select[data-bind=_fh2]", "HT40");
  await apply();
  await dialog().waitFor({ timeout: 8000 }).catch(() => {});
  const lost = [];
  p.on("request", (q) => { if (/\/router\/wifi\/main\/cancel\?op=lw-[^&]+&lost=true/.test(q.url())) lost.push(q.url()); });
  await p.route("**/router/wifi/main/set*", async (route) => {
    await route.fetch({ timeout: 1500 }).catch(() => {});
    await route.abort("internetdisconnected");
  });
  dropping = true;
  await p.click("dialog#wf-main-confirm .nd-btn--confirm");
  await sleep(1700);
  await p.context().setOffline(true);
  await sleep(2500);
  await p.context().setOffline(false);
  await p.unroute("**/router/wifi/main/set*");
  await until(p, () => /设置已生效|连接中断|没有生效/.test(document.querySelector("#wf-main-op")?.textContent ?? ""), null, 30000);
  await sleep(500);
  dropping = false;
  ok(await txt(p, "#wf-main-op") === "Wi-Fi 设置已生效", `dropped mid-change, back: ${await txt(p, "#wf-main-op")}`);
  ok(same((await lastOp()).params, { htmode_2g: "HT40" }), "sent once");
  ok(lost.length >= 1, `the lost reply was followed by its op_id (${lost.length} posts)`);
  await post("/mock/wifi", { ms: 0 });
  await noKey("after the writes");
  await reset();
}

const browser = await chromium.launch({ channel: "chrome" });
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => { if (!(dropping && /FetchFailed/.test(e.message))) errs.push(e.message); else console.log("expected during the drop:", e.message.slice(0, 80)); });
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text()) && !(dropping && /FetchFailed|ERR_INTERNET_DISCONNECTED/.test(m.text()))) errs.push(m.text()); });
  await reset(); await sleep(500);
  await p.goto(WEB + "/router/wifi", { waitUntil: "load" });
  await ready(p).catch(() => ok(false, `${name} ${lang}: live content never showed`));
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
    await own(p);
    await p.reload({ waitUntil: "load" });
    await ready(p).catch(() => {});
    await p.screenshot({ path: `${OUT}/wifi-${name}-${lang}-${theme}-view.png`, fullPage: true });
  }
  await p.click("#wf-editbtn");
  await p.waitForSelector("input[data-bind=_fs2]", { timeout: 8000 }).catch(() => ok(false, `${name} ${lang}: the form opened`));
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
  await p.screenshot({ path: `${OUT}/wifi-${name}-${lang}-${theme}.png`, fullPage: true });
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
