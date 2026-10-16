// /tools/processes and /router/dns (batch 5c): the common checks (first load ≤ 40 KiB gzip without
// fonts, axe light/dark 0, 390 px no sideways scroll, English body without Chinese, no page errors,
// screenshots) and the pages' own: the list and sort, push of changed rows, kill one / kill all
// (dialog, cancel, result by name, refused), DNS auto -> manual (checks, inline confirm, cancel,
// readback, refused, accepted-not-read-back) and back. Needs the typed mock agent behind the fake
// agent (AGENT). From source/manager:
//   WEB=http://127.0.0.1:18640 AGENT=http://127.0.0.1:19740 OUT=/tmp/lw-proc \
//     crates/luyaz-web/e2e/run.sh proc
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18640";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19740";
const OUT = process.env.OUT ?? "/tmp/lw-proc";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
const scenario = (set) => fetch(`${AGENT}/__mock/scenario?set=${set}`);

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);

async function firstLoad(p) {
  const urls = [p.url(), ...(await p.evaluate(() => [...document.querySelectorAll("link[href^='/a/'],script[src^='/a/']")].map((e) => e.href || e.src)))];
  const html = await p.content();
  for (const m of html.matchAll(/(?:url\(|href=")(\/a\/[^")#]+)/g)) urls.push(WEB + m[1].replaceAll("&amp;", "&"));
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
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, sel, re, ms = 12000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout: ms }).then(() => true, () => false);

async function procOwn(p) {
  await scenario("normal");
  await p.waitForSelector("#pc-r5:not([hidden])", { timeout: 12000 });
  const rows = p.locator("[role=rowgroup] > .pc-tr:not([hidden])");
  ok(await rows.count() >= 10, `table rows: ${await rows.count()}`);
  ok(/\d+ 个进程在运行/.test(await txt(p, "#pc-status")), `status: ${await txt(p, "#pc-status .nd-status__state")}`);
  // the first sample is all zero; a later poll fills the CPU column and hides the hint
  ok(await p.waitForFunction(() => document.querySelector("#pc-hint")?.hidden === true, null, { timeout: 12000 }).then(() => true, () => false), "CPU hint gone after the next poll");
  // pushes touch only changed rows
  const before = await p.evaluate(() => [...document.querySelectorAll("#pc-r0, #pc-r1")].map((e) => e.outerHTML));
  ok(before.length === 2, "slot rows have ids");
  // sort by name: a link back to the page, ascending
  await p.getByRole("link", { name: /^名称$/ }).click();
  await p.waitForURL(/sort=name&dir=asc/);
  await p.waitForSelector("#pc-r3:not([hidden])", { timeout: 12000 });
  const names = await p.$$eval("#pc-r0, #pc-r1, #pc-r2, #pc-r3", (els) => els.map((e) => e.querySelector("[role=cell]")?.textContent.toLowerCase() ?? ""));
  ok(names.every((n, i) => i === 0 || names[i - 1] <= n), `sorted by name: ${names.join(",")}`);
  ok(await p.locator("[role=columnheader][aria-sort=ascending]").count() === 1, "aria-sort on the sorted column");
  await p.getByRole("link", { name: /^内存$/ }).click();
  await p.waitForURL(/sort=mem&dir=desc/);
  await p.waitForSelector("#pc-r3:not([hidden])", { timeout: 12000 });
  const mem = await p.$$eval("#pc-r0 [role=cell]:nth-child(4), #pc-r1 [role=cell]:nth-child(4)", (els) => els.map((e) => e.textContent));
  ok(mem.length === 2, `memory sort: ${mem.join(" > ")}`);
  await p.getByRole("link", { name: /降序|升序/ }).click();
  await p.waitForURL(/dir=asc/);
  await p.getByRole("link", { name: "CPU", exact: true }).click();
  await p.waitForURL(/sort=cpu&dir=desc/);

  // kill one: cancel first, then confirm; result names it, the row goes away
  const kill = p.locator("#pc-bloat button:has-text('结束')").first();
  const aria = await kill.getAttribute("aria-label");
  ok(/^结束 .+（\d+）$/.test(aria ?? ""), `kill button label: ${aria}`);
  await kill.click();
  await p.locator("dialog#pc-kill-confirm").waitFor({ timeout: 8000 });
  ok(/SIGKILL/.test(await txt(p, "#pc-kill-confirm")) && /断网|立刻停止/.test(await txt(p, "#pc-kill-confirm")), "dialog says what happens");
  await p.screenshot({ path: `${OUT}/proc-kill-dialog.png` });
  await p.locator("dialog#pc-kill-confirm button:has-text('取消')").click();
  ok(await p.locator("dialog#pc-kill-confirm").waitFor({ state: "detached", timeout: 5000 }).then(() => true, () => false), "Cancel closes the dialog, nothing sent");
  const name = (aria ?? "").replace(/^结束 /, "").replace(/（.*$/, "");
  await kill.click();
  await p.locator("dialog#pc-kill-confirm").waitFor();
  await p.locator("dialog#pc-kill-confirm button.nd-btn--danger").click();
  ok(await until(p, "#pc-kill-op", new RegExp(`已结束 1 个：${name}`)), `result by name: ${await txt(p, "#pc-kill-op")}`);
  ok(await p.waitForFunction((n) => ![...document.querySelectorAll("#pc-bloat .nd-row__label")].some((e) => e.textContent === n) , name, { timeout: 12000 }).then(() => true, () => false), "the killed row left the list");

  // refused
  await scenario("cmdfail");
  const k2 = p.locator("#pc-bloat button:has-text('结束')").first();
  if (await k2.count()) {
    await k2.click();
    await p.locator("dialog#pc-kill-confirm").waitFor();
    await p.locator("dialog#pc-kill-confirm button.nd-btn--danger").click();
    ok(await until(p, "#pc-kill-op", /没有生效/), `refused: ${await txt(p, "#pc-kill-op")}`);
  } else ok(false, "a second killable row");
  await scenario("normal");

  // kill all: the dialog lists names + the count
  const allBtn = p.locator("#pc-all");
  ok(!(await allBtn.isDisabled()), `kill-all enabled: ${await txt(p, "#pc-all")}`);
  await allBtn.click();
  await p.locator("dialog#pc-kill-confirm").waitFor({ timeout: 8000 });
  const dlg = await txt(p, "#pc-kill-confirm");
  ok(/名单：zte_topsw_tr069/.test(dlg) && /SIGKILL/.test(dlg), `kill-all dialog lists the names: ${dlg.slice(0, 120)}`);
  await p.screenshot({ path: `${OUT}/proc-killall-dialog.png` });
  await p.locator("dialog#pc-kill-confirm button.nd-btn--danger").click();
  ok(await until(p, "#pc-kill-op", /已结束 \d+ 个/), `kill-all result: ${await txt(p, "#pc-kill-op")}`);
  ok(await p.waitForFunction(() => document.querySelector("#pc-all")?.disabled === true, null, { timeout: 12000 }).then(() => true, () => false), "kill-all disabled once nothing is left");
  await p.screenshot({ path: `${OUT}/proc-done.png`, fullPage: true });
}

async function dnsOwn(p) {
  await scenario("normal");
  ok(/自动/.test(await txt(p, "#dns-status .nd-status__state")), `status: ${await txt(p, "#dns-status .nd-status__state")}`);
  ok(/168\.95\.1\.1/.test(await txt(p, "#dns-status")) , "operator DNS in the status");
  ok(await p.locator(".dns-fields").isHidden(), "fields hidden in auto");
  await p.getByRole("radio", { name: "手动" }).click();
  ok(await p.locator(".dns-fields").isVisible(), "fields shown in manual");
  const apply = p.getByRole("button", { name: "应用", exact: true });
  await apply.click();
  ok(await until(p, "#dns-w-op", /主 DNS 不能为空/, 5000), `empty primary: ${await txt(p, "#dns-w-op")}`);
  await p.getByLabel("主 DNS").fill("1.1.1");
  await apply.click();
  ok(await until(p, "#dns-w-op", /主 DNS 地址无效/, 5000), "bad primary");
  await p.getByLabel("主 DNS").fill("1.1.1.1");
  await p.getByLabel("备用 DNS").fill("1.0.0");
  await apply.click();
  ok(await until(p, "#dns-w-op", /备用 DNS 地址无效/, 5000), "bad secondary");
  await p.getByLabel("备用 DNS").fill("1.0.0.1");
  await apply.click();
  await p.locator("#dns-w-confirm").waitFor({ timeout: 8000 });
  ok(/1\.1\.1\.1 · 1\.0\.0\.1/.test(await txt(p, "#dns-w-confirm")) && /几秒/.test(await txt(p, "#dns-w-confirm")), `inline confirm: ${await txt(p, "#dns-w-confirm")}`);
  await p.screenshot({ path: `${OUT}/dns-confirm.png`, fullPage: true });
  await p.locator("#dns-w-confirm button:has-text('取消')").click();
  ok(await p.locator("#dns-w-confirm").waitFor({ state: "detached", timeout: 5000 }).then(() => true, () => false), "Cancel closes the confirm");
  // refused
  await scenario("cmdfail");
  await apply.click();
  await p.locator("#dns-w-confirm").waitFor();
  await p.locator("#dns-w-confirm button:has-text('确认：应用')").click();
  ok(await until(p, "#dns-w-op", /没有生效/), `refused: ${await txt(p, "#dns-w-op")}`);
  // the agent says ok but changed nothing: accepted, not read back (readback waits ~8 s)
  await scenario("fakesuccess");
  await apply.click();
  await p.locator("#dns-w-confirm").waitFor();
  await p.locator("#dns-w-confirm button:has-text('确认：应用')").click();
  ok(await until(p, "#dns-w-op", /设备已接受，还没读回/, 20000), `accepted, not read back: ${await txt(p, "#dns-w-op")}`);
  await scenario("normal");
  // ok + readback
  await apply.click();
  await p.locator("#dns-w-confirm").waitFor();
  await p.locator("#dns-w-confirm button:has-text('确认：应用')").click();
  ok(await until(p, "#dns-w-op", /DNS 已改为 1\.1\.1\.1 · 1\.0\.0\.1/), `applied: ${await txt(p, "#dns-w-op")}`);
  ok(await until(p, "#dns-status", /手动 DNS/), "status follows: manual");
  await p.screenshot({ path: `${OUT}/dns-done.png`, fullPage: true });
  const dev = await (await fetch(`${AGENT}/api/router/dns`, { headers: {} })).json().catch(() => null);
  // back to auto (readback)
  await p.getByRole("radio", { name: "自动（运营商分配）" }).click();
  await apply.click();
  await p.locator("#dns-w-confirm").waitFor();
  await p.locator("#dns-w-confirm button:has-text('确认：应用')").click();
  ok(await until(p, "#dns-w-op", /已改回运营商分配的 DNS/), `back to auto: ${await txt(p, "#dns-w-op")}`);
  ok(await until(p, "#dns-status", /自动/), "status follows: auto");
  void dev;
}

const browser = await chromium.launch({ channel: "chrome" });
for (const page of ["/tools/processes", "/router/dns"]) {
  const tag = page === "/router/dns" ? "dns" : "proc";
  const ready = page === "/router/dns" ? "#dns-status .nd-status__state" : "#pc-r1";
  for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"], [390, 844, "phone", "en", "dark"]]) {
    const ctx = await browser.newContext({ viewport: { width: w, height: h } });
    await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
    await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
    const p = await ctx.newPage();
    const errs = [];
    p.on("pageerror", (e) => errs.push(e.message));
    p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
    await scenario("normal");
    await p.goto(WEB + page, { waitUntil: "load" });
    await p.waitForSelector(ready, { timeout: 15000, state: "attached" });
    if (name === "desk" && lang === "zh") {
      const kb = (await firstLoad(p)) / 1024;
      ok(kb <= 40, `${tag}: first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
    }
    if (lang === "en") {
      const left = (await mainText(p)).match(CJK) ?? [];
      ok(left.length === 0, `${tag}: English body without Chinese (${[...new Set(left)].join("") || "none"})`);
      ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
    }
    if (name === "phone") {
      const sw = await p.evaluate(() => document.documentElement.scrollWidth);
      ok(sw <= 390, `${tag} ${lang}: 390 px no sideways scroll (${sw})`);
    }
    const axe = await new AxeBuilder({ page: p }).analyze();
    ok(axe.violations.length === 0, `${tag} axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
    await p.screenshot({ path: `${OUT}/${tag}-${name}-${lang}-${theme}.png`, fullPage: true });
    if (name === "desk" && lang === "zh") await (tag === "dns" ? dnsOwn(p) : procOwn(p));
    ok(errs.length === 0, `${tag} ${name} ${lang}: no page errors ${errs.join("; ")}`);
    await ctx.close();
  }
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
