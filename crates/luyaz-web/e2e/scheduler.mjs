// /scheduler (batch 5a), with /router/schedule folded in: the common checks (first load ≤ 40 KiB
// gzip without fonts, axe light/dark 0, 390 px no sideways scroll, English body without Chinese,
// no page errors, screenshots) and each write path once against the typed mock agent (via the
// fake agent; the mock u60d says "not mine" for /api/scheduler/*): create (tier 2), the forbidden
// list refused here, a bad body refused here, switch on a reboot job (tier 3 dialog), edit keeps
// the restore step, delete, and the agent's own refusal (job deleted behind the page's back).
// The fake agent's jobs live in its memory: restart it before each run.
//   WEB=http://127.0.0.1:18253 AGENT=http://127.0.0.1:19270 OUT=/tmp/lw-sch \
//     crates/luyaz-web/e2e/run.sh scheduler
import { chromium } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mkdirSync } from "node:fs";

const WEB = process.env.WEB ?? "http://127.0.0.1:18253";
const AGENT = process.env.AGENT ?? "http://127.0.0.1:19270";
const OUT = process.env.OUT ?? "/tmp/lw-sch";
mkdirSync(OUT, { recursive: true });
let failed = 0;
const ok = (c, m) => { console.log(`${c ? "ok  " : "FAIL"} ${m}`); if (!c) failed++; };
const CJK = /[　-〿一-鿿＀-￯]/g;

// luyaz-web has no agent token any more (8d-2): the checks read the typed mock agent with their own
// login each time (the mock keeps one token; the u60d front logs in again when it finds it dead).
const r = await fetch(`${WEB}/login`, { method: "POST", redirect: "manual", headers: { origin: new URL(WEB).origin, "content-type": "application/x-www-form-urlencoded" }, body: "password=x&next=/" });
const sid = /lw_sid=([0-9a-f]+)/.exec(r.headers.get("set-cookie") ?? "")?.[1];
if (!sid) throw new Error(`login failed: ${r.status}`);
const agent = async (method, body) => {
  const tok = (await (await fetch(`${AGENT}/api/auth/login`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ password: "x" }) })).json()).data.token;
  return fetch(`${AGENT}/api/scheduler/jobs`, { method, headers: { authorization: `Bearer ${tok}`, "content-type": "application/json" }, body: body && JSON.stringify(body) }).then((x) => x.json());
};
const agentJobs = async () => (await agent("GET")).data ?? [];

async function firstLoad(p) {
  const urls = [p.url(), ...(await p.evaluate(() => [...document.querySelectorAll("link[href^='/a/'],script[src^='/a/']")].map((e) => e.href || e.src)))];
  const html = await p.content();
  for (const m of html.matchAll(/(?:url\(|href=")(\/a\/[^")#]+)/g)) urls.push(WEB + m[1].replaceAll("&amp;", "&"));
  let body = 0;
  for (const u of new Set(urls)) {
    if (u.includes(".woff2")) continue;
    const res = await fetch(u.split("#")[0], { headers: { cookie: `lw_sid=${sid}`, "accept-encoding": "gzip" } });
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
const until = (p, sel, re, timeout = 10000) => p.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent ?? ""), [sel, re.source], { timeout }).catch(() => {});

async function own(p) {
  ok(/2 个任务里 1 个开着/.test(await txt(p, "#sch-status")), `status: ${await txt(p, "#sch-status")}`);
  ok(/没有定时重启.*有 1 个重启任务，但都关着/.test(await txt(p, "#sch-reboot")), `reboot section: ${await txt(p, "#sch-reboot")}`);
  ok(await p.locator("#sch-list .sch-row").count() === 2, "two jobs listed");
  ok(/07:30 恢复 · 上次恢复 .* · HTTP 200/.test(await txt(p, "#sch-list .sch-row >> nth=0")), "restore line shown");
  ok(await p.getByRole("switch", { name: "启用： 每周一凌晨重启" }).count() === 1, "row switch named by its job");

  // a bad body and a forbidden path: refused here, nothing sent
  const n0 = (await agentJobs()).length;
  await p.getByRole("button", { name: "新建任务", exact: true }).click();
  await p.getByLabel("任务名称").waitFor();
  await p.getByLabel("任务名称").fill("测试");
  await p.getByLabel("路径").fill("/api/esim/switch");
  await p.getByRole("button", { name: "创建", exact: true }).click();
  await until(p, "#sch-save-op", /eSIM/);
  ok(/定时任务不能调eSIM/.test(await txt(p, "#sch-save-op")), `forbidden refused: ${await txt(p, "#sch-save-op")}`);
  await p.getByLabel("路径").fill("/api/device/Factory-Reset/");
  await p.getByRole("button", { name: "创建", exact: true }).click();
  await until(p, "#sch-save-op", /恢复出厂/);
  ok(/恢复出厂/.test(await txt(p, "#sch-save-op")), "spelling variant refused too");
  await p.getByLabel("路径").fill("/api/wifi/power-save");
  await p.getByLabel("请求体（JSON，可选）").fill("{nope");
  await p.getByRole("button", { name: "创建", exact: true }).click();
  await until(p, "#sch-save-op", /JSON/);
  ok(/不是有效的 JSON/.test(await txt(p, "#sch-save-op")), "bad JSON body blocks the save");
  ok((await agentJobs()).length === n0, "nothing sent");

  // create (tier 2): inline confirm, readback
  await p.getByLabel("请求体（JSON，可选）").fill('{"enable": 1}');
  await p.getByLabel("方法").selectOption("PUT");
  await p.getByRole("button", { name: "周二" }).click(); // all seven on → Tuesday off
  await p.getByRole("button", { name: "创建", exact: true }).click();
  await p.locator("#sch-save-confirm").waitFor({ timeout: 5000 }).catch(() => {});
  ok(/从现在起，设备会在 03:00 — 周一、周三、周四、周五、周六、周日 发出 PUT \/api\/wifi\/power-save/.test(await txt(p, "#sch-save-confirm")), `create confirm (tier 2 inline): ${await txt(p, "#sch-save-op")}`);
  ok(await p.locator("dialog[open]").count() === 0, "no dialog for a tier-2 job");
  await p.screenshot({ path: `${OUT}/sch-create-confirm.png`, fullPage: true });
  await p.getByRole("button", { name: "确认：创建" }).click();
  await until(p, "#sch-save-op", /已创建|没有生效|设备已接受/);
  ok(/已创建「测试」/.test(await txt(p, "#sch-save-op")), `created: ${await txt(p, "#sch-save-op")}`);
  ok(await p.locator("#sch-form section").count() === 0, "form closed");
  const made = (await agentJobs()).find((j) => j.name === "测试");
  ok(made && made.action.method === "PUT" && made.action.body?.enable === 1 && JSON.stringify(made.schedule.days) === "[0,2,3,4,5,6]", `agent has it: ${JSON.stringify(made)}`);
  await until(p, "#sch-list", /测试/);
  ok(await p.locator("#sch-list .sch-row").count() === 3, "list re-read after the write");

  // switch on the reboot job: tier 3 dialog
  await p.getByRole("switch", { name: "启用： 每周一凌晨重启" }).click();
  await p.locator("dialog[open]").waitFor({ timeout: 5000 }).catch(() => {});
  const dlg = await txt(p, "dialog[open]");
  ok(/打开「每周一凌晨重启」/.test(dlg) && /约 90 秒/.test(dlg) && /POST \/api\/device\/reboot/.test(dlg), `tier 3 dialog: ${dlg.slice(0, 160)}`);
  await p.screenshot({ path: `${OUT}/sch-reboot-dialog.png` });
  await p.getByRole("button", { name: "确认：打开" }).click();
  await until(p, "#sch-row-op", /已打开|没有生效|设备已接受/);
  ok(/已打开「每周一凌晨重启」/.test(await txt(p, "#sch-row-op")), `switched on: ${await txt(p, "#sch-row-op")}`);
  ok((await agentJobs()).find((j) => j.id === 2)?.enabled === true, "agent: job 2 on");
  await until(p, "#sch-reboot", /定时重启已开/);
  ok(/定时重启已开.*04:30 — 周一/.test(await txt(p, "#sch-reboot")), `reboot section follows: ${await txt(p, "#sch-reboot")}`);

  // edit job 1 (has a restore step): keeps restore + on/off
  await p.locator("#sch-e-1").click();
  await p.getByLabel("任务名称").waitFor();
  ok(await p.getByLabel("任务名称").inputValue() === "夜间省电" && /power_saver_mode/.test(await p.getByLabel("请求体（JSON，可选）").inputValue()), "edit form filled from the job");
  await p.getByLabel("任务名称").fill("夜间省电2");
  await p.getByRole("button", { name: "更新", exact: true }).click();
  await p.getByRole("button", { name: "确认：保存" }).click();
  await until(p, "#sch-save-op", /已保存|没有生效|设备已接受/);
  ok(/已保存「夜间省电2」/.test(await txt(p, "#sch-save-op")), `saved: ${await txt(p, "#sch-save-op")}`);
  const j1 = (await agentJobs()).find((j) => j.id === 1);
  ok(j1?.name === "夜间省电2" && j1.enabled === true && j1.restore?.time === "07:30", `edit kept restore + on: ${JSON.stringify(j1?.restore)}`);

  // delete (tier 2)
  await p.locator(`#sch-d-${made.id}`).click();
  await p.getByRole("button", { name: "确认：删除" }).click();
  await until(p, "#sch-row-op", /已删除|没有生效|设备已接受/);
  ok(/任务已删除/.test(await txt(p, "#sch-row-op")) && !(await agentJobs()).some((j) => j.id === made.id), `deleted: ${await txt(p, "#sch-row-op")}`);

  // the agent refuses: the job goes away between the confirm and the send
  await p.locator("#sch-d-2").click();
  await p.getByRole("button", { name: "确认：删除" }).waitFor();
  await agent("DELETE", { id: 2 });
  await p.getByRole("button", { name: "确认：删除" }).click();
  await until(p, "#sch-row-op", /没有生效|已删除/);
  ok(/没有生效：job not found/.test(await txt(p, "#sch-row-op")), `agent refusal shown: ${await txt(p, "#sch-row-op")}`);
  await p.screenshot({ path: `${OUT}/sch-after.png`, fullPage: true });
}

const browser = await chromium.launch({ channel: "chrome" });
// /router/schedule → /scheduler#reboot
{
  const res = await fetch(`${WEB}/router/schedule`, { redirect: "manual", headers: { cookie: `lw_sid=${sid}`, accept: "text/html" } });
  ok(res.status === 302 && res.headers.get("location") === "/scheduler#reboot", `/router/schedule → ${res.status} ${res.headers.get("location")}`);
  const sub = await fetch(`${WEB}/router/schedule/x`, { redirect: "manual", headers: { cookie: `lw_sid=${sid}`, accept: "text/html" } });
  ok(sub.status === 302 && sub.headers.get("location") === "/scheduler#reboot", `/router/schedule/x → ${sub.status}`);
}
for (const [w, h, name, lang, theme] of [[1280, 900, "desk", "en", "dark"], [390, 844, "phone", "zh", "light"], [390, 844, "phone", "en", "dark"], [1280, 900, "desk", "zh", "light"]]) {
  const ctx = await browser.newContext({ viewport: { width: w, height: h } });
  await ctx.addCookies([{ name: "lw_sid", value: sid, url: WEB }, { name: "u60_lang", value: lang, url: WEB }]);
  await ctx.addInitScript((t) => localStorage.setItem("u60.theme", t), theme);
  const p = await ctx.newPage();
  const errs = [];
  p.on("pageerror", (e) => errs.push(e.message));
  p.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errs.push(m.text()); });
  await p.goto(WEB + (name === "phone" && lang === "zh" ? "/router/schedule" : "/scheduler"), { waitUntil: "load" });
  if (name === "phone" && lang === "zh") ok(new URL(p.url()).pathname === "/scheduler" && new URL(p.url()).hash === "#reboot", `browser lands on ${p.url()}`);
  await p.locator("#sch-list .sch-row").first().waitFor({ timeout: 10000 }).catch(() => {});
  ok(await p.locator("#sch-list .sch-row").count() >= 2, `${name} ${lang}: jobs shown`);
  if (name === "desk" && lang === "zh") {
    const kb = (await firstLoad(p)) / 1024;
    ok(kb <= 40, `first load ${kb.toFixed(1)} KiB gzip without fonts (≤ 40)`);
  }
  if (lang === "en") {
    ok(/1 of 2 jobs on/.test(await txt(p, "#sch-status")), `English status: ${await txt(p, "#sch-status")}`);
    const left = (await mainText(p)).match(CJK) ?? [];
    ok(left.length === 0, `English body without Chinese (${[...new Set(left)].join("") || "none"})`);
    ok(await p.getAttribute("html", "lang") === "en", "html lang=en");
  }
  if (name === "phone") {
    // the form open too: the widest state
    await p.locator("main button.nd-btn--primary").first().click();
    await p.locator("#sch-form section").waitFor({ timeout: 5000 }).catch(() => {});
    const sw = await p.evaluate(() => document.documentElement.scrollWidth);
    ok(sw <= 390, `${lang}: 390 px no sideways scroll, form open (${sw})`);
  }
  const axe = await new AxeBuilder({ page: p }).analyze();
  ok(axe.violations.length === 0, `axe ${name} ${lang} ${theme}: ${axe.violations.map((v) => v.id + "×" + v.nodes.length + " " + v.nodes.slice(0, 2).map((n) => n.target).join("|")).join(", ") || "0"}`);
  await p.screenshot({ path: `${OUT}/sch-${name}-${lang}-${theme}.png`, fullPage: true });
  if (name === "desk" && lang === "zh") await own(p);
  ok(errs.length === 0, `${name} ${lang}: no page errors ${errs.join("; ")}`);
  await ctx.close();
}
await browser.close();
console.log(failed ? `${failed} FAILED` : "all ok");
process.exit(failed ? 1 : 0);
