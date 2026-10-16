// The signal page in English (8c-A step 5): no Chinese left in the page body (operator names
// are data and may be anything), also in the roaming confirm and its pending line; axe; phone.
// Same servers as topbar.mjs; run from source/manager with e2e/run.sh:
//   WEB=http://127.0.0.1:18081 \
//     OUT=/tmp/lw crates/luyaz-web/e2e/run.sh signal-en
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const OUT = process.env.OUT ?? "/tmp/lw";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);

const browser = await chromium.launch({ channel: "chrome" });
for (const [w, h, name] of [[1280, 900, "desktop"], [390, 844, "phone"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: "en", url: WEB }]);
  const p = await ctx.newPage();
  await p.goto(`${WEB}/signal`);
  await p.waitForSelector("#ro-rsrp:not(:has(.nd-skel))", { timeout: 15000 }).catch(() => {});
  await p.waitForTimeout(1000);
  // everything in main except the operator name in the status meta line
  const body = async () => p.evaluate(() => {
    const m = document.querySelector("main").cloneNode(true);
    m.querySelectorAll(".nd-status__meta").forEach((e) => e.remove());
    m.querySelectorAll("[popover]").forEach((e) => e.setAttribute("data-t", e.textContent));
    return m.innerText + " " + [...m.querySelectorAll("[aria-label],[popover]")].map((e) => (e.getAttribute("aria-label") ?? "") + " " + (e.getAttribute("data-t") ?? "")).join(" ");
  });
  let left = (await body()).match(CJK) ?? [];
  ok(left.length === 0, `${name}: no Chinese in the page body (${[...new Set(left)].join("") || "none"})`);
  const meta = await p.locator(".nd-status__meta").textContent().catch(() => "");
  ok(/bars|carriers/.test(meta), `${name}: status meta in English: "${meta}"`);
  if (name === "desktop") {
    await p.click("#roam-sw button");
    await p.waitForSelector("#roam-confirm", { timeout: 5000 });
    left = (await body()).match(CJK) ?? [];
    ok(left.length === 0, `roaming confirm in English (${[...new Set(left)].join("") || "none"})`);
    await p.screenshot({ path: `${OUT}/signal-en-confirm.png` });
    await p.keyboard.press("Escape");
    await p.waitForSelector("#roam-confirm", { state: "detached", timeout: 5000 }).catch(() => {});
    for (const theme of ["light", "dark"]) {
      await p.emulateMedia({ colorScheme: theme });
      await p.waitForTimeout(200);
      const axe = await new AxeBuilder({ page: p }).analyze();
      ok(axe.violations.length === 0, `axe ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
    }
    ok(await p.evaluate(() => document.documentElement.lang) === "en", "html lang=en");
  } else {
    const [sw, iw] = await p.evaluate(() => [document.documentElement.scrollWidth, innerWidth]);
    ok(sw <= iw, `phone: scrollWidth ${sw} ≤ ${iw}`);
  }
  await p.screenshot({ path: `${OUT}/signal-en-${name}.png`, fullPage: true });
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} failed` : "all ok");
process.exit(failed ? 1 : 0);
