// Response schemas for the ROUTER area: DNS / DoH, LAN, firewall, VPN
// passthrough, QoS, domain filter, APN and operator (WAN) IPv6.
//
// Types only, no runtime code. Most of these endpoints are ubus passthroughs
// (zte-agent returns the firmware JSON as-is inside {ok,data}); for those the
// page's inline types and web/docs/controls-inventory.md are the only
// evidence, and every field the inventory marks 未确认 carries
// `// unconfirmed`. The iOS app (mobile/ios/OpenU60/Core/Models/*.swift)
// parses several of these responses with DIFFERENT key names; those are noted
// in the JSDoc so the real-device recorder can settle them.

// ---------------------------------------------------------------------------
// DNS

/**
 * GET /api/router/dns — router.rs:6 `router_dns_get`.
 * ubus passthrough: `zwrt_router.api router_get_dns_para`, with the firmware's
 * `wan_` key prefix stripped by the agent (e.g. `wan_dns_mode` -> `dns_mode`).
 * When `dns_mode` is "manual" and `prefer_dns_manual` is empty, the agent
 * fills `prefer_dns_manual` / `standby_dns_manual` from `uci network.wan.dns`.
 * Shape from page (router/dns) + iOS DNSParser. Note: PUT sends the
 * unprefixed keys straight to `router_set_wan_dns`; whether the firmware
 * wants the `wan_` prefix on writes is unconfirmed.
 */
export interface RouterDns {
  /** "auto" | "manual" */
  dns_mode?: string;
  prefer_dns_manual?: string;
  standby_dns_manual?: string;
  /** DNS pushed by the operator (not shown by the page). */
  prefer_dns_auto?: string; // unconfirmed
  standby_dns_auto?: string; // unconfirmed
  /** iOS reads these (from `wan_ipv6_*` after prefix strip); web page ignores them. */
  ipv6_dns_mode?: string; // unconfirmed
  ipv6_prefer_dns_manual?: string; // unconfirmed
  ipv6_standby_dns_manual?: string; // unconfirmed
  [key: string]: unknown;
}

/** PUT /api/router/dns body (page sends unprefixed keys). router.rs:43. */
export interface RouterDnsSetBody {
  dns_mode: "auto" | "manual";
  prefer_dns_manual: string;
  standby_dns_manual: string;
}

// ---------------------------------------------------------------------------
// DoH (agent's own in-memory DNS-over-HTTPS proxy; zte-agent/src/doh/)

/**
 * `config` inside GET /api/doh/status — doh/config.rs:6 `DohConfig`
 * (serialized as-is). Persisted to /data/local/tmp/doh_config.json.
 */
export interface DohConfig {
  enabled: boolean;
  /** Default "127.0.0.1:5353". */
  listen_addr: string;
  /** Default "https://1.1.1.1/dns-query". */
  upstream_url: string;
  timeout_ms: number;
  cache_enabled: boolean;
  cache_max_entries: number;
}

/**
 * GET /api/doh/status — server.rs:432 `doh_status` -> doh/mod.rs:68
 * `DohProxy::status`. Agent-built JSON (Rust shape is authoritative).
 */
export interface DohStatus {
  /** Listener thread running. iOS treats this as "enabled"; web uses config.enabled. */
  running: boolean;
  config: DohConfig;
  stats: {
    queries_total: number;
    cache_hits: number;
    cache_misses: number;
    cache_entries: number;
  };
}

/**
 * One element of GET /api/doh/cache — server.rs:473 -> doh/mod.rs:101
 * `cache_entries`. Response is a bare array of these.
 * page expects `name` (router/dns DoHCacheEntry) — agent sends `domain`, so
 * the page's name column always shows "—".
 */
export interface DohCacheEntry {
  domain: string;
  /** "A" | "AAAA" | "CNAME" | "HTTPS" | ... | "OTHER" (doh/mod.rs:144). */
  type: string;
  type_id: number;
  /** Remaining seconds. */
  ttl: number;
}

/**
 * PUT /api/doh/config body — doh/config.rs:29 `DohConfigPatch`. Only these
 * keys are honoured; anything else (e.g. `upstreams`, `enabled`) is silently
 * ignored by serde and the endpoint still returns {ok:true} with no data
 * (server.rs:436). Wrong JSON / wrong field types -> 400
 * "invalid config JSON: ...".
 */
export interface DohConfigPatchBody {
  listen_addr?: string;
  upstream_url?: string;
  timeout_ms?: number;
  cache_enabled?: boolean;
  cache_max_entries?: number;
}

/** POST /api/doh/enable -> {status:"enabled"} (server.rs:443); POST /api/doh/disable -> {status:"disabled"} (server.rs:458). */
export interface DohToggleResult {
  status: "enabled" | "disabled";
}

// ---------------------------------------------------------------------------
// LAN / DHCP

/**
 * GET /api/router/lan — router.rs:54 `router_lan_get`. Agent-built from six
 * `uci get` calls; each failure silently becomes "" (still ok:true).
 */
export interface RouterLan {
  /** uci network.lan.ipaddr */
  lan_ipaddr: string;
  /** uci network.lan.netmask */
  lan_netmask: string;
  /** "0" when uci dhcp.lan.ignore == "1", else "1" (also "1" when unreadable). */
  dhcp_enable: "0" | "1";
  /**
   * Raw uci dhcp.lan.start — a host number such as "100", NOT a full IP
   * (page placeholder suggests a full IP).
   */
  dhcp_start: string;
  /** Computed full IP: first three octets of lan_ipaddr + (start + limit - 1) (router.rs:70). */
  dhcp_end: string;
  /**
   * Raw uci dhcp.lan.leasetime. OpenWrt often stores "12h"; the page requires
   * a positive integer (seconds) and would block submit.
   */
  dhcp_lease_time: string; // unconfirmed
}

/** PUT /api/router/lan body — passed unchanged to ubus `router_set_lan_para` (router.rs:82). */
export interface RouterLanSetBody {
  lan_ipaddr: string;
  lan_netmask: string;
  dhcp_enable: "0" | "1";
  dhcp_start: string;
  dhcp_end: string;
  dhcp_lease_time: string;
}

// ---------------------------------------------------------------------------
// Firewall

/**
 * GET /api/router/firewall — router.rs:93. ubus passthrough:
 * `zwrt_router.api router_get_firewall_para`, shape from page.
 * DOUBT: the iOS FirewallParser reads the write-style names instead
 * (`firewall_switch`, `firewall_level`, `nat_switch`, `dmz_enabled`,
 * `port_forward_switch`, `wan_ping_filter`) — if the firmware uses those, the
 * web page's toggles always show off.
 */
export interface RouterFirewall {
  /** "1" = on */
  firewall_enable?: string; // unconfirmed
  /** "low" | "medium" | "high"; page defaults to "medium" when absent */
  level?: string; // unconfirmed
  nat_enable?: string; // unconfirmed
  dmz_enable?: string; // unconfirmed
  dmz_ip?: string; // unconfirmed
  /** page falls back to this when dmz_ip is absent */
  dmz_hostname?: string; // unconfirmed
  portforward_enable?: string; // unconfirmed
  /** declared by the page, not rendered */
  wan_ping_enable?: string; // unconfirmed
  /** declared by the page, not rendered */
  remote_web_access_enable?: string; // unconfirmed
  // Keys seen on B27 (recorded 2026-09-25), not rendered by the page:
  domain_filter_policy?: string;
  macipport_filter_enable?: string;
  macipport_filter_policy?: string;
  portmapping_enable?: string;
  porttrigger_enable?: string;
  [key: string]: unknown;
}

/**
 * GET /api/router/firewall/upnp — router.rs:144. ubus passthrough:
 * `zwrt_router.api router_get_upnp_switch`, shape from page.
 * B27 (recorded 2026-09-25): 503 "… router_get_upnp_switch … (Method not
 * found)". The ubus probe of the original author's firmware lists
 * `router_get_upnp` / `router_set_upnp_switch` instead, so the agent's GET
 * method name may be what is wrong. Pages treat the 503 as "unsupported"
 * (lib/api/unsupported.ts).
 */
export interface RouterUpnp {
  /** B27 router_get_upnp: "0" / "1". Also enabled, notify_interval, ttl (may be ""), enable_natpmp. */
  enable_upnp?: string;
  enabled?: string;
  notify_interval?: string;
  ttl?: string;
  enable_natpmp?: string;
  upnp_switch?: string; // unconfirmed; older assumption
  [key: string]: unknown;
}

/**
 * One port-forward rule. GET /api/router/firewall/port-forward — router.rs:162.
 * ubus passthrough: `zwrt_router.api router_get_portforward_rule`.
 * Shape from page, which reads the response as a bare array.
 * DOUBT: the firmware most likely returns an object — iOS parses
 * `{rule_list: [...]}` — in which case the web table is always empty.
 */
export interface RouterPortForwardRule {
  id: string; // unconfirmed
  name?: string; // unconfirmed
  /** "TCP" | "UDP" | "TCP+UDP" (page form options) */
  protocol?: string; // unconfirmed
  wan_port?: string; // unconfirmed
  lan_ip?: string; // unconfirmed
  lan_port?: string; // unconfirmed
  enabled?: string; // unconfirmed
}

/**
 * GET /api/router/firewall/port-forward. B27 answers `{}` when there are no
 * rules (recorded 2026-09-25); the non-empty shape is still unconfirmed, so
 * a bare array, `{rule_list}` and an object with one array-valued key are all
 * accepted (firewall/normalize.ts).
 */
export type RouterPortForwardList =
  | RouterPortForwardRule[]
  | { rule_list?: RouterPortForwardRule[]; [key: string]: unknown }
  | Record<string, never>;

/**
 * POST /api/router/firewall/port-forward body — passed to ubus
 * `router_set_portforward` (router.rs:169). Whether the firmware understands
 * this `action` format is unconfirmed.
 */
export type RouterPortForwardSetBody =
  | { action: "add"; name: string; protocol: string; wan_port: string; lan_ip: string; lan_port: string; enabled: "0" | "1" }
  | { action: "delete"; id: string };

/**
 * One MAC/IP/port filter rule. GET /api/router/firewall/filter-rules —
 * router.rs:191. ubus passthrough: `zwrt_router.api
 * router_get_macipport_filter_rule`. Shape from page (bare array).
 * DOUBT: iOS parses `{rule_list: [...]}` with `src_mac`, `src_ip`,
 * `src_port`, `dest_ip`, `dest_port`, `protocol`, `enabled` (no name/action).
 */
export interface RouterFilterRule {
  id?: string; // unconfirmed
  name?: string; // unconfirmed
  protocol?: string; // unconfirmed
  src_ip?: string; // unconfirmed
  dst_ip?: string; // unconfirmed
  dst_port?: string; // unconfirmed
  action?: string; // unconfirmed
}

/** GET /api/router/firewall/filter-rules. Same as port-forward: B27 sends `{}` when empty. */
export type RouterFilterRuleList =
  | RouterFilterRule[]
  | { rule_list?: RouterFilterRule[]; [key: string]: unknown }
  | Record<string, never>;

/**
 * Data of every router passthrough write (firewall switch/level/nat/dmz/upnp,
 * port-forward, lan, dns, vpn, qos, domain-filter, apn mode/profiles):
 * whatever the ubus set method prints. Pages never read it.
 */
export interface RouterUbusSetResult {
  result?: string; // unconfirmed
  [key: string]: unknown;
}

// ---------------------------------------------------------------------------
// VPN passthrough, QoS, domain filter

/**
 * GET /api/router/vpn — router.rs:198. ubus passthrough:
 * `zwrt_router.api router_get_alg_para`, shape from page (iOS agrees).
 * PUT sends a single `{<field>: "0"|"1"}` to `router_set_alg_switch`.
 */
export interface RouterVpn {
  /**
   * SIP ALG, "1" = on. The only key B27 returns (recorded 2026-09-25); the
   * three passthrough keys below are absent there. The page shows it
   * read-only.
   */
  alg_sip_enable?: string;
  l2tp_passthrough?: string; // unconfirmed
  pptp_passthrough?: string; // unconfirmed
  ipsec_passthrough?: string; // unconfirmed
  [key: string]: unknown;
}

/**
 * GET /api/router/qos — router.rs:216. ubus passthrough:
 * `zwrt_router.api router_get_qos_switch`, shape from page (iOS agrees).
 * B27 (recorded 2026-09-25): 503 "… router_get_qos_switch … (Method not
 * found)". The original author's ubus probe lists `router_get_qos` /
 * `router_set_qos` (no `_switch`), so the agent's method names may be wrong.
 */
export interface RouterQos {
  qos_switch?: string; // never existed on B27; the page no longer reads it
  /** zwrt_smart_mng.smart_qos.mode (router.rs): Nomal_mode = 极速, Ai/Live/Chat/Game/Video_mode. */
  smart_qos_mode?: string | null;
  /** zwrt_smart_mng.smart_mng.xdpi_support: "0" = app recognition off (on purpose, CPU). */
  xdpi_support?: string | null;
  [key: string]: unknown;
}

/**
 * GET /api/router/domain-filter — router.rs:234. ubus passthrough:
 * `zwrt_router.api router_get_domainfilter_rule`. Shape from page (its
 * comment claims "Actual shape: { action_response, blocked_domains }").
 * DOUBT: iOS parses `{enable, rule_list: [{id, domain, enabled}]}`.
 * B27 answers `{}` with nothing blocked (recorded 2026-09-25) = empty list.
 * PUT body `{action: "add"|"delete", domain}` -> `router_set_domain_filter`.
 */
export interface RouterDomainFilter {
  action_response?: Record<string, unknown>; // unconfirmed
  blocked_domains?: string[]; // unconfirmed
  [key: string]: unknown;
}

// ---------------------------------------------------------------------------
// APN

/**
 * GET /api/router/apn/mode — router.rs:252. ubus passthrough:
 * `zwrt_apn_object get_apn_mode`. 1 = manual, 0 = auto. iOS also accepts a
 * string.
 */
export interface RouterApnMode {
  apn_mode?: number; // unconfirmed (number vs string)
  [key: string]: unknown;
}

/**
 * One APN profile in `apnListArray`.
 * DOUBTS:
 * - page uses `String(cid)` as the profileId for modify/activate/delete;
 *   iOS and the Rust wan-ipv6 handler use a separate string `profileId`.
 * - B27 (recorded 2026-09-25) sends `pdpType`, `pppAuthMode`,
 *   `roamingPdpType` and `extraInt1` as integers (pdp 1=IPv4 2=IPv6
 *   3=IPv4v6, the scheme router.rs:381 `router_wan_ipv6_set` writes back;
 *   auth 0=none 1=PAP 2=CHAP). `roamingPdpType` 0 was seen too — not in
 *   that scheme, shown raw. Older mocks used strings ("IPv4v6", "none");
 *   both are accepted, and writes use the type the device list uses.
 * - iOS filters out empty pre-allocated slots (no name and no APN); the web
 *   page would render them as blank rows.
 */
export interface RouterApnProfile {
  cid?: number; // unconfirmed
  profileId?: string; // unconfirmed
  profilename?: string;
  wanapn?: string;
  username?: string;
  password?: string;
  pdpType?: string | number;
  roamingPdpType?: string | number;
  pppAuthMode?: string | number;
  /** Integer on B27 (0); meaning unknown. */
  extraInt1?: number;
  isEnable?: boolean; // unconfirmed (iOS accepts "1"/1/true)
  isValid?: number;
  [key: string]: unknown;
}

/**
 * GET /api/router/apn/profiles — router.rs:270, ubus `zwrt_apn_object
 * get_manu_apn_list`; GET /api/router/apn/auto-profiles — router.rs:299,
 * ubus `zwrt_apn_object get_auto_apn_list`. Both passthrough, shape from page.
 */
export interface RouterApnList {
  apnListArray?: RouterApnProfile[]; // unconfirmed
  [key: string]: unknown;
}

/** POST /api/router/apn/profiles (add, router.rs:277) and PUT (modify, router.rs:288; adds `profileId`). */
export interface RouterApnProfileBody {
  profileId?: string;
  profilename: string;
  wanapn: string;
  /** Number (1/2/3) when the device's list uses numbers (B27), else the old string label. */
  pdpType: string | number;
  /** Number (0/1/2) when the device's list uses numbers (B27), else the old string label. */
  pppAuthMode: string | number;
  /** Sent only with numbers: the edited profile's own value, or pdpType for a new one. */
  roamingPdpType?: number;
  username: string;
  password: string;
}

// ---------------------------------------------------------------------------
// Operator (WAN) IPv6

/**
 * GET /api/router/wan-ipv6 — router.rs:332 `router_wan_ipv6_get`. Agent-built:
 * reads the dialing APN (`zwrt_apn_object get_apn_at_cid {cid:1}`) and the
 * live PDN (`zwrt_data get_wwaniface`). 503 when the APN read fails.
 */
export interface RouterWanIpv6 {
  /** pdp_type is 2 or 3. */
  ipv6_enabled: boolean;
  /** 1 = IPv4, 2 = IPv6, 3 = IPv4v6; 0 when the field is missing. */
  pdp_type: number;
  /** Live PDN has a non-empty ipv6_address (false if that read fails). May lag the config. */
  wan_has_ipv6: boolean;
}

/**
 * PUT /api/router/wan-ipv6 {enabled: boolean} -> data (router.rs:359).
 * The live-leg step (`set_qcliiface`) failing is ignored (router.rs:400).
 */
export interface RouterWanIpv6SetResult {
  ipv6_enabled: boolean;
  /** 3 when enabled, 1 when disabled. */
  pdp_type: 1 | 3;
}

// ---------------------------------------------------------------------------

/** GET endpoint -> response data type. */
export interface RouterGetMap {
  "/api/router/dns": RouterDns;
  "/api/router/lan": RouterLan;
  "/api/router/firewall": RouterFirewall;
  "/api/router/firewall/upnp": RouterUpnp;
  "/api/router/firewall/port-forward": RouterPortForwardList;
  "/api/router/firewall/filter-rules": RouterFilterRuleList;
  "/api/router/vpn": RouterVpn;
  "/api/router/qos": RouterQos;
  "/api/router/domain-filter": RouterDomainFilter;
  "/api/router/apn/mode": RouterApnMode;
  "/api/router/apn/profiles": RouterApnList;
  "/api/router/apn/auto-profiles": RouterApnList;
  "/api/router/wan-ipv6": RouterWanIpv6;
  "/api/doh/status": DohStatus;
  "/api/doh/cache": DohCacheEntry[];
}
