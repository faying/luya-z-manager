// Read-only recorder: fetch an audited allowlist of GET endpoints from the
// REAL zte-agent, save the responses, and print a key diff against the mock
// fixtures. For later use on the device, with the owner's OK (Decision
// ledger R5 / D11). Do not run it during the power test.
//
//   RECORD_CONFIRM=yes AGENT_URL=http://192.168.0.1:9090 TOKEN=<bearer> \
//     node scripts/mock-agent/record.ts
//
// Output: scripts/mock-agent/recorded/<path>.json (git-ignored; contains real
// device data — IMEI, ICCID, numbers — never commit or publish it).
//
// SAFETY: "GET" does not mean "read-only" on this agent (homemode/scan
// rewrites uci and reloads Wi-Fi). Only the endpoints in ALLOWLIST below are
// requested; each entry names the handler that was read to confirm it has no
// side effects. Anything not on the list is never requested.

import fs from "node:fs";
import path from "node:path";
import type { Ctx, Scenario } from "./lib.ts";
import { ALL_ROUTES } from "./routes.ts";

interface Entry {
  path: string;
  /** Handler file:line checked, and what it does. */
  checked: string;
}

// ------------------------------------------------------------------ allowlist
// Every entry: GET only, handler audited (zte-agent/src, 2026-09-24 tree).
const ALLOWLIST: Entry[] = [
  // public.rs:29 — ubus reads (nwinfo_get_netinfo, get_wwaniface, zwrt_wlan report,
  // wms capacity), sysfs battery, pidof + `tailscale status` (read), local files.
  { path: "/api/public/status", checked: "public.rs:29" },
  // handlers.rs:109 — ubus zte_nwinfo_api nwinfo_get_netinfo (read).
  { path: "/api/network/signal", checked: "handlers.rs:109" },
  // handlers.rs:117 — /proc/net/dev.
  { path: "/api/network/traffic", checked: "handlers.rs:117 → system.rs:244" },
  // handlers.rs:123 — in-memory ring buffer snapshot.
  { path: "/api/network/speed", checked: "handlers.rs:123 → system.rs:351" },
  // network_ext.rs — ubus network.interface.* status (netifd read).
  { path: "/api/network/wan", checked: "network_ext.rs:16" },
  { path: "/api/network/wan6", checked: "network_ext.rs:23" },
  { path: "/api/network/lan-status", checked: "network_ext.rs:30" },
  // network_ext.rs:37 — luci-rpc getHostHints + getDHCPLeases (read).
  { path: "/api/network/clients", checked: "network_ext.rs:37" },
  // handlers.rs:137 — uci get zwrt_data_commit.wwancid1dst.* only.
  { path: "/api/data-usage", checked: "handlers.rs:137" },
  // handlers.rs:92 — /proc/stat delta in memory.
  { path: "/api/cpu", checked: "handlers.rs:92 → system.rs:46 (+ sysfs cpufreq)" },
  // handlers.rs battery / memory — sysfs power_supply, /proc/meminfo.
  { path: "/api/battery", checked: "handlers.rs battery → system.rs read_battery_at" },
  { path: "/api/memory", checked: "handlers.rs memory → system.rs read_meminfo" },
  // wifi.rs:80 — ~25 uci get, zwrt_wlan report (fallback), iw info / station dump.
  { path: "/api/wifi/status", checked: "wifi.rs:80" },
  // wifi_radio.rs:329 → observe :268 — uci get ×2, ps w, hostapd_cli status.
  { path: "/api/wifi/radio", checked: "wifi_radio.rs:329" },
  // wifi.rs:307 — uci get ×8 + ubus wlan_get_guest_access_left_time (read).
  { path: "/api/wifi/guest", checked: "wifi.rs:307" },
  // homemode.rs:103 / :314 — local files under /data only.
  { path: "/api/homemode", checked: "homemode.rs:103" },
  { path: "/api/homemode/log", checked: "homemode.rs:314" },
  // --- router (audited by router pass; ZTE get_* methods assumed read-only by name)
  { path: "/api/router/dns", checked: "router.rs:6 — ubus router_get_dns_para (+ uci get network.wan.dns)" },
  { path: "/api/router/lan", checked: "router.rs:54 — 6× uci get" },
  { path: "/api/router/firewall", checked: "router.rs:93 — ubus router_get_firewall_para" },
  { path: "/api/router/firewall/upnp", checked: "router.rs:144 — ubus router_get_upnp_switch" },
  { path: "/api/router/firewall/port-forward", checked: "router.rs:162 — ubus router_get_portforward_rule" },
  { path: "/api/router/firewall/filter-rules", checked: "router.rs:191 — ubus router_get_macipport_filter_rule" },
  { path: "/api/router/vpn", checked: "router.rs:198 — ubus router_get_alg_para" },
  { path: "/api/router/qos", checked: "router.rs:216 — ubus router_get_qos_switch" },
  { path: "/api/router/domain-filter", checked: "router.rs:234 — ubus router_get_domainfilter_rule" },
  { path: "/api/router/apn/mode", checked: "router.rs:252 — ubus zwrt_apn_object get_apn_mode" },
  { path: "/api/router/apn/profiles", checked: "router.rs:270 — ubus get_manu_apn_list" },
  { path: "/api/router/apn/auto-profiles", checked: "router.rs:299 — ubus get_auto_apn_list" },
  { path: "/api/router/wan-ipv6", checked: "router.rs:332 — ubus get_apn_at_cid {cid:1} + zwrt_data get_wwaniface" },
  { path: "/api/doh/status", checked: "server.rs:432 → doh/mod.rs:68 — in-memory" },
  { path: "/api/doh/cache", checked: "server.rs:473 → doh/mod.rs:101 — in-memory" },
  // --- modem / cell / sim (ubus getters only; the agent sends no AT and writes no uci for these.
  //     Firmware daemons may still query the modem internally — not visible from this repo.)
  { path: "/api/modem/status", checked: "handlers.rs:129 — uci get zte_nwinfo.sys_info.operate_mode" },
  { path: "/api/modem/data", checked: "modem_ext.rs:6 — ubus zwrt_data get_wwaniface {cid:1}" },
  { path: "/api/modem/scan/status", checked: "modem_ext.rs:57 — ubus nwinfo_m_netselect_status (reads job state)" },
  { path: "/api/modem/scan/results", checked: "modem_ext.rs:64 — ubus nwinfo_m_netselect_contents" },
  { path: "/api/modem/register/result", checked: "modem_ext.rs:82 — ubus nwinfo_m_netselect_result" },
  { path: "/api/cell/neighbors/nr", checked: "cell.rs:42 — ubus nwinfo_get_nr5g_nbr_contents (scan itself is the POST)" },
  { path: "/api/cell/neighbors/lte", checked: "cell.rs:49 — ubus nwinfo_get_lte_nbr_contents" },
  { path: "/api/cell/stc/params", checked: "cell.rs:85 — ubus nwinfo_get_stc_white_list_par (may 503)" },
  { path: "/api/cell/stc/status", checked: "cell.rs:103 — ubus nwinfo_get_stc_white_list_status (may 503)" },
  { path: "/api/cell/signal-detect/results", checked: "cell.rs:145 — ubus nwinfo_get_detect_quality_recorder" },
  { path: "/api/cell/signal-detect/progress", checked: "cell.rs:152 — ubus nwinfo_get_progress_and_quality" },
  { path: "/api/sim/info", checked: "sim.rs:6 — ubus zwrt_zte_mdm.api get_sim_info" },
  { path: "/api/sim/imei", checked: "sim.rs:13 — ubus get_imei" },
  { path: "/api/sim/lock-trials", checked: "sim.rs:64 — ubus get_simlock_available_trials (getter)" },
  // --- sms
  { path: "/api/sms/capacity", checked: "sms.rs:42 — ubus zwrt_wms zwrt_wms_get_wms_capacity (read)" },
  // --- device / system / tools
  { path: "/api/device/battery-info", checked: "network_ext.rs:55 — ubus zwrt_bsp.battery list" },
  { path: "/api/device/thermal", checked: "device_ext.rs:8 — ubus zwrt_bsp.thermal get_cpu_temp (getter)" },
  { path: "/api/device/charger", checked: "device_ext.rs:15 — ubus zwrt_bsp.charger list" },
  { path: "/api/device/system", checked: "device_ext.rs:22 — ubus system info" },
  { path: "/api/device", checked: "handlers.rs:67 — /proc hostname, uptime, loadavg, version" },
  { path: "/api/device/charge-control", checked: "device_ext.rs:44 — sysfs battery + ubus charger list + in-memory limit" },
  { path: "/api/device/fast-boot", checked: "device_ext.rs:156 — ubus zwrt_mc.device.manager get_device_info quicken_power_on" },
  { path: "/api/usb/status", checked: "usb.rs:6 — ubus zwrt_bsp.usb list" },
  // system/top only moves the agent's in-memory CPU-tick baseline (skews a watching page's CPU% for one tick).
  { path: "/api/system/top", checked: "handlers.rs:181 → system.rs:468 — /proc reads" },
  // alerts includes the alert-SMS phone number: recorded/ is git-ignored, don't share it.
  { path: "/api/alerts", checked: "alerts.rs:262 — /data/alerts/* files, /proc/uptime, cached uci get" },
  // plain /api/health only (no refresh=1): cached snapshot + /data/crashlog listing.
  { path: "/api/health", checked: "health.rs:184 — cached snapshot, reads /data/crashlog" },
  { path: "/api/scheduler/jobs", checked: "scheduler.rs:311 — in-memory jobs" },
  { path: "/api/speedtest/progress", checked: "speedtest.rs:425 — in-memory state" },
  { path: "/api/lan/ping", checked: "lan_test.rs:25 — constant reply" },
  // --- services / scenario
  { path: "/api/services/tailscale", checked: "services.rs:34 — pidof + `tailscale status --json` (read; no agent timeout)" },
  { path: "/api/services/tailscale/log", checked: "services.rs:182 — tails /data/tailscaled.log" },
  // scenario config holds real home SSIDs/BSSIDs: recorded/ is git-ignored, don't share it.
  { path: "/api/scenario", checked: "scenario.rs:1614 — ubus get_sim_info (read) + /data/scenario files" },
  { path: "/api/scenario/template", checked: "scenario.rs:1638 — pure" },
  { path: "/api/scenario/log", checked: "scenario.rs:1742 — reads /data/scenario/log" },
];

// ------------------------------------------------------------------ excluded
// Never requested, and refused below even if someone adds them to ALLOWLIST:
// - /api/homemode/scan        homemode.rs:235 — `iw scan`; when 2.4G is off it
//                              uci-sets wifi0 on, commits, reloads Wi-Fi, scans,
//                              turns it back off and reloads again (≈60 s, holds the Wi-Fi lock).
// - /api/scenario/scan        scenario.rs — adds/deletes a scan interface on the phy (wifi_scan.rs).
// - /api/health?refresh=1     health.rs — forces a fresh run of every health check.
// - /api/stk/menu             telephony.rs — talks to the SIM toolkit (AT/STK session).
// - /api/at/port              at_terminal.rs — AT port probe.
// - /api/network/qos          qos.rs:18 — sends AT+CGCONTRDP / AT+CGEQOSRDP on the serial AT port.
// - /api/speedtest/servers    speedtest.rs — fetches the server list from the internet (cellular traffic).
// - /api/esim/*               esim.rs — every call runs lpac against the eUICC card.
// - /api/lan/download         lan_test.rs — 50 MB byte stream.
// - POST-served reads (POST /api/sms/list, POST /api/device/power-save): this
//   recorder is GET-only by design.
// - /api/call/status          telephony.rs:334 — AT+CLCC via at_cmd::send (fixed sleep, no mutex; first call
//                              runs detect(), writing AT to up to 5 device nodes). ~2.3 s.
// - /api/stk/menu (again)     telephony.rs:457 — AT+CUAD, AT+STIN?, AT+CUSATD=1 (USAT activation), AT+STGI; 6–20 s.
// - /api/sms/forward/config  sms_forward.rs:1079 — side-effect free (in-memory) but returns secrets
//                              (Telegram bot token, webhook URLs/headers, ntfy token). Excluded until a scrubber exists.
// - /api/sms/forward/log     sms_forward.rs:1283 — side-effect free but holds full SMS bodies, senders and codes.
// - /api/esim/job            esim.rs:317 — in-memory only, but reveals ICCIDs and is meaningless outside a job;
//                              kept out with the rest of /api/esim/* (status/profiles/notifications spawn lpac → eUICC APDUs).
// - /api/health/crashlog      health.rs:209 — read-only, but needs program/file params and returns raw logs; not needed for shape.
// - /api/at/port (detail)     at_terminal.rs:63 → at_cmd.rs:23 — uncached: writes AT\r to up to 5 serial ports (~1.3 s each).
// - /api/speedtest/servers    speedtest.rs:355 — cold cache: HTTPS to www.speedtest.net over cellular (roaming data,
//                              leaks public IP/location, primes the /start cache).
// - /api/scenario/scan (detail) scenario.rs:1731 → wifi_scan.rs:187 — off-channel `iw scan` on a live AP, or adds/deletes
//                              a scen-scan0 vdev; output holds nearby SSIDs/BSSIDs.
const DENY = [
  /^\/api\/homemode\/scan$/,
  /^\/api\/scenario\/scan$/,
  /^\/api\/health\b.*refresh/,
  /^\/api\/stk\//,
  /^\/api\/at\//,
  /^\/api\/network\/qos$/,
  /^\/api\/speedtest\/servers$/,
  /^\/api\/esim\//,
  /^\/api\/lan\/download$/,
  /^\/api\/call\//,
  /^\/api\/sms\/forward\//,
  /^\/api\/(homemode|scenario|wifi)\/scan/,
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
  die("set AGENT_URL explicitly, e.g. AGENT_URL=http://192.168.0.1:9090 (no default on purpose).");
}
const TOKEN = process.env.TOKEN ?? "";
if (!TOKEN) die("set TOKEN to a bearer token from POST /api/auth/login.");

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
