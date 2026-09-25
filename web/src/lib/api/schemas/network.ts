// Network area: signal, speed, traffic, WAN/LAN interface status, clients,
// QoS/QCI, data usage, CPU. Types only.
//
// Evidence for the firmware passthroughs (nwinfo_get_netinfo, netifd status,
// luci-rpc): the page types, web/docs/controls-inventory.md, the field mapping
// zwrt-datad uses on the same device (source/data-service/rust/src/state.rs
// ~1100-1140, docs/STATE_SCHEMA.md for nrca/lteca), and the upstream iOS app
// (mobile/ios/OpenU60/Core/Models/SignalModels.swift, DeviceModels.swift),
// which was written against a real device.

/**
 * GET /api/network/signal — handlers.rs:109 `network_signal`.
 * ubus passthrough: `zte_nwinfo_api nwinfo_get_netinfo {}`; 503 {ok:false,error}
 * on ubus failure. Shape from pages + datad mapping (state.rs:1100-1140).
 * Numbers arrive as numbers, SNRs as strings; with no service the radio
 * numbers are 0 / "" or absent (pages treat 0 as "no value").
 *
 * Pages: dashboard, signal, bandlock, device-info, router/celllock,
 * router/network-mode. The same call feeds /api/public/status `network`.
 */
export interface NetworkSignal {
  /** "SA" | "NSA" | "LTE" | "NO_SERVICE" | "LIMITED_SERVICE…" | "" (sms_forward.rs:572). */
  network_type: string;
  network_provider?: string;
  network_provider_fullname?: string;
  /** Registration state string (datad `roaming`: "Home" / "Roaming"; iOS compares "1"). */
  simcard_roam?: string; // unconfirmed
  /** Signal bars as a string, "0".."5". */
  signalbar?: string;
  /** Numbers on B27 (recorded 2026-09-25); strings in the default mock. Nothing renders them yet. */
  rmcc?: string | number;
  rmnc?: string | number;
  /** Serving band summary, e.g. "n78" in SA, LTE band in LTE mode (datad `band`). */
  wan_active_band?: string;
  /** Serving channel; the LTE EARFCN in LTE mode (datad `lte_channel`, iOS LTE PCC EARFCN). */
  wan_active_channel?: number;
  nr5g_action_band?: string;
  nr5g_action_channel?: number;
  /**
   * Bandwidth string. STATE_SCHEMA's (CN) sample is "100MHz".
   * // page expects a bare number: dashboard/signal append " MHz" → "100MHz MHz".
   */
  nr5g_bandwidth?: string; // unconfirmed
  nr5g_rsrp?: number;
  nr5g_rsrq?: number;
  nr5g_rssi?: number;
  nr5g_snr?: string;
  nr5g_pci?: number;
  nr5g_cell_id?: number;
  lte_rsrp?: number;
  lte_rsrq?: number;
  lte_rssi?: number;
  lte_snr?: string;
  lte_pci?: number;
  /**
   * LTE serving cell id (datad `lte_cell_id` ← `cell_id`). Absent in SA on B27
   * (recorded), like `lte_pci` and `wan_active_channel`: pages use
   * nr5g_pci / nr5g_cell_id then (lib/home.ts carriers / servingCellId).
   */
  cell_id?: number;
  // Device-only keys seen on B27 (not rendered):
  lte_neighbor_cell?: string;
  nr_neighbor_cell?: string;
  lteca_state?: number;
  rssi?: number;
  /**
   * NR carriers: ';'-separated, 11 ','-fields each:
   * `idx,PCI,?,band,arfcn,bw,?,rsrp,rsrq,sinr,rssi`; "" without CA. RSRP at the
   * -140 floor = configured but not scheduled. No web page reads it yet; the
   * touch screen / new-design carrier table shows the serving cell (from
   * nr5g_*) + every nrca entry + every lteca entry (touch-ui ui.c:2450).
   */
  nrca?: string;
  /** LTE carriers, same 11-field format (legacy firmwares: 5 fields `PCI,Band,Index,EARFCN,BW`). */
  lteca?: string;
  /** LTE SCC signal `rsrp,rsrq,sinr,rssi;…` (often SCC only, iOS/touch-ui). */
  ltecasig?: string; // unconfirmed
  nrcasig?: string; // unconfirmed
  /**
   * Radio-mode preference, written by nwinfo_set_netselect. B27 has reported
   * "WL_AND_5G" and, after a manual register, "TCHGWL_5G" (both automatic).
   * The firmware's full list is in zte-agent modem_ext.rs NET_SELECT_VALUES.
   * Router/network-mode reads and writes this.
   */
  net_select?: string;
  /** Operator selection, measured on B27: "auto_select" | "manual_select" (after a manual register). */
  net_select_mode?: string;
  /** NR SA band list, comma-separated numbers (datad: sa_bands / nr_sa_supported_bands). Lock or capability list. */
  nr5g_sa_band_lock?: string; // unconfirmed
  nr5g_nsa_band_lock?: string; // unconfirmed
  /**
   * LTE band list, comma-separated numbers (datad: lte_bands / lte_supported_bands).
   * // page expects the current LTE band here (bandlock "LTE band:", signal
   * // page `band` in LTE mode; inventory 未确认).
   */
  lte_band?: string; // unconfirmed
  /** // page expects (bandlock/page.tsx "Current NR band:"); not in datad's mapping, likely absent. */
  nr5g_band?: string; // unconfirmed
  /** // page expects (bandlock/page.tsx, declared, unused); likely absent. */
  nr5g_type?: string; // unconfirmed
  /** // page expects (signal, celllock: LTE EARFCN); datad reads `wan_active_channel`; likely absent. */
  lte_earfcn?: number; // unconfirmed
  /** // page expects (signal, celllock: LTE cell id); datad reads `cell_id`; likely absent. */
  lte_cell_id?: number; // unconfirmed
}

/**
 * GET /api/network/traffic — handlers.rs:117 → system.rs:244 `read_network_traffic`.
 * /proc/net/dev counters since boot; [] when unreadable. No page reads it.
 */
export interface NetInterfaceTraffic {
  name: string;
  rx_bytes: number;
  tx_bytes: number;
  rx_packets: number;
  tx_packets: number;
}
export type NetworkTraffic = NetInterfaceTraffic[];

/**
 * GET /api/network/speed — handlers.rs:123 → system.rs:283 `SpeedSnapshot`.
 * Background thread samples rmnet_data0 + rmnet_ipa0 sysfs every 1 s into a
 * 16-sample ring; this returns the latest rolling value. Always 200.
 * Dashboard reads rx_speed / tx_speed (bytes/s, shown ×8 as bit/s).
 */
export interface NetworkSpeed {
  rx_bytes: number;
  tx_bytes: number;
  /** bytes/second (f64) */
  rx_speed: number;
  tx_speed: number;
  /** Window length, ~15000 once the ring is full. */
  elapsed_ms: number;
}

/** One netifd address entry. */
export interface NetifdAddress {
  address: string;
  mask: number;
  preferred?: number;
  valid?: number;
}

export interface NetifdRoute {
  target: string;
  mask: number;
  nexthop: string;
  source?: string;
}

/**
 * GET /api/network/wan | /api/network/wan6 | /api/network/lan-status —
 * network_ext.rs:6 / :13 / :20. ubus passthrough:
 * `network.interface.<zte_wan|zte_wan6|lan> status` (standard OpenWrt netifd
 * shape, from page + OpenWrt); 503 on ubus failure. device-info reads only
 * `ipv4-address[0].address`, `route[0].nexthop`, `dns-server[0]`,
 * `ipv6-address[0].address`.
 */
export interface NetifdStatus {
  up: boolean;
  pending?: boolean;
  available?: boolean;
  autostart?: boolean;
  dynamic?: boolean;
  uptime?: number;
  l3_device?: string;
  proto?: string;
  device?: string;
  metric?: number;
  dns_metric?: number;
  delegation?: boolean;
  "ipv4-address": NetifdAddress[];
  "ipv6-address": NetifdAddress[];
  "ipv6-prefix"?: unknown[];
  "ipv6-prefix-assignment"?: unknown[];
  route: NetifdRoute[];
  "dns-server": string[];
  "dns-search"?: string[];
  neighbors?: unknown[];
  inactive?: Record<string, unknown>;
  data?: Record<string, unknown>;
}

/** luci-rpc getHostHints value per MAC (shape from iOS DeviceModels.swift:227). */
export interface HostHint {
  ipaddrs?: string[];
  ip6addrs?: string[];
  name?: string;
}

/** luci-rpc getDHCPLeases {family:4} entry. */
export interface DhcpLease {
  /**
   * Page treats it as an absolute device-clock unix time (clients/page.tsx
   * fmtLease, compared with deviceNow()). Stock LuCI reports seconds
   * remaining instead, which would make every lease look expired.
   */
  expires?: number; // unconfirmed
  hostname?: string;
  /** Casing relative to the `hosts` keys is unconfirmed (iOS uppercases both before matching). */
  macaddr?: string; // unconfirmed
  ipaddr?: string;
  duid?: string;
}

/**
 * GET /api/network/clients — network_ext.rs:27 `network_clients`. The agent
 * builds `{hosts, dhcp_leases?}` from two luci-rpc calls: `hosts` =
 * getHostHints (null when the call fails); `dhcp_leases` = getDHCPLeases
 * `.dhcp_leases`, key omitted when that call fails. Always 200.
 * // page expects hosts: Record<string, string> (clients, bypass lists)
 * // bypass) — luci-rpc returns Record<MAC, HostHint>; rendering hosts[mac]
 * // as a name would render an object.
 */
/**
 * One wireless client as the AP sees it (netinfo.rs `station_wifi_json`):
 * band from `iw dev <ap> info`, generation and link rates from `iw … station
 * dump` tx/rx bitrate. Wired clients are not in this list.
 */
export interface WifiStation {
  mac: string;
  iface: string;
  /** "2.4 GHz" | "5 GHz" | "6 GHz"; null if the AP's channel wasn't readable. */
  band: string | null;
  channel: number | null;
  width_mhz: number | null;
  /** 7 / 6 / 5 / 4 (EHT / HE / VHT / HT in the bitrate); null for legacy rates. */
  wifi_gen: number | null;
  /** Negotiated link rate, Mbit/s (AP tx = client download). */
  link_down_mbps: number | null;
  link_up_mbps: number | null;
  signal: number | null;
  connected_secs: number;
}

export interface NetworkClients {
  hosts: Record<string, HostHint> | null;
  dhcp_leases?: DhcpLease[];
  /** Added 2026-09-25; older agents don't send it. */
  wifi?: WifiStation[];
}

/** One PDP context — qos.rs:72. */
export interface QosContext {
  cid: number;
  bearer_id: number;
  apn: string;
  /** Measured via AT+CGEQOSRDP (dedicated bearers only). */
  qci: number | null;
  /** uci zwrt_data_tmp.wwaniface<cid>.qci (same as the device's About screen). */
  qci_device: number | null;
  /** 5 for IMS APNs, else 9. */
  qci_inferred: number;
  dl_gbr_kbps: number | null;
  ul_gbr_kbps: number | null;
  dl_mbr_kbps: number | null;
  ul_mbr_kbps: number | null;
}

/**
 * GET /api/network/qos — qos.rs:18 `network_qos`. The agent builds this from
 * AT+CGCONTRDP + AT+CGEQOSRDP=<cid> on the serial AT port; 503
 * "CGCONTRDP failed: …" when the port can't be opened. Page: router/qci.
 */
export interface NetworkQos {
  contexts: QosContext[];
  raw_cgcontrdp: string;
  note: string;
}

/** One period of /api/data-usage: number, raw string if not numeric, null when the uci key is absent. */
export interface DataUsagePeriod {
  tx_bytes: number | string | null;
  rx_bytes: number | string | null;
  time_secs: number | string | null;
  tx_packets: number | string | null;
  rx_packets: number | string | null;
}

/** GET /api/data-usage — handlers.rs:137 (`uci get zwrt_data_commit.wwancid1dst.<day|month|total>_*`). No page reads it. */
export interface DataUsage {
  day: DataUsagePeriod;
  month: DataUsagePeriod;
  total: DataUsagePeriod;
}

/** GET /api/cpu — handlers.rs → system.rs `CpuUsage` (percent since the previous call; the
 *  baseline is shared by every caller). Read by /tools/cpu. */
export interface CpuUsage {
  /** Online cores only, /proc/stat order. */
  cores: number[];
  overall: number;
  /** Every core sysfs knows, offline ones included (usage/freq null then). */
  per_core?: CoreInfo[];
}

export interface CoreInfo {
  id: number;
  online: boolean;
  usage: number | null;
  freq_mhz: number | null;
  max_mhz: number | null;
}

/** GET /api/memory — system.rs `MemInfo`. */
export interface MemInfo {
  total_kb: number;
  free_kb: number;
  available_kb: number;
  buffers_kb: number;
  cached_kb: number;
  used_kb: number;
  usage_pct: number;
}

/** One exit's public IP, from netinfo.rs's lookups (null fields = not known). */
export interface NetInfoExit {
  ip: string | null;
  geo: string | null;
  isp: string | null;
  /** Proxy exit only: the node it resolves to. */
  node: string | null;
  source: string | null;
  fetched_at: number;
  /** Last lookup failed; ip/geo may still be the previous good answer. */
  error: string | null;
}

export interface NetInfoOperator {
  mcc: string | null;
  mnc: string | null;
  name: string | null;
  country: string | null;
}

/**
 * GET /api/netinfo — netinfo.rs `get`. Answers from cache; asking is what
 * keeps the lookups going (at most one pass per 20 s, each exit re-looked-up
 * when it changes or every 10 min).
 */
export interface NetInfo {
  now: number;
  direct: NetInfoExit | null;
  /** null when no proxy is running. */
  proxy: NetInfoExit | null;
  home_operator: NetInfoOperator | null;
  serving_operator: NetInfoOperator | null;
  roaming: boolean | null;
  roaming_raw: string;
  network_type: string;
  data_connected: boolean;
  selection: { mode: "auto" | "manual" | null; checked_at: number };
  guard: NetInfoGuard;
  /**
   * Neighbour cells. "unsupported" since 2026-09-25: the stock scan drops
   * mobile data without redialling and returns no cells, so the agent
   * refuses it (POST /api/cell/neighbors/scan → 410) and `error` says why.
   */
  neighbors?: { state: string; error: string | null };
  /** The operator search job (netinfo.rs operator_scan). */
  scan?: {
    state: "idle" | "scanning" | "done" | "error";
    error: string | null;
    operators: { plmn: string; name: string; rat: string; status: string; country: string | null }[];
  };
}

/** GET /api/modem/register/guard — the manual-register guard. */
export interface NetInfoGuard {
  /** idle | registering | ok | reverting | reverted */
  phase: string;
  target: string;
  rat: string;
  reason: string;
  started_at: number;
  finished_at: number;
  last_result: string;
}

/** Endpoint map for record.ts. */
export interface NetworkGetMap {
  "/api/network/signal": NetworkSignal;
  "/api/network/traffic": NetworkTraffic;
  "/api/network/speed": NetworkSpeed;
  "/api/network/wan": NetifdStatus;
  "/api/network/wan6": NetifdStatus;
  "/api/network/lan-status": NetifdStatus;
  "/api/network/clients": NetworkClients;
  "/api/network/qos": NetworkQos;
  "/api/data-usage": DataUsage;
  "/api/cpu": CpuUsage;
}
