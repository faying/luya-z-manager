// The tailnet-poller banner on / and its allow list on /settings (docs/designs/tailnet-poller.md):
// an event in the mock core shows the banner (who, port, minutes, the 0.3–0.6 W line, the gateway
// caveat); 「是我自己的」 allows it for 24 h and the banner goes; /settings lists the entry, 「改成永久」
// makes it permanent, 「删除」 empties the list; a new event and 「知道了」 hide the banner; axe 0 on
// both pages with the banner / the list showing. Needs the mock u60d (MOCK) behind luyaz-web; the
// rest of the home page's agent reads may fail (not checked here). From source/manager:
// 「知道了」 also moves the alert queue's read cursor to the poller alert (one dismiss).
//   WEB=http://127.0.0.1:18081 MOCK=http://127.0.0.1:19461 OUT=/tmp/lw-poller crates/luyaz-web/e2e/run.sh poller
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mkdirSync } from "node:fs";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19461";
const OUT = process.env.OUT ?? "/tmp/lw-poller";
mkdirSync(OUT, { recursive: true });
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const hook = (b) => fetch(`${MOCK}/mock/poller`, { method: "POST", body: JSON.stringify(b) });

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);

// PW_EXE: a browser already on the machine when the pinned one is not downloaded
const browser = await chromium.launch(process.env.PW_EXE ? { executablePath: process.env.PW_EXE } : {});
const ctx = await browser.newContext({ viewport: { width: 390, height: 844 } });
await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: "zh", url: WEB }]);
const p = await ctx.newPage();
const errors = [];
p.on("pageerror", (e) => errors.push(String(e)));
const axe = async (what) => {
  const a = await new AxeBuilder({ page: p }).include("main").analyze();
  ok(a.violations.length === 0, `${what}: axe 0 (${a.violations.map((v) => v.id).join(",")})`);
};

// ---- the banner ----
await hook({ reset: true, event: true });
await p.goto(`${WEB}/`, { waitUntil: "load" });
const banner = p.locator("#hm-poller");
await banner.getByRole("button", { name: "是我自己的" }).waitFor({ timeout: 15000 }).catch(() => {});
const text = (await banner.innerText().catch(() => "")).replace(/\s+/g, " ");
ok(await banner.isVisible(), "event: the banner shows");
ok(text.includes("有设备在经 Tailscale 持续访问管理接口"), "title");
ok(text.includes("来自 192.168.50.88（经家里网关转发") && text.includes(":80，已 12 分钟（约 550 包/分钟）"), `who, port, how long: ${text.slice(0, 120)}`);
ok(text.includes("0.3–0.6 W"), "the cost");
ok(text.includes("整个网段"), "the gateway caveat beside the button");
const scroll = await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth);
ok(scroll, "390 px: no sideways scroll");
await axe("home with the banner");
await p.screenshot({ path: `${OUT}/home-banner.png` });

await banner.getByRole("button", { name: "是我自己的" }).click();
await p.getByText("已放行 24 小时").waitFor({ timeout: 10000 }).catch(() => {});
ok(await p.getByText("已放行 24 小时").isVisible(), "「是我自己的」: result line");
await p.waitForTimeout(2500);
ok(await p.locator("#hm-poller-op").getByText("已放行 24 小时").isVisible(), "the result line survives the push that hides the banner");
ok((await banner.getByRole("button", { name: "是我自己的" }).count()) === 0, "the banner's buttons are gone");
const list = await (await fetch(`${MOCK}/v2/api/poller`)).json();
ok(list.data.allow.length === 1 && list.data.allow[0].peer === "192.168.50.88" && list.data.allow[0].port === 80 && list.data.allow[0].until > Date.now() / 1000 + 86000, `allowed 24 h: ${JSON.stringify(list.data.allow)}`);

// ---- the allow list ----
await p.goto(`${WEB}/settings`, { waitUntil: "load" });
const sec = p.locator("section[aria-labelledby='st-pl-t']");
let st = (await sec.innerText()).replace(/\s+/g, " ");
ok(st.includes("192.168.50.88 :80") && st.includes("到 "), `settings: the entry with its end: ${st.slice(0, 160)}`);
await axe("settings with the list");
await p.screenshot({ path: `${OUT}/settings-list.png`, fullPage: true });
await sec.getByRole("button", { name: "改成永久：192.168.50.88 :80" }).click();
await p.waitForURL(/\/settings\?pm=forever/);
st = (await p.locator("section[aria-labelledby='st-pl-t']").innerText()).replace(/\s+/g, " ");
ok(st.includes("已改成永久") && st.includes("永久") && !st.includes("到 "), `made permanent: ${st.slice(0, 160)}`);
await p.locator("section[aria-labelledby='st-pl-t']").getByRole("button", { name: "删除：192.168.50.88 :80" }).click();
await p.waitForURL(/\/settings\?pm=removed/);
st = (await p.locator("section[aria-labelledby='st-pl-t']").innerText()).replace(/\s+/g, " ");
ok(st.includes("已删除") && st.includes("还没有放行的设备"), `deleted: ${st.slice(0, 160)}`);

// ---- 「知道了」 ----
await fetch(`${MOCK}/mock/alerts?unread=0`, { method: "POST" });
await hook({ reset: true, event: true });
await p.goto(`${WEB}/`, { waitUntil: "load" });
await banner.getByRole("button", { name: "知道了" }).waitFor({ timeout: 15000 }).catch(() => {});
await banner.getByRole("button", { name: "知道了" }).click();
await p.getByText("已收起").waitFor({ timeout: 10000 }).catch(() => {});
ok((await banner.getByRole("button", { name: "知道了" }).count()) === 0, "「知道了」: the banner goes");
const after = await (await fetch(`${MOCK}/v2/api/poller`)).json();
ok(after.data.events[0].acked === true && after.data.allow.length === 0, "acked, nothing allowed");
const hk = await (await fetch(`${MOCK}/mock/poller`)).json();
ok(hk.read === 3, `「知道了」 also marks the tailnet-poller alert read (cursor ${hk.read})`);

await p.reload({ waitUntil: "load" });
await p.waitForTimeout(1500);
ok(!(await banner.isVisible()), "after a reload it stays hidden");

// ---- English ----
await hook({ reset: true, event: true });
await ctx.addCookies([{ name: "u60_lang", value: "en", url: WEB }]);
await p.goto(`${WEB}/`, { waitUntil: "load" });
await banner.getByRole("button", { name: "It's mine" }).waitFor({ timeout: 15000 }).catch(() => {});
const en = await banner.innerText().catch(() => "");
ok(en.includes("keeps polling the admin ports over Tailscale") && !/[一-鿿]/.test(en), "English banner without Chinese");

await hook({ reset: true });
ok(errors.length === 0, `no page errors ${errors.join(" | ")}`);
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
