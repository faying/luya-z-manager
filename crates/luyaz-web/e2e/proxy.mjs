// Login and the gate (8d-2: luyaz-web forwards nothing; W10/W14): a page sends you to /login and
// back, the login is checked against --password-file (the --agent given is unreachable, so an
// agent being asked would fail the login), an unrouted page is a 404 page, an unrouted /api/* a JSON
// 404, writes need this site as origin. From source/manager:
//   WEB=http://127.0.0.1:18081 crates/luyaz-web/e2e/run.sh proxy
// The password in the file must be `x`. Server: luyaz-web serve --password-file F --agent
// http://127.0.0.1:1 + luyaz-web mock, e.g. a small local run script (PWFILE=F).
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };

const browser = await chromium.launch({ channel: "chrome" });
const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale: "zh-CN" });
const p = await ctx.newPage();
const apiBad = [];
p.on("response", (r) => { const u = new URL(r.url()); if (u.pathname.startsWith("/api/") && r.status() >= 400) apiBad.push(`${r.status()} ${u.pathname}`); });

// 1. not logged in: a page sends you to the login page, and back after
// (since batch 5c every page of the old web's sidebar is migrated; /charts is a hub page)
await p.goto(WEB + "/charts");
ok(new URL(p.url()).pathname === "/login" && p.url().includes("next=/charts"), `page → login (${p.url()})`);
const axe = await new AxeBuilder({ page: p }).analyze();
ok(axe.violations.length === 0, `axe login: ${axe.violations.map((v) => v.id).join(", ") || "0"}`);
await p.fill("#pw", "x");
await p.click("button[type=submit]");
await p.waitForURL(/\/charts$/, { timeout: 5000 }).catch(() => {});
ok(p.url().endsWith("/charts"), `login → back to the page (${p.url()})`);
const cookies = await ctx.cookies();
const sid = cookies.find((c) => c.name === "lw_sid");
ok(sid && sid.httpOnly && sid.sameSite === "Strict", `session cookie HttpOnly + Strict (${sid && sid.sameSite})`);

const wrong = await fetch(WEB + "/login", { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=nope&next=/" });
ok(!/lw_sid=/.test(wrong.headers.get("set-cookie") ?? ""), `wrong password: no session (${wrong.status})`);

// 2. the page works (no 401s, no expired dialog)
await p.waitForTimeout(2500);
ok(await p.locator("h1").first().isVisible(), `page rendered: "${(await p.locator("h1").first().textContent())?.trim()}"`);
ok(apiBad.length === 0, `API calls all fine (${apiBad.slice(0, 5).join(", ") || "none failed"})`);
ok(await p.evaluate(() => !document.querySelector("[role=dialog]")?.textContent?.includes("过期")), "no session-expired dialog");

// 3. sidebar → signal page
await p.locator("aside a[href='/signal']").first().click();
await p.waitForSelector("#sig-status", { timeout: 8000 }).catch(() => {});
ok(await p.locator("#sig-status").count() === 1, `sidebar → signal page (${p.url()})`);
// W9: u60d's signal block has net_select_mode now (the mock fills it into old recordings)
await p.waitForFunction(() => /选网方式[\s\S]{0,400}自动/.test(document.getElementById("sig-cell")?.textContent ?? ""), null, { timeout: 8000 }).catch(() => {});
ok(/选网方式[\s\S]{0,400}自动/.test(await p.locator("#sig-cell").textContent()), "选网方式 row shows 自动");

// 4. (new → old link: gone with batch 5c, no old page is left in the sidebar)

// 5. nothing is forwarded: unrouted /api/* is a JSON 404, an unrouted page a 404 page
const cookieHdr = `lw_sid=${sid?.value}`;
const post = (path, origin) => fetch(WEB + path, { method: "POST", headers: { cookie: cookieHdr, origin, "content-type": "application/json" }, body: "{}" });
for (const [m, h] of [["with the cookie", { cookie: cookieHdr }], ["without it", {}], ["with a Bearer", { authorization: "Bearer mock-token-1" }]]) {
  const r = await fetch(WEB + "/api/network/signal", { headers: h });
  const j = await r.json().catch(() => null);
  ok(r.status === 404 && (r.headers.get("content-type") ?? "").includes("json") && j?.ok === false && j?.error === "not_found" && Object.keys(j).length === 2, `GET /api/network/signal ${m} → JSON 404 (${r.status})`);
}
const w = await post("/api/sms/delete", new URL(WEB).origin);
ok(w.status === 404, `POST /api/sms/delete from this site → 404, not forwarded (${w.status})`);
const pg = await fetch(WEB + "/no/such/page", { headers: { cookie: cookieHdr, accept: "text/html" } });
ok(pg.status === 404 && /text\/html/.test(pg.headers.get("content-type") ?? "") && (await pg.text()).includes('href="/"'), `unrouted page → 404 page with a link home (${pg.status})`);
const sch = await fetch(WEB + "/router/schedule", { headers: { cookie: cookieHdr }, redirect: "manual" });
ok([301, 302, 303, 307, 308].includes(sch.status) && new URL(sch.headers.get("location") ?? "", WEB).pathname === "/scheduler", `/router/schedule redirects (${sch.status} → ${sch.headers.get("location")})`);
// writes need this site as origin
ok((await post("/roam/ask", "http://evil.example")).status === 403, "own write from another origin → 403");
ok((await fetch(WEB + "/roam/ask", { method: "POST", headers: { cookie: cookieHdr }, body: "{}" })).status === 403, "write without Origin/Referer → 403");
ok((await post("/roam/ask", new URL(WEB).origin)).status === 200, "own write from this site → 200");

// 6. logout ends the session
await p.goto(WEB + "/signal");
await p.click("form[action='/logout'] button");
await p.waitForURL(/\/login/, { timeout: 5000 }).catch(() => {});
ok(new URL(p.url()).pathname === "/login", "logout → login page");
await p.goto(WEB + "/signal");
ok(new URL(p.url()).pathname === "/login", "after logout the new page needs login again");

await browser.close();
console.log(failed ? `${failed} failed` : "all passed");
process.exit(failed ? 1 : 0);
