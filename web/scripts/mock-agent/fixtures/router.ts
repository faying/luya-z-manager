// ROUTER area mock: DNS / DoH, LAN, firewall, VPN passthrough, QoS, domain
// filter, APN and operator (WAN) IPv6.
//
// Most real handlers (zte-agent/src/router.rs) are ubus passthroughs whose
// firmware shapes are unconfirmed; this mock returns the shape the web pages
// expect so they work, and mirrors the agent's own behaviour where the agent
// builds the JSON itself (lan, wan-ipv6, doh/*). See
// src/lib/api/schemas/router.ts for the doubts.
//
// Passthrough writes: invalid/empty JSON -> 400 "invalid JSON" (router.rs);
// a value the firmware would reject -> 503 "ubus call <obj> <method> failed:
// Command failed: Invalid argument" (mock guess at the firmware); under
// `fakesuccess` -> 200 {result:"failure"} with no state change (mock guess:
// the agent never inspects the ubus result, inventory marks these 否·透传).

import type { Ctx, Reply, Route } from "../lib.ts";
import { clone, fail, methodNotFound, ok } from "../lib.ts";
import { shared } from "../shared.ts";
import type {
  DohCacheEntry,
  DohConfig,
  DohStatus,
  RouterApnList,
  RouterApnMode,
  RouterApnProfile,
  RouterDns,
  RouterDomainFilter,
  RouterFilterRule,
  RouterFirewall,
  RouterLan,
  RouterPortForwardRule,
  RouterQos,
  RouterUpnp,
  RouterVpn,
  RouterWanIpv6,
  RouterWanIpv6SetResult,
} from "../../../src/lib/api/schemas/router.ts";

type Obj = Record<string, unknown>;

function isObj(v: unknown): v is Obj {
  return !!v && typeof v === "object" && !Array.isArray(v);
}

function str(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}

function isIPv4(s: string): boolean {
  const p = s.split(".");
  return p.length === 4 && p.every((x) => /^\d+$/.test(x) && Number(x) <= 255);
}

function is01(v: unknown): v is "0" | "1" {
  return v === "0" || v === "1";
}

/**
 * Wrap a router.rs ubus passthrough write. `apply` returns false when the
 * firmware would reject the request (mock guess), true after mutating state.
 */
function ubusSet(obj: string, method: string, apply: (body: Obj, ctx: Ctx) => boolean) {
  return (ctx: Ctx): Reply => {
    if (ctx.body === undefined) return fail("invalid JSON", 400);
    const invalid = fail(`ubus call ${obj} ${method} failed: Command failed: Invalid argument`, 503);
    if (!isObj(ctx.body)) return invalid;
    if (ctx.has("fakesuccess")) return ok({ result: "failure" });
    if (!apply(ctx.body, ctx)) return invalid;
    return ok({ result: "success" });
  };
}

// ---------------------------------------------------------------------------
// DNS (router.rs:6 / :43)

const dns: RouterDns = {
  dns_mode: "auto",
  prefer_dns_manual: "",
  standby_dns_manual: "",
  prefer_dns_auto: "168.95.1.1",
  standby_dns_auto: "168.95.192.1",
  ipv6_dns_mode: "auto",
  ipv6_prefer_dns_manual: "",
  ipv6_standby_dns_manual: "",
} satisfies RouterDns;

/** uci network.wan.dns — the agent's fallback for empty manual values. */
let uciWanDns = "";

function dnsGet(ctx: Ctx): Reply {
  const out = clone(dns);
  if (ctx.has("nosignal")) {
    out.prefer_dns_auto = "";
    out.standby_dns_auto = "";
  }
  // router.rs:18-34: manual mode with empty primary -> fill from uci.
  if (out.dns_mode === "manual" && !out.prefer_dns_manual && uciWanDns) {
    const [a, b] = uciWanDns.split(/\s+/);
    if (a) out.prefer_dns_manual = a;
    if (b) out.standby_dns_manual = b;
  }
  return ok(out);
}

const dnsSet = ubusSet("zwrt_router.api", "router_set_wan_dns", (body) => {
  // Accept both the page's unprefixed keys and firmware-style `wan_` keys.
  const b: Obj = {};
  for (const [k, v] of Object.entries(body)) b[k.startsWith("wan_") ? k.slice(4) : k] = v;
  const mode = b.dns_mode ?? dns.dns_mode;
  if (mode !== "auto" && mode !== "manual") return false;
  const p = str(b.prefer_dns_manual) ?? dns.prefer_dns_manual ?? "";
  const s = str(b.standby_dns_manual) ?? dns.standby_dns_manual ?? "";
  if (mode === "manual" && (!isIPv4(p) || (s !== "" && !isIPv4(s)))) return false;
  dns.dns_mode = mode;
  dns.prefer_dns_manual = p;
  dns.standby_dns_manual = s;
  uciWanDns = mode === "manual" ? `${p} ${s}`.trim() : "";
  return true;
});

// ---------------------------------------------------------------------------
// DoH (server.rs:432-479, doh/)

const dohConfig: DohConfig = {
  enabled: false,
  listen_addr: "127.0.0.1:5353",
  upstream_url: "https://1.1.1.1/dns-query",
  timeout_ms: 3000,
  cache_enabled: true,
  cache_max_entries: 512,
} satisfies DohConfig;

const doh = {
  running: false,
  stats: { queries_total: 0, cache_hits: 0, cache_misses: 0 },
  /** qname, qtype, expiry (ms epoch). */
  cache: [] as { domain: string; type_id: number; expires: number }[],
};

const QTYPE: Record<number, string> = {
  1: "A", 2: "NS", 5: "CNAME", 6: "SOA", 12: "PTR", 15: "MX", 16: "TXT", 28: "AAAA",
  33: "SRV", 43: "DS", 46: "RRSIG", 48: "DNSKEY", 65: "HTTPS", 255: "ANY",
};

function dohLive(now: number) {
  return doh.cache.filter((e) => e.expires > now);
}

/** Simulate LAN clients resolving through the proxy while it runs. */
function dohTick(now: number) {
  if (!doh.running || !dohConfig.cache_enabled) return;
  if (doh.cache.length === 0) {
    const seed: [string, number, number][] = [
      ["www.google.com.tw", 1, 280],
      ["www.google.com.tw", 28, 280],
      ["line.me", 1, 55],
      ["api.line.me", 1, 170],
      ["www.youtube.com", 65, 240],
      ["github.com", 1, 50],
      ["news.example.com", 5, 590],
    ];
    for (const [domain, type_id, ttl] of seed) doh.cache.push({ domain, type_id, expires: now + ttl * 1000 });
    doh.stats.cache_misses += seed.length;
    doh.stats.queries_total += seed.length;
  }
  doh.stats.cache_hits += 5;
  doh.stats.cache_misses += 1;
  doh.stats.queries_total += 6;
}

function dohStatus(ctx: Ctx): Reply {
  dohTick(ctx.now);
  const s: DohStatus = {
    running: doh.running,
    config: clone(dohConfig),
    stats: { ...doh.stats, cache_entries: doh.cache.length },
  };
  return ok(s);
}

function dohCache(ctx: Ctx): Reply {
  const list: DohCacheEntry[] = dohLive(ctx.now).map((e) => ({
    domain: e.domain, // page expects `name` — faithful to doh/mod.rs:106
    type: QTYPE[e.type_id] ?? "OTHER",
    type_id: e.type_id,
    ttl: Math.floor((e.expires - ctx.now) / 1000),
  }));
  return ok(list);
}

function serdeType(v: unknown): string {
  if (v === null) return "null";
  if (Array.isArray(v)) return "sequence";
  if (typeof v === "string") return `string ${JSON.stringify(v)}`;
  if (typeof v === "boolean") return `boolean \`${v}\``;
  if (typeof v === "number") return Number.isInteger(v) ? `integer \`${v}\`` : `floating point \`${v}\``;
  return "map";
}

/**
 * PUT /api/doh/config — faithful to DohConfigPatch (doh/config.rs:29): only
 * the five known keys are honoured, everything else (`upstreams`, `enabled`,
 * ...) is silently ignored and the reply is still {ok:true} with no data.
 */
function dohConfigSet(ctx: Ctx): Reply {
  const b = ctx.body;
  if (b === undefined) return fail("invalid config JSON: EOF while parsing a value at line 1 column 0", 400);
  if (!isObj(b)) return fail(`invalid config JSON: invalid type: ${serdeType(b)}, expected struct DohConfigPatch`, 400);
  const bad = (v: unknown, exp: string) => fail(`invalid config JSON: invalid type: ${serdeType(v)}, expected ${exp}`, 400);
  const u32 = (v: unknown) => typeof v === "number" && Number.isInteger(v) && v >= 0 && v <= 0xffffffff;
  const patch: Partial<DohConfig> = {};
  const { listen_addr, upstream_url, timeout_ms, cache_enabled, cache_max_entries } = b;
  if (listen_addr != null) { if (typeof listen_addr !== "string") return bad(listen_addr, "a string"); patch.listen_addr = listen_addr; }
  if (upstream_url != null) { if (typeof upstream_url !== "string") return bad(upstream_url, "a string"); patch.upstream_url = upstream_url.trim(); }
  if (timeout_ms != null) { if (!u32(timeout_ms)) return bad(timeout_ms, "u32"); patch.timeout_ms = timeout_ms as number; }
  if (cache_enabled != null) { if (typeof cache_enabled !== "boolean") return bad(cache_enabled, "a boolean"); patch.cache_enabled = cache_enabled; }
  if (cache_max_entries != null) { if (!u32(cache_max_entries)) return bad(cache_max_entries, "usize"); patch.cache_max_entries = cache_max_entries as number; }
  Object.assign(dohConfig, patch);
  doh.cache = dohLive(ctx.now); // cache.prune()
  return ok();
}

/** POST /api/doh/enable — body ignored. 500 "already running" when running (doh/mod.rs:42). */
function dohEnable(ctx: Ctx): Reply {
  if (doh.running) return fail("already running", 500);
  const addr = dohConfig.listen_addr;
  if (!/^(\d{1,3}(\.\d{1,3}){3}|\[[0-9a-fA-F:]+\]):\d{1,5}$/.test(addr)) {
    return fail(`bind ${addr}: invalid socket address`, 500);
  }
  doh.running = true;
  dohConfig.enabled = true;
  // fakesuccess: the dnsmasq drop-in / restart failed (results dropped,
  // server.rs:449-452) — agent says enabled but no queries ever arrive.
  if (ctx.has("fakesuccess")) doh.cache = [];
  else dohTick(ctx.now);
  return ok({ status: "enabled" });
}

/** POST /api/doh/disable — body ignored; never fails (server.rs:458). Cache and counters survive. */
function dohDisable(): Reply {
  doh.running = false;
  dohConfig.enabled = false;
  return ok({ status: "disabled" });
}

/** POST /api/doh/cache/clear — body ignored; {ok:true} no data. Counters are not reset. */
function dohCacheClear(): Reply {
  doh.cache = [];
  return ok();
}

// ---------------------------------------------------------------------------
// LAN (router.rs:54 / :82) — agent-built from uci

const lan = {
  ipaddr: "10.0.66.1",
  netmask: "255.255.255.0",
  ignore: "0",
  start: "100",
  limit: "101", // 10.0.66.100 - 10.0.66.200
  leasetime: "43200", // unconfirmed: OpenWrt often stores "12h"
};

function lanGet(ctx: Ctx): Reply {
  // `missing`: some `uci get`s fail -> "" (router.rs:55-60); dhcp_end then
  // falls back to limit=50 (router.rs:72).
  const m = ctx.has("missing");
  const start = lan.start;
  const limit = m ? "" : lan.limit;
  const s = Number.parseInt(start, 10);
  const l = Number.parseInt(limit, 10);
  const endHost = (Number.isNaN(s) ? 100 : s) + (Number.isNaN(l) ? 50 : l) - 1;
  const dot = lan.ipaddr.lastIndexOf(".");
  const out: RouterLan = {
    lan_ipaddr: lan.ipaddr,
    lan_netmask: lan.netmask,
    dhcp_enable: lan.ignore === "1" ? "0" : "1",
    dhcp_start: start,
    dhcp_end: dot >= 0 ? `${lan.ipaddr.slice(0, dot)}.${endHost}` : `192.168.0.${endHost}`,
    dhcp_lease_time: m ? "" : lan.leasetime,
  };
  return ok(out);
}

function hostPart(v: string): number {
  const last = v.includes(".") ? v.slice(v.lastIndexOf(".") + 1) : v;
  return /^\d+$/.test(last) ? Number(last) : Number.NaN;
}

const lanSet = ubusSet("zwrt_router.api", "router_set_lan_para", (b) => {
  const ip = str(b.lan_ipaddr) ?? lan.ipaddr;
  const mask = str(b.lan_netmask) ?? lan.netmask;
  if (!isIPv4(ip) || !isIPv4(mask)) return false;
  const start = b.dhcp_start !== undefined ? hostPart(String(b.dhcp_start)) : Number(lan.start);
  const end = b.dhcp_end !== undefined ? hostPart(String(b.dhcp_end)) : Number(lan.start) + Number(lan.limit) - 1;
  if (Number.isNaN(start) || Number.isNaN(end) || end < start || end > 254 || start < 1) return false;
  lan.ipaddr = ip;
  lan.netmask = mask;
  if (is01(b.dhcp_enable)) lan.ignore = b.dhcp_enable === "1" ? "0" : "1";
  lan.start = String(start);
  lan.limit = String(end - start + 1);
  if (b.dhcp_lease_time !== undefined) lan.leasetime = String(b.dhcp_lease_time);
  return true;
});

// ---------------------------------------------------------------------------
// Firewall (router.rs:93-196)

const firewall: RouterFirewall = {
  firewall_enable: "1",
  level: "medium",
  nat_enable: "1",
  dmz_enable: "0",
  dmz_ip: "",
  portforward_enable: "1",
  wan_ping_enable: "0",
  remote_web_access_enable: "0",
} satisfies RouterFirewall;

// B27 router_get_upnp shape (recorded 2026-09-25; values are the defaults).
const upnp: RouterUpnp = { enabled: "0", enable_upnp: "0", notify_interval: "60", ttl: "", enable_natpmp: "0" } satisfies RouterUpnp;

const portForward: RouterPortForwardRule[] = [
  { id: "1", name: "NAS HTTPS", protocol: "TCP", wan_port: "8443", lan_ip: "10.0.66.120", lan_port: "443", enabled: "1" },
] satisfies RouterPortForwardRule[];
let nextPfId = 2;

const filterRules: RouterFilterRule[] = [
  { id: "1", name: "Block Telnet", protocol: "TCP", src_ip: "10.0.66.150", dst_ip: "", dst_port: "23", action: "DROP" },
] satisfies RouterFilterRule[];

// firmware-b27: the lists answer `{}` when empty (recorded). Only rules added
// while the scenario is on are listed there, so it starts empty; a non-empty
// list is shown as {rule_list: [...]} — the real non-empty shape is unconfirmed.
const b27PfIds = new Set<string>();
const b27Domains: string[] = [];

function b27List<T>(rows: T[]): { rule_list: T[] } | Record<string, never> {
  return rows.length === 0 ? {} : { rule_list: rows };
}

const PROTOCOLS = ["TCP", "UDP", "TCP+UDP", "TCP/UDP", "BOTH"];
const isPort = (v: unknown) => typeof v === "string" && /^\d+(-\d+)?$/.test(v) && v.split("-").every((p) => Number(p) >= 1 && Number(p) <= 65535);

// ---------------------------------------------------------------------------
// VPN / QoS / domain filter

const vpn: RouterVpn = { alg_sip_enable: "1", l2tp_passthrough: "1", pptp_passthrough: "1", ipsec_passthrough: "1" } satisfies RouterVpn;
const qos: RouterQos = { smart_qos_mode: "Nomal_mode", xdpi_support: "0" } satisfies RouterQos;
const domainFilter: RouterDomainFilter = {
  action_response: {},
  blocked_domains: ["iot.zte.com.cn", "ztems.com"],
} satisfies RouterDomainFilter;

// ---------------------------------------------------------------------------
// APN (router.rs:252-326) + WAN IPv6 (router.rs:332 / :359)

interface ApnRec {
  cid: number;
  profileId: string;
  profilename: string;
  wanapn: string;
  username: string;
  password: string;
  /** 1 = IPv4, 2 = IPv6, 3 = IPv4v6 */
  pdp: number;
  roamingPdp: number;
  /** 0 = none, 1 = PAP, 2 = CHAP */
  auth: number;
  isEnable: boolean;
  isValid: number;
}

const PDP_NAMES = ["", "IPv4", "IPv6", "IPv4v6"];
const AUTH_NAMES = ["none", "PAP", "CHAP"];

function parsePdp(v: unknown): number | undefined {
  if (typeof v === "number" && v >= 1 && v <= 3) return v;
  if (typeof v === "string") {
    const i = PDP_NAMES.findIndex((n) => n !== "" && n.toLowerCase() === v.toLowerCase());
    if (i > 0) return i;
    if (/^[123]$/.test(v)) return Number(v);
  }
  return undefined;
}

function parseAuth(v: unknown): number | undefined {
  if (typeof v === "number" && v >= 0 && v <= 2) return v;
  if (typeof v === "string") {
    const i = AUTH_NAMES.findIndex((n) => n.toLowerCase() === v.toLowerCase());
    if (i >= 0) return i;
    if (/^[012]$/.test(v)) return Number(v);
  }
  return undefined;
}

const apnMode: RouterApnMode = { apn_mode: 0 } satisfies RouterApnMode;

/** Auto-detected profile for the roaming network (中華電信 466-92). */
const autoApns: ApnRec[] = [
  {
    cid: 1, profileId: "auto_46692_1", profilename: "中華電信", wanapn: "internet",
    username: "", password: "", pdp: 3, roamingPdp: 3, auth: 0, isEnable: true, isValid: 1,
  },
];

const manualApns: ApnRec[] = [
  {
    cid: 2, profileId: "101", profilename: "China Mobile", wanapn: "cmnet",
    username: "", password: "", pdp: 3, roamingPdp: 3, auth: 0, isEnable: false, isValid: 1,
  },
];
let nextCid = 3;
let nextProfileId = 102;

/** The profile the data call dials with (what `get_apn_at_cid {cid:1}` reports). */
function dialingApn(): ApnRec {
  if (apnMode.apn_mode === 1) {
    const m = manualApns.find((p) => p.isEnable);
    if (m) return m;
  }
  return autoApns[0];
}

function renderApn(p: ApnRec, isEnable: boolean, b27: boolean): RouterApnProfile {
  if (b27) {
    // B27 (recorded): integers for pdpType / pppAuthMode / roamingPdpType,
    // plus extraInt1, cid 0 and isValid 0. (The device also showed
    // roamingPdpType 0, outside the 1/2/3 scheme; the page shows such values raw.)
    return {
      cid: 0,
      profileId: p.profileId,
      profilename: p.profilename,
      wanapn: p.wanapn,
      username: p.username,
      password: p.password,
      pdpType: p.pdp,
      roamingPdpType: p.roamingPdp,
      pppAuthMode: p.auth,
      extraInt1: 0,
      isEnable,
      isValid: 0,
    };
  }
  // Default persona: string PDP / auth labels (older shape; still accepted).
  return {
    cid: p.cid,
    profileId: p.profileId,
    profilename: p.profilename,
    wanapn: p.wanapn,
    username: p.username,
    password: p.password,
    pdpType: PDP_NAMES[p.pdp] ?? "IPv4",
    roamingPdpType: PDP_NAMES[p.roamingPdp] ?? "IPv4",
    pppAuthMode: AUTH_NAMES[p.auth] ?? "none",
    isEnable,
    isValid: p.isValid,
  };
}

function findManual(id: unknown): ApnRec | undefined {
  if (id === undefined || id === null) return undefined;
  const s = String(id);
  return manualApns.find((p) => p.profileId === s || String(p.cid) === s);
}

function apnFields(b: Obj, target: ApnRec): boolean {
  if (b.profilename !== undefined) { const v = str(b.profilename); if (!v) return false; target.profilename = v; }
  if (b.wanapn !== undefined) { const v = str(b.wanapn); if (!v) return false; target.wanapn = v; }
  if (b.username !== undefined) target.username = String(b.username);
  if (b.password !== undefined) target.password = String(b.password);
  if (b.pdpType !== undefined) {
    const v = parsePdp(b.pdpType);
    if (v === undefined) return false;
    target.pdp = v;
    target.roamingPdp = parsePdp(b.roamingPdpType) ?? v;
  }
  if (b.pppAuthMode !== undefined) { const v = parseAuth(b.pppAuthMode); if (v === undefined) return false; target.auth = v; }
  return true;
}

const qosSet = ubusSet("zwrt_router.api", "router_set_qos_switch", (b) => {
  if (!is01(b.qos_switch)) return false;
  qos.qos_switch = b.qos_switch;
  return true;
});

function domainFilterGet(ctx: Ctx): RouterDomainFilter {
  if (ctx.has("firmware-b27")) return b27Domains.length === 0 ? {} : { blocked_domains: [...b27Domains] };
  return clone(domainFilter);
}

/** Live IPv6 leg of the PDN (set_qcliiface type 2). */
let ipv6LegUp = true;

function wanHasIpv6(ctx: Ctx): boolean {
  if (ctx.has("nosignal") || shared.airplane || !shared.mobileData) return false;
  return ipv6LegUp;
}

// ---------------------------------------------------------------------------

export const routes: Route[] = [
  // DNS
  { method: "GET", path: "/api/router/dns", handler: dnsGet },
  { method: "PUT", path: "/api/router/dns", handler: dnsSet },

  // DoH (agent-built, always complete — no generic stripping)
  { method: "GET", path: "/api/doh/status", handler: dohStatus, ownMissing: true },
  { method: "PUT", path: "/api/doh/config", handler: dohConfigSet },
  { method: "POST", path: "/api/doh/enable", handler: dohEnable },
  { method: "POST", path: "/api/doh/disable", handler: dohDisable },
  { method: "GET", path: "/api/doh/cache", handler: dohCache, ownMissing: true },
  { method: "POST", path: "/api/doh/cache/clear", handler: dohCacheClear },

  // LAN
  { method: "GET", path: "/api/router/lan", handler: lanGet, ownMissing: true },
  { method: "PUT", path: "/api/router/lan", handler: lanSet },

  // Firewall
  { method: "GET", path: "/api/router/firewall", handler: () => ok(clone(firewall)) },
  {
    method: "PUT", path: "/api/router/firewall/switch",
    handler: ubusSet("zwrt_router.api", "router_set_firewall_switch", (b) => {
      if (!is01(b.firewall_switch)) return false;
      firewall.firewall_enable = b.firewall_switch;
      return true;
    }),
  },
  {
    method: "PUT", path: "/api/router/firewall/level",
    handler: ubusSet("zwrt_router.api", "router_set_firewall_level", (b) => {
      const l = b.firewall_level;
      if (l !== "low" && l !== "medium" && l !== "high") return false;
      firewall.level = l;
      return true;
    }),
  },
  {
    method: "PUT", path: "/api/router/firewall/nat",
    handler: ubusSet("zwrt_router.api", "router_set_nat_switch", (b) => {
      if (!is01(b.nat_switch)) return false;
      firewall.nat_enable = b.nat_switch;
      return true;
    }),
  },
  {
    method: "PUT", path: "/api/router/firewall/dmz",
    handler: ubusSet("zwrt_router.api", "router_set_dmz", (b) => {
      if (!is01(b.dmz_enabled)) return false;
      const ip = str(b.dmz_ip) ?? "";
      if (b.dmz_enabled === "1" && !isIPv4(ip)) return false;
      firewall.dmz_enable = b.dmz_enabled;
      if (ip) firewall.dmz_ip = ip;
      return true;
    }),
  },
  {
    method: "GET", path: "/api/router/firewall/upnp",
    handler: (ctx) => (ctx.has("old-agent-names") ? methodNotFound("zwrt_router.api", "router_get_upnp_switch") : ok(clone(upnp))),
  },
  {
    method: "PUT", path: "/api/router/firewall/upnp",
    handler: ubusSet("zwrt_router.api", "router_set_upnp_switch", (b) => {
      // router_set_upnp_switch takes integers.
      if (b.enable_upnp !== 0 && b.enable_upnp !== 1) return false;
      upnp.enable_upnp = String(b.enable_upnp);
      upnp.enabled = String(b.enable_upnp);
      return true;
    }),
  },
  {
    method: "GET", path: "/api/router/firewall/port-forward",
    handler: (ctx) => ok(ctx.has("firmware-b27") ? b27List(clone(portForward.filter((r) => b27PfIds.has(r.id)))) : clone(portForward)),
  },
  {
    method: "POST", path: "/api/router/firewall/port-forward",
    handler: ubusSet("zwrt_router.api", "router_set_portforward", (b, ctx) => {
      if (b.action === "add") {
        const name = str(b.name);
        const lanIp = str(b.lan_ip) ?? "";
        const protocol = str(b.protocol) ?? "TCP";
        if (!name || !isIPv4(lanIp) || !isPort(b.wan_port) || !isPort(b.lan_port)) return false;
        if (!PROTOCOLS.includes(protocol.toUpperCase())) return false;
        const id = String(nextPfId++);
        if (ctx.has("firmware-b27")) b27PfIds.add(id);
        portForward.push({
          id, name, protocol, wan_port: b.wan_port as string, lan_ip: lanIp,
          lan_port: b.lan_port as string, enabled: is01(b.enabled) ? b.enabled : "1",
        });
        return true;
      }
      if (b.action === "delete") {
        const i = portForward.findIndex((r) => r.id === String(b.id));
        if (i < 0) return false;
        portForward.splice(i, 1);
        return true;
      }
      return false;
    }),
  },
  {
    method: "PUT", path: "/api/router/firewall/port-forward/switch",
    handler: ubusSet("zwrt_router.api", "router_set_portforward_switch", (b) => {
      if (!is01(b.port_forward_switch)) return false;
      firewall.portforward_enable = b.port_forward_switch;
      return true;
    }),
  },
  {
    method: "GET", path: "/api/router/firewall/filter-rules",
    handler: (ctx) => ok(ctx.has("firmware-b27") ? {} : clone(filterRules)),
  },

  // VPN passthrough
  {
    method: "GET", path: "/api/router/vpn",
    // B27 reports SIP ALG only (recorded).
    handler: (ctx) => ok(ctx.has("firmware-b27") ? { alg_sip_enable: vpn.alg_sip_enable } satisfies RouterVpn : clone(vpn)),
  },
  {
    method: "PUT", path: "/api/router/vpn",
    handler: ubusSet("zwrt_router.api", "router_set_alg_switch", (b) => {
      const keys = ["l2tp_passthrough", "pptp_passthrough", "ipsec_passthrough"] as const;
      const present = keys.filter((k) => b[k] !== undefined);
      if (present.length === 0 || !present.every((k) => is01(b[k]))) return false;
      for (const k of present) vpn[k] = b[k] as string;
      return true;
    }),
  },

  // QoS (page comment says the real device returned 503; the mock serves data so the page can be exercised)
  {
    method: "GET", path: "/api/router/qos",
    // B27 router_get_qos answers {}; the agent adds the vendor bandwidth mode and
    // xdpi from uci (owner's device 2026-09-25: Nomal_mode, xdpi_support 0).
    handler: (ctx) =>
      ctx.has("old-agent-names") ? methodNotFound("zwrt_router.api", "router_get_qos_switch") : ok(clone(qos)),
  },
  {
    method: "PUT", path: "/api/router/qos",
    handler: (ctx) =>
      ctx.has("firmware-b27") && ctx.body !== undefined ? methodNotFound("zwrt_router.api", "router_set_qos_switch") : qosSet(ctx), // setter is router_set_qos (rate limits) on B27
  },
  { method: "GET", path: "/api/router/domain-filter", handler: (ctx) => ok(domainFilterGet(ctx)) },
  {
    method: "PUT", path: "/api/router/domain-filter",
    handler: ubusSet("zwrt_router.api", "router_set_domain_filter", (b, ctx) => {
      const domain = str(b.domain)?.trim().toLowerCase();
      if (!domain || !/^[a-z0-9-]+(\.[a-z0-9-]+)+$/.test(domain)) return false;
      const list = ctx.has("firmware-b27") ? b27Domains : (domainFilter.blocked_domains ??= []);
      if (b.action === "add") {
        if (!list.includes(domain)) list.push(domain);
      } else if (b.action === "delete") {
        const i = list.indexOf(domain);
        if (i >= 0) list.splice(i, 1);
      } else {
        return false;
      }
      return true;
    }),
  },
  // APN
  { method: "GET", path: "/api/router/apn/mode", handler: () => ok(clone(apnMode)) },
  {
    method: "PUT", path: "/api/router/apn/mode",
    handler: ubusSet("zwrt_apn_object", "set_apn_mode", (b) => {
      const m = b.apn_mode;
      if (m !== 0 && m !== 1 && m !== "0" && m !== "1") return false;
      apnMode.apn_mode = Number(m);
      return true;
    }),
  },
  {
    method: "GET", path: "/api/router/apn/profiles",
    handler: (ctx) => {
      // isEnable = the selected manual profile (kept even in auto mode, so
      // activate reads back; mock guess). It only dials when apn_mode is 1.
      // apn-manual-pick: the owner's device on 2026-09-25 — auto mode, yet the
      // first manual profile carries isEnable (it is only manual mode's pick).
      const pick = ctx.has("apn-manual-pick");
      const list: RouterApnList = {
        apnListArray: manualApns.map((p, i) => renderApn(p, p.isEnable || (pick && i === 0), ctx.has("firmware-b27"))),
      };
      return ok(list);
    },
  },
  {
    method: "GET", path: "/api/router/apn/auto-profiles",
    handler: (ctx) => {
      const dial = dialingApn();
      const list: RouterApnList = { apnListArray: autoApns.map((p) => renderApn(p, p === dial, ctx.has("firmware-b27"))) };
      return ok(list);
    },
  },
  {
    method: "POST", path: "/api/router/apn/profiles",
    handler: ubusSet("zwrt_apn_object", "add_manu_apn", (b) => {
      if (!str(b.profilename) || !str(b.wanapn)) return false;
      const rec: ApnRec = {
        cid: nextCid, profileId: String(nextProfileId), profilename: "", wanapn: "",
        username: "", password: "", pdp: 1, roamingPdp: 1, auth: 0, isEnable: false, isValid: 1,
      };
      if (!apnFields(b, rec)) return false;
      nextCid++;
      nextProfileId++;
      manualApns.push(rec);
      return true;
    }),
  },
  {
    method: "PUT", path: "/api/router/apn/profiles",
    handler: ubusSet("zwrt_apn_object", "modify_manu_apn", (b) => {
      const target = findManual(b.profileId);
      if (!target) return false;
      const draft = { ...target };
      if (!apnFields(b, draft)) return false;
      Object.assign(target, draft);
      return true;
    }),
  },
  {
    method: "POST", path: "/api/router/apn/profiles/delete",
    handler: ubusSet("zwrt_apn_object", "delete_manu_apn", (b) => {
      const target = findManual(b.profileId);
      if (!target || target.isEnable) return false; // mock guess: firmware refuses the active one
      manualApns.splice(manualApns.indexOf(target), 1);
      return true;
    }),
  },
  {
    method: "POST", path: "/api/router/apn/profiles/activate",
    handler: ubusSet("zwrt_apn_object", "enable_manu_apn_id", (b) => {
      const target = findManual(b.profileId);
      if (!target) return false;
      for (const p of manualApns) p.isEnable = p === target;
      return true;
    }),
  },

  // Operator (WAN) IPv6 — agent-built
  {
    method: "GET", path: "/api/router/wan-ipv6", ownMissing: true,
    handler: (ctx) => {
      // `missing`: pdpType absent from get_apn_at_cid -> 0 (router.rs:337),
      // and get_wwaniface read fails -> false.
      const pdp = ctx.has("missing") ? 0 : dialingApn().pdp;
      const out: RouterWanIpv6 = {
        ipv6_enabled: pdp === 2 || pdp === 3,
        pdp_type: pdp,
        wan_has_ipv6: ctx.has("missing") ? false : wanHasIpv6(ctx),
      };
      return ok(out);
    },
  },
  {
    method: "PUT", path: "/api/router/wan-ipv6",
    handler: (ctx) => {
      if (ctx.body === undefined) return fail("invalid JSON", 400);
      const enabled = isObj(ctx.body) ? ctx.body.enabled : undefined;
      if (typeof enabled !== "boolean") return fail("missing 'enabled' boolean", 400);
      const pdp = enabled ? 3 : 1;
      const apn = dialingApn();
      apn.pdp = pdp;
      apn.roamingPdp = pdp;
      apn.isEnable = true;
      // router.rs:400 drops the set_qcliiface result: under fakesuccess the
      // PDP type changes but the live IPv6 leg stays as it was.
      if (!ctx.has("fakesuccess")) ipv6LegUp = enabled;
      const out: RouterWanIpv6SetResult = { ipv6_enabled: enabled, pdp_type: pdp };
      return ok(out);
    },
  },
];
