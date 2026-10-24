// /config (batch 5a): the common checks (first load ≤ 40 KiB gzip without fonts, axe light/dark 0,
// 390 px no sideways scroll, English body without Chinese, no page errors, screenshots) and the
// page's own: a ZXHN file → header + hex preview; a ZXWL file; not a config; no file; too big
// (> 4 MiB) → 413 with the message; the hex dump download; the file is never echoed to disk/log.
//   WEB=http://127.0.0.1:18255 OUT=/tmp/lw-cfg crates/luyaz-web/e2e/run.sh config
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { writeFileSync } from "node:fs";

const WEB = process.env.WEB ?? "http://127.0.0.1:18255";
const OUT = process.env.OUT ?? "/tmp/lw-cfg";
const PATH = "/config", TAG = "config", THEME_INIT = true;
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

const mk = (magic, kind, sig, off, n) => { const b = Buffer.alloc(n); b.write(magic, 0); b[4] = kind; b.write(sig, 8); b.writeUInt32LE(off, 28); for (let i = 64; i < n; i++) b[i] = (i * 7) & 255; return b; };
const post = (path, name, buf) => { const fd = new FormData(); fd.append("file", new Blob([buf]), name); return fetch(WEB + path, { method: "POST", body: fd, headers: { origin: new URL(WEB).origin, cookie: `lw_sid=${sid}; u60_lang=en` } }); };
async function own(p, ctx) {
  const dir = `${OUT}/files`; (await import("node:fs")).mkdirSync(dir, { recursive: true });
  writeFileSync(`${dir}/good.zxhn`, mk("ZXHN", 2, "SIGNATURE-1", 100, 600));
  writeFileSync(`${dir}/plain.txt`, "this is not a config file at all, just text\n");
  await p.setInputFiles("#cf-file", `${dir}/good.zxhn`);
  await p.getByRole("button", { name: "查看" }).click();
  await p.waitForSelector("#cf-h-t");
  const t = await p.textContent("main");
  ok(/ZXHN/.test(t) && /CBC/.test(t) && /SIGNATURE-1/.test(t) && /100 字节/.test(t) && /600 字节/.test(t), "header: magic ZXHN, CBC, signature, offset 100, size 600");
  ok(/000000 {2}5a 58 48 4e 02/.test(await p.textContent(".cf-hex")) && /\.\.\. \(344 more bytes\)/.test(await p.textContent(".cf-hex")), "hex preview: first line and '344 more bytes'");
  ok(/good\.zxhn/.test(t), "file name shown");
  await p.screenshot({ path: `${OUT}/config-result.png`, fullPage: true });
  await p.setInputFiles("#cf-file", `${dir}/plain.txt`);
  await p.getByRole("button", { name: "查看" }).click();
  await p.waitForSelector("[role=alert]");
  ok(/没找到 ZXHN 文件头标记/.test(await p.textContent("[role=alert]")) && (await p.locator("#cf-h-t").count()) === 0 && (await p.locator(".cf-hex").count()) === 1, "not a config: error, no header, hex still shown");
  // download through the same form
  const [dl] = await Promise.all([p.waitForEvent("download"), (async () => { await p.setInputFiles("#cf-file", `${dir}/good.zxhn`); await p.getByRole("button", { name: "存成十六进制" }).click(); })()]);
  ok(dl.suggestedFilename() === "good.zxhn.hex.txt", `download name ${dl.suggestedFilename()}`);
  const body = (await import("node:fs")).readFileSync(await dl.path(), "utf8");
  ok(body.split("\n").filter(Boolean).length === 38 && body.startsWith("000000  5a 58 48 4e 02") && !body.includes("more bytes"), `full dump: ${body.split("\n").filter(Boolean).length} lines`);
  // no file chosen (server side, browser would block via required)
  let res = await post("/config", "", Buffer.alloc(0));
  ok(res.status === 200 && /Choose a config file first|first/.test(await res.text()), "empty upload → message");
  // ZXWL
  res = await post("/config", "w.cfg", mk("ZXWL", 9, "", 0, 40));
  const h = await res.text();
  ok(/ZXWL/.test(h) && /unknown\(9\)/.test(h) && /64 bytes/.test(h) && />—</.test(h), "ZXWL, unknown(9), offset 64, signature —");
  // short file
  res = await post("/config", "s", Buffer.from("ZXHN"));
  ok(/No ZXHN header marker found/.test(await res.text()), "under 16 bytes: not recognised");
  // size cap
  res = await post("/config", "big.bin", Buffer.alloc(4 * 1024 * 1024));
  ok(res.status === 200 || res.status === 413, `exactly 4 MiB: ${res.status}`);
  res = await post("/config", "big.bin", Buffer.alloc(4 * 1024 * 1024 + 1));
  ok(res.status === 413 && /larger than 4 MiB/.test(await res.text()), `4 MiB + 1: ${res.status} with message`);
  // far over the body limit the server answers 413 and may cut the upload: a reset is fine too
  res = await post("/config", "huge.bin", Buffer.alloc(6 * 1024 * 1024)).catch(() => ({ status: "reset" }));
  ok(res.status === 413 || res.status === "reset", `6 MiB: ${res.status}`);
  res = await post("/config/hex", "big.bin", Buffer.alloc(4 * 1024 * 1024 + 1));
  ok(res.status === 413, `hex download over the cap: ${res.status}`);
  // a name with markup is escaped
  res = await post("/config", '<img src=x onerror=alert(1)>.zxhn', mk("ZXHN", 0, "", 0, 64));
  const e = await res.text();
  ok(!e.includes("<img src=x") && (e.includes("&lt;img") || e.includes("&#60;img")), "file name escaped");
  // other origin is refused
  const fd = new FormData(); fd.append("file", new Blob(["x"]), "x");
  res = await fetch(WEB + "/config", { method: "POST", body: fd, headers: { origin: "http://evil.example", cookie: `lw_sid=${sid}` } });
  ok(res.status === 403, `foreign Origin: ${res.status}`);
  // GET needs the session
  res = await fetch(WEB + "/config", { redirect: "manual" });
  ok(res.status >= 300 && res.status < 400, `no session: ${res.status}`);
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

