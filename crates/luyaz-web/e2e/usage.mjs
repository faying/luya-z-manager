// /usage (流量, docs/designs/travel-assist.md §4): the mock core's three days and two cards draw
// three bars, the two-card day stacks two colours, the legend names both cards, the per-card totals
// and the day rows carry the numbers; the home page's 「今天」 row links here; 390 px no sideways
// scroll; axe 0. Needs the mock u60d (MOCK) behind luyaz-web. From source/manager:
//   WEB=http://127.0.0.1:18081 OUT=/tmp/lw-usage crates/luyaz-web/e2e/run.sh usage
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mkdirSync } from "node:fs";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const OUT = process.env.OUT ?? "/tmp/lw-usage";
mkdirSync(OUT, { recursive: true });
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);

const browser = await chromium.launch(process.env.PW_EXE ? { executablePath: process.env.PW_EXE } : { channel: "chrome" });
const ctx = await browser.newContext({ viewport: { width: 390, height: 844 } });
await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: "zh", url: WEB }]);
const p = await ctx.newPage();
const errors = [];
p.on("pageerror", (e) => errors.push(String(e)));

await p.goto(`${WEB}/usage`, { waitUntil: "load" });
const main = (await p.locator("main").innerText()).replace(/\s+/g, " ");
ok((await p.locator(".us-bar").count()) === 3, "three days, three bars");
ok((await p.locator(".us-bar").nth(1).locator("i").count()) === 2, "10-20: two cards stacked");
ok(main.includes("中国电信 · 9403") && main.includes("中国联通 · 3668"), "legend names both cards");
ok(main.includes("按卡合计（近 3 天）"), "per-card totals title");
ok(main.includes("2026-10-20") && main.includes("中国联通 · 3668 68.9 MB"), `the two-card day lists both: ${main.slice(0, 300)}`);
ok(main.includes("10-19") && main.includes("10-21"), "axis: oldest to newest");
ok(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "390 px: no sideways scroll");
const a = await new AxeBuilder({ page: p }).include("main").analyze();
ok(a.violations.length === 0, `axe 0 (${a.violations.map((v) => v.id).join(",")})`);
await p.screenshot({ path: `${OUT}/usage.png`, fullPage: true });

await p.goto(`${WEB}/`, { waitUntil: "load" });
ok((await p.locator('a.nd-row[href="/usage"]').count()) === 1, "home: 「今天」 links to /usage");
ok(errors.length === 0, `no page errors (${errors.join(" | ")})`);
await browser.close();
console.log(failed ? `${failed} failed` : "all ok");
process.exit(failed ? 1 : 0);
