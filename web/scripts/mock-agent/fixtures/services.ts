// Tailscale + CHILL — mirrors zte-agent/src/services.rs, chill.rs and
// chill_proxy.rs (dashboard info only; the /chill-api/* mihomo reverse proxy
// and /chill-ui/ static files are NOT mocked — the only consumer is the
// zashboard iframe in services/chill/page.tsx DashboardEmbed).
//
// Model:
//  * `stateFile` is /tmp/chill.state as chill.sh write_state() leaves it.
//  * `mihomo` is what the controller on 127.0.0.1:9999 would answer (mode +
//    the select groups). It is only reachable while CHILL is running, and
//    never under the "fakesuccess" scenario (mihomo wedged: the agent still
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
import type {
  TailscaleStatus,
  TailscalePeer,
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
/** Device-clock ISO, chill.sh style: "2026-09-24T14:02:11Z" (local digits). */
const devIso = (devSec: number): string => new Date(devSec * 1000).toISOString().replace(/\.\d{3}Z$/, "Z");
/** tailscaled log style: "2026/09/24 14:02:11". */
const tsStamp = (devSec: number): string => devIso(devSec).replace("T", " ").replace("Z", "").replace(/-/g, "/");

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
 *  Tailscale
 * ======================================================================== */

const TS_PEERS: TailscalePeer[] = [
  {
    id: "nH7kQ2vR4x11CNTRL",
    hostname: "home-nas",
    dns_name: "home-nas.example.ts.net.",
    os: "linux",
    ips: ["100.88.14.2", "fd7a:115c:a1e0::5a01:e02"],
    online: true,
    exit_node: false,
    rx_bytes: 184_320_512,
    tx_bytes: 21_904_118,
    last_seen: "0001-01-01T00:00:00Z",
    last_handshake: "", // filled per request
  },
  {
    id: "nP3mW8cT6y11CNTRL",
    hostname: "macbook",
    dns_name: "macbook.example.ts.net.",
    os: "macOS",
    ips: ["100.71.203.45", "fd7a:115c:a1e0::4e01:cb2d"],
    online: true,
    exit_node: false,
    rx_bytes: 9_812_004,
    tx_bytes: 64_550_903,
    last_seen: "0001-01-01T00:00:00Z",
    last_handshake: "",
  },
  {
    id: "nZ9fL1bN2q11CNTRL",
    hostname: "iphone",
    dns_name: "iphone.example.ts.net.",
    os: "iOS",
    ips: ["100.102.66.9", "fd7a:115c:a1e0::9f01:4209"],
    online: true,
    exit_node: false,
    rx_bytes: 2_310_776,
    tx_bytes: 5_120_331,
    last_seen: "0001-01-01T00:00:00Z",
    last_handshake: "",
  },
  {
    id: "nC4dS7gJ5k11CNTRL",
    hostname: "ipad",
    dns_name: "ipad.example.ts.net.",
    os: "iOS",
    ips: ["100.93.120.77", "fd7a:115c:a1e0::2b01:784d"],
    online: false,
    exit_node: false,
    rx_bytes: 0,
    tx_bytes: 0,
    last_seen: "", // filled per request
    last_handshake: "0001-01-01T00:00:00Z",
  },
  {
    id: "nV6hX3eM8r11CNTRL",
    hostname: "office-pc",
    dns_name: "office-pc.example.ts.net.",
    os: "windows",
    ips: ["100.64.201.18", "fd7a:115c:a1e0::c801:c912"],
    online: false,
    exit_node: false,
    rx_bytes: 0,
    tx_bytes: 0,
    last_seen: "",
    last_handshake: "0001-01-01T00:00:00Z",
  },
];

function tailscaleStatus(ctx: Ctx): Reply {
  if (!shared.tailscale.running) {
    const data: TailscaleStatus = {
      installed: true,
      running: false,
      error:
        "failed to connect to local Tailscale daemon for /localapi/v0/status; not running? Error: dial unix /tmp/tailscaled.sock: connect: no such file or directory",
    };
    return ok(data);
  }
  if (ctx.has("fakesuccess")) {
    // services.rs:50-62: `tailscale status --json` exited non-zero.
    const data: TailscaleStatus = {
      installed: true,
      running: true,
      error:
        "failed to connect to local Tailscale daemon for /localapi/v0/status; not running? Error: dial unix /tmp/tailscaled.sock: connect: connection refused",
    };
    return ok(data);
  }
  const now = deviceNow();
  const peers = clone(TS_PEERS).map((p, i) => {
    if (p.online) p.last_handshake = devIso(now - 20 - i * 37);
    else p.last_seen = devIso(now - (i === 3 ? 3 * 3600 + 540 : 2 * 86400 + 4200));
    return p;
  });
  if (ctx.has("missing")) {
    // Realistic absences: fields tailscale leaves out (older build, no DERP
    // home yet, peers never talked to); the flags services.rs always sends stay.
    for (const p of peers) {
      p.rx_bytes = null;
      p.tx_bytes = null;
      p.last_seen = null;
      p.last_handshake = null;
      p.os = null;
    }
  }
  const data: TailscaleStatus = {
    installed: true,
    running: true,
    backend_state: "Running",
    version: ctx.has("missing") ? null : "1.88.1-t8c1e0b2a4-g3f5d7a9c1",
    auth_url: null,
    self: {
      hostname: shared.tailscale.node,
      dns_name: ctx.has("missing") ? null : `${shared.tailscale.node}.example.ts.net.`,
      ips: ["100.101.12.34", "fd7a:115c:a1e0::6a01:c22"],
      online: true,
      relay: ctx.has("missing") ? null : "tok",
      exit_node_option: false,
    },
    exit_node: null,
    peer_count: peers.length,
    peer_online: peers.filter((p) => p.online).length,
    peers,
  };
  return ok(data);
}

function tailscaleLogLines(): string[] {
  const t0 = BOOT - 5 * 3600;
  const at = (s: number, m: string) => `${tsStamp(t0 + s)} ${m}`;
  const lines = [
    at(0, "logtail started"),
    at(0, "Program starting: v1.88.1-t8c1e0b2a4-g3f5d7a9c1, Go 1.25.1: []string{\"/data/tailscale/tailscaled\", \"--state=/data/tailscale/tailscaled.state\", \"--socket=/tmp/tailscaled.sock\", \"--port=41641\"}"),
    at(0, "LogID: disabled (TS_NO_LOGS_NO_SUPPORT)"),
    at(1, "wgengine.NewUserspaceEngine(tun \"tailscale0\") ..."),
    at(2, "portmapper: disabled by TS_DISABLE_PORTMAPPER"),
    at(3, "Backend: logs: be:disabled fe:"),
    at(4, "control: client.Login(0)"),
    at(5, "control: RegisterReq: got response; nodeKeyExpired=false, machineAuthorized=true; authURL=false"),
    at(5, "health(warnable=login-state): ok"),
    at(6, "magicsock: home is now derp-9 (tok)"),
    at(6, "magicsock: endpoints changed: 1.34.201.17:41641 (stun), 10.0.66.1:41641 (local)"),
    at(7, "Switching ipn state Starting -> Running (WantRunning=true, nm=true)"),
    at(12, "magicsock: derp-9 connected; connGen=1"),
    at(40, "wgengine: Reconfig: configuring userspace WireGuard config (with 3/5 peers)"),
    at(41, "magicsock: disco: node [H7kQ2] d:5a01e02 now using 36.227.88.14:41641 mtu=1360 tx=3f2a1c"),
    at(95, "magicsock: disco: node [P3mW8] d:4e01cb2 now using 114.36.91.207:52318 mtu=1360 tx=81b0d4"),
    at(3600, "netcheck: report: udp=true v6=false mapvarydest=false hair=false portmap= v4a=1.34.201.17:41641 derp=9 derpdist=9v4:38ms,1v4:61ms,7v4:74ms"),
    at(7200, "magicsock: disco: node [Z9fL1] d:9f014209 now using 101.12.44.180:61022 mtu=1360 tx=c02e77"),
    at(4 * 3600, "netcheck: report: udp=true v6=false mapvarydest=false hair=false portmap= v4a=1.34.201.17:41641 derp=9 derpdist=9v4:36ms,1v4:63ms,7v4:71ms"),
    at(5 * 3600 - 300, "wgengine: idle peer [C4dS7] now inactive, removing from wireguard"),
  ];
  return lines;
}

/* ======================================================================== *
 *  CHILL
 * ======================================================================== */

const MAIN_GROUP = "🚀 节点选择";
const AI_GROUP = "🤖 AI";
// chill.rs:44 REGION_GROUPS, in order.
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
/** Owner's on/off flag: !/data/chill/disabled (chill.rs:492). */
let switchedOn = true;
/** chill.env CHILL_BYPASS_IP */
let bypassIps: string[] = ["10.0.66.105"];

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
    at(0, "bypass: 10.0.66.105 已绕行（lan=br-lan）"),
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

// chill.rs:325 exit_state
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
    } else {
      // chill.rs:122-140 with every mihomo call failing.
      data.version = null;
      data.groups = [];
      data.region = null;
      data.ai_exit = null;
      data.mode = null;
    }
  }
  return ok(data);
}

// ── providers ───────────────────────────────────────────────────────────────

const GiB = 1024 ** 3;
const providers: ChillProvider[] = [
  { name: "nexi", vehicle_type: "File", updated_at: "", node_count: 36, subscription: null, editable: false },
  { name: "oix", vehicle_type: "File", updated_at: "", node_count: 18, subscription: null, editable: false },
  {
    name: "shouhou",
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
  nexi: BOOT - 5 * 3600 - 12 * 60,
  oix: BOOT - 5 * 3600 - 12 * 60,
  shouhou: BOOT - 2 * 3600 - 17 * 60,
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
      if (p.name !== "shouhou") continue;
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

/** chill.rs:553 start_job. `work` runs when the job finishes and returns Ok message or throws Err message. */
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
  if (name !== "shouhou") return fail(`provider '${name}' has no editable URL`, 400);
  const url = String(bodyField(ctx.body, "url") ?? "").trim();
  const valid =
    /^https?:\/\//.test(url) && url.length <= 4096 && !/['"]/.test(url) && !/[\s\u0000-\u001f\u007f]/.test(url);
  if (!valid) return fail("invalid URL (http(s) only, no quotes/spaces)", 400);
  const e = precheck();
  if (e) return fail(e, 409);
  return startJob("reload", () => {
    if (stateFile.state !== "running") {
      throw new Error("reload failed: reload 失败（配置未通过校验时会保留旧配置）");
    }
    providerLoaded.shouhou = deviceNow();
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
  return { data: undefined, delayMs: name === "shouhou" ? 1200 : 150 };
}

// ── regions / exit (exported for the scenario engine) ────────────────────────

/** chill.rs:270 regions_set core. Returns null on success or [status, error]. */
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

/** chill.rs:354 exit_set core. */
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
  { method: "GET", path: "/api/services/tailscale", handler: tailscaleStatus, ownMissing: true },
  {
    method: "GET",
    path: "/api/services/tailscale/log",
    handler: (ctx) => tailReply("/data/tailscaled.log", tailscaleLogLines(), ctx.query),
  },
  { method: "GET", path: "/api/services/chill", handler: chillStatus, ownMissing: true },
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
