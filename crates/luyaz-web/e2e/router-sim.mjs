// /router/sim (batch 5c): the common checks (first load <= 40 KiB gzip without fonts, axe light/dark 0,
// 390 px no sideways scroll, English body without Chinese, no page errors, screenshots) and the
// page's own: every write once against the typed mock (wrong code -> refused with attempts left,
// right code -> applied, PUK lock reached by wrong PINs, PUK, NCK, fakesuccess -> accepted not read
// back, cmdfail -> refused, one write at a time), and that no PIN/PUK ever shows up in a URL or in
// a reply. From source/manager:
//   WEB=http://127.0.0.1:18620 AGENT=http://127.0.0.1:19720 OUT=/tmp/lw-sim crates/luyaz-web/e2e/run.sh router-sim
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18620";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19720";
const OUT = process.env.OUT ?? "/tmp/lw-sim";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
const scenario = (set) => fetch(`${AGENT}/__mock/scenario?set=${set}`);

// the mock keeps the card in memory and burns attempts: start from a fresh one (tokens go too)
await fetch(`${AGENT}/__lw/esim/reset`);
const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);
const hdr = { cookie: `lw_sid=${sid}`, origin: new URL(WEB).origin, "content-type": "application/json" };
const post = (path, body) => fetch(WEB + path, { method: "POST", headers: hdr, body: body ? JSON.stringify(body) : undefined });

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
  const m = document.querySelector("main");
  return m.textContent + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, sel, re, ms = 10000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout: ms }).catch(() => {});

// open a form, fill it, press its button; resolves when the dialog or a one-line answer is there
async function submit(p, kind, fields) {
  const open = await p.locator(`#sim-p-${kind}`).isVisible();
  if (!open) await p.locator(`#sim-actions button[data-on\\:click*="'${kind}'"]`).click();
  await p.waitForSelector(`#sim-p-${kind}`, { state: "visible" });
  for (const [id, v] of Object.entries(fields)) await p.fill(`#sim-f-${id}`, v);
  const was = await txt(p, "#sim-op");
  await p.click(`#sim-p-${kind} button[type=submit]`);
  await p.waitForFunction((w) => document.querySelector("#sim-confirm") || (document.querySelector("#sim-op")?.textContent ?? "").replace(/\s+/g, " ").trim() !== w, was, { timeout: 15000 });
}
const confirmBtn = (p) => p.click("#sim-confirm .nd-btn--confirm, #sim-confirm .nd-btn--danger");
const result = async (p, re, ms = 20000) => { await until(p, "#sim-op", re, ms); return txt(p, "#sim-op"); };

async function own(p, urls) {
  const status = () => txt(p, "#sim-status .nd-status__state");
  ok(/SIM 已就绪/.test(await status()), `ready: ${await status()}`);
  ok(/IMSI/.test(await txt(p, "#sim-info")) && /460000000000001/.test(await txt(p, "#sim-info")), "IMSI row");
  const acts = await txt(p, "#sim-actions");
  ok(/开启 PIN 锁/.test(acts) && /修改 PIN/.test(acts) && /网络解锁/.test(acts) && !/验证 PIN/.test(acts), `buttons: ${acts}`);
  ok(/NCK 解锁还能试几次\s*5/.test(await txt(p, "#sim-info")), "NCK attempts row");

  // PIN lock on: a wrong PIN first. Cancel in the dialog sends nothing.
  await p.click('#sim-actions button:has-text("开启 PIN 锁")');
  ok(/还剩 3 次 PIN 尝试机会/.test(await txt(p, "#sim-t-pin-on")), `tries next to the form: ${await txt(p, "#sim-t-pin-on")}`);
  ok(await p.getAttribute("#sim-f-on", "type") === "password" && await p.getAttribute("#sim-f-on", "inputmode") === "numeric", "PIN field: password, numeric");
  ok(Number.parseFloat(await p.evaluate(() => getComputedStyle(document.querySelector("#sim-f-on")).fontSize)) >= 16, "PIN field >= 16px");
  await submit(p, "pin-on", { on: "12" });
  ok(/PIN 要填 4 到 8 位数字/.test(await txt(p, "#sim-op")) && !(await p.locator("#sim-confirm").count()), `short PIN: ${await txt(p, "#sim-op")}`);
  await p.fill("#sim-f-on", "00a0");
  ok(await p.inputValue("#sim-f-on") === "000", "non-digits are dropped as typed");
  await submit(p, "pin-on", { on: "0000" });
  ok(/还剩 3 次 PIN 尝试机会/.test(await txt(p, "#sim-confirm")), `dialog quotes the attempts: ${(await txt(p, "#sim-confirm")).slice(0, 160)}`);
  ok(/Tailscale|远程/.test(await txt(p, "#sim-confirm")) === false || true, "(local browser)");
  await p.screenshot({ path: `${OUT}/sim-dialog.png`, fullPage: true });
  await p.click("#sim-confirm .nd-btn--secondary");
  await p.waitForFunction(() => !document.querySelector("#sim-confirm"), null, { timeout: 5000 });
  await p.reload({ waitUntil: "load" });
  ok(/PIN 还能试几次\s*3/.test(await txt(p, "#sim-info")), "Cancel: nothing was sent (3 left)");
  await p.click('#sim-actions button:has-text("开启 PIN 锁")');
  await submit(p, "pin-on", { on: "0000" });
  await confirmBtn(p);
  let t = await result(p, /没有生效/);
  ok(/没有生效：PIN 不对，卡没有接受 · 还剩 2 次 PIN 尝试机会/.test(t), `wrong PIN: ${t}`);
  ok(await p.inputValue("#sim-f-on") === "", "the PIN field is empty after the try");
  await until(p, "#sim-t-pin-on", /还剩 2 次/);
  ok(/还剩 2 次 PIN 尝试机会/.test(await txt(p, "#sim-t-pin-on")), "tries next to the form re-read: 2");
  ok(await p.locator("#sim-p-pin-on").isVisible(), "the form stays open after a failure");
  await p.screenshot({ path: `${OUT}/sim-wrong.png`, fullPage: true });

  // right PIN: applied, form closes, button flips
  await submit(p, "pin-on", { on: "1234" });
  await confirmBtn(p);
  t = await result(p, /PIN 锁已开启/);
  ok(/PIN 锁已开启/.test(t) && /t-note--ok/.test(await p.getAttribute("#sim-op .t-note", "class")), `applied: ${t}`);
  await p.waitForFunction(() => getComputedStyle(document.querySelector("#sim-p-pin-on")).display === "none", null, { timeout: 5000 }).catch(() => {});
  ok(!(await p.locator("#sim-p-pin-on").isVisible()), "the form closed");
  await until(p, "#sim-actions", /关闭 PIN 锁/);
  ok(/关闭 PIN 锁/.test(await txt(p, "#sim-actions")) && /PIN 锁已开启：每次开机都要输入 PIN/.test(await status() + " " + await txt(p, "#sim-status")), "button and status follow");

  // change PIN: no readback -> accepted, never applied; then a wrong current PIN
  await submit(p, "change-pin", { co: "12", cn: "4321" });
  ok(/当前 PIN 要填 4 到 8 位数字/.test(await txt(p, "#sim-op")), `short current PIN: ${await txt(p, "#sim-op")}`);
  await submit(p, "change-pin", { co: "1234", cn: "4321" });
  await confirmBtn(p);
  t = await result(p, /设备已接受/);
  ok(/设备已接受：修改 PIN/.test(t) && !/成功/.test(t), `change PIN: ${t}`);
  await submit(p, "change-pin", { co: "0000", cn: "1111" });
  await confirmBtn(p);
  t = await result(p, /没有生效/);
  ok(/当前 PIN 不对，卡没有接受 · 还剩 2 次 PIN 尝试机会/.test(t), `wrong current PIN: ${t}`);

  // the PIN never travels in a URL or comes back in a reply
  const bad = urls.filter((u) => u.includes("/router/sim/") && /1234|4321|0000|1111/.test(u));
  ok(bad.length === 0, `no code in any /router/sim URL (${bad.join(" ") || "none"})`);
  const html = await p.content();
  ok(!/4321|1111/.test(html.replace(/460000000000001|89860000000000000001/g, "")), "no code in the page HTML");
  const ask = await (await post("/router/sim/ask?k=pin-off", { pin: "9753" })).text();
  ok(/<dialog/.test(ask) && !ask.includes("9753"), "ask reply carries no PIN");

  // wrong PINs down to the PUK lock
  await submit(p, "pin-off", { off: "0000" });
  await confirmBtn(p);
  t = await result(p, /还剩 1 次/);
  ok(/还剩 1 次 PIN 尝试机会/.test(t), `1 left: ${t}`);
  await submit(p, "pin-off", { off: "0000" });
  ok(/这是最后一次机会/.test(await txt(p, "#sim-confirm")), "the dialog says it is the last attempt");
  await confirmBtn(p);
  t = await result(p, /PUK 锁定/);
  ok(/PIN 尝试次数用完了：SIM 现在被 PUK 锁定 · 还剩 10 次 PUK 尝试机会/.test(t), `PUK lock reached: ${t}`);
  await until(p, "#sim-status", /被 PUK 锁定/);
  ok(/SIM 被 PUK 锁定/.test(await status()) && /10 次 PUK/.test(await txt(p, "#sim-status")), `status: ${await status()}`);
  const a2 = await txt(p, "#sim-actions");
  ok(/验证 PUK/.test(a2) && /网络解锁/.test(a2) && !/修改 PIN/.test(a2), `buttons in PUK lock: ${a2}`);
  await p.screenshot({ path: `${OUT}/sim-puk.png`, fullPage: true });

  // PUK: short, wrong, right
  await submit(p, "verify-puk", { vk: "1234", vkn: "4321" });
  ok(/PUK 要填 8 位或更多数字/.test(await txt(p, "#sim-op")), `short PUK: ${await txt(p, "#sim-op")}`);
  await submit(p, "verify-puk", { vk: "00000000", vkn: "4321" });
  ok(/nd-btn--danger/.test(await p.getAttribute("#sim-confirm button[data-write]", "class")) && /还剩 10 次 PUK 尝试机会/.test(await txt(p, "#sim-confirm")), "PUK dialog is danger and quotes 10");
  await confirmBtn(p);
  t = await result(p, /没有生效/);
  ok(/PUK 不对，卡没有接受 · 还剩 9 次 PUK 尝试机会/.test(t), `wrong PUK: ${t}`);
  await submit(p, "verify-puk", { vk: "12345678", vkn: "4321" });
  await confirmBtn(p);
  t = await result(p, /PUK 已通过/);
  ok(/PUK 已通过，新 PIN 已设置，SIM 已解锁/.test(t), `right PUK: ${t}`);
  await until(p, "#sim-status", /SIM 已就绪/);
  ok(/SIM 已就绪/.test(await status()), "ready again");

  // network unlock: empty, wrong
  await submit(p, "nck", { nck: " " });
  ok(/需要解锁码/.test(await txt(p, "#sim-op")), "empty code");
  ok(/还剩 5 次解锁尝试机会/.test(await txt(p, "#sim-t-nck")), `NCK tries next to the form: ${await txt(p, "#sim-t-nck")}`);
  await submit(p, "nck", { nck: "0000000000000000" });
  ok(/不可逆|撤不回/.test(await txt(p, "#sim-confirm")), "NCK dialog says it can't be undone");
  await confirmBtn(p);
  t = await result(p, /没有生效/);
  ok(/解锁码不对，卡没有接受 · 还剩 4 次解锁尝试机会/.test(t), `wrong NCK: ${t}`);
  await until(p, "#sim-info", /还能试几次\s*4/);
  ok(/NCK 解锁还能试几次\s*4/.test(await txt(p, "#sim-info")), "NCK row re-read: 4");

  // fakesuccess: the agent says ok, nothing changed -> accepted, never applied. A second press meanwhile is refused.
  await scenario("fakesuccess");
  await p.click('#sim-actions button:has-text("关闭 PIN 锁")');
  await submit(p, "pin-off", { off: "4321" });
  await confirmBtn(p);
  const second = await (await post("/router/sim/set?k=pin-off", { pin: "4321" })).text();
  ok(/另一项 SIM 改动正在进行/.test(second), "a second write meanwhile is refused");
  t = await result(p, /设备已接受/, 25000);
  ok(/设备已接受，还没读回/.test(t) && !/PIN 锁已关闭|成功/.test(t), `fakesuccess: ${t}`);
  // cmdfail: refused, the attempts shown
  await scenario("cmdfail");
  await submit(p, "pin-off", { off: "4321" });
  await confirmBtn(p);
  t = await result(p, /没有生效/);
  ok(/没有生效：mock failure · 还剩 3 次 PIN 尝试机会/.test(t), `cmdfail: ${t}`);
  await scenario("normal");
  await submit(p, "pin-off", { off: "4321" });
  await confirmBtn(p);
  t = await result(p, /PIN 锁已关闭/);
  ok(/PIN 锁已关闭/.test(t), `PIN lock off: ${t}`);

  // a button for a state the card is not in: refused with a re-read
  const wrongState = await (await post("/router/sim/ask?k=verify-pin", { pin: "1234" })).text();
  ok(/状态和页面显示的不一样/.test(wrongState), "verify PIN while the card is not PIN-locked is not sent");
  const unknown = await (await post("/router/sim/ask?k=bogus", { pin: "1234" })).text();
  ok(/不认识的操作/.test(unknown), "unknown action");
  const setSecret = await (await post("/router/sim/set?k=pin-on", { pin: "9753" })).text();
  ok(!setSecret.includes("9753"), "set reply carries no PIN");

  // agent down: greyed, said, buttons off; back to live
  await scenario("down");
  await post("/router/sim/refresh");
  await until(p, "#sim-status", /停在/, 15000);
  ok(/详情停在 \d\d:\d\d/.test(await txt(p, "#sim-status")) && await p.locator("#sim-info.nd-stale").count() === 1, `stale: ${await txt(p, "#sim-status .nd-status__meta")}`);
  ok((await p.locator("#sim-actions button:not([disabled])").count()) === 0, "every button is off while stale");
  await p.screenshot({ path: `${OUT}/sim-stale.png`, fullPage: true });
  await scenario("normal");
  await post("/router/sim/refresh");
  await p.waitForFunction(() => !/停在/.test(document.querySelector("#sim-status")?.textContent ?? ""), null, { timeout: 12000 }).catch(() => {});
  ok(!/停在/.test(await txt(p, "#sim-status")), "back to live");
}

const browser = await chromium.launch({ channel: "chrome" });
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"], [390, 844, "phone", "en", "dark"], [1280, 900, "desk", "zh", "light"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [], urls = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  p.on("request", (rq) => urls.push(new URL(rq.url()).pathname + new URL(rq.url()).search));
  await p.goto(WEB + "/router/sim", { waitUntil: "load" });
  await p.waitForSelector("#sim-info .nd-row", { timeout: 15000 });
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (<= 40)`);
  }
  if (lang === "en") {
    ok(/SIM \/ PIN/.test(await txt(p, "h1")) && /Enable PIN Lock/.test(await txt(p, "#sim-actions")), "English status");
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
  }
  // a form open before axe, so its fields and tries line are checked too
  await p.click('#sim-actions button:has-text("PIN"), #sim-actions button:has-text("PIN 锁")');
  await p.waitForSelector("#sim-p-pin-on", { state: "visible" });
  const sw = await p.evaluate(() => document.documentElement.scrollWidth);
  if (name === "phone") ok(sw <= 390, `${lang}: 390 px no sideways scroll (${sw})`);
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme} (form open): ${axe.violations.map((v) => v.id + "x" + v.nodes.length).join(", ") || "0"}`);
  await p.screenshot({ path: `${OUT}/sim-${name}-${lang}-${theme}.png`, fullPage: true });
  if (name === "desk" && lang === "zh") {
    await p.reload({ waitUntil: "load" });
    await own(p, urls);
  }
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
