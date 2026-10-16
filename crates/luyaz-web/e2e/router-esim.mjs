// /router/esim (batch 5a): the common checks (first load ≤ 40 KiB gzip without fonts, axe
// light/dark 0, 390 px no sideways scroll, English body without Chinese, no page errors,
// screenshots) and each write path once against the typed mock agent (fixtures/esim.ts) behind
// e2e/fake-agent.mjs (`/__lw/esim`: POST counts, a fresh mock): rename, send notifications,
// download, delete (typed word), switch (read back on the card), the 300 s cooldown refusal, and
// the "fakesuccess" switch (job done + rebooting, card unchanged: never success). No ICCID or
// activation code may appear in any request URL.
//   WEB=http://127.0.0.1:18254 AGENT=http://127.0.0.1:19280 OUT=/tmp/lw-esim \
//     crates/luyaz-web/e2e/run.sh router-esim
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mkdirSync } from "node:fs";

const WEB = process.env.WEB ?? "http://127.0.0.1:18254";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19280";
const OUT = process.env.OUT ?? "/tmp/lw-esim";
mkdirSync(OUT, { recursive: true });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
const scenario = (s) => fetch(`${AGENT}/__mock/scenario?set=${s}`);
const posts = () => fetch(`${AGENT}/__lw/esim`).then((r) => r.json());
const nPost = async (p) => (await posts())[p] ?? 0;
const ICCIDS = ["8988600000000000001", "8986000000000000002", "8981000000000000003"];
const CODE = "LPA:1$rsp.novatravel.example$ABCD-1234-EFGH";

async function login() {
  const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
  const s = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
  if (!s) throw new Error(`login failed: ${r.status}`);
  return s;
}
let sid = await login();

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
  m.querySelectorAll("[data-data], [hidden]").forEach((e) => e.remove());
  return m.innerText + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, sel, re, ms = 20000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout: ms }).catch(() => {});
const ready = (p) => p.waitForFunction(() => document.querySelectorAll("#es-list .es-row").length >= 3 && !document.querySelector("#es-p0 button[disabled]"), null, { timeout: 20000 });
const row = (p, name) => p.locator("#es-list .es-row", { has: p.locator(`[id^=es-n]:text-is("${name}")`) });
const dialog = (p) => p.locator("dialog#es-prof-confirm[open]");

async function own(p, ctx) {
  ok(/使用中：台湾旅行/.test(await txt(p, "#es-status")), `status: ${await txt(p, "#es-status .nd-status__state")}`);
  ok(/Nova Travel · Taiwan 20GB 10 Days/.test(await txt(p, "#es-status")) && /8988600000000000001/.test(await txt(p, "#es-status")), "status reason: carrier · plan, ICCID");
  ok(await p.locator("#es-list .es-row").count() === 3, "three profiles");
  ok(await row(p, "台湾旅行").locator("button[aria-labelledby^=es-sw]").count() === 0, "the profile in use has no switch / delete");
  ok(/#12 · disable/.test(await txt(p, "#es-notif")) && /#13 · enable/.test(await txt(p, "#es-notif")), "two pending notifications");
  ok(/89049000000000000000000000000041/.test(await txt(p, "#es-chip")) && /318 KB/.test(await txt(p, "#es-chip")) && /2\.2\.2/.test(await txt(p, "#es-chip")), "chip: EID, free space, SGP.22");
  const sw = p.locator("#es-sw2");
  ok((await sw.getAttribute("aria-labelledby")) === "es-sw2 es-n2" && /Switch|切换/.test(await sw.textContent()), "switch button named with the profile");

  // ---- rename (tier 2): too long → the agent's refusal; then ok + readback
  await row(p, "Pacific Roam").locator("button[id^=es-rn]").click();
  const input = p.locator("#es-ni2");
  await input.waitFor({ state: "visible", timeout: 5000 });
  await input.fill("长".repeat(25));
  await row(p, "Pacific Roam").locator("button[id^=es-sv]").click();
  await p.waitForSelector("#es-prof-confirm", { timeout: 8000 });
  ok(/显示为/.test(await txt(p, "#es-prof-confirm")), "rename: inline confirm states the consequence");
  await p.click("#es-prof-confirm .nd-btn--confirm");
  await until(p, "#es-prof-op", /没有生效/);
  ok(/没有生效：nickname too long/.test(await txt(p, "#es-prof-op")), `rename refused: ${await txt(p, "#es-prof-op")}`);
  await input.fill("日本旅行");
  await input.press("Enter");
  await p.waitForSelector("#es-prof-confirm", { timeout: 8000 });
  await p.click("#es-prof-confirm .nd-btn--confirm");
  await until(p, "#es-prof-op", /昵称已保存/);
  ok(/昵称已保存/.test(await txt(p, "#es-prof-op")), `rename ok: ${await txt(p, "#es-prof-op")}`);
  await until(p, "#es-list", /日本旅行/);
  ok(await row(p, "日本旅行").count() === 1 && !(await input.isVisible()), "list reads the new nickname back; the box closed");
  ok(await nPost("/api/esim/nickname") === 2, "rename: one POST per confirm");

  // ---- send notifications (tier 2): no network → refused; then ok
  await scenario("nosignal");
  await p.click("#es-notif button");
  await p.waitForSelector("#es-nt-confirm", { timeout: 8000 });
  ok(/2 条回执/.test(await txt(p, "#es-nt-confirm")), "notifications: confirm says how many");
  await p.click("#es-nt-confirm .nd-btn--confirm");
  await until(p, "#es-nt-op", /没有生效|已发送/);
  ok(/没有生效：.*Could not resolve host/.test(await txt(p, "#es-nt-op")), `notifications refused: ${await txt(p, "#es-nt-op")}`);
  await scenario("normal");
  await p.waitForFunction(() => !document.querySelector("#es-notif button")?.disabled, null, { timeout: 15000 }).catch(() => {});
  await p.click("#es-notif button");
  await p.waitForSelector("#es-nt-confirm", { timeout: 8000 });
  await p.click("#es-nt-confirm .nd-btn--confirm");
  await until(p, "#es-nt-op", /通知已发送|没有生效/);
  ok(/通知已发送/.test(await txt(p, "#es-nt-op")), `notifications sent: ${await txt(p, "#es-nt-op")}`);
  await p.waitForFunction(() => !document.querySelector("#es-notif section"), null, { timeout: 15000 }).catch(() => {});
  ok(await p.locator("#es-notif section").count() === 0, "list empty → section hidden");
  ok(await nPost("/api/esim/notifications/process") === 2, "notifications: one POST per confirm");

  // ---- download (tier 2): a bad code never sent; no network → refused; ok → one more profile
  await p.fill("#es-code", "not-a-code");
  await p.click("#es-dlgo button");
  await until(p, "#es-dl-op", /LPA:1/);
  ok(/请输入形如 LPA:1/.test(await txt(p, "#es-dl-op")) && await nPost("/api/esim/download") === 0, "bad code: said, not sent");
  await scenario("nosignal");
  await p.fill("#es-code", CODE);
  await p.click("#es-dlgo button");
  await p.waitForSelector("#es-dl-confirm", { timeout: 8000 });
  ok(!(await txt(p, "#es-dl-confirm")).includes("ABCD-1234"), "the confirm doesn't echo the code");
  await p.click("#es-dl-confirm .nd-btn--confirm");
  await until(p, "#es-dl-op", /没有生效|已下载/, 30000);
  ok(/没有生效：.*Could not resolve host/.test(await txt(p, "#es-dl-op")), `download refused: ${await txt(p, "#es-dl-op")}`);
  await scenario("normal");
  await p.waitForFunction(() => !document.querySelector("#es-dlgo button")?.disabled, null, { timeout: 15000 }).catch(() => {});
  await p.click("#es-dlgo button");
  await p.waitForSelector("#es-dl-confirm", { timeout: 8000 });
  await p.click("#es-dl-confirm .nd-btn--confirm");
  await until(p, "#es-status", /下载中|正在下载/, 8000);
  ok(/正在下载配置/.test(await txt(p, "#es-status")) || /正在下载/.test(await txt(p, "#es-dl-op")), "busy while downloading");
  await until(p, "#es-dl-op", /已下载|没有生效|还没读回/, 40000);
  ok(/配置已下载到卡上/.test(await txt(p, "#es-dl-op")), `download ok: ${await txt(p, "#es-dl-op")}`);
  await p.waitForFunction(() => document.querySelectorAll("#es-list .es-row").length === 4, null, { timeout: 15000 }).catch(() => {});
  ok(await p.locator("#es-list .es-row").count() === 4 && (await p.inputValue("#es-code")) === "", "4 profiles; the code box cleared");
  ok(await nPost("/api/esim/download") === 2, "download: one POST per confirm");

  // ---- delete (tier 3, typed word)
  await ready(p).catch(() => {});
  await row(p, "Nova Travel").locator(".es-del").click();
  await dialog(p).waitFor({ timeout: 8000 });
  const ax = await new AxeBuilder({ page: p }).include("dialog").analyze();
  ok(ax.violations.length === 0, `delete dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
  ok(/不能撤销/.test(await txt(p, "dialog#es-prof-confirm")) && /怎么恢复/.test(await txt(p, "dialog#es-prof-confirm")), "delete dialog: what + recovery");
  const go = dialog(p).locator(".nd-btn--danger");
  await dialog(p).locator("input").fill("删");
  ok(await go.isDisabled(), "wrong word: confirm disabled");
  await dialog(p).locator("input").fill("删除");
  await go.click();
  await until(p, "#es-prof-op", /已从卡上删除|没有生效|还没读回/, 30000);
  ok(/已从卡上删除「Nova Travel」/.test(await txt(p, "#es-prof-op")), `delete ok: ${await txt(p, "#es-prof-op")}`);
  await p.waitForFunction(() => document.querySelectorAll("#es-list .es-row").length === 3, null, { timeout: 15000 }).catch(() => {});
  ok(await p.locator("#es-list .es-row").count() === 3 && await nPost("/api/esim/delete") === 1, "gone from the list; one POST");

  // ---- the "fakesuccess" switch: job done + rebooting, the card never changed → never success
  await scenario("fakesuccess");
  await ready(p).catch(() => {});
  await row(p, "国内主号").locator("button[id^=es-sw]").click();
  await dialog(p).waitFor({ timeout: 8000 });
  const dt = await txt(p, "dialog#es-prof-confirm");
  ok(/切换到「国内主号」？/.test(dt) && /断开多久/.test(dt) && /切回「台湾旅行」/.test(dt), `switch dialog: title, downtime, recovery (${dt.slice(0, 60)}…)`);
  await dialog(p).locator(".nd-btn--confirm").click();
  await until(p, "#es-prof-op", /正在重启/, 30000);
  ok(/设备正在重启并切换到新配置/.test(await txt(p, "#es-prof-op")) && await p.locator("#es-after[data-init]").count() === 1, "rebooting: said, the verifier is on the page");
  await until(p, "#es-status", /重启中/, 15000);
  ok(/重启中/.test(await txt(p, "#es-status")), "status: 重启中…");
  await until(p, "#es-prof-op", /没有切过去|已切换/, 60000);
  const fr = await txt(p, "#es-prof-op");
  ok(/没有切过去：设备没有重启，卡上用的还是「台湾旅行」/.test(fr) && !/已切换/.test(fr), `fakesuccess: never success (${fr})`);
  ok(await nPost("/api/esim/switch") === 1, "switch: one POST");

  // ---- the cooldown: a second switch within 300 s is refused by the agent
  await scenario("normal");
  await ready(p).catch(() => {});
  await row(p, "国内主号").locator("button[id^=es-sw]").click();
  await dialog(p).waitFor({ timeout: 8000 });
  await dialog(p).locator(".nd-btn--confirm").click();
  await until(p, "#es-prof-op", /没有生效|已切换/, 20000);
  ok(/没有生效：card needs a cooldown/.test(await txt(p, "#es-prof-op")), `cooldown refused: ${await txt(p, "#es-prof-op")}`);

  // ---- a real switch (a fresh mock: no cooldown), read back on the card
  await fetch(`${AGENT}/__lw/esim/reset`, { method: "POST" });
  sid = await login();
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }]);
  await p.reload({ waitUntil: "load" });
  await ready(p).catch(() => {});
  await row(p, "国内主号").locator("button[id^=es-sw]").click();
  await dialog(p).waitFor({ timeout: 8000 });
  await dialog(p).locator(".nd-btn--confirm").click();
  await until(p, "#es-prof-op", /正在切换/, 8000);
  ok(/正在切换到「国内主号」/.test(await txt(p, "#es-prof-op")), "switching…");
  await until(p, "#es-prof-op", /已切换|没有|还不是/, 60000);
  ok(/已切换到「国内主号」/.test(await txt(p, "#es-prof-op")), `switch ok: ${await txt(p, "#es-prof-op")}`);
  await until(p, "#es-status", /使用中：国内主号/);
  ok(/使用中：国内主号/.test(await txt(p, "#es-status")), "status reads the new profile");
  ok(await nPost("/api/esim/switch") === 1, "switch: one POST");
}

const browser = await chromium.launch({ channel: "chrome" });
const urls = [];
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  p.on("request", (r) => { urls.push(r.url()); if (process.env.DEBUG) console.log(`  [${new Date().toISOString().slice(11, 19)}] ${r.method()} ${r.url().replace(WEB, "")}`); });
  if (process.env.DEBUG) p.on("response", async (r) => { if (r.status() !== 200) console.log(`  [resp ${r.status()}] ${r.url().replace(WEB, "")}`); });
  await p.goto(WEB + "/router/esim", { waitUntil: "load" });
  await ready(p).catch(() => ok(false, `${name} ${lang}: live content never showed`));
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
    await own(p, ctx);
    // let the last pushes and the status colour transition settle before axe reads colours (an
    // axe pass straight after the switch flaked on color-contrast ×5, 2 runs out of 6)
    await sleep(1500);
  }
  if (lang === "en") {
    // open a dialog so its text is checked too
    await p.locator("#es-list button[id^=es-sw]").first().click();
    await dialog(p).waitFor({ timeout: 8000 }).catch(() => {});
    // the profiles' nicknames are the card's data (the dialog's title quotes one)
    const left = (await mainText(p)).replace(/台湾旅行|国内主号|日本旅行/g, "").match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
    await dialog(p).locator("button", { hasText: "Cancel" }).click().catch(() => {});
  }
  if (name === "phone") {
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `390 px no sideways scroll (${sw})`);
  }
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length + (process.env.PROBE || v.id === "color-contrast" ? " " + v.nodes.map((n) => n.html + " " + n.failureSummary).join(" / ") : "")).join(", ") || "0"}`);
  ok(errs.length === 0, `${name}: no page errors ${errs.join("; ")}`);
  await p.screenshot({ path: `${OUT}/esim-${name}-${lang}-${theme}.png`, fullPage: true });
  await ctx.close();
}
const leaked = urls.filter((u) => ICCIDS.some((i) => u.includes(i)) || /LPA|ABCD-1234|%24/i.test(u));
ok(leaked.length === 0, `no ICCID or activation code in ${urls.length} request URLs ${leaked.join(" ")}`);
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
