// /settings (batch 5a): the common checks (first load ≤ 40 KiB gzip without fonts, axe light/dark 0,
// 390 px no sideways scroll, English body without Chinese, no page errors, screenshots) and the
// page's own: theme buttons write localStorage u60.theme and <html data-theme> and survive a
// reload, language form (POST /lang) switches the page, log out (POST /logout) ends the session.
//   WEB=http://127.0.0.1:18255 OUT=/tmp/lw-set crates/luyaz-web/e2e/run.sh settings
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { writeFileSync } from "node:fs";

const WEB = process.env.WEB ?? "http://127.0.0.1:18255";
const OUT = process.env.OUT ?? "/tmp/lw-set";
const PATH = "/settings", TAG = "settings", THEME_INIT = true;
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
async function login() {
  const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
  const s = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
  if (!s) throw new Error(`login failed: ${r.status}`);
  return s;
}
let sid = await login();
async function firstLoad(p) {
  const urls = [p.url()];
  const html = await p.content();
  for (const m of html.matchAll(/(?:url\(|href=")(\/a\/[^")#]+)/g)) urls.push(WEB + m[1].replaceAll("&amp;", "&"));
  for (const m of html.matchAll(/src="(\/a\/[^"]+)"/g)) urls.push(WEB + m[1]);
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
  m.querySelectorAll("[data-data]").forEach((e) => e.remove());
  return m.innerText + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});

const theme = (p) => p.evaluate(() => [localStorage.getItem("u60.theme"), document.documentElement.dataset.theme]);
async function own(p, ctx) {
  const pick = async (name) => { await p.getByRole("radio", { name }).click(); };
  ok((await p.getByRole("radio", { name: "浅色" }).getAttribute("aria-checked")) === "true", "initial: stored light is the checked one");
  await pick("深色");
  ok(JSON.stringify(await theme(p)) === '["dark","dark"]', `dark: ${JSON.stringify(await theme(p))}`);
  ok((await p.getByRole("radio", { name: "深色" }).getAttribute("aria-checked")) === "true" && (await p.getByRole("radio", { name: "浅色" }).getAttribute("aria-checked")) === "false", "dark is the checked one");
  await p.reload({ waitUntil: "load" });
  await p.waitForTimeout(300);
  ok(JSON.stringify(await theme(p)) === '["dark","dark"]' && (await p.getByRole("radio", { name: "深色" }).getAttribute("aria-checked")) === "true", "dark survives a reload (applied and shown)");
  await p.screenshot({ path: `${OUT}/settings-dark-after.png`, fullPage: true });
  await pick("跟随系统");
  const t = await theme(p);
  ok(t[0] === null && (t[1] === "light" || t[1] === "dark"), `auto: key removed, theme follows system (${JSON.stringify(t)})`);
  await pick("浅色");
  ok(JSON.stringify(await theme(p)) === '["light","light"]', "light again");
  // language: the form posts /lang and comes back here in English
  await p.getByRole("radio", { name: "English" }).click();
  await p.waitForURL(WEB + PATH);
  await p.waitForSelector("h1");
  ok((await p.textContent("h1")).trim() === "Settings", "English after the language form");
  ok((await p.getByRole("radio", { name: "English" }).getAttribute("aria-checked")) === "true", "English is the checked one");
  ok((await ctx.cookies()).some((c) => c.name === "u60_lang" && c.value === "en"), "cookie u60_lang=en");
  await p.getByRole("radio", { name: "中文" }).click();
  await p.waitForSelector("h1:has-text('设置')");
  ok(true, "back to Chinese");
  // log out: the session ends, the page needs a login again
  await p.getByRole("button", { name: "退出登录" }).last().click();
  await p.waitForURL(/\/login/);
  ok(true, `logged out → ${p.url()}`);
  const again = await ctx.request.get(WEB + PATH, { maxRedirects: 0 });
  ok(again.status() === 302 || again.status() === 303 || /login/.test(await again.text()), `/settings after logout: ${again.status()}`);
}
const browser = await chromium.launch({ channel: "chrome" });
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"], [390, 844, "phone", "en", "dark"]]) {
  sid = await login();
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  if (THEME_INIT) await ctx.addInitScript((t) => { if (!sessionStorage.getItem("seeded")) { localStorage.setItem("u60.theme", t); sessionStorage.setItem("seeded", "1"); } }, theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  await p.goto(WEB + PATH, { waitUntil: "load" });
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
  }
  if (lang === "en") {
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
  }
  if (name === "phone") {
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `${lang}: 390 px no sideways scroll (${sw})`);
  }
  await p.waitForTimeout(300);
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
  await p.screenshot({ path: `${OUT}/${TAG}-${name}-${lang}-${theme}.png`, fullPage: true });
  if (name === "desk" && lang === "zh") await own(p, ctx);
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);

