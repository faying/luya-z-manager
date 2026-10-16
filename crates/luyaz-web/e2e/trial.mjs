// Browser checks for the 8b trial (u60-web.md §3 pass lines 3, 5, 7, the automatable part of 8;
// E2; the tier-2 roaming write).
// Runs against `luyaz-web serve` + `luyaz-web mock` replaying a capture fast (SPEED=6 ≈ one push a
// second). Uses Playwright and axe (crates/luyaz-web/package.json; run from source/manager):
//
//   WEB=http://127.0.0.1:18081 MOCK=http://127.0.0.1:19461 node crates/luyaz-web/e2e/trial.mjs
//
// Exit code = number of failed checks.
// or with run.sh:
//   WEB=… crates/luyaz-web/e2e/run.sh trial
// Servers: luyaz-web serve (--agent = fake-agent.mjs on the Mac, reached as host.docker.internal)
// + luyaz-web mock, e.g. a small local run script in a u60-rust-ci container.
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const WEB = process.env.WEB ?? "http://127.0.0.1:18081";
const MOCK = process.env.MOCK ?? "http://127.0.0.1:19461";
const IDLE = Number(process.env.IDLE ?? 10);
const results = [];
const check = async (name, fn) => {
  try {
    const note = await fn();
    results.push([true, name, note ?? ""]);
  } catch (e) {
    results.push([false, name, String(e.message ?? e).split("\n")[0]]);
  }
};
const assert = (c, msg) => {
  if (!c) throw new Error(msg);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const stats = async () => (await fetch(`${MOCK}/mock/stats`)).json();
const health = async () => (await fetch(`${WEB}/health`)).json();
const mockCtl = (q) => fetch(`${MOCK}/mock/control?${q}`, { method: "POST" });
async function waitFor(what, f, ms = 20000) {
  const end = Date.now() + ms;
  for (;;) {
    if (await f()) return;
    if (Date.now() > end) throw new Error(`timed out: ${what}`);
    await sleep(200);
  }
}

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
const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale: "zh-CN" });
await ctx.addCookies(SESSION);
const open = async () => {
  const p = await ctx.newPage();
  await p.goto(`${WEB}/signal`);
  await p.waitForSelector("#sig-readouts .nd-readout__value span");
  return p;
};
/** Counts how many times the readouts fragment was patched. */
const watchPatches = (p) =>
  p.evaluate(() => {
    window.__patches = 0;
    new MutationObserver(() => window.__patches++).observe(document.querySelector("main"), { subtree: true, childList: true, characterData: true });
  });
const patches = (p) => p.evaluate(() => window.__patches);

// Let any earlier connection run out first.
await waitFor("mock idle", async () => (await stats()).connections === 0, (IDLE + 5) * 1000);

await check("E2: two tabs, one upstream connection", async () => {
  const a = await open();
  const b = await open();
  await sleep(1500);
  const [s, h] = [await stats(), await health()];
  assert(s.connections === 1, `mock sees ${s.connections} connections`);
  assert(h.tabs === 2, `luyaz-web counts ${h.tabs} tabs`);
  // pass line 3: the upstream goes away N s after the last tab closes
  await a.close();
  await b.close();
  const t0 = Date.now();
  await waitFor("upstream closed", async () => (await stats()).connections === 0, (IDLE + 10) * 1000);
  const secs = (Date.now() - t0) / 1000;
  assert(secs <= IDLE + 2, `took ${secs.toFixed(1)} s`);
  return `upstream gone ${secs.toFixed(1)} s after the last tab (N = ${IDLE})`;
});

await check("first visit after the idle disconnect never shows an alarm", async () => {
  await waitFor("mock idle", async () => (await stats()).connections === 0, (IDLE + 10) * 1000);
  const q = await ctx.newPage();
  await q.addInitScript(() => {
    window.__tones = [];
    const seen = () => {
      const s = document.querySelector("#sig-status .nd-status");
      if (s) window.__tones.push(s.className);
    };
    document.addEventListener("DOMContentLoaded", () => {
      seen();
      new MutationObserver(seen).observe(document.body, { subtree: true, childList: true, attributes: true });
    });
  });
  await q.goto(`${WEB}/signal`);
  await q.waitForSelector("#sig-status .nd-status--ok, #sig-status .nd-status--warn", { timeout: 10000 });
  const tones = await q.evaluate(() => window.__tones);
  await q.close();
  assert(!tones.some((t) => t.includes("nd-status--bad")), `saw ${tones.find((t) => t.includes("--bad"))}`);
  return `${new Set(tones).size} status states seen, none red`;
});

const p = await open();
await watchPatches(p);

await check("line 7: typing during pushes loses nothing, focus stays", async () => {
  const text = "xinhao signal 信号 lte nr 5g sa";
  // the ⌘K search box, open over the live page (the patches land underneath it)
  await p.keyboard.press(process.platform === "darwin" ? "Meta+k" : "Control+k");
  await p.focus("#palette-q");
  const before = await patches(p);
  await p.keyboard.type(text, { delay: 180 });
  await sleep(1500);
  const got = await p.inputValue("#palette-q");
  const focused = await p.evaluate(() => document.activeElement?.getAttribute("type"));
  const n = (await patches(p)) - before;
  assert(n >= 3, `only ${n} DOM patches while typing; the test proves nothing`);
  assert(got === text, `input holds "${got}"`);
  assert(focused === "search", `focus moved to ${focused}`);
  await p.fill("#palette-q", "");
  await p.keyboard.press("Escape");
  return `${n} patches while typing ${text.length} characters`;
});

await check("help popover stays open across pushes", async () => {
  await p.click("#sig-readouts button[popovertarget=h-rsrp]");
  const before = await patches(p);
  await waitFor("a push", async () => (await patches(p)) - before >= 3, 15000);
  const open = await p.evaluate(() => document.getElementById("h-rsrp")?.matches(":popover-open"));
  assert(open, "popover closed by a push");
  await p.keyboard.press("Escape");
});

await check("line 8 (automated part): pushes touch a live region only when its text changes", async () => {
  await p.evaluate(() => {
    const text = () => [...document.querySelectorAll("[aria-live]")].map((e) => e.textContent).join("|");
    window.__liveNoop = 0;
    window.__liveChange = 0;
    let last = text();
    new MutationObserver((ms) => {
      const hit = ms.some((m) => (m.target.nodeType === 3 ? m.target.parentElement : m.target)?.closest("[aria-live]"));
      const now = text();
      if (hit && now === last) window.__liveNoop++;
      if (now !== last) window.__liveChange++;
      last = now;
    }).observe(document.body, { subtree: true, childList: true, characterData: true });
  });
  const before = await patches(p);
  await waitFor("pushes", async () => (await patches(p)) - before >= 8, 30000);
  const [noop, change] = await p.evaluate(() => [window.__liveNoop, window.__liveChange]);
  assert(noop === 0, `${noop} pushes rewrote a live region without changing its text`);
  return `0 no-op live-region writes; ${change} real verdict changes over ≥8 pushes`;
});

await check("the trend line stays an SVG element after pushes", async () => {
  await waitFor("trend drawn", async () => (await p.$("#trend-plot polyline")) !== null, 90000);
  const before = await patches(p);
  await waitFor("pushes", async () => (await patches(p)) - before >= 4, 20000);
  const [ns, w] = await p.evaluate(() => {
    const l = document.querySelector("#trend-plot polyline");
    return [l?.namespaceURI, l?.getBoundingClientRect().width ?? 0];
  });
  assert(ns === "http://www.w3.org/2000/svg", `trend line namespace ${ns}`);
  assert(w > 10, `trend line is ${w}px wide`);
});

const roamOn = () => p.getAttribute("#roam [role=switch]", "aria-checked");

await check("line 7: pending confirm survives 6 s of pushes, focus on Cancel", async () => {
  await p.click("#roam [role=switch]");
  await p.waitForSelector("#roam-confirm");
  const before = await patches(p);
  await sleep(6000);
  assert((await patches(p)) - before >= 3, "no pushes during the wait");
  assert(await p.isVisible("#roam-confirm"), "confirm vanished");
  const f = await p.evaluate(() => document.activeElement?.textContent?.trim());
  assert(f === "取消", `focus on "${f}"`);
});

await check("Esc closes the confirm without writing", async () => {
  const s0 = (await stats()).roam;
  await p.keyboard.press("Escape");
  await waitFor("closed", async () => !(await p.isVisible("#roam-confirm")), 5000);
  assert((await stats()).roam === s0, "a write happened");
});

await check("confirm writes, shows progress, reads back", async () => {
  const was = await roamOn();
  await p.click("#roam [role=switch]");
  await p.click("#roam-confirm .nd-btn--confirm");
  await p.waitForSelector("#roam :text-matches('正在(打开|关掉)')", { timeout: 3000 });
  await p.waitForSelector("#roam .t-note--ok", { timeout: 15000 });
  const now = await roamOn();
  assert(now !== was, `switch still ${now}`);
  assert((await stats()).roam === (now === "true" ? 1 : 0), "mock state differs");
  return (await p.textContent("#roam .t-note")).trim();
});

await check("a refused write says why and keeps the old value", async () => {
  const was = await roamOn();
  await mockCtl("fail=true");
  await p.click("#roam [role=switch]");
  await p.click("#roam-confirm .nd-btn--confirm");
  await p.waitForSelector("#roam .t-note--bad", { timeout: 10000 });
  assert((await roamOn()) === was, "switch moved");
  return (await p.textContent("#roam .t-note")).trim();
});

await check("no readback in 15 s: 'accepted, not read back', not 'success'", async () => {
  const was = await roamOn();
  await mockCtl("noreadback=true");
  await p.click("#roam [role=switch]");
  await p.click("#roam-confirm .nd-btn--confirm");
  await p.waitForSelector("#roam .t-note--neutral", { timeout: 20000 });
  assert((await roamOn()) === was, "switch moved without a readback");
  return (await p.textContent("#roam .t-note")).trim();
});

await check("a dropped reply is sent once, never re-POSTed, and says 'not confirmed'", async () => {
  let sets = 0;
  await p.route("**/roam/set*", (r) => { sets++; r.abort(); });
  const was = await roamOn();
  await p.click("#roam [role=switch]");
  await p.click("#roam-confirm .nd-btn--confirm");
  await p.waitForSelector("#roam .t-note--warn", { timeout: 8000 });
  await sleep(3000);
  await p.unroute("**/roam/set*");
  assert(sets === 1, `${sets} POSTs (Datastar retried a write)`);
  assert((await roamOn()) === was, "switch moved");
  assert(!(await p.isDisabled("#roam [role=switch]")), "switch still busy");
  return (await p.textContent("#roam .t-note")).trim();
});

await check("every send carries its own op_id (write.rs), none resent", async () => {
  const ids = (await stats()).op_ids.slice(-3); // the dropped one never reached the mock
  assert(ids.length === 3, `${ids.length} sends seen`);
  assert(ids.every((i) => /^lw-\d+-\d+$/.test(i)), ids.join(" "));
  assert(new Set(ids).size === 3, `repeated: ${ids.join(" ")}`);
  return ids.join(" ");
});

for (const theme of ["light", "dark"]) {
  await check(`line 5: axe, ${theme}`, async () => {
    const q = await browser.newContext({ viewport: { width: 1280, height: 900 }, colorScheme: theme, locale: "zh-CN" });
    await q.addCookies(SESSION);
    const pg = await q.newPage();
    await pg.goto(`${WEB}/signal`);
    await pg.waitForSelector("#sig-readouts .nd-readout__value span");
    await pg.click("#roam [role=switch]");
    await pg.waitForSelector("#roam-confirm");
    const r = await new AxeBuilder({ page: pg }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "best-practice"]).analyze();
    await q.close();
    assert(r.violations.length === 0, r.violations.map((v) => `${v.id} (${v.nodes.length}): ${v.nodes[0]?.target}`).join("; "));
    return `${r.passes.length} rules pass`;
  });
}

await browser.close();
for (const [ok, name, note] of results) console.log(`${ok ? "PASS" : "FAIL"}  ${name}${note ? `  — ${note}` : ""}`);
process.exit(results.filter((r) => !r[0]).length);
