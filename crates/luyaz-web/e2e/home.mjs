// / (home, batch 5b): the common checks (first load ≤ 40 KiB gzip without fonts, axe light/dark 0,
// 390 px no sideways scroll, English body without Chinese, no page errors, screenshots) and the
// page's own: every value arrives (hub blocks + polled agent reads), the two-column layout and the
// phone reading order, 「查原因 →」 follows the verdict, scenario states, the CHILL notice, the battery
// estimate, and the CHILL exit control (ok / refused / readback not held). Needs the typed mock agent
// behind the fake agent (AGENT) and the mock u60d (MOCK). From source/manager:
//   WEB=http://127.0.0.1:18591 MOCK=http://127.0.0.1:19491 AGENT=http://127.0.0.1:19591 OUT=/tmp/lw-home \
//     crates/luyaz-web/e2e/run.sh home
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18591";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19491";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19591";
const OUT = process.env.OUT ?? "/tmp/lw-home";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
const scenario = (set) => fetch(`${AGENT}/__mock/scenario?set=${set}`);
const hook = (b) => fetch(`${MOCK}/mock/home`, { method: "POST", body: JSON.stringify(b) });

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
// device-supplied text (names, SSIDs, IPs, geo, operator) is not ours to translate
const mainText = (p) => p.evaluate(() => {
  const m = document.querySelector("main").cloneNode(true);
  m.querySelectorAll("[data-data]").forEach((e) => e.remove());
  return m.innerText + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, sel, re, ms = 10000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout: ms }).catch(() => {});
const box = (p, sel) => p.locator(sel).first().boundingBox();

async function own(p) {
  await hook({ reset: true });
  // values: hub blocks
  ok(/U60 Pro · MU5250/.test(await txt(p, "#hm-title")), `title: ${await txt(p, "#hm-title")}`);
  ok((await txt(p, "#hm-sub")).length > 0, `subtitle: ${await txt(p, "#hm-sub")}`);
  for (const id of ["ro-bw", "ro-rsrp", "ro-sinr", "ro-dl", "ro-ul", "ro-temp", "ro-bat"]) {
    ok(/^-?\d/.test(await txt(p, `#${id}`)), `${id}: ${await txt(p, `#${id}`)}`);
  }
  ok(/RSRQ -?\d+/.test(await txt(p, "#ro-rsrp-w")), `RSRP sub: ${await txt(p, "#ro-rsrp-w")}`);
  ok(/使用电池/.test(await txt(p, "#ro-bat-w")) && /约可用/.test(await txt(p, "#ro-bat-w")), `battery sub (on battery + estimate): ${await txt(p, "#ro-bat-w")}`);
  ok(/^\d+$/.test(await txt(p, "#hd-cl")), `connected devices: ${await txt(p, "#hd-cl")}`);
  ok(/↓ .*GB.*↑/.test(await txt(p, "#hd-day")) && /↓ .*GB.*↑/.test(await txt(p, "#hd-mon")), `today / month: ${await txt(p, "#hd-day")} | ${await txt(p, "#hd-mon")}`);
  ok(/小时|分钟|天/.test(await txt(p, "#hd-up")), `uptime: ${await txt(p, "#hd-up")}`);
  ok(/^2$/.test(await txt(p, "#hm-sms .hm-count__n")), `SMS badge: ${await txt(p, "#hm-sms")}`);
  ok(await p.locator("#hm-sms").getAttribute("href") === "/sms" && await p.locator("#hm-alerts").getAttribute("href") === "/alerts", "SMS and alerts links");
  ok((await p.locator("#hm-ca .nd-row").count()) >= 2, `carrier group rows: ${await p.locator("#hm-ca .nd-row").count()}`);
  ok(/Cell \d+/.test(await txt(p, "#ca-0")) && /服务小区/.test(await txt(p, "#ca-0")), `serving carrier: ${await txt(p, "#ca-0")}`);
  ok(await p.locator("#hc-sel").count() === 1, "network selection row");
  // polled agent reads
  await p.waitForFunction(() => !/^—$/.test(document.querySelector("#hi-ip")?.textContent.trim() ?? "—"), null, { timeout: 8000 }).catch(() => {});
  ok(/\d+\.\d+\.\d+\.\d+/.test(await txt(p, "#hi-ip")), `cellular exit IP: ${await txt(p, "#hi-ip")}`);
  ok(/CHILL 出口/.test(await txt(p, "#hi-chill")) && await p.locator("#hi-chill").isVisible(), `CHILL exit row: ${await txt(p, "#hi-chill")}`);
  ok(/蜂窝出口/.test(await txt(p, "#hi-ip-l")), "label says 蜂窝出口 while two exits");
  ok(/46692|漫游/.test(await txt(p, "#hi-reg") + await txt(p, "#hi-roam")), `registered / roaming: ${await txt(p, "#hi-reg")} | ${await txt(p, "#hi-roam")}`);
  // operator logo (oplogo.rs): the file from --logo-dir, only in the pushed leaves. The logos are
  // with no chunghwa file in --logo-dir the page shows the letter badge instead.
  const lg = await fetch(`${WEB}/oplogo/chunghwa`, { headers: { cookie: `lw_sid=${sid}` } });
  if (lg.status === 404) {
    ok(await p.locator("#hi-reg .op-badge").count() === 1 && await p.locator("#hm-sub .op-badge").count() === 1 && await p.locator("#hi-reg img, #hm-sub img").count() === 0, "no logo file: Registered on + header subtitle show the letter badge");
  } else {
    const imgOk = (sel) => p.waitForFunction((q) => { const i = document.querySelector(q); return i && i.complete && i.naturalWidth > 0; }, sel, { timeout: 8000 }).then(() => true, () => false);
    ok(await imgOk("#hi-reg img[src='/oplogo/chunghwa']"), "Registered on: logo image loads");
    ok(await imgOk("#hm-sub img[src='/oplogo/chunghwa']"), "header subtitle: logo image loads");
    ok(await p.locator("#hi-reg img").getAttribute("alt") === "" && await p.locator("#hi-reg img").getAttribute("loading") === "lazy" && await p.locator("#hi-reg img").getAttribute("height") === "18", "img has alt='', loading=lazy, explicit height");
    ok(lg.status === 200 && /image\/svg\+xml/.test(lg.headers.get("content-type") ?? "") && /max-age=\d{6,}/.test(lg.headers.get("cache-control") ?? ""), `/oplogo/chunghwa: ${lg.status} ${lg.headers.get("content-type")} ${lg.headers.get("cache-control")}`);
  }
  for (const bad of ["nope", "cmlink", "..%2f..%2fetc%2fpasswd", "china-broadnet"]) {
    ok((await fetch(`${WEB}/oplogo/${bad}`, { headers: { cookie: `lw_sid=${sid}` } })).status === 404, `/oplogo/${bad}: 404`);
  }
  ok((await fetch(`${WEB}/oplogo/chunghwa`, { redirect: "manual" })).status !== 200, "/oplogo needs a session");
  ok(/SIM/.test(await txt(p, "#hi-reg-s")), `SIM line under Registered on: ${await txt(p, "#hi-reg-s")}`);
  ok(/信道 \d+/.test(await txt(p, "#hw-2")) && /MHz/.test(await txt(p, "#hw-5")), `Wi-Fi lines (actual channel + width): ${await txt(p, "#hw-2")} | ${await txt(p, "#hw-5")}`);
  ok(/在线|离线/.test(await txt(p, "#hm-ts")) && /台设备在线/.test(await txt(p, "#hm-ts")), `Tailscale: ${await txt(p, "#hm-ts")}`);
  ok(/国外/.test(await txt(p, "#hm-scene")) && /切换/.test(await txt(p, "#hm-scene")), `scenario: ${await txt(p, "#hm-scene")}`);
  ok(/运行中/.test(await txt(p, "#hm-chill-t")) && await p.locator("#hm-chill-seg [role=radio]").count() === 3, `CHILL card: ${await txt(p, "#hm-chill")}`);
  ok(await p.locator("#hm-chill-seg [aria-checked=true]").count() === 1, "one exit selected");
  // the diagnose link is hidden when the verdict is ok, and follows the verdict
  await p.waitForFunction(() => document.querySelector("#hm-why")?.hidden, null, { timeout: 15000 }).catch(() => {}); // the finding line from the common checks goes with the next status read
  ok(await p.locator("#hm-why").isHidden(), "no 查原因 link with an ok verdict");
  // the links of the old page
  for (const [sel, href] of [["#hd-cl", "/clients"], ["#hi-reg", "/router/mobile-network"], ["#hw-on", "/router/wifi"], ["#hm-scene", "/router/scenario"], ["#hm-ts", "/services/tailscale"]]) {
    const h = await p.locator(`a:has(${sel}), a${sel}`).first().getAttribute("href");
    ok(h === href, `link ${sel} → ${h}`);
  }

  // hooks: verdict, notice, scenario, estimate (u60d's public status is read every 10 s, /api/screen too)
  await hook({ verdict: "stall", notice: "alldown", scenario: "pinned", bat: "charging" });
  await p.waitForFunction(() => document.querySelector("#hm-why") && !document.querySelector("#hm-why").hidden, null, { timeout: 15000 }).catch(() => {});
  ok(await p.locator("#hm-why a").getAttribute("href") === "/tools/diagnose?start=1" && /查原因/.test(await txt(p, "#hm-why")), `verdict stall → 「${await txt(p, "#hm-why")}」`);
  await until(p, "#hm-chill-body", /去选节点/, 15000);
  ok(/都不通/.test(await txt(p, "#hm-chill-body")) && await p.locator("#hm-chill-body a").getAttribute("href") === "/services/chill#chill-nodes", `CHILL notice alldown + picker link: ${await txt(p, "#hm-chill-body")}`);
  ok(/已固定/.test(await txt(p, "#hm-scene")), `scenario pinned: ${await txt(p, "#hm-scene")}`);
  await until(p, "#ro-bat-w", /充满/, 15000);
  ok(/约 1 小时 35 分充满/.test(await txt(p, "#ro-bat-w")), `charging estimate: ${await txt(p, "#ro-bat-w")}`);
  await p.screenshot({ path: `${OUT}/home-hooks.png`, fullPage: true });
  await hook({ scenario: "none", notice: null, verdict: "ok", bat: "gone" });
  await until(p, "#hm-scene", /还没有情景/, 15000);
  ok(/还没有情景/.test(await txt(p, "#hm-scene")), `no scenarios: ${await txt(p, "#hm-scene")}`);
  await p.waitForFunction(() => document.querySelector("#hm-why")?.hidden, null, { timeout: 15000 }).catch(() => {});
  ok(await p.locator("#hm-why").isHidden(), "verdict ok again: link gone");
  // T6: the core's open "sure" finding — its sentence + a link to the evidence, over an ok verdict
  await hook({ finding: true });
  await until(p, "#hm-why", /数据连接没建立/, 15000);
  ok(!(await p.locator("#hm-why").isHidden()) && await p.locator("#hm-why a").getAttribute("href") === "/tools/diagnose" && /看证据/.test(await txt(p, "#hm-why")) && /已暂停代理重连/.test(await txt(p, "#hm-why")), `finding (network.health words) → 「${await txt(p, "#hm-why")}」`);
  await hook({ finding: false });
  await p.waitForFunction(() => document.querySelector("#hm-why")?.hidden, null, { timeout: 15000 }).catch(() => {});
  ok(await p.locator("#hm-why").isHidden(), "finding closed: line gone");
  await hook({ reset: true });

  // the exit control: ok
  const aria = (x) => p.locator(`#hm-chill-seg [role=radio]:has-text("${x}")`);
  await p.evaluate(() => {
    window.__dis = false;
    // the pending patch and the result can land in one batch (a fast agent): look at what was added, not only the DOM now
    const seen = (n) => n.nodeType === 1 && (n.querySelectorAll?.("[role=radio][disabled]").length === 3 || /正在把出口切到全局/.test(n.textContent ?? ""));
    new MutationObserver((recs) => { if (recs.some((r) => [...r.addedNodes].some(seen) || (r.type === "attributes" && r.attributeName === "disabled" && r.target.matches?.("#hm-chill-seg [role=radio]") && r.target.hasAttribute("disabled")) || (r.type === "characterData" && /正在把出口切到全局/.test(r.target.data ?? ""))) || document.querySelectorAll("#hm-chill-seg [role=radio][disabled]").length === 3 || /正在把出口切到全局/.test(document.querySelector("#hm-chill-op")?.textContent ?? "")) window.__dis = true; })
      .observe(document.body, { subtree: true, attributes: true, childList: true, characterData: true });
  });
  await fetch(`${AGENT}/__lw/home?slow=1500`, { method: "POST" });
  await aria("全局").click();
  await until(p, "#hm-chill-op", /出口已改为全局/, 20000);
  await fetch(`${AGENT}/__lw/home?slow=0`, { method: "POST" });
  ok(/出口已改为全局/.test(await txt(p, "#hm-chill-op")), `exit applied: ${await txt(p, "#hm-chill-op")}`);
  ok(await p.evaluate(() => window.__dis), "while it runs: the control is off and the pending line is said");
  ok(await aria("全局").getAttribute("aria-checked") === "true", "全局 is selected after the readback");
  await p.screenshot({ path: `${OUT}/home-exit-ok.png`, fullPage: true });
  // a push (here: a kick from another event) keeps the result line
  await hook({ clients: 4 });
  await p.waitForFunction(() => document.querySelector("#hd-cl")?.textContent.trim() === "4", null, { timeout: 8000 }).catch(() => {});
  ok(/出口已改为全局/.test(await txt(p, "#hm-chill-op")), "a push does not wipe the result line");
  await hook({ clients: 3 });
  // back to proxy
  await aria("代理").click();
  await until(p, "#hm-chill-op", /出口已改为代理/, 20000);
  ok(await aria("代理").getAttribute("aria-checked") === "true", "back to 代理");

  // refused: the proxy core does not answer (the typed mock refuses the PUT with 409)
  await scenario("fakesuccess");
  await aria("全局").click();
  await until(p, "#hm-chill-op", /没有生效/, 20000);
  ok(/没有生效/.test(await txt(p, "#hm-chill-op")) && !/出口已改为/.test(await txt(p, "#hm-chill-op")), `refused: ${await txt(p, "#hm-chill-op")}`);
  ok(await aria("代理").getAttribute("aria-checked") === "true", "refused: the control keeps the real exit");
  await p.screenshot({ path: `${OUT}/home-exit-refused.png`, fullPage: true });
  await scenario("normal");

  // accepted but the readback never shows it
  await fetch(`${AGENT}/__lw/home?hold=1`, { method: "POST" });
  await aria("全局").click();
  await until(p, "#hm-chill-op", /设备已接受/, 30000);
  ok(/设备已接受，还没读回/.test(await txt(p, "#hm-chill-op")) && !/出口已改为/.test(await txt(p, "#hm-chill-op")), `readback not held: ${await txt(p, "#hm-chill-op")}`);
  await fetch(`${AGENT}/__lw/home?hold=0`, { method: "POST" });
  await aria("代理").click();
  await until(p, "#hm-chill-op", /出口已改为代理/, 20000);
  ok(await aria("代理").getAttribute("aria-checked") === "true", "restored 代理");
}

async function layout(browser) {
  const mk = async (w, h) => {
    const ctx = await browser.newContext({ viewport: { width: w, height: h } });
    await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: "zh", url: WEB }]);
    const p = await ctx.newPage();
    await p.goto(WEB + "/", { waitUntil: "load" });
    await p.waitForFunction(() => /^-?\d/.test(document.querySelector("#ro-rsrp")?.textContent.trim() ?? ""), null, { timeout: 15000 });
    await p.waitForSelector("#hm-chill-seg [role=radio]", { timeout: 15000 });
    return [ctx, p];
  };
  let [ctx, p] = await mk(1280, 900);
  const [rd, ch, sc, ts, ca] = await Promise.all(["#hm-readouts", "#hm-chill", "#hm-scene", "#hm-ts", "#hm-ca"].map((s) => box(p, s)));
  ok(ch.x > rd.x + rd.width - 2 && sc.x > rd.x + rd.width - 2 && ts.x > rd.x + rd.width - 2, `desk: CHILL, scenario, Tailscale are in the right column (x ${Math.round(ch.x)} vs readouts ${Math.round(rd.x)}+${Math.round(rd.width)})`);
  ok(ch.y < sc.y && sc.y < ts.y, "desk: right column order CHILL, scenario, Tailscale");
  ok(ca.x < ch.x, "desk: carriers on the left");
  await ctx.close();
  [ctx, p] = await mk(390, 844);
  const ys = [];
  for (const s of ["#sig-status", "#hm-chill", "#hm-readouts", "#hd-cl", "#hi-ip", "#hw-on", "#ca-0", "#hm-scene", "#hm-ts"]) ys.push([s, (await box(p, s)).y]);
  ok(ys.every(([, y], i) => i === 0 || y > ys[i - 1][1]), `phone reading order: ${ys.map(([s, y]) => `${s}@${Math.round(y)}`).join(" ")}`);
  await ctx.close();
}

const browser = await chromium.launch({ channel: "chrome" });
await hook({ finding: true }); // the common checks with the finding line showing (T6)
await new Promise((r) => setTimeout(r, 11000)); // luyaz-web reads the public status every 10 s
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"], [390, 844, "phone", "en", "dark"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  await p.goto(WEB + "/", { waitUntil: "load" });
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
  }
  await p.waitForFunction(() => /^-?\d/.test(document.querySelector("#ro-rsrp")?.textContent.trim() ?? ""), null, { timeout: 15000 }).catch(() => {});
  await p.waitForSelector("#hm-chill-seg [role=radio]", { timeout: 15000 }).catch(() => {});
  await p.waitForFunction(() => !/^—$/.test(document.querySelector("#hi-ip")?.textContent.trim() ?? "—") && /MHz/.test(document.querySelector("#hw-5")?.textContent ?? ""), null, { timeout: 12000 }).catch(() => {});
  if (lang === "en") {
    ok(/Detail/.test(await txt(p, "#hm-detail-t")) && /Aggregate bandwidth/.test(await txt(p, "#hm-readouts")), "English labels");
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
  }
  if (name === "phone") {
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `${lang}: 390 px no sideways scroll (${sw})`);
  }
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
  await p.screenshot({ path: `${OUT}/home-${name}-${lang}-${theme}.png`, fullPage: true });
  if (name === "desk" && lang === "zh") {
    ok(/数据连接没建立/.test(await txt(p, "#hm-why")), `common checks ran with the finding line: ${await txt(p, "#hm-why")}`);
    await own(p);
    await hook({ finding: true });
    await p.waitForTimeout(11000);
  }
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await layout(browser);
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
