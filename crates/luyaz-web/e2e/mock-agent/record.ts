// Read-only recorder: fetch an audited allowlist of GET endpoints from the
// REAL device API (luyaz-core), save the responses, and print a key diff against the mock
// fixtures. Only with the owner's OK (Decision ledger R5 / D11), and never
// during a power measurement. The core listens on 127.0.0.1:9460 on the device;
// reach it through an SSH tunnel.
//
//   RECORD_CONFIRM=yes AGENT_URL=http://127.0.0.1:9460 TOKEN=<bearer> \
//     node crates/luyaz-web/e2e/mock-agent/record.ts
//
// Output: crates/luyaz-web/e2e/mock-agent/recorded/<path>.json (git-ignored; contains real
// device data — IMEI, ICCID, numbers — never commit or publish it).
//
// SAFETY: some core GETs do change things (scenario/scan scans on a live
// Wi-Fi radio). Only the ALLOWLIST paths below are fetched, each naming the
// core handler that was read to confirm it only reads; nothing else is asked for.

import fs from "node:fs";
import path from "node:path";
import type { Ctx, Scenario } from "./lib.ts";
import { ALL_ROUTES } from "./routes.ts";

interface Entry {
  path: string;
  /** Core handler that was read (file + fn), and what it touches. */
  checked: string;
}

// ------------------------------------------------------------------ allowlist
// Every entry: GET only; the luyaz-core handler (crates/luyaz-core/src/api/…) was read
// to confirm it only reads (re-audited 2026-10-23).
const ALLOWLIST: Entry[] = [
  // ubus reads, the doctor snapshot, the scenario engine's files; no writes.
  { path: "/api/public/status", checked: "status.rs public_status — ubus reads + snapshot/state files" },
  // --- system reads (sys_reads.rs)
  { path: "/api/cpu", checked: "sys_reads.rs cpu — /proc/stat against the last sample kept in memory" },
  { path: "/api/device", checked: "sys_reads.rs device — /proc hostname, uptime, loadavg, version" },
  { path: "/api/battery", checked: "sys_reads.rs battery — sysfs power_supply + the core's own estimate" },
  { path: "/api/modem/status", checked: "sys_reads.rs modem_status — uci/ubus getters" },
  { path: "/api/usb/status", checked: "sys_reads.rs usb_status — ubus zwrt_bsp.usb list" },
  { path: "/api/sms/capacity", checked: "sys_reads.rs sms_capacity — ubus zwrt_wms capacity getter" },
  { path: "/api/router/wan-ipv6", checked: "sys_reads.rs wan_ipv6 — ubus get_apn_at_cid {cid:1} + get_wwaniface" },
  // --- network / Wi-Fi / cell
  { path: "/api/network/clients", checked: "net_reads.rs clients — host hints + DHCP leases (read)" },
  { path: "/api/wifi/status", checked: "wifi_reads.rs status — `iw … info` / `station dump`" },
  { path: "/api/wifi/power-save", checked: "wifi_reads.rs power_save — `iw dev … get power_save` + saved flag" },
  { path: "/api/cell/signal-detect/results", checked: "cell_detect.rs results — ubus nwinfo_get_detect_quality_recorder" },
  { path: "/api/cell/stc/params", checked: "device_admin.rs stc_params — ubus nwinfo_get_stc_white_list_par (may 503)" },
  { path: "/api/cell/stc/status", checked: "device_admin.rs stc_status — ubus nwinfo_get_stc_white_list_status (may 503)" },
  { path: "/api/sim/info", checked: "device_admin.rs sim_info — ubus zwrt_zte_mdm.api get_sim_info" },
  { path: "/api/sim/lock-trials", checked: "device_admin.rs sim_lock_trials — ubus get_simlock_available_trials (getter)" },
  { path: "/api/router/dns", checked: "device_admin.rs dns_get — ubus DNS getter" },
  // --- device / system / tools
  { path: "/api/device/fast-boot", checked: "device_admin.rs fast_boot_get — ubus get_device_info quicken_power_on" },
  { path: "/api/device/charge-control", checked: "charge.rs get — sysfs battery + the limit kept in memory" },
  // system/top keeps the last CPU-tick sample, so a page watching it sees one odd CPU% after this read.
  { path: "/api/system/top", checked: "device_admin/procs.rs top — /proc reads" },
  // alerts includes the alert-SMS phone number: recorded/ is git-ignored, don't share it.
  { path: "/api/alerts", checked: "api.rs alerts_get — the alert files, read only" },
  // plain /api/health only (no refresh=1): the stored doctor snapshot.
  { path: "/api/health", checked: "health.rs get — snapshot file (refresh=1 would run the doctor)" },
  { path: "/api/scheduler/jobs", checked: "scheduler.rs list — jobs held in memory" },
  { path: "/api/speedtest/progress", checked: "speed.rs progress — state held in memory" },
  // --- services / scenario
  { path: "/api/services/tailscale", checked: "ts_reads.rs status — pidof + `tailscale status --json` (read, with a timeout)" },
  { path: "/api/services/tailscale/log", checked: "ts_reads.rs log — tail of /data/tailscaled.log" },
  // chill: up to 3 loopback GETs to mihomo (/version, /proxies, /configs), 3 s each — read-only.
  { path: "/api/services/chill", checked: "chill_page.rs status — CHILL state file + mihomo GETs" },
  { path: "/api/services/chill/providers", checked: "chill_page.rs providers_list — mihomo GET /providers/proxies" },
  { path: "/api/services/chill/bypass", checked: "chill_page.rs bypass_get — reads the CHILL env + state files" },
  { path: "/api/services/chill/job", checked: "chill_jobs.rs job — state held in memory" },
  { path: "/api/services/chill/log", checked: "chill_page.rs log — tail of the CHILL log" },
  // scenario config holds real home SSIDs/BSSIDs: recorded/ is git-ignored, don't share it.
  { path: "/api/scenario", checked: "scenario.rs get → engines/scenario.rs http_get — engine files + SIM info getter" },
  { path: "/api/scenario/template", checked: "scenario.rs template — built from constants" },
  { path: "/api/scenario/log", checked: "scenario.rs log — reads the scenario log file" },
];
// Dropped on 2026-10-23 because the core no longer serves them: network/signal, network/speed,
// network/wan, network/wan6, network/lan-status, data-usage, memory, wifi/radio, GET wifi/guest,
// router/lan, router/firewall/*, router/vpn, router/qos, router/domain-filter, router/apn/*,
// doh/*, modem/data, cell/neighbors/*, cell/signal-detect/progress, sim/imei,
// device/battery-info, device/thermal, device/charger, device/system.

// ------------------------------------------------------------------ excluded
// Never requested, and refused below even if someone adds them to ALLOWLIST:
// - /api/scenario/scan        engines/scenario.rs http_scan — runs a Wi-Fi scan on a live radio; output holds
//                              nearby SSIDs/BSSIDs.
// - /api/health?refresh=1     health.rs get — runs every doctor check first.
// - /api/stk/menu             device_admin/phone.rs stk_menu — opens a SIM toolkit session over AT (6–20 s).
// - /api/at/port              device_admin/phone.rs at_port — may probe the serial ports with AT.
// - /api/network/qos          net_reads.rs qos — sends AT+CGCONTRDP / AT+CGEQOSRDP on the AT port.
// - /api/speedtest/servers    speed.rs servers — cold cache fetches the server list over cellular
//                              (roaming data, shows the public IP, primes the /start cache).
// - /api/esim/*               esim.rs — status/profiles/notifications run lpac against the eUICC; job
//                              shows ICCIDs and means nothing outside a job.
// - /api/sms/forward/*        sms_forward.rs — config holds secrets (bot tokens, webhook URLs/headers),
//                              the log holds whole SMS bodies and codes. Out until a scrubber exists.
// - /api/health/crashlog      health.rs crashlog — read-only, but returns raw logs; not needed for shapes.
// - /chill-api/*, /api/services/chill/dashboard — no longer in the core; kept on the deny list.
// - POST-served reads (POST /api/device/power-save): this recorder is GET-only by design.
const DENY = [
  /^\/api\/scenario\/scan$/,
  /^\/api\/services\/chill\/dashboard$/,
  /^\/api\/health\b.*refresh/,
  /^\/api\/stk\//,
  /^\/chill-api/,
  /^\/api\/at\//,
  /^\/api\/network\/qos$/,
  /^\/api\/speedtest\/servers$/,
  /^\/api\/esim\//,
  /^\/api\/lan\/download$/,
  /^\/api\/call\//,
  /^\/api\/sms\/forward\//,
  /^\/api\/(scenario|wifi)\/scan/,
];

// ------------------------------------------------------------------ guards

function die(msg: string): never {
  console.error(`record.ts: ${msg}`);
  process.exit(2);
}

if (process.env.RECORD_CONFIRM !== "yes") {
  die("refusing to run: set RECORD_CONFIRM=yes (only with the device owner's OK).");
}
const AGENT_URL = (process.env.AGENT_URL ?? "").replace(/\/$/, "");
if (!/^https?:\/\/[^/]+$/.test(AGENT_URL)) {
  die("set AGENT_URL explicitly, e.g. AGENT_URL=http://127.0.0.1:9460 (no default on purpose).");
}
const TOKEN = process.env.TOKEN ?? "";
if (!TOKEN) die("set TOKEN to a bearer token from the core (POST /auth/login).");

for (const e of ALLOWLIST) {
  if (DENY.some((re) => re.test(e.path))) die(`allowlist entry ${e.path} matches the deny list — remove it.`);
}

const OUT_DIR = path.join(import.meta.dirname, "recorded");

// ------------------------------------------------------------------ key diff

type Shape = Map<string, string>; // key path → JSON type

function jsonType(v: unknown): string {
  if (v === null) return "null";
  if (Array.isArray(v)) return "array";
  return typeof v;
}

function shapeOf(v: unknown, prefix = "", out: Shape = new Map()): Shape {
  const t = jsonType(v);
  const prev = out.get(prefix || "$");
  out.set(prefix || "$", prev && prev !== t ? `${prev}|${t}` : t);
  if (Array.isArray(v)) {
    for (const el of v) shapeOf(el, `${prefix}[]`, out);
  } else if (v && typeof v === "object") {
    const rec = v as Record<string, unknown>;
    // Maps keyed by MAC / id: collapse keys that look like data, not field names.
    const keys = Object.keys(rec);
    const dataKeyed = keys.length > 0 && keys.every((k) => /^([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}$|^\d+$/.test(k));
    for (const k of keys) shapeOf(rec[k], dataKeyed ? `${prefix}.<key>` : `${prefix}.${k}`, out);
  }
  return out;
}

async function mockData(p: string): Promise<unknown> {
  const route = ALL_ROUTES.find((r) => r.method === "GET" && r.path === p);
  if (!route) return undefined;
  const scen: ReadonlySet<Scenario> = new Set();
  const ctx: Ctx = {
    method: "GET",
    path: p,
    query: new URLSearchParams(),
    body: undefined,
    scenarios: scen,
    has: () => false,
    now: Date.now(),
  };
  const r = await route.handler(ctx);
  return r.raw !== undefined ? r.raw : r.data;
}

function diff(p: string, real: unknown, mock: unknown): number {
  const a = shapeOf(real);
  const b = shapeOf(mock);
  const lines: string[] = [];
  for (const [k, t] of a) {
    if (!b.has(k)) lines.push(`  + device only  ${k}: ${t}`);
    else if (b.get(k) !== t) lines.push(`  ~ type differs ${k}: device ${t}, mock ${b.get(k)}`);
  }
  for (const [k, t] of b) if (!a.has(k)) lines.push(`  - mock only    ${k}: ${t}`);
  console.log(lines.length ? `${p}\n${lines.join("\n")}` : `${p}  (same keys)`);
  return lines.length;
}

// ------------------------------------------------------------------ run

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

let failed = 0;
let differing = 0;
for (const e of ALLOWLIST) {
  const url = `${AGENT_URL}${e.path}`;
  let body: unknown;
  try {
    const res = await fetch(url, {
      method: "GET",
      headers: { Authorization: `Bearer ${TOKEN}` },
      signal: AbortSignal.timeout(15_000),
    });
    const text = await res.text();
    try {
      body = JSON.parse(text);
    } catch {
      body = { non_json: text.slice(0, 200) };
    }
    if (res.status === 401) die("401 — TOKEN is not valid (log in again).");
    const file = path.join(OUT_DIR, `${e.path.replace(/^\//, "")}.json`);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, JSON.stringify({ status: res.status, checked: e.checked, body }, null, 2) + "\n");
  } catch (err) {
    failed++;
    console.log(`${e.path}  FETCH FAILED: ${(err as Error).message}`);
    continue;
  }
  const env = body as { ok?: boolean; data?: unknown };
  const real = env && typeof env === "object" && "ok" in env ? env.data : body;
  differing += diff(e.path, real, await mockData(e.path)) ? 1 : 0;
  await sleep(300); // be gentle with a small device
}

console.log(`\n${ALLOWLIST.length} endpoints, ${failed} failed, ${differing} with key differences. Saved under ${OUT_DIR}`);
