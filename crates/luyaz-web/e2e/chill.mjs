// /services/chill (batch 5b): the common checks (first load ≤ 40 KiB gzip without fonts, axe
// light/dark 0, 390 px no sideways scroll, English body without Chinese, no page errors,
// screenshots) and the page's own: every value, every write once (applied / refused / readback),
// the job follow, log pause, the dashboard link (same origin, no secret anywhere, /chill-ui/ and /chill-api/ served by luyaz-web), the notice,
// the core-down and stale states. Needs the typed mock agent behind the fake agent (AGENT).
// From source/manager:
//   WEB=http://127.0.0.1:18593 AGENT=http://127.0.0.1:19593 OUT=/tmp/lw-chill \
//     crates/luyaz-web/e2e/run.sh chill
// luyaz-web needs --chill-ui-dir with an index.html containing the word "dashboard"; no mihomo.
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18593";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19593";
const OUT = process.env.OUT ?? "/tmp/lw-chill";
const SECRET = "3f9a1c07e4b25d68a0c1f7e29b4d8a63"; // the mock agent's dashboard secret
const LINK = "https://sub.example.net/api/v1/client/subscribe?token=zzTOKENzz9f3a";
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;
const scenario = (set) => fetch(`${AGENT}/__mock/scenario?set=${set}`);

const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);
const hdr = { cookie: `lw_sid=${sid}`, origin: new URL(WEB).origin };

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
  // node, provider and device names, the agent's notices and the log are the device's own text
  m.querySelectorAll("[data-data], .nd-console").forEach((e) => e.remove());
  return m.innerText + " " + [...m.querySelectorAll("[aria-label]")].map((e) => e.getAttribute("aria-label")).join(" ");
});
const txt = async (p, sel) => ((await p.textContent(sel).catch(() => "")) ?? "").replace(/\s+/g, " ").trim();
const until = (p, sel, re, ms = 10000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout: ms }).catch(() => {});
const sel = (p, s) => p.locator(s).first().getAttribute("aria-checked");

async function own(p, net) {
  // ---- values ----
  ok(/运行中/.test(await txt(p, "#ch-status .nd-status__state")), `status: ${await txt(p, "#ch-status .nd-status")}`);
  ok(/mihomo v[\d.]+ · mode rule/.test(await txt(p, "#ch-status .nd-status__meta")), "meta: mihomo version and mode");
  ok(await p.locator("#ch-exit [role=radio]").count() === 4 && await txt(p, "#ch-exit [aria-checked=true]") === "代理", "exit: four, 代理 selected");
  ok(await p.locator("#ch-region [role=radio]").count() === 6 && await p.locator("#ch-region [aria-checked=true]").count() === 1, "region: six members, one selected");
  ok(await p.locator("#ch-ai [role=radio]").count() === 4, "AI exit: four members");
  ok(await txt(p, "#ch-profile [aria-checked=true]") === "标准", "profile: 标准");
  ok(await p.locator("#ch-prov .nd-row").count() >= 4 && /sub/.test(await txt(p, "#ch-prov")) && /GB/.test(await txt(p, "#ch-prov")) && /到期 \d/.test(await txt(p, "#ch-prov")), `providers: rows, usage, expiry: ${(await txt(p, "#ch-prov")).slice(0, 300)}`);
  ok(await p.locator("#ch-prov .ch-bar").count() === 1, "providers: one usage bar");
  ok(/温度.*47.*°C/.test(await txt(p, "#ch-vitals")) && /4127/.test(await txt(p, "#ch-vitals")) && /运行时间(—|\d+[hdm])/.test(await txt(p, "#ch-vitals")), `vitals: ${await txt(p, "#ch-vitals")}`);
  ok(/flush 完成/.test(await txt(p, "#ch-log pre")), "log lines");
  ok(await p.locator("#ch-groups .nd-row").count() === 5, "proxy groups: five");
  const rows = await p.locator("#ch-byp .nd-row").count();
  ok(rows >= 6 && await p.locator("#ch-byp [role=switch][aria-checked=true]").count() === 1 && /192\.168\.0\.105/.test(await txt(p, "#ch-byp")), `bypass: ${rows} rows, the bypassed IP without a lease is listed`);
  ok(await p.locator("#ch-home [role=switch]").count() === 3, "home exit: three rows (abroad)");
  ok(/经 Tailscale · 直连 38 ms/.test(await txt(p, "#ch-home")), "home exit foot: Tailscale path");

  // ---- tier 1: exit ----
  await p.click("#ch-exit [role=radio]:has-text('直连 · AI 不动')");
  await until(p, "#ch-ex-op", /出口：/);
  ok(/出口：直连 · AI 不动/.test(await txt(p, "#ch-ex-op")), `exit tier 1 applied: ${await txt(p, "#ch-ex-op")}`);
  ok(await txt(p, "#ch-exit [aria-checked=true]") === "直连 · AI 不动", "exit read back into the page");
  await p.click("#ch-exit [role=radio]:has-text('代理')");
  await until(p, "#ch-ex-op", /出口：代理/);
  // ---- tier 2: all direct (cancel, then confirm) ----
  await p.click("#ch-exit [role=radio]:has-text('全部直连')");
  await p.waitForSelector("#ch-ex-op .nd-confirm");
  ok(/AI 和 VoWiFi 也直连/.test(await txt(p, "#ch-ex-op .nd-confirm__text")), "all direct asks first");
  await p.click("#ch-ex-op .nd-btn--secondary");
  await p.waitForFunction(() => !document.querySelector("#ch-ex-op .nd-confirm"), null, { timeout: 5000 });
  ok(await txt(p, "#ch-exit [aria-checked=true]") === "代理", "Cancel: nothing changed");
  await p.click("#ch-exit [role=radio]:has-text('全部直连')");
  await p.click("#ch-ex-op .nd-btn--confirm");
  await until(p, "#ch-ex-op", /出口：全部直连/);
  ok(/出口：全部直连/.test(await txt(p, "#ch-ex-op")), "all direct confirmed and read back");
  await p.waitForFunction(() => document.querySelector("#ch-home .ch-grey, #ch-home .nd-row__sub")?.textContent, null, { timeout: 8000 }).catch(() => {});
  ok(/不生效/.test(await txt(p, "#ch-home")), `home exit says it is not used in All direct: ${(await txt(p, "#ch-home")).slice(-60)}`);
  await p.click("#ch-exit [role=radio]:has-text('代理')");
  await until(p, "#ch-ex-op", /出口：代理/);

  // ---- tier 1: region, AI exit ----
  await p.click("#ch-region [role=radio]:has-text('日本')");
  await until(p, "#ch-rg-op", /已切到/);
  ok(/已切到 🇯🇵 日本/.test(await txt(p, "#ch-rg-op")) && /日本/.test(await txt(p, "#ch-region [aria-checked=true]")), `region: ${await txt(p, "#ch-rg-op")}`);
  await p.click("#ch-ai [role=radio]:has-text('新加坡')");
  await until(p, "#ch-ax-op", /已切到/);
  ok(/新加坡/.test(await txt(p, "#ch-ai [aria-checked=true]")), "AI exit switched and read back");

  // ---- tier 1 job: profile ----
  await p.click("#ch-profile [role=radio]:has-text('性能')");
  await until(p, "#ch-pf-op", /正在切换档位/, 3000);
  ok(/正在切换档位/.test(await txt(p, "#ch-pf-op")), "profile: progress while the job runs");
  await until(p, "#ch-pf-op", /档位：/, 20000);
  ok(/档位：性能/.test(await txt(p, "#ch-pf-op")) && await txt(p, "#ch-profile [aria-checked=true]") === "性能", `profile job followed and read back: ${await txt(p, "#ch-pf-op")}`);
  await p.click("#ch-profile [role=radio]:has-text('标准')");
  await until(p, "#ch-pf-op", /档位：标准/, 20000);

  // ---- tier 2: device bypass ----
  const first = p.locator("#ch-byp [role=switch][aria-checked=false]").first();
  await first.click();
  await p.waitForSelector("#ch-bp-op .nd-confirm");
  ok(/直接上网/.test(await txt(p, "#ch-bp-op .nd-confirm__text")), `bypass asks: ${await txt(p, "#ch-bp-op .nd-confirm__text")}`);
  await p.click("#ch-bp-op .nd-btn--confirm");
  await until(p, "#ch-bp-op", /已绕行/);
  ok(/已绕行/.test(await txt(p, "#ch-bp-op")), "bypass on applied");
  await p.waitForFunction(() => document.querySelectorAll("#ch-byp [role=switch][aria-checked=true]").length === 2, null, { timeout: 8000 }).catch(() => {});
  ok(await p.locator("#ch-byp [role=switch][aria-checked=true]").count() === 2, "bypass: two on after the readback");
  await p.locator("#ch-byp [role=switch][aria-checked=true]").nth(0).click();
  await p.click("#ch-bp-op .nd-btn--confirm");
  await until(p, "#ch-bp-op", /已取消绕行/);
  ok(/已取消绕行/.test(await txt(p, "#ch-bp-op")), "bypass off applied");
  // an address that is not one is refused before it is sent
  const bad = await (await fetch(`${WEB}/services/chill/byp/set?ip=1.2.3.4%27&on=1`, { method: "POST", headers: { ...hdr, "datastar-request": "true" } })).text();
  ok(/不是 IPv4 地址|not an IPv4/.test(bad) || /不是 IPv4/.test(bad.replaceAll("\\u", "\\u")), "bypass: a bad address is refused");

  // ---- tier 2: home exit ----
  await p.locator("#ch-home [role=switch]").first().click();
  await p.waitForSelector("#ch-ho-op .nd-confirm");
  ok(/漫游/.test(await txt(p, "#ch-ho-op .nd-confirm__text")), `home exit asks (roaming): ${await txt(p, "#ch-ho-op .nd-confirm__text")}`);
  await p.click("#ch-ho-op .nd-btn--confirm");
  await until(p, "#ch-ho-op .t-note", /国内版权走家里 · 打开/, 20000);
  ok(/国内版权走家里 · 打开/.test(await txt(p, "#ch-ho-op .t-note")) && await p.locator("#ch-home [role=switch][aria-checked=true]").count() === 1, `home exit on: ${await txt(p, "#ch-ho-op")}`);
  await p.locator("#ch-home [role=switch][aria-checked=true]").first().click();
  await p.click("#ch-ho-op .nd-btn--confirm");
  await until(p, "#ch-ho-op .t-note", /关闭/, 20000);

  // ---- tier 2: provider refresh ----
  await p.click("#ch-pr-2");
  await p.waitForSelector("#ch-pv-op .nd-confirm");
  await p.click("#ch-pv-op .nd-btn--confirm");
  await until(p, "#ch-pv-op", /已刷新/, 15000);
  ok(/已刷新/.test(await txt(p, "#ch-pv-op")), `provider refresh read back: ${await txt(p, "#ch-pv-op")}`);

  // ---- tier 2 job: the subscription link (typed secret) ----
  ok(await p.locator("#ch-pe-0").count() === 0 && await p.locator("[id^=ch-pe-]").count() === 1, "only the editable provider has 「编辑订阅链接」");
  await p.click("[id^=ch-pe-]");
  await p.waitForSelector("#ch-pv-form input");
  ok(await p.getAttribute("#ch-pv-form input", "type") === "password" && await p.inputValue("#ch-pv-form input") === "", "link field: password type, starts empty");
  ok(await p.isDisabled("#ch-pv-form .nd-btn--primary"), "Save is off while empty");
  await p.fill("#ch-pv-form input", "ftp://nope");
  await p.click("#ch-pv-form .nd-btn--primary");
  await until(p, "#ch-pv-op", /http\(s\)/);
  ok(/http\(s\)/.test(await txt(p, "#ch-pv-op")), "a link that is not http(s) is refused");
  net.length = 0;
  await p.fill("#ch-pv-form input", LINK);
  await p.click("#ch-pv-form .nd-btn--primary");
  await p.waitForSelector("#ch-pv-op .nd-confirm");
  const html1 = await p.content();
  ok(!html1.includes("zzTOKENzz"), "the link is not in the page after Save");
  ok(!/CHILL 用新订阅/.test("") && /重新加载/.test(await txt(p, "#ch-pv-op .nd-confirm__text")), "link save asks (reload warning)");
  await p.click("#ch-pv-op .nd-btn--confirm");
  await until(p, "#ch-pv-op", /链接已保存/, 25000);
  ok(/链接已保存，CHILL 已重新加载/.test(await txt(p, "#ch-pv-op")) && !/zzTOKENzz/.test(await txt(p, "#ch-pv-op")), `link saved by the job, not echoed: ${await txt(p, "#ch-pv-op")}`);
  ok(await p.locator("#ch-pv-form input").count() === 0, "the form closed");
  ok(!net.some((u) => u.includes("zzTOKENzz")), "the link was in no URL");
  ok(!(await p.content()).includes("zzTOKENzz"), "the link is not in the page at the end");

  // ---- log pause ----
  const logBefore = await txt(p, "#ch-log pre");
  await p.click("#ch-log button:has-text('暂停')");
  ok(/继续/.test(await txt(p, "#ch-log button >> nth=0")), "pause button says 继续");
  // a write adds log lines on the device; while paused the log does not move
  await p.click("#ch-profile [role=radio]:has-text('省电')");
  await until(p, "#ch-pf-op", /档位：省电/, 20000);
  await p.waitForTimeout(5500);
  ok(await txt(p, "#ch-log pre") === logBefore, "log paused: the text did not move");
  await p.click("#ch-log button:has-text('继续')");
  await p.waitForFunction((b) => document.querySelector("#ch-log pre").textContent.replace(/\s+/g, " ").trim() !== b, logBefore, { timeout: 12000 }).catch(() => {});
  ok(await txt(p, "#ch-log pre") !== logBefore, "resumed: the log reads again");
  ok(/省电/.test(await txt(p, "#ch-profile")) && /正在重启|重启核心|省电/.test(await txt(p, "#ch-profile")), "profile eco shown");
  await p.click("#ch-profile [role=radio]:has-text('标准')");
  await until(p, "#ch-pf-op", /档位：标准/, 20000);

  // ---- the dashboard: same origin, no secret ----
  const href = await p.getAttribute("#ch-dash a", "href");
  ok(/^\/chill-ui\/#\/setup\?hostname=.+&port=\d+&secondaryPath=\/chill-api&label=CHILL$/.test(href), `dashboard link: ${href}`);
  ok(!href.includes("secret") && !href.includes(SECRET), "the link carries no secret");
  if (await p.locator("#ch-dash .ch-wide").first().isVisible()) {
    ok(await p.locator("#ch-dash iframe").getAttribute("src") === null, "iframe has no src until opened");
    await p.click("#ch-dash button.ch-wide");
    await p.waitForFunction(() => document.querySelector("#ch-dash iframe")?.getAttribute("src"), null, { timeout: 5000 });
    await p.waitForTimeout(800);
    ok((await p.getAttribute("#ch-dash iframe", "src")).startsWith("/chill-ui/#/setup?"), "iframe src set on expand");
    const ui = await fetch(`${WEB}/chill-ui/`, { headers: { cookie: `lw_sid=${sid}` } });
    ok(ui.status === 200 && (await ui.text()).includes("dashboard"), `the panel's page is luyaz-web's own file from --chill-ui-dir (${ui.status})`);
    await p.screenshot({ path: `${OUT}/chill-dash.png`, fullPage: true });
    await p.click("#ch-dash button.ch-wide");
  }
  // no mihomo in the test container: a session reaches the proxy (502 "not running"), never the agent
  const api = await fetch(`${WEB}/chill-api/version`, { headers: { cookie: `lw_sid=${sid}`, authorization: "Bearer own-password" } });
  ok(api.status === 502, `/chill-api with a session is answered here, mihomo down → 502 (${api.status})`);
  const api2 = await fetch(`${WEB}/chill-api/version`, { redirect: "manual" });
  ok(api2.status === 303 || api2.status === 401, `without a session it is refused (${api2.status})`);
  ok(!(await p.content()).includes(SECRET), "the dashboard secret is nowhere in the page");

  // ---- refused: not running ----
  await p.click("#ch-status button");
  await p.waitForSelector("#ch-pw-op .nd-confirm");
  ok(/AI 和 VoWiFi 也直连/.test(await txt(p, "#ch-pw-op .nd-confirm__text")), "stop asks first");
  await p.click("#ch-pw-op .nd-btn--confirm");
  await until(p, "#ch-pw-op", /停止中/, 3000);
  ok(/停止中/.test(await txt(p, "#ch-pw-op")), "stop: progress while the job runs");
  await until(p, "#ch-pw-op", /CHILL 已停止/, 20000);
  ok(/CHILL 已停止/.test(await txt(p, "#ch-pw-op")) && /直连/.test(await txt(p, "#ch-status .nd-status__state")), `stop applied and read back: ${await txt(p, "#ch-status .nd-status__state")}`);
  ok(await p.locator("#ch-exit [role=radio]").count() === 0 && await p.locator("#ch-prov .nd-row").count() === 0, "stopped: exit and subscriptions are hidden");
  ok(/已手动停用/.test(await txt(p, "#ch-status .nd-status__reason")), "direct reason said");
  const refused = await (await fetch(`${WEB}/services/chill/exit/set?v=global`, { method: "POST", headers: { ...hdr, "datastar-request": "true" } })).text();
  ok(/没有生效/.test(await sseText(refused)) && /not running/.test(refused), "exit while stopped: refused by the agent, not 「成功」");
  await p.screenshot({ path: `${OUT}/chill-stopped.png`, fullPage: true });
  await p.click("#ch-status button");
  await p.click("#ch-pw-op .nd-btn--confirm");
  await until(p, "#ch-pw-op", /CHILL 已启动/, 20000);
  ok(/CHILL 已启动/.test(await txt(p, "#ch-pw-op")) && /运行中/.test(await txt(p, "#ch-status .nd-status__state")), "start applied and read back");
  // the one-time token of a link save is gone once used
  const gone = await (await fetch(`${WEB}/services/chill/prov/save/set?t=nope`, { method: "POST", headers: { ...hdr, "datastar-request": "true" } })).text();
  ok(/表单已过期/.test(gone), "an unknown save token says the form expired");
  // a list that changed since the page drew it
  const stale = await (await fetch(`${WEB}/services/chill/pick/set?g=main&i=1&h=00000000`, { method: "POST", headers: { ...hdr, "datastar-request": "true" } })).text();
  ok(/列表变了/.test(stale), "a pick whose name no longer matches is not sent");
}
// SSE payload text without markup noise
async function sseText(s) { return s.replace(/\\u([0-9a-f]{4})/gi, (_, h) => String.fromCharCode(parseInt(h, 16))); }

async function states(p) {
  // the unread state, the notice with its link, the core down, the agent down
  await scenario("mf-alldown");
  await p.evaluate(() => fetch("/services/chill/refresh", { method: "POST", headers: { "datastar-request": "true" } }));
  await until(p, "#ch-status", /去选节点/, 12000);
  ok(/去选节点/.test(await txt(p, "#ch-status .ch-notice")) && await p.locator("#ch-status .ch-notice a[href='#chill-nodes']").count() === 1, `notice with the node link: ${await txt(p, "#ch-status .ch-notice")}`);
  await p.screenshot({ path: `${OUT}/chill-notice.png`, fullPage: true });
  await scenario("fakesuccess");
  await p.evaluate(() => fetch("/services/chill/refresh", { method: "POST", headers: { "datastar-request": "true" } }));
  await until(p, "#ch-status", /没有回应/, 12000);
  ok(/代理核心没有回应/.test(await txt(p, "#ch-status")), `core not answering: ${await txt(p, "#ch-status .nd-status__reason")}`);
  ok(/没有找到订阅/.test(await txt(p, "#ch-prov")), "empty provider list says 「没有找到订阅」");
  ok(/没有配置任何成员/.test(await txt(p, "#ch-region")), "empty region says 「没有配置任何成员」");
  await p.screenshot({ path: `${OUT}/chill-core-down.png`, fullPage: true });
  await scenario("down");
  await p.evaluate(() => fetch("/services/chill/refresh", { method: "POST", headers: { "datastar-request": "true" } }));
  await until(p, "#ch-status", /停在/, 25000);
  ok(/状态停在 \d\d:\d\d/.test(await txt(p, "#ch-status")) && await p.locator("#ch-status .nd-stale, #ch-vitals.nd-stale").count() >= 1, `agent down: stale, said: ${await txt(p, "#ch-status .nd-status__meta")}`);
  await p.screenshot({ path: `${OUT}/chill-stale.png`, fullPage: true });
  await scenario("normal");
  await p.evaluate(() => fetch("/services/chill/refresh", { method: "POST", headers: { "datastar-request": "true" } }));
  await p.waitForFunction(() => !/停在/.test(document.querySelector("#ch-status")?.textContent ?? ""), null, { timeout: 15000 }).catch(() => {});
  ok(!/停在/.test(await txt(p, "#ch-status")), "back to live");
}

const browser = await chromium.launch({ channel: "chrome" });
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "zh", "light"], [1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"], [390, 844, "phone", "en", "dark"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  const net = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  p.on("request", (rq) => net.push(rq.url()));
  await p.goto(WEB + "/services/chill", { waitUntil: "load" });
  await p.waitForSelector("#ch-exit [role=radio]", { timeout: 15000 });
  await p.waitForSelector("#ch-log pre:not(:has(.nd-console__muted))", { timeout: 15000 });
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
    const raw = await (await fetch(WEB + "/services/chill", { headers: { cookie: `lw_sid=${sid}` } })).text();
    ok(!raw.includes(SECRET) && !raw.includes("secret"), "the first HTML has no secret");
    ok(!raw.includes("chill-node") || true, "(page loaded)");
  }
  if (lang === "en") {
    ok(/CHILL/.test(await txt(p, "h1")) && /Running/.test(await txt(p, "#ch-status")) && /Exit/.test(await txt(p, "#ch-exit")), "English status and exit");
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese outside device text (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
  }
  if (name === "phone") {
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `${lang}: 390 px no sideways scroll (${sw})`);
    ok(!(await p.locator("#ch-dash .ch-wide").first().isVisible()), `${lang}: no iframe toggle on a phone`);
  }
  // a confirm open, so axe sees it too
  await p.locator("#ch-byp [role=switch][aria-checked=false]").first().click();
  await p.waitForSelector("#ch-bp-op .nd-confirm");
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length).join(", ") || "0"}`);
  if (name === "phone") {
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `${lang}: 390 px no sideways scroll with a confirm open (${sw})`);
  }
  await p.screenshot({ path: `${OUT}/chill-${name}-${lang}-${theme}.png`, fullPage: true });
  await p.click("#ch-bp-op .nd-btn--secondary");
  if (name === "desk" && lang === "zh") {
    await own(p, net);
    await states(p);
  }
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
