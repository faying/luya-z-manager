// Frame checks (8c-A step 1): three widths, ⌘K, language switch, axe, first-load gzip budget.
// From source/manager/web, with luyaz-web serving on WEB:
//   WEB=http://127.0.0.1:18081 OUT=/tmp/lw node crates/luyaz-web/e2e/shell.mjs
// or with run.sh:
//   WEB=… crates/luyaz-web/e2e/run.sh shell
// Servers: luyaz-web serve (--agent = fake-agent.mjs on the Mac, reached as host.docker.internal)
// + luyaz-web mock, e.g. a small local run script in a u60-rust-ci container.
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const OUT = process.env.OUT ?? "/tmp/lw";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };

// pages need a session now (8c-A step 2): log in once over HTTP and give each context the cookie
// (the fake agent's mock accepts any password)
async function sessionCookie() {
  const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
  const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
  if (!sid) throw new Error(`login failed: ${r.status} (is luyaz-web's --agent the fake agent?)`);
  return [{ name: "lw_sid", value: sid, url: WEB }];
}
const SESSION = await sessionCookie();
const browser = await chromium.launch({ channel: "chrome" });
for (const [name, width] of [["phone", 390], ["rail", 800], ["desk", 1280]]) {
  for (const theme of ["light", "dark"]) {
    const ctx = await browser.newContext({ viewport: { width, height: 900 }, colorScheme: theme });
    await ctx.addCookies(SESSION);
    const p = await ctx.newPage();
    let bytes = 0;
    const cdp = await ctx.newCDPSession(p);
    await cdp.send("Network.enable");
    cdp.on("Network.loadingFinished", (e) => { bytes += e.encodedDataLength; });
    for (const path of ["/signal", "/functions"]) {
      bytes = 0;
      await p.goto(WEB + path, { waitUntil: "load" });
      await p.waitForTimeout(300);
      // pass line 1: gzip bodies of the page and everything it loads, fonts listed apart
      if (path === "/signal" && name === "desk" && theme === "light") {
        const urls = [WEB + path, ...(await p.evaluate(() => [...document.querySelectorAll("link[href^='/a/'],script[src^='/a/']")].map((e) => e.href || e.src)))];
        const html = await p.content();
        for (const m of html.matchAll(/(?:url\(|href=")(\/a\/[^")#]+)/g)) urls.push(WEB + m[1].replaceAll("&amp;", "&"));
        let body = 0, fonts = 0;
        for (const u of new Set(urls)) {
          const r = await fetch(u, { headers: { cookie: `lw_sid=${SESSION[0].value}`, "accept-encoding": "gzip" } });
          const n = Number(r.headers.get("content-length") ?? (await r.arrayBuffer()).byteLength);
          if (u.includes(".woff2")) fonts += n; else body += n;
        }
        ok(body <= 40 * 1024, `first load ${(body / 1024).toFixed(1)} KiB gzip without fonts (≤ 40), fonts ${(fonts / 1024).toFixed(1)} KiB, ${new Set(urls).size} files`);
      }
      await p.screenshot({ path: `${OUT}/${name}-${theme}${path.replace("/", "-")}.png`, fullPage: false });
      const axe = await new AxeBuilder({ page: p }).analyze();
      ok(axe.violations.length === 0, `axe ${name} ${theme} ${path}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
    }
    if (theme === "light") {
      await p.keyboard.press(process.platform === "darwin" ? "Meta+k" : "Control+k");
      ok(await p.locator("#palette[open]").count() === 1, `${name}: ⌘K opens search`);
      await p.keyboard.type("xinhao");
      const vis = await p.locator("#palette-list li:not([hidden])").count();
      ok(vis === 2, `${name}: "xinhao" leaves ${vis} results (信号, 信号检测)`);
      const axe = await new AxeBuilder({ page: p }).analyze();
      ok(axe.violations.length === 0, `axe ${name} palette: ${axe.violations.map((v) => v.id).join(", ") || "0"}`);
      await p.screenshot({ path: `${OUT}/${name}-palette.png` });
      await p.keyboard.press("Enter");
      await p.waitForURL(/\/signal$/, { timeout: 3000 }).catch(() => {});
      ok(p.url().endsWith("/signal"), `${name}: Enter goes to the result (${p.url()})`);
    }
    await ctx.close();
  }
}
// language switch round trip
{
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  await ctx.addCookies(SESSION);
  const p = await ctx.newPage();
  await p.goto(WEB + "/functions");
  await p.click("form[action='/lang'] button");
  await p.waitForLoadState("load");
  ok(p.url().endsWith("/functions") && (await p.locator("h1").textContent()) === "Functions", "language → English, same page");
  ok((await p.evaluate(() => localStorage.getItem("u60_lang"))) === "en", "old web's language follows");
  await p.click("form[action='/lang'] button");
  ok((await p.locator("h1").textContent()) === "功能", "and back to Chinese");
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} failed` : "all passed");
process.exit(failed ? 1 : 0);
