// CHILL — stands in for the core's /api/services/chill* (api/chill_page.rs,
// api/chill_jobs.rs, luyaz-api chill.rs). mihomo's own controller and dashboard are not mocked.
//
// Model:
//  * `stateFile` is /tmp/chill.state as chill.sh write_state() leaves it.
//  * `mihomo` is what the controller on 127.0.0.1:9999 would answer (mode +
//    the select groups). It is only reachable while CHILL is running, and
//    never under the "fakesuccess" scenario (mihomo wedged: the core still
//    answers ok:true with version/region null and groups []).
//  * enable / disable / profile / provider-URL go through the single job slot
//    (409 "operation in progress" while one runs); the job finishes ~3 s later
//    and only then changes state (and shared.chill).
//
// Persona: travelling in Taiwan, CHILL running on profile "standard", exit
// "proxy", main group on 🇹🇼 台湾 (region TW). The abroad scenario put the main
// group on DIRECT on arrival; the owner switched it back to 🇹🇼 台湾 by hand
// (see fixtures/scenario.ts, whose pending restore remembers 🇯🇵 日本).
//
// Device clock = local wall time labelled UTC; the mock uses a +8 h offset
// (Taipei/Beijing), so device epoch = real epoch + 28800 and chill.sh-style
// ISO strings carry local digits with a "Z".

import type { Route, Ctx, Reply } from "../lib.ts";
import { ok, fail, bodyField, clone, unixNow } from "../lib.ts";
import { shared } from "../shared.ts";
import { scenarioChillToggled, scenarioChillExitChanged } from "./scenario.ts";
import { MF_ALLDOWN } from "./public.ts";
import type {
  ServiceLog,
  ChillStatus,
  ChillExit,
  ChillProfile,
  ChillProviders,
  ChillProvider,
  ChillBypass,
  ChillJob,
  ChillDashboard,
  ChillJobStarted,
  ChillGroupSummary,
  ChillGroupChoice,
} from "../../../src/lib/api/schemas/services.ts";

// ── clock helpers ───────────────────────────────────────────────────────────

const DEVICE_OFFSET = 8 * 3600;
const deviceNow = (): number => unixNow() + DEVICE_OFFSET;
/** Device-clock ISO: "2026-09-24T14:02:11Z" (local digits). */
const devIso = (devSec: number): string => new Date(devSec * 1000).toISOString().replace(/\.\d{3}Z$/, "Z");

const BOOT = deviceNow();

// ── log tail helper ─────────────────────────────────────────────────────────

const LOG_TAIL_DEFAULT = 200;
const LOG_TAIL_MAX = 2000;

function parseLines(q: URLSearchParams): number {
  const raw = q.get("lines") ?? q.get("n");
  if (raw != null && /^\d+$/.test(raw)) return Math.min(LOG_TAIL_MAX, Math.max(1, Number(raw)));
  return LOG_TAIL_DEFAULT;
}

function tailReply(path: string, all: string[], q: URLSearchParams): Reply {
  const n = parseLines(q);
  const data: ServiceLog = { path, lines: all.slice(-n), limit: n };
  return ok(data);
}

/* ======================================================================== *
 *  CHILL
 * ======================================================================== */

const MAIN_GROUP = "🚀 节点选择";
const AI_GROUP = "🤖 AI";
// luyaz-api chill.rs REGION_GROUPS, in order.
const REGION_GROUPS = [MAIN_GROUP, AI_GROUP, "📺 流媒体", "🍎 Apple", "🐟 漏网之鱼"] as const;

interface MihomoGroup {
  now: string;
  all: string[];
}

// scripts/chill/template.yaml:222-232.
const mihomo = {
  version: "v1.19.14",
  mode: "rule" as "rule" | "global" | "direct",
  groups: {
    [MAIN_GROUP]: { now: "🇹🇼 台湾", all: ["🇯🇵 日本", "🇹🇼 台湾", "🇸🇬 新加坡", "🇺🇸 美国", "🎯 手动节点", "DIRECT"] },
    [AI_GROUP]: { now: "🇺🇸 美国", all: ["🇹🇼 台湾", "🇯🇵 日本", "🇸🇬 新加坡", "🇺🇸 美国"] },
    "📺 流媒体": { now: "🚀 节点选择", all: ["🚀 节点选择", "🇹🇼 台湾", "🇯🇵 日本", "🇸🇬 新加坡", "🇺🇸 美国"] },
    "🍎 Apple": { now: "DIRECT", all: ["DIRECT", "🚀 节点选择", "🇹🇼 台湾", "🇯🇵 日本", "🇸🇬 新加坡", "🇺🇸 美国"] },
    "🐟 漏网之鱼": { now: "🚀 节点选择", all: ["🚀 节点选择", "DIRECT", "🇹🇼 台湾", "🇯🇵 日本", "🇸🇬 新加坡", "🇺🇸 美国"] },
  } as Record<string, MihomoGroup>,
};

/** /data/chill/last-main */
let lastMain = "🇯🇵 日本";
/** Owner's on/off flag: !/data/chill/disabled (luyaz-api chill.rs CHILL_DISABLED). */
let switchedOn = true;
/** chill.env CHILL_BYPASS_IP */
let bypassIps: string[] = ["192.168.0.105"];

type Config = { id: string; name: string; kind: string; source_host: string; has_url: boolean; updated_at: number; provider_count: number; group_count: number; last_error: null; refresh_minutes: number };
const configs: Config[] = [{ id: "p-initial", name: "Home", kind: "full_url", source_host: "example.net", has_url: true, updated_at: BOOT, provider_count: 4, group_count: 5, last_error: null, refresh_minutes: 360 }];
let activeConfig = "p-initial";
let configSerial = 0;
function configList(): Reply {
  return ok({ active_id: activeConfig, profiles: configs.map((p) => ({ ...p, active: p.id === activeConfig })) });
}
function configAction(ctx: Ctx, action: string): Reply {
  if (action === "import") {
    const name = String(bodyField(ctx.body, "name") ?? "");
    const kind = String(bodyField(ctx.body, "kind") ?? "");
    const url = String(bodyField(ctx.body, "url") ?? "");
    if (!name || !["full_url", "node_url", "local_yaml"].includes(kind)) return fail("invalid configuration", 400);
    let host = "";
    try { if (kind !== "local_yaml") host = new URL(url).hostname; } catch { return fail("invalid URL", 400); }
    return startJob("config_import", () => {
      configs.push({ id: `p-mock-${++configSerial}`, name, kind, source_host: host, has_url: !!host, updated_at: deviceNow(), provider_count: 1, group_count: 2, last_error: null, refresh_minutes: kind === "local_yaml" ? 0 : 360 });
      return "Imported";
    });
  }
  const id = String(bodyField(ctx.body, "id") ?? "");
  const p = configs.find((x) => x.id === id);
  if (!p || (action === "delete" && (id === activeConfig || configs.length === 1))) return fail("invalid configuration", 409);
  return startJob(`config_${action}`, () => {
    if (action === "activate") activeConfig = id;
    if (action === "delete") configs.splice(configs.indexOf(p), 1);
    if (action === "refresh") p.updated_at = deviceNow();
    return "Updated";
  });
}

type StateFile = Omit<ChillStatus, "version" | "groups" | "region" | "ai_exit" | "mode" | "exit">;

const stateFile: StateFile = {
  state: "running",
  reason: null,
  cpuss_c: 47,
  mem_avail_mb: 612,
  core_pid: 4127,
  started_at: devIso(BOOT - 5 * 3600 - 12 * 60),
  updated_at: devIso(BOOT),
  bypass_stale: [],
  rules_drift: false,
  mem_pressure: false,
  profile: "standard",
  profile_effective: "standard",
  thermal_eco: false,
} satisfies StateFile;

const chillLog: string[] = (() => {
  const t0 = BOOT - 5 * 3600 - 12 * 60;
  const at = (s: number, m: string) => `${devIso(t0 + s)} ${m}`;
  return [
    at(-2, "flush 完成"),
    at(0, "bypass: 192.168.0.105 已绕行（lan=br-lan）"),
    at(3, "按档位 standard 启动核心"),
    at(6, "apply_dns: dnsmasq 上游已指向 mihomo"),
    at(7, "出口模式恢复为 rule"),
    at(2 * 3600 + 610, "内存紧张但核心 RSS 88412kB 未超标，仅标记"),
  ];
})();

function chillLogLine(msg: string): void {
  chillLog.push(`${devIso(deviceNow())} ${msg}`);
}

/** Is the mihomo controller answering? */
function mihomoUp(ctx?: Ctx): boolean {
  if (ctx?.has("fakesuccess")) return false;
  return stateFile.state === "running";
}

// luyaz-api chill.rs exit_state
function exitState(mode: string, mainNow: string): ChillExit {
  if (mode === "direct") return "direct_all";
  if (mode === "global") return "global";
  if (mainNow === "DIRECT") return "direct_keep_ai";
  return "proxy";
}

const REGION_CODE: Record<string, string> = {
  "🇹🇼 台湾": "TW",
  "🇯🇵 日本": "JP",
  "🇸🇬 新加坡": "SG",
  "🇺🇸 美国": "US",
  "🎯 手动节点": "MANUAL",
  DIRECT: "DIRECT",
};

/** Keep shared.chill in step with this module's state. */
function syncShared(): void {
  // Handlers call adoptShared() first, so a change another area made to
  // shared.chill.state has already been taken over by the time this runs.
  shared.chill.state = stateFile.state;
  shared.chill.profile = stateFile.profile ?? "standard";
  const main = mihomo.groups[MAIN_GROUP];
  shared.chill.exit = exitState(mihomo.mode, main.now);
  shared.chill.region = REGION_CODE[main.now] ?? main.now;
}
syncShared();

/** Adopt a state change made to shared.chill.state by another area. */
function adoptShared(): void {
  const s = shared.chill.state;
  if (s !== stateFile.state && (s === "running" || s === "direct" || s === "unknown")) {
    stateFile.state = s;
    if (s === "running") {
      stateFile.reason = null;
      if (!stateFile.core_pid) stateFile.core_pid = 4000 + Math.floor(Math.random() * 3000);
    } else if (s === "direct") {
      stateFile.reason = stateFile.reason ?? "disabled";
      stateFile.core_pid = 0;
    }
  }
}

function groupNow(name: string): ChillGroupChoice | null {
  const g = mihomo.groups[name];
  return g ? { active: g.now, options: [...g.all] } : null;
}

function chillStatus(ctx: Ctx): Reply {
  adoptShared();
  if (stateFile.state === "unknown") return ok({ state: "unknown" } satisfies ChillStatus);
  const now = deviceNow();
  const data: ChillStatus = {
    ...clone(stateFile),
    updated_at: devIso(now - (now % 60)),
  };
  if (stateFile.state === "running") {
    // "missing": the realistic partial answer is the mihomo-unreachable shape.
    if (mihomoUp(ctx) && !ctx.has("missing")) {
      const groups: ChillGroupSummary[] = REGION_GROUPS.map((name) => ({
        name,
        now: mihomo.groups[name].now,
        size: mihomo.groups[name].all.length,
      }));
      data.version = mihomo.version;
      data.groups = groups;
      data.region = groupNow(MAIN_GROUP);
      data.ai_exit = groupNow(AI_GROUP);
      data.exit = exitState(mihomo.mode, mihomo.groups[MAIN_GROUP].now);
      data.mode = mihomo.mode;
      if (ctx.has("mf-alldown")) {
        data.manual_first = { on_backup: true, manual: "JP 03", backup: "🇯🇵 日本", ...MF_ALLDOWN };
      }
    } else {
      // api/chill_page.rs status with every mihomo call failing.
      data.version = null;
      data.groups = [];
      data.region = null;
      data.ai_exit = null;
      data.mode = null;
    }
  }
  return ok({
    ...data,
    ipv6_mode: "blocked",
    profile_id: activeConfig,
    active_profile: configs.find((p) => p.id === activeConfig) ?? null,
    tun: mihomoUp(ctx) ? { enabled: true, auto_route: true, stack: "system", device: "chill0" } : { enabled: false, auto_route: false, stack: "", device: "" },
    connections: mihomoUp(ctx) ? {
      count: 6,
      upload_total: 1024 * 1024 * 3,
      download_total: 1024 * 1024 * 27,
      items: [
        { id: "c-1", host: "example.com", network: "tcp", rule: "MATCH", upload: 1024, download: 8192 },
        { id: "c-2", host: "api.example.net", network: "tcp", rule: "🚀 节点选择", upload: 2048, download: 16384 },
      ],
    } : null,
  });
}

// ── providers ───────────────────────────────────────────────────────────────

const GiB = 1024 ** 3;
const providers: ChillProvider[] = [
  { name: "snap2", vehicle_type: "File", updated_at: "", node_count: 36, subscription: null, editable: false },
  { name: "snap1", vehicle_type: "File", updated_at: "", node_count: 18, subscription: null, editable: false },
  {
    name: "sub",
    vehicle_type: "HTTP",
    updated_at: "",
    node_count: 24,
    subscription: {
      Upload: Math.round(3.4 * GiB),
      Download: Math.round(61.8 * GiB),
      Total: 200 * GiB,
      Expire: 1_798_732_800, // 2027-01-01
    },
    editable: true,
  },
  { name: "tag", vehicle_type: "File", updated_at: "", node_count: 6, subscription: null, editable: false },
];
/** Device-clock second of each provider's last (re)load. */
const providerLoaded: Record<string, number> = {
  snap2: BOOT - 5 * 3600 - 12 * 60,
  snap1: BOOT - 5 * 3600 - 12 * 60,
  sub: BOOT - 2 * 3600 - 17 * 60,
  tag: BOOT - 5 * 3600 - 12 * 60,
};
/** mihomo's own RFC 3339 with nanoseconds (Go, TZ=UTC, device-clock digits). */
const goTime = (devSec: number): string => devIso(devSec).replace("Z", ".412773021Z");

function providersList(ctx: Ctx): Reply {
  if (!mihomoUp(ctx)) return ok({ providers: [] } satisfies ChillProviders);
  const list: ChillProvider[] = clone(providers).map((p) => ({ ...p, updated_at: goTime(providerLoaded[p.name] ?? BOOT) }));
  if (ctx.has("missing")) {
    // mihomo omits updatedAt/subscriptionInfo for a provider that never loaded.
    for (const p of list) {
      if (p.name !== "sub") continue;
      p.updated_at = null;
      p.subscription = null;
      p.node_count = 0;
    }
  }
  return ok({ providers: list } satisfies ChillProviders);
}

// ── job slot ────────────────────────────────────────────────────────────────

const job: ChillJob = {
  id: 0,
  kind: "",
  status: "idle",
  message: "",
  started_unix: 0,
  finished_unix: 0,
} satisfies ChillJob;

const JOB_MS = 3000;

/** The CHILL job slot (api/chill_jobs.rs). `work` runs when the job finishes and returns Ok message or throws Err message. */
function startJob(kind: string, work: () => string): Reply {
  if (job.status === "running") return fail("operation in progress", 409);
  job.id += 1;
  job.kind = kind;
  job.status = "running";
  job.message = "";
  job.started_unix = deviceNow();
  job.finished_unix = 0;
  const id = job.id;
  const t = setTimeout(() => {
    if (job.id !== id) return;
    try {
      job.message = work();
      job.status = "done";
    } catch (e) {
      job.message = e instanceof Error ? e.message : String(e);
      job.status = "error";
    }
    job.finished_unix = deviceNow();
    syncShared();
  }, JOB_MS);
  t.unref?.();
  const data: ChillJobStarted = { job_id: id, status: "running" };
  return ok(data);
}

function precheck(): string | null {
  if (stateFile.state === "unknown") return null;
  if ((stateFile.mem_avail_mb ?? 999) < 100) {
    return `device low on memory (${stateFile.mem_avail_mb} MB available) — this risks a hang; retry once it settles`;
  }
  if ((stateFile.cpuss_c ?? 0) >= 78) {
    return `device too hot (${stateFile.cpuss_c}°C) — this risks a hang; let it cool, then retry`;
  }
  return null;
}

function coreUp(): void {
  const now = deviceNow();
  stateFile.state = "running";
  stateFile.reason = null;
  stateFile.core_pid = 4000 + Math.floor(Math.random() * 3000);
  stateFile.started_at = devIso(now);
  stateFile.mem_avail_mb = 598;
  chillLogLine("flush 完成");
  for (const ip of bypassIps) chillLogLine(`bypass: ${ip} 已绕行（lan=br-lan）`);
  chillLogLine(`按档位 ${stateFile.profile_effective ?? "standard"} 启动核心`);
  chillLogLine("apply_dns: dnsmasq 上游已指向 mihomo");
  chillLogLine(`出口模式恢复为 ${mihomo.mode}`);
}

/** Start CHILL (the enable job's work). */
function doEnable(): string {
  switchedOn = true;
  if (stateFile.state !== "running") coreUp();
  return "chill.sh start ok";
}

function doDisable(): string {
  switchedOn = false;
  chillLogLine("收到 TERM/INT");
  chillLogLine("restore_dns: dnsmasq 已回滚到备份");
  chillLogLine("flush 完成");
  // Real device: the TERM trap only flushes, so /tmp/chill.state would keep
  // saying "running" (stale). The mock shows what the supervisor loop writes
  // for the disabled flag instead: direct / disabled.
  stateFile.state = "direct";
  stateFile.reason = "disabled";
  stateFile.core_pid = 0;
  chillLogLine("进入直连：disabled");
  return "chill.sh stop ok";
}

function enable(): Reply {
  const e = precheck();
  if (e) return fail(e, 409);
  const r = startJob("enable", doEnable);
  if (!r.error) scenarioChillToggled(true, false);
  return r;
}

function disable(): Reply {
  const wasOn = switchedOn;
  const r = startJob("disable", doDisable);
  if (!r.error) scenarioChillToggled(false, wasOn);
  return r;
}

function profileSet(ctx: Ctx): Reply {
  const want = bodyField(ctx.body, "profile");
  if (want !== "eco" && want !== "standard" && want !== "perf") {
    return fail("profile must be eco, standard or perf", 400);
  }
  return startJob("profile", () => {
    const p = want as ChillProfile;
    const old = stateFile.profile_effective ?? "standard";
    stateFile.profile = p;
    const eff: ChillProfile = stateFile.thermal_eco ? "eco" : p;
    stateFile.profile_effective = eff;
    if (old === eff) return `档位 ${p}（实际 ${eff}，不用动核心）`;
    if (stateFile.state !== "running") return `档位 ${p}（核心没在跑，下次启动生效）`;
    if (old === "eco" || eff === "eco") {
      chillLogLine(`按档位 ${eff} 重启核心`);
      stateFile.core_pid = 4000 + Math.floor(Math.random() * 3000);
      stateFile.started_at = devIso(deviceNow());
      return `档位 ${p}：重启核心（约 10 秒）`;
    }
    return `档位 ${p}：已热重载`;
  });
}

function providersSetUrl(ctx: Ctx): Reply {
  if (ctx.body === undefined) return fail("invalid JSON", 400);
  const name = String(bodyField(ctx.body, "name") ?? "").trim();
  if (name !== "sub") return fail(`provider '${name}' has no editable URL`, 400);
  const url = String(bodyField(ctx.body, "url") ?? "").trim();
  const valid =
    /^https?:\/\//.test(url) && url.length <= 4096 && !/['"]/.test(url) && !/[\s\u0000-\u001f\u007f]/.test(url);
  if (!valid) return fail("bad URL: use http:// or https://, without spaces or quotes", 400);
  const e = precheck();
  if (e) return fail(e, 409);
  return startJob("reload", () => {
    if (stateFile.state !== "running") {
      throw new Error("reload failed: reload 失败（配置未通过校验时会保留旧配置）");
    }
    providerLoaded.sub = deviceNow();
    return "Reloaded";
  });
}

function providersRefresh(ctx: Ctx): Reply {
  if (ctx.body === undefined) return fail("invalid JSON", 400);
  const name = String(bodyField(ctx.body, "name") ?? "").trim();
  if (!name) return fail("missing 'name'", 400);
  if (!mihomoUp(ctx)) {
    return fail("refresh request failed: http://127.0.0.1:9999/providers/proxies/" + encodeURIComponent(name) + ": Connection refused", 502);
  }
  if (!(name in providerLoaded)) return fail("mihomo rejected refresh (HTTP 404)", 502);
  providerLoaded[name] = deviceNow();
  return { data: undefined, delayMs: name === "sub" ? 1200 : 150 };
}

// ── regions / exit (exported for the scenario engine) ────────────────────────

/** PUT /api/services/chill/regions (api/chill_page.rs regions). Returns null on success or [status, error]. */
export function chillSetGroup(group: string, member: string, ctx?: Ctx): [number, string] | null {
  if (!(REGION_GROUPS as readonly string[]).includes(group)) return [400, "unknown group"];
  if (!member) return [400, "missing 'member'"];
  if (!mihomoUp(ctx)) return [502, "mihomo unreachable"];
  const g = mihomo.groups[group];
  if (!g.all.includes(member)) return [400, "member not in group"];
  if (group === MAIN_GROUP && member === "DIRECT" && g.now && g.now !== "DIRECT") lastMain = g.now;
  g.now = member;
  syncShared();
  return null;
}

/** Main group's current member, or null when mihomo does not answer (scenario restore read). */
export function chillMainGroupNow(): string | null {
  return mihomoUp() ? mihomo.groups[MAIN_GROUP].now : null;
}

/** PUT /api/services/chill/exit (luyaz-api chill.rs). */
export function chillSetExit(want: string, ctx?: Ctx): [number, string] | null {
  if (want !== "proxy" && want !== "direct_keep_ai" && want !== "direct_all" && want !== "global") {
    return [400, "state must be proxy, direct_keep_ai, direct_all or global"];
  }
  if (!mihomoUp(ctx)) return [409, "CHILL is not running"];
  const main = mihomo.groups[MAIN_GROUP];
  if (want === "direct_keep_ai" && main.now !== "DIRECT") {
    if (!main.all.includes("DIRECT")) return [409, "the main group has no DIRECT option"];
    if (main.now) lastMain = main.now;
    main.now = "DIRECT";
  } else if (want === "proxy" && (main.now === "DIRECT" || !main.now)) {
    const back =
      lastMain !== "DIRECT" && main.all.includes(lastMain) ? lastMain : main.all.find((m) => m !== "DIRECT");
    if (!back) return [409, "the main group has no node to go back to"];
    main.now = back;
  }
  mihomo.mode = want === "direct_all" ? "direct" : want === "global" ? "global" : "rule";
  syncShared();
  return null;
}

/** Start CHILL through the job slot without the "switched on by hand" hook (scenario restore). */
export function chillEnableJob(): Reply {
  const e = precheck();
  if (e) return fail(e, 409);
  return startJob("enable", doEnable);
}

/** Current job, for the scenario engine's verify_job. */
export function chillJob(): ChillJob {
  return clone(job);
}

function regionsSet(ctx: Ctx): Reply {
  if (ctx.body === undefined) return fail("invalid JSON", 400);
  const group = String(bodyField(ctx.body, "group") ?? "").trim();
  const member = String(bodyField(ctx.body, "member") ?? "").trim();
  const err = chillSetGroup(group, member, ctx);
  if (err) return fail(err[1], err[0]);
  return ok({ group, active: member });
}

function exitSet(ctx: Ctx): Reply {
  const want = String(bodyField(ctx.body, "state") ?? "");
  const err = chillSetExit(want, ctx);
  if (err) return fail(err[1], err[0]);
  scenarioChillExitChanged();
  return ok({ exit: want as ChillExit });
}

// ── bypass ──────────────────────────────────────────────────────────────────

function validIpv4(s: string): boolean {
  const parts = s.split(".");
  return parts.length === 4 && parts.every((p) => /^\d{1,3}$/.test(p) && Number(p) <= 255);
}

function bypassGet(): Reply {
  const data: ChillBypass = { ips: [...bypassIps], stale: [...(stateFile.bypass_stale ?? [])] };
  return ok(data);
}

function bypassSet(ctx: Ctx): Reply {
  if (ctx.body === undefined) return fail("invalid JSON", 400);
  const arr = bodyField(ctx.body, "ips");
  if (!Array.isArray(arr)) return fail("missing 'ips' array", 400);
  let ips = arr
    .filter((v): v is string => typeof v === "string")
    .map((s) => s.trim())
    .filter((s) => s !== "");
  if (ips.length > 32) return fail("too many IPs", 400);
  const bad = ips.find((ip) => !validIpv4(ip));
  if (bad) return fail(`invalid IP: ${bad}`, 400);
  ips = Array.from(new Set(ips)).sort();
  bypassIps = ips;
  // A stale entry that was removed is no longer stale.
  stateFile.bypass_stale = (stateFile.bypass_stale ?? []).filter((ip) => ips.includes(ip));
  return ok({ ips: [...ips] });
}

// ── dashboard ───────────────────────────────────────────────────────────────

const dashboard: ChillDashboard = {
  secret: "3f9a1c07e4b25d68a0c1f7e29b4d8a63",
  ui: "/chill-ui/",
  api: "/chill-api",
} satisfies ChillDashboard;

/* ======================================================================== *
 *  Routes
 * ======================================================================== */

export const routes: Route[] = [
  // ownMissing: generic stripping would null flags Rust always sends
  // (installed/state/job id/secret) and break the pages in unrealistic ways.
  { method: "GET", path: "/api/services/chill", handler: chillStatus, ownMissing: true },
  { method: "GET", path: "/api/services/chill/configs", handler: configList },
  { method: "PUT", path: "/api/services/chill/configs/schedule", handler: (ctx: Ctx) => {
    const id = String(bodyField(ctx.body, "id") ?? "");
    const minutes = Number(bodyField(ctx.body, "refresh_minutes"));
    const p = configs.find((profile) => profile.id === id);
    if (!p || ![0, 60, 360, 720, 1440].includes(minutes) || (p.kind === "local_yaml" && minutes !== 0)) return fail("invalid schedule", 400);
    p.refresh_minutes = minutes;
    return ok({ profile: { ...p, active: p.id === activeConfig } });
  } },
  ...["import", "refresh", "activate", "delete"].map((action) => ({ method: "POST" as const, path: `/api/services/chill/configs/${action}`, handler: (ctx: Ctx) => configAction(ctx, action) })),
  { method: "PUT", path: "/api/services/chill/mode", handler: (ctx) => {
    const mode = bodyField(ctx.body, "mode");
    if (!mihomoUp(ctx) || (mode !== "rule" && mode !== "global" && mode !== "direct")) return fail("invalid mode", 400);
    mihomo.mode = mode;
    return ok({ mode });
  } },
  { method: "GET", path: "/api/services/chill/dashboard", handler: () => ok(clone(dashboard)), ownMissing: true },
  { method: "GET", path: "/api/services/chill/providers", handler: providersList, ownMissing: true },
  { method: "PUT", path: "/api/services/chill/providers", handler: providersSetUrl },
  { method: "POST", path: "/api/services/chill/providers/refresh", handler: providersRefresh },
  { method: "PUT", path: "/api/services/chill/regions", handler: regionsSet },
  { method: "PUT", path: "/api/services/chill/exit", handler: exitSet },
  { method: "GET", path: "/api/services/chill/bypass", handler: bypassGet },
  { method: "PUT", path: "/api/services/chill/bypass", handler: bypassSet },
  { method: "POST", path: "/api/services/chill/enable", handler: enable },
  { method: "POST", path: "/api/services/chill/disable", handler: disable },
  { method: "PUT", path: "/api/services/chill/profile", handler: profileSet },
  { method: "GET", path: "/api/services/chill/job", handler: () => ok(clone(job)), ownMissing: true },
  {
    method: "GET",
    path: "/api/services/chill/log",
    handler: (ctx) => tailReply("/tmp/chill.log", chillLog, ctx.query),
  },
];
