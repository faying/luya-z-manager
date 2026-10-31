// Screenshots of luyaz-web on synthetic data; started by web.sh (see README.md).
//   WEB=http://127.0.0.1:28081 OUT=dir [CHROME=path] node shoot-web.mjs [name…]
// Uses @playwright/test from crates/luyaz-web/node_modules (npm ci there once).
import { createRequire } from "node:module";
import { existsSync } from "node:fs";
const require = createRequire(new URL("../../crates/luyaz-web/package.json", import.meta.url));
const { chromium } = require("@playwright/test");
const WEB = process.env.WEB ?? "http://127.0.0.1:28081";
const OUT = process.env.OUT ?? "out/web";
const only = process.argv.slice(2);

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = (r.headers.get("set-cookie") ?? "").match(/lw_sid=([^;]+)/)?.[1];
if (!sid) throw new Error(`login failed ${r.status}`);

const desk = { width: 1280, height: 800 }, phone = { width: 390, height: 844 };
const shots = [
  ["home", "/", desk], ["home-phone", "/", phone], ["home-phone-dark", "/", phone, "dark"],
  ["signal", "/signal", desk], ["clients", "/clients", desk], ["sms", "/sms", desk], ["health", "/health", desk],
  ["trends", "/trends", desk], ["changes", "/changes", desk], ["system", "/system", desk],
  ["device-info", "/device-info", desk], ["bandlock", "/bandlock", desk], ["usb", "/usb", desk],
  ["wifi", "/router/wifi", desk], ["sim", "/router/sim", desk], ["esim", "/router/esim", desk],
  ["login-phone", "/login", phone, "light", true],
];
// CHROME, else Playwright's own browser, else the system Google Chrome (macOS)
const SYSTEM_CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const b = process.env.CHROME
  ? await chromium.launch({ executablePath: process.env.CHROME })
  : await chromium.launch().catch(async (e) => {
      if (!existsSync(SYSTEM_CHROME)) throw e;
      console.error(`Playwright's browser not installed; using ${SYSTEM_CHROME}`);
      return chromium.launch({ executablePath: SYSTEM_CHROME });
    });
for (const [name, path, vp, scheme = "light", anon = false] of shots) {
  if (only.length && !only.includes(name)) continue;
  const ctx = await b.newContext({ viewport: vp, colorScheme: scheme, deviceScaleFactor: vp === phone ? 2 : 1 });
  const ck = [{ name: "u60_lang", value: "en", url: WEB }];
  if (!anon) ck.push({ name: "lw_sid", value: sid, url: WEB });
  await ctx.addCookies(ck);
  const p = await ctx.newPage();
  await p.goto(WEB + path, { waitUntil: "domcontentloaded" });
  await p.waitForTimeout(4500);
  if (name === "sms") { await p.getByRole("button", { name: "SIM", exact: true }).first().click().catch(() => p.getByText("SIM", { exact: true }).first().click()); await p.waitForTimeout(1500); }
  const h = Math.min(await p.evaluate(() => document.documentElement.scrollHeight), vp === phone ? 1800 : 2000);
  await p.setViewportSize({ width: vp.width, height: Math.max(vp.height, h) });
  await p.waitForTimeout(500);
  const file = `${OUT}/web-${name}.png`;
  await p.screenshot({ path: file });
  const zh = await p.evaluate(() => (document.body.innerText.match(/[一-鿿][^\n]{0,30}/g) ?? []).slice(0, 8));
  console.log(file, p.url().replace(WEB, ""), zh.length ? "ZH: " + zh.join(" | ") : "");
  await ctx.close();
}
await b.close();
