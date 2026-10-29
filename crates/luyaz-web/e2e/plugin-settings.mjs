// The generic plugin settings page /settings/plugin (DR13, R26–R29, OV2) against the mock core's
// made-up plugin (src/mock/plugin_settings.rs), whose text carries markup and Datastar-looking
// strings: they show as literal text, never run, never land in an attribute; no plugin → no entry
// on /settings; a plain switch writes and reads back; a switch with a confirm sentence asks first;
// stale_revision re-reads and shows the current value; result_unknown reads back (changed / not);
// the plugin away → read only with 「最后确认于」, or only 「插件没在运行」 without a cache; an
// invalid description says so; axe 0 and no sideways scroll at 390 px. From source/manager:
//   WEB=http://127.0.0.1:18081 MOCK=http://127.0.0.1:19461 OUT=/tmp/lw-plugin crates/luyaz-web/e2e/run.sh plugin-settings
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mkdirSync } from "node:fs";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19461";
const OUT = process.env.OUT ?? "/tmp/lw-plugin";
mkdirSync(OUT, { recursive: true });
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const hook = (b) => fetch(`${MOCK}/mock/plugin-settings`, { method: "POST", body: JSON.stringify(b) });
const mock = async () => (await fetch(`${MOCK}/mock/plugin-settings`)).json();

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);

const browser = await chromium.launch(process.env.PW_EXE ? { executablePath: process.env.PW_EXE } : { channel: "chrome" });
const ctx = await browser.newContext({ viewport: { width: 390, height: 844 } });
await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: "zh", url: WEB }]);
const p = await ctx.newPage();
const errors = [];
const strays = [];
p.on("pageerror", (e) => errors.push(String(e)));
p.on("request", (q) => { if (new URL(q.url()).pathname === "/x") strays.push(q.url()); });
await p.addInitScript(() => { window.__pwn = 0; });
const main = async () => (await p.locator("main").innerText()).replace(/\s+/g, " ");
const axe = async (what) => {
  const a = await new AxeBuilder({ page: p }).include("main").analyze();
  ok(a.violations.length === 0, `${what}: axe 0 (${a.violations.map((v) => v.id).join(",")})`);
};
const noScroll = async (what) => ok(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `${what}: no sideways scroll at 390 px`);
const switchOf = (label) => p.getByRole("switch", { name: label });

// ---- no plugin: no entry ----
await hook({ reset: true });
await p.goto(`${WEB}/settings`, { waitUntil: "load" });
ok((await p.locator('a[href="/settings/plugin"]').count()) === 0, "not installed: no entry on /settings");
await p.goto(`${WEB}/settings/plugin`, { waitUntil: "load" });
ok((await main()).includes("没有可设置的插件"), "not installed: the page says so");
await hook({ state: "nosettings" });
await p.goto(`${WEB}/settings`, { waitUntil: "load" });
ok((await p.locator('a[href="/settings/plugin"]').count()) === 0, "no settings declared: no entry");

// ---- online ----
await hook({ state: "online" });
await p.goto(`${WEB}/settings`, { waitUntil: "load" });
const entry = p.locator('a[href="/settings/plugin"]');
ok((await entry.count()) === 1, "installed: the entry on /settings");
ok((await entry.innerText()).includes("<b>Demo</b> 插件"), "the entry shows the plugin's title as text");
await entry.click();
await p.waitForURL(/\/settings\/plugin$/);
let t = await main();
ok(t.includes("<script>window.__pwn=1</script>甲"), "markup in a label shows literally");
ok(t.includes("$pwn = 1; @post('/x')"), "Datastar-looking note shows literally");
ok(t.includes("正常 ${x}"), "status value shows literally");
ok((await p.locator("main script, main img").count()) === 0, "no element made from plugin text");
const attrHit = await p.evaluate(() => [...document.querySelectorAll("main *")].flatMap((e) => [...e.attributes].map((a) => a.value)).filter((v) => /pwn|@post|确定|<script|Demo/.test(v)));
ok(attrHit.length === 0, `no plugin text in any attribute (${attrHit.join(" | ")})`);
ok(t.includes("甲关着时乙不能改") === false && (await switchOf("乙").isEnabled()), "乙 can be switched while 甲 is on");
ok(!t.includes("等待时间"), "the sub group is not on the main page");
await noScroll("online");
await axe("online");
await p.screenshot({ path: `${OUT}/online.png`, fullPage: true });

// a plain switch: writes and reads back
await switchOf("乙").click();
await p.waitForURL(/m=done/);
ok((await main()).includes("已改好：乙现在是开"), "乙: written and read back");
ok((await switchOf("乙").getAttribute("aria-checked")) === "true", "乙 shows on");
let m = await mock();
ok(m.writes.at(-1)?.key === "beta" && m.writes.at(-1)?.value === true && m.writes.at(-1)?.rev === 1, `the write carried key, value, rev (${JSON.stringify(m.writes.at(-1))})`);

// a switch with a confirm sentence asks first, as text
await switchOf("<script>window.__pwn=1</script>甲").click();
await p.waitForURL(/ask=alpha/);
const ask = p.locator("#ps-ask");
ok((await ask.innerText()).includes(`<img src=x onerror=window.__pwn=1> data-on:click="@post('/x')" 确定关掉甲？`), "the confirm sentence shows literally");
ok((await mock()).writes.length === 1, "asking wrote nothing");
await axe("confirm");
await p.screenshot({ path: `${OUT}/confirm.png`, fullPage: true });
await ask.getByRole("button", { name: "确认：关掉" }).click();
await p.waitForURL(/m=done/);
ok((await switchOf("<script>window.__pwn=1</script>甲").getAttribute("aria-checked")) === "false", "甲 off after confirming");
ok((await main()).includes("甲关着时乙不能改") && !(await switchOf("乙").isEnabled()), "乙 disabled with the plugin's reason");

// stale revision: re-read, current value, nothing written
await hook({ next: "stale" });
await switchOf("<script>window.__pwn=1</script>甲").click();
await p.waitForURL(/ask=alpha/);
await p.locator("#ps-ask").getByRole("button", { name: "确认：打开" }).click();
await p.waitForURL(/m=stale/);
ok((await main()).includes("已被别处改过"), "stale: says changed elsewhere");
ok((await switchOf("<script>window.__pwn=1</script>甲").getAttribute("aria-checked")) === "false", "stale: shows the current value");

// changed elsewhere for real (rev moved): the old page's write is refused
await p.goto(`${WEB}/settings/plugin?ask=alpha`, { waitUntil: "load" });
await hook({ bump: true });
const before = (await mock()).rows.beta.value;
await p.locator("#ps-ask").getByRole("button", { name: "确认：打开" }).click();
await p.waitForURL(/m=stale/);
ok((await mock()).rows.alpha.value === false && (await mock()).rows.beta.value === before, "an old rev changes nothing");

// result unknown: read back decides
await hook({ next: "unknown" });
await p.goto(`${WEB}/settings/plugin?ask=alpha`, { waitUntil: "load" });
await p.locator("#ps-ask").getByRole("button", { name: "确认：打开" }).click();
await p.waitForURL(/m=unknown_not/);
ok((await main()).includes("读回显示没有改成"), "unknown, not applied: read back says so");
await hook({ next: "unknown_applied" });
await p.goto(`${WEB}/settings/plugin?ask=alpha`, { waitUntil: "load" });
await p.locator("#ps-ask").getByRole("button", { name: "确认：打开" }).click();
await p.waitForURL(/m=done/);
ok((await switchOf("<script>window.__pwn=1</script>甲").getAttribute("aria-checked")) === "true", "unknown, applied underneath: read back shows it changed");

// the sub page
await p.goto(`${WEB}/settings/plugin`, { waitUntil: "load" });
await p.getByRole("link", { name: "更多" }).click();
await p.waitForURL(/page=more/);
t = await main();
ok(t.includes("等待时间") && t.includes("25 秒") && !t.includes("乙"), "the sub page shows only its group");

// ---- the plugin away ----
await hook({ state: "offline" });
await p.goto(`${WEB}/settings/plugin`, { waitUntil: "load" });
t = await main();
ok(t.includes("插件没在运行") && t.includes("最后确认于") && t.includes("10-28 09:30"), "offline: read only with the last confirmed time");
ok((await p.locator("main form").count()) === 0 && !(await switchOf("乙").isEnabled()), "offline: no switch can be pressed");
await axe("offline");
await p.screenshot({ path: `${OUT}/offline.png`, fullPage: true });
const off = await fetch(`${WEB}/settings/plugin/set`, { method: "POST", redirect: "manual", headers: { cookie: `lw_sid=${sid}`, origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "key=beta&to=false&rev=99" });
ok((off.headers.get("location") ?? "").includes("m=offline"), `offline: a write is refused (${off.headers.get("location")})`);
await hook({ state: "offline_empty" });
await p.goto(`${WEB}/settings/plugin`, { waitUntil: "load" });
t = await main();
ok(t.includes("插件没在运行") && !t.includes("最后确认于") && !t.includes("乙"), "offline, no cache: only 「插件没在运行」");
await p.goto(`${WEB}/settings`, { waitUntil: "load" });
ok((await p.locator('a[href="/settings/plugin"]').count()) === 1, "offline: the entry stays");

// ---- invalid ----
await hook({ state: "invalid" });
await p.goto(`${WEB}/settings/plugin`, { waitUntil: "load" });
ok((await main()).includes("插件描述无效"), "invalid: says so");

ok(errors.length === 0, `no page errors (${errors.join(" | ")})`);
ok(strays.length === 0, `nothing requested /x (${strays.join(" | ")})`);
ok(await p.evaluate(() => window.__pwn === 0), "nothing the plugin wrote ran");
await hook({ reset: true });
await browser.close();
console.log(failed ? `${failed} failed` : "all ok");
process.exit(failed ? 1 : 0);
