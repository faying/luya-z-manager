// The home page's landing card (docs/designs/travel-assist.md §5): the mock core landed in Japan
// from China 2 hours ago; the card names the country, where from, the network, the home exit and
// the data since landing; 「收起」 hides it; 390 px no sideways scroll; axe 0. Needs the mock u60d
// behind luyaz-web. From source/manager:
//   WEB=http://127.0.0.1:18081 OUT=/tmp/lw-landing crates/luyaz-web/e2e/run.sh landing
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mkdirSync } from "node:fs";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const OUT = process.env.OUT ?? "/tmp/lw-landing";
mkdirSync(OUT, { recursive: true });
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);
const browser = await chromium.launch(process.env.PW_EXE ? { executablePath: process.env.PW_EXE } : {});
const ctx = await browser.newContext({ viewport: { width: 390, height: 844 } });
await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: "zh", url: WEB }]);
const p = await ctx.newPage();
const errors = [];
p.on("pageerror", (e) => errors.push(String(e)));

await p.goto(`${WEB}/`, { waitUntil: "load" });
const card = p.locator("#hm-land");
await card.getByRole("button", { name: "收起" }).waitFor({ timeout: 15000 }).catch(() => {});
const text = (await card.innerText().catch(() => "")).replace(/\s+/g, " ");
ok(await card.isVisible(), "the card shows");
ok(text.includes("到了日本") && text.includes("从中国来 · 2 小时前"), `title and where from: ${text.slice(0, 160)}`);
ok(text.includes("网络") && text.includes("家里出口") && text.includes("落地以来流量") && text.includes("876"), `rows: ${text}`);
ok(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "390 px: no sideways scroll");
const a = await new AxeBuilder({ page: p }).include("main").analyze();
ok(a.violations.length === 0, `axe 0 (${a.violations.map((v) => v.id).join(",")})`);
await card.screenshot({ path: `${OUT}/card.png` });
await card.getByRole("button", { name: "收起" }).click();
await p.waitForTimeout(1500);
ok(!(await card.isVisible()), "「收起」 hides it");
await p.reload({ waitUntil: "load" });
await p.waitForTimeout(3000);
ok(!(await card.isVisible()), "stays hidden after a reload");
ok(errors.length === 0, `no page errors ${errors.join(" | ")}`);
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
