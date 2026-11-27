// /router/esim per card on a dual-slot unit (dual-slot design §6, T8) and the slot rows of
// /router/sim. The luyaz-web mock publishes a made-up `cards` block (`POST /mock/cards?set=`);
// the typed mock agent (fixtures/esim.ts) serves the matching cards (scenarios esim-dual1 /
// esim-dual2) and, on a dual-slot unit, refuses a write that does not name its card (`slot`).
//   1. dual2 (two eUICCs): one block per slot, each with its own card's profiles; a rename on
//      slot 1's card lands on that card only.
//   2. pending: slot 2's card being identified → a state line, not an error.
//   3. dual1 (plain SIM in slot 1, eUICC in slot 2): only slot 2's block, 「卡槽 2 · 待机」, its
//      profiles; rename and switch on it (each needs the card's EID at the mock); the EID never
//      in a URL; axe, 390 px, English text.
//   4. /router/sim on dual1: a row per slot, status only.
// The single-slot page is checked unchanged by router-esim.mjs.
//   WEB=http://127.0.0.1:18081 MOCK=http://127.0.0.1:19461 AGENT=http://127.0.0.1:19090 \
//     OUT=/tmp/lw-esim-dual crates/luyaz-web/e2e/run.sh router-esim-dual
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mkdirSync } from "node:fs";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19461";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19090";
const OUT = process.env.OUT ?? "/tmp/lw-esim-dual";
mkdirSync(OUT, { recursive: true });
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
const EIDS = ["89049000000000000000000000000041", "89049000000000000000000000005678"];
const scenario = (s) => fetch(`${AGENT}/__mock/scenario?set=${s}`);
const cards = (s) => fetch(`${MOCK}/mock/cards?set=${s}`, { method: "POST" });
const posts = () => fetch(`${AGENT}/__lw/esim`).then((r) => r.json());
const nPost = async (p) => (await posts())[p] ?? 0;

async function login() {
  const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
  const s = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
  if (!s) throw new Error(`login failed: ${r.status}`);
  return s;
}
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, sel, re, ms = 20000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout: ms }).catch(() => {});
const mainText = (p) => p.evaluate(() => {
  const m = document.querySelector("main").cloneNode(true);
  m.querySelectorAll("[data-data], [hidden]").forEach((e) => e.remove());
  return m.innerText + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});
const row = (p, name) => p.locator("#es-list-s2 .es-row", { has: p.locator(`[id^=es-n]:text-is("${name}")`) });
const dialog = (p) => p.locator("dialog#es-prof-s2-confirm[open]");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

await fetch(`${AGENT}/__lw/esim/reset`, { method: "POST" });
let sid = await login();
const browser = await chromium.launch({ channel: "chrome" });
const urls = [];

async function open(lang, w = 1280, theme = "light") {
  const ctx = await browser.newContext({ viewport: { width: w, height: 900 } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  p.on("request", (r) => urls.push(r.url()));
  return { ctx, p, errs };
}

// ---- 1. dual2: two eUICCs, one block each, a write lands on its own card
await cards("dual2");
await scenario("esim-dual2");
await sleep(500);
{
  const { ctx, p, errs } = await open("zh");
  await p.goto(WEB + "/router/esim", { waitUntil: "load" });
  await until(p, "#es-status-s1", /使用中/);
  await until(p, "#es-status-s2", /使用中/);
  ok((await txt(p, "#es-hd-s1")) === "卡槽 1 · 上网中" && (await txt(p, "#es-hd-s2")) === "卡槽 2 · 待机", `headings: ${await txt(p, "#es-hd-s1")} / ${await txt(p, "#es-hd-s2")}`);
  ok(await p.locator("#es-status").count() === 0 && await p.locator("#es-list").count() === 0, "no single-slot block on a dual-slot unit");
  ok(/使用中：台湾旅行/.test(await txt(p, "#es-status-s1")) && /使用中：英国卡/.test(await txt(p, "#es-status-s2")), `each slot its own card: ${await txt(p, "#es-status-s1")} / ${await txt(p, "#es-status-s2")}`);
  ok(await p.locator("#es-list-s1 .es-row").count() === 3 && await p.locator("#es-list-s2 .es-row").count() === 2, "3 + 2 profiles");
  ok(/89049000000000000000000000000041/.test(await txt(p, "#es-chip-s1")) && /89049000000000000000000000005678/.test(await txt(p, "#es-chip-s2")), "each chip its own EID");
  // rename on slot 1's card: the same ISD-P AID exists on card B, only card A changes
  await p.waitForFunction(() => !document.querySelector("#es-p-s1-2 button[disabled]"), null, { timeout: 20000 }).catch(() => {});
  await p.locator("#es-rn-s1-2").click();
  await p.locator("#es-ni-s1-2").waitFor({ state: "visible", timeout: 5000 });
  ok(!(await p.locator("#es-ni-s2-0").isVisible()), "the rename box opens on one card only");
  await p.locator("#es-ni-s1-2").fill("太平洋");
  await p.locator("#es-ni-s1-2").press("Enter");
  await p.waitForSelector("#es-prof-s1-confirm", { timeout: 8000 });
  await p.click("#es-prof-s1-confirm .nd-btn--confirm");
  await until(p, "#es-prof-s1-op", /昵称已保存|没有生效/);
  ok(/昵称已保存/.test(await txt(p, "#es-prof-s1-op")), `rename on slot 1: ${await txt(p, "#es-prof-s1-op")}`);
  await until(p, "#es-list-s1", /太平洋/);
  ok(/太平洋/.test(await txt(p, "#es-list-s1")) && !/太平洋/.test(await txt(p, "#es-list-s2")), "only slot 1's card has the new name");
  ok(errs.length === 0, `dual2: no page errors ${errs.join("; ")}`);
  await p.screenshot({ path: `${OUT}/esim-dual2.png`, fullPage: true });
  await ctx.close();
}

// ---- 2. pending: a state line, not an error
await cards("pending");
await scenario("esim-dual1");
await sleep(500);
{
  const { ctx, p, errs } = await open("zh");
  await p.goto(WEB + "/router/esim", { waitUntil: "load" });
  await until(p, "#es-status-s2", /正在识别/);
  ok(/正在识别卡/.test(await txt(p, "#es-status-s2")) && await p.locator("#es-status-s2 [role=alert]").count() === 0, `pending: ${await txt(p, "#es-status-s2")}`);
  ok(/正在识别这个卡槽里的卡/.test(await txt(p, "#es-list-s2")), `pending list line: ${await txt(p, "#es-list-s2")}`);
  ok(await p.locator("#es-hd-s1").count() === 0, "slot 1's plain SIM has no block");
  // the card is identified while the page is open: the same block fills in
  await cards("dual1");
  await until(p, "#es-status-s2", /使用中：英国卡/, 30000);
  ok(/使用中：英国卡/.test(await txt(p, "#es-status-s2")), `identified in place: ${await txt(p, "#es-status-s2")}`);
  ok(errs.length === 0, `pending: no page errors ${errs.join("; ")}`);
  await ctx.close();
}

// ---- 3. dual1: the eUICC in slot 2, managed per card
{
  const { ctx, p, errs } = await open("zh");
  await p.goto(WEB + "/router/esim", { waitUntil: "load" });
  await p.waitForFunction(() => document.querySelectorAll("#es-list-s2 .es-row").length === 2 && !document.querySelector("#es-p-s2-1 button[disabled]"), null, { timeout: 20000 }).catch(() => {});
  ok((await txt(p, "#es-hd-s2")) === "卡槽 2 · 待机", `heading: ${await txt(p, "#es-hd-s2")}`);
  ok(await p.locator("[id^=es-hd]").count() === 1, "one block: slot 1's plain SIM is not shown");
  ok(/使用中：英国卡/.test(await txt(p, "#es-status-s2")), `status: ${await txt(p, "#es-status-s2")}`);
  ok(await p.locator("#es-list-s2 .es-row").count() === 2, "card B's two profiles");
  ok(/89049000000000000000000000005678/.test(await txt(p, "#es-chip-s2")), "chip: card B's EID");
  ok(/不在上网的卡槽只换卡上的配置/.test(await txt(p, "main")), "says what a switch does on a slot that isn't the data slot");

  // rename (tier 2) on card B
  await row(p, "Lion Roam").locator("button[id^=es-rn]").click();
  const input = p.locator("#es-ni-s2-1");
  await input.waitFor({ state: "visible", timeout: 5000 });
  await input.fill("狮城");
  await input.press("Enter");
  await p.waitForSelector("#es-prof-s2-confirm", { timeout: 8000 });
  await p.click("#es-prof-s2-confirm .nd-btn--confirm");
  await until(p, "#es-prof-s2-op", /昵称已保存|没有生效/);
  ok(/昵称已保存/.test(await txt(p, "#es-prof-s2-op")), `rename on card B (EID sent): ${await txt(p, "#es-prof-s2-op")}`);
  await until(p, "#es-list-s2", /狮城/);
  ok(await row(p, "狮城").count() === 1, "card B's list reads the new nickname back");

  // switch (tier 3) on card B
  await row(p, "狮城").locator("button[id^=es-sw]").click();
  await dialog(p).waitFor({ timeout: 8000 });
  const ax = await new AxeBuilder({ page: p }).include("dialog").analyze();
  ok(ax.violations.length === 0, `switch dialog axe: ${ax.violations.map((v) => v.id).join(", ") || "0"}`);
  ok(/移动数据不会中断/.test(await txt(p, "dialog#es-prof-s2-confirm")) && !/断开多久/.test(await txt(p, "dialog#es-prof-s2-confirm")), "a standby slot's switch says data isn't cut");
  await dialog(p).locator(".nd-btn--confirm").click();
  await until(p, "#es-prof-s2-op", /正在切换/, 8000);
  await until(p, "#es-prof-s2-op", /已切换到|没有生效|没有切过去|还不是/, 60000);
  ok(/已切换到「狮城」/.test(await txt(p, "#es-prof-s2-op")), `switch on card B: ${await txt(p, "#es-prof-s2-op")}`);
  await until(p, "#es-status-s2", /使用中：狮城/);
  ok(/使用中：狮城/.test(await txt(p, "#es-status-s2")), "status reads the new profile");
  ok(await nPost("/api/esim/switch") === 1 && await nPost("/api/esim/nickname") === 2, "one POST per confirm (a rename in step 1, one here)");

  // the cards change under the open page: it says so
  await cards("dual2");
  await until(p, "#es-cards", /卡变了/, 15000);
  ok(/重新载入这一页/.test(await txt(p, "#es-cards")) && await p.locator("#es-cards:not([hidden]) a[href='/router/esim']").count() === 1, `cards changed: ${await txt(p, "#es-cards")}`);
  await cards("dual1");
  ok(errs.length === 0, `dual1: no page errors ${errs.join("; ")}`);
  await p.screenshot({ path: `${OUT}/esim-dual1-zh.png`, fullPage: true });
  await ctx.close();
}
for (const [lang, w, theme] of [["en", 1280, "dark"], ["zh", 390, "light"]]) {
  const { ctx, p, errs } = await open(lang, w, theme);
  await p.goto(WEB + "/router/esim", { waitUntil: "load" });
  await p.waitForFunction(() => document.querySelectorAll("#es-list-s2 .es-row").length === 2, null, { timeout: 20000 }).catch(() => {});
  await sleep(1000);
  if (lang === "en") {
    ok((await txt(p, "#es-hd-s2")) === "Slot 2 · Standby", `English heading: ${await txt(p, "#es-hd-s2")}`);
    const left = (await mainText(p)).replace(/英国卡|狮城/g, "").match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
  } else {
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `390 px no sideways scroll (${sw})`);
  }
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${lang} ${w} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
  ok(errs.length === 0, `${lang} ${w}: no page errors ${errs.join("; ")}`);
  await p.screenshot({ path: `${OUT}/esim-dual1-${lang}-${w}.png`, fullPage: true });
  await ctx.close();
}

// ---- 4. /router/sim: a row per slot, status only
{
  const { ctx, p, errs } = await open("zh");
  await p.goto(WEB + "/router/sim", { waitUntil: "load" });
  await until(p, "#sim-slots", /卡槽 2/);
  const s = await txt(p, "#sim-slots");
  ok(/卡槽 1\s*上网中 · 普通 SIM 卡/.test(s) && /卡槽 2\s*待机 · eSIM 卡/.test(s) && /…5678/.test(s), `slot rows: ${s}`);
  ok(/只显示状态/.test(s) && await p.locator("#sim-slots button").count() === 0, "slot 2: status only, no PIN action");
  const axe = await new AxeBuilder({ page: p }).include("#sim-slots").analyze();
  ok(axe.violations.length === 0, `sim slots axe: ${axe.violations.map((v) => v.id).join(", ") || "0"}`);
  ok(errs.length === 0, `sim: no page errors ${errs.join("; ")}`);
  await p.screenshot({ path: `${OUT}/sim-dual1.png`, fullPage: true });
  await ctx.close();
}

const leaked = urls.filter((u) => EIDS.some((e) => u.includes(e)));
ok(leaked.length === 0, `no EID in ${urls.length} request URLs ${leaked.join(" ")}`);
await scenario("normal");
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
