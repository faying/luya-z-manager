// Network area: signal (nwinfo_get_netinfo), speed, /proc/net/dev traffic,
// netifd WAN/WAN6/LAN status, LAN clients (luci-rpc), QoS/QCI (AT port),
// data usage (uci), CPU — handlers.rs, network_ext.rs, qos.rs.
//
// All endpoints here are reads. The nwinfo payload is built in one place
// (`buildNetinfo`) and shared with fixtures/public.ts, which derives its
// `network` block from it exactly like public.rs does. Scenario / shared
// state handled here:
//   weak      RSRP -115 / RSRQ -17 / SINR -2, 1 bar, CA dropped, slow speeds
//   nosignal  network_type NO_SERVICE, radio numbers absent, no WAN address
//   carriers8 serving cell + nrca + lteca = 8 carriers (touch-ui ui.c counting)
//   shared.airplane    looks like nosignal
//   shared.mobileData  false → signal normal, WAN down, no traffic
//   shared.bandLock    band lists + serving band (falls back to LTE when no
//                      Chunghwa NR band is allowed; unconfirmed firmware behaviour)
//   modemPrivate.netSelect / cellLock → net_select / serving PCI+ARFCN

import type { Ctx, Route } from "../lib.ts";
import { ok, clone } from "../lib.ts";
import { shared } from "../shared.ts";
import { modemPrivate, NBR_UNSUPPORTED } from "./modem.ts";
import type {
  NetworkSignal,
  NetworkTraffic,
  NetInterfaceTraffic,
  NetworkSpeed,
  NetifdStatus,
  NetworkClients,
  DhcpLease,
  HostHint,
  NetworkQos,
  QosContext,
  DataUsage,
  CpuUsage,
  MemInfo,
  WifiStation,
} from "../../../src/lib/api/schemas/network.ts";

/** Device clock = local wall time labelled UTC; Taiwan is UTC+8 (clock.rs utc_offset). */
export const DEVICE_UTC_OFFSET = 8 * 3600;

/** Device-clock unix seconds for a request. */
export function deviceNow(ctx: Ctx): number {
  return Math.floor(ctx.now / 1000) + DEVICE_UTC_OFFSET;
}

// ─── radio model ─────────────────────────────────────────────────────────────

interface Carrier {
  band: number;
  pci: number;
  arfcn: number;
  bw: number;
  /** dB offsets from the primary's RSRP / SINR. */
  dRsrp: number;
  dSinr: number;
}

/** Chunghwa Telecom NR carriers the mock can see (PCell candidates first). */
const NR_CARRIERS: Carrier[] = [
  { band: 78, pci: 318, arfcn: 633984, bw: 90, dRsrp: 0, dSinr: 0 },
  { band: 1, pci: 77, arfcn: 428000, bw: 20, dRsrp: -4, dSinr: -3 },
  { band: 28, pci: 402, arfcn: 156510, bw: 10, dRsrp: 6, dSinr: -5 },
];

/** Chunghwa Telecom LTE carriers (B3+B7+B1 normal; B8/B28 extra for carriers8). */
const LTE_CARRIERS: Carrier[] = [
  { band: 3, pci: 211, arfcn: 1650, bw: 20, dRsrp: -2, dSinr: -2 },
  { band: 7, pci: 211, arfcn: 3050, bw: 20, dRsrp: -6, dSinr: -4 },
  { band: 1, pci: 188, arfcn: 300, bw: 15, dRsrp: -5, dSinr: -3 },
  { band: 8, pci: 95, arfcn: 3650, bw: 10, dRsrp: 4, dSinr: -6 },
  { band: 28, pci: 402, arfcn: 9435, bw: 10, dRsrp: 5, dSinr: -7 },
];

const NR_CELL_ID = 8231542785;
const LTE_CELL_ID = 118447361;

/** What the modem supports when nothing is locked (comma lists as firmware reports them). */
const NR_SUPPORTED = "1,2,3,5,7,8,12,20,25,28,38,40,41,48,66,71,75,77,78,79";
const LTE_SUPPORTED = "1,2,3,4,5,7,8,12,17,18,19,20,25,26,28,32,34,38,39,40,41,42,43,48,66,71";

function parseBands(s: string | null): number[] | null {
  if (s == null) return null;
  const out = s
    .split(",")
    .map((x) => parseInt(x.replace(/^[nNbB]/, "").trim(), 10))
    .filter((n) => Number.isFinite(n));
  return out;
}

function allowed(list: number[] | null, band: number): boolean {
  return list == null || list.includes(band);
}

/** Radio is off: airplane mode or the nosignal scenario. */
export function radioDown(ctx: Ctx): boolean {
  return shared.airplane || ctx.has("nosignal");
}

/** wwan data bearer up (public.rs:50 connect_status contains "connected"). */
export function wwanConnected(ctx: Ctx): boolean {
  if (!shared.mobileData || radioDown(ctx)) return false;
  return servingRat() !== "none";
}

/** Which RAT the band lock leaves us on. */
function servingRat(): "nr" | "lte" | "none" {
  const nr = parseBands(shared.bandLock.nr);
  const lte = parseBands(shared.bandLock.lte);
  if (NR_CARRIERS.some((c) => allowed(nr, c.band))) return "nr";
  if (LTE_CARRIERS.some((c) => allowed(lte, c.band))) return "lte";
  return "none";
}

/** ';'-joined 11-field CA descriptor: idx,PCI,?,band,arfcn,bw,?,rsrp,rsrq,sinr,rssi */
function caString(list: Carrier[], rsrp: number, rsrq: number, sinr: number, startIdx: number): string {
  return list
    .map((c, i) => {
      const r = rsrp + c.dRsrp;
      const s = sinr + c.dSinr;
      const rssi = r + 27;
      return `${startIdx + i},${c.pci},1,${c.band},${c.arfcn},${c.bw},0,${r.toFixed(1)},${(rsrq - 1).toFixed(1)},${s.toFixed(1)},${rssi.toFixed(1)}`;
    })
    .map((x) => x + ";")
    .join("");
}

function caSig(list: Carrier[], rsrp: number, rsrq: number, sinr: number): string {
  return list
    .map((c) => `${(rsrp + c.dRsrp).toFixed(1)},${(rsrq - 1).toFixed(1)},${(sinr + c.dSinr).toFixed(1)},${(rsrp + c.dRsrp + 27).toFixed(1)};`)
    .join("");
}

/**
 * The `zte_nwinfo_api nwinfo_get_netinfo` payload for this request. Shared
 * with fixtures/public.ts so both endpoints agree.
 */
export function buildNetinfo(ctx: Ctx): NetworkSignal {
  const t = ctx.now / 1000;
  const nrLock = parseBands(shared.bandLock.nr);
  const lteLock = parseBands(shared.bandLock.lte);

  const common = {
    net_select: modemPrivate.netSelect,
    // Automatic vs manual operator selection (measured on B27), not the radio mode.
    net_select_mode: modemPrivate.registeredPlmn === "46692" ? "auto_select" : "manual_select",
    nr5g_sa_band_lock: shared.bandLock.nr ?? NR_SUPPORTED,
    nr5g_nsa_band_lock: shared.bandLock.nr ?? NR_SUPPORTED,
    lte_band: shared.bandLock.lte ?? LTE_SUPPORTED,
  };

  const rat = servingRat();
  if (radioDown(ctx) || rat === "none") {
    // No service: type NO_SERVICE (sms_forward.rs:572), operator empty, radio
    // numbers absent.
    return {
      network_type: "NO_SERVICE",
      network_provider: "",
      network_provider_fullname: "",
      simcard_roam: "",
      signalbar: "0",
      nrca: "",
      lteca: "",
      ltecasig: "",
      ...common,
    };
  }

  const weak = ctx.has("weak");
  const many = ctx.has("carriers8");
  // Small jitter so the signal page's sparkline moves.
  const jitter = Math.round(2 * Math.sin(t / 7) + Math.sin(t / 2.3));
  const rsrp = (weak ? -115 : -95) + jitter;
  const rsrq = (weak ? -17 : -11) + Math.round(Math.sin(t / 11));
  const sinr = (weak ? -2 : 18) + Math.round(1.5 * Math.sin(t / 5)) + (weak ? 0 : 0.3);
  const bar = weak ? "1" : rsrp >= -90 ? "5" : "4";

  const base = {
    network_provider: "Chunghwa Telecom",
    network_provider_fullname: "中華電信",
    simcard_roam: "Roaming",
    signalbar: bar,
    rmcc: "466",
    rmnc: "92",
    rssi: 0,
    ...common,
  };

  const nrAllowed = NR_CARRIERS.filter((c) => allowed(nrLock, c.band));
  const lteAllowed = LTE_CARRIERS.filter((c) => allowed(lteLock, c.band));

  if (rat === "nr") {
    const pcell = { ...nrAllowed[0] };
    const lock = modemPrivate.cellLock.nr;
    if (lock) {
      const pci = parseInt(lock.pci, 10);
      const arfcn = parseInt(lock.earfcn, 10);
      if (Number.isFinite(pci)) pcell.pci = pci;
      if (Number.isFinite(arfcn)) pcell.arfcn = arfcn;
    }
    // CA: carriers8 → serving + 2 NR SCC + 5 LTE = 8; normal → serving + 1 NR
    // SCC + LTE B3/B7/B1 = 5; weak → serving only.
    const nrScc = many ? nrAllowed.slice(1, 3) : weak ? [] : nrAllowed.slice(1, 2);
    const lteList = many ? lteAllowed.slice(0, 5) : weak ? [] : lteAllowed.slice(0, 3);
    const sinrS = sinr.toFixed(1);
    return {
      network_type: "SA",
      ...base,
      wan_active_band: `n${pcell.band}`,
      wan_active_channel: pcell.arfcn,
      nr5g_action_band: `n${pcell.band}`,
      nr5g_action_channel: pcell.arfcn,
      nr5g_bandwidth: `${pcell.bw}MHz`,
      nr5g_rsrp: rsrp,
      nr5g_rsrq: rsrq,
      nr5g_rssi: rsrp + 27,
      nr5g_snr: sinrS,
      nr5g_pci: pcell.pci,
      nr5g_cell_id: NR_CELL_ID,
      lte_rsrp: 0,
      lte_rsrq: 0,
      lte_rssi: 0,
      lte_snr: "",
      lte_pci: 0,
      cell_id: 0,
      nrca: caString(nrScc, rsrp, rsrq, sinr, 1),
      lteca: caString(lteList, rsrp, rsrq, sinr, 0),
      ltecasig: caSig(lteList.slice(1), rsrp, rsrq, sinr),
    };
  }

  // LTE fallback (NR locked away from every Chunghwa NR band).
  const pcell = { ...lteAllowed[0] };
  const lock = modemPrivate.cellLock.lte;
  if (lock) {
    const pci = parseInt(lock.pci, 10);
    const arfcn = parseInt(lock.earfcn, 10);
    if (Number.isFinite(pci)) pcell.pci = pci;
    if (Number.isFinite(arfcn)) pcell.arfcn = arfcn;
  }
  const scc = many ? lteAllowed.slice(1, 8) : weak ? [] : lteAllowed.slice(1, 3);
  const lteList = [pcell, ...scc];
  return {
    network_type: "LTE",
    ...base,
    wan_active_band: `B${pcell.band}`,
    wan_active_channel: pcell.arfcn,
    nr5g_action_band: "",
    nr5g_action_channel: 0,
    nr5g_bandwidth: "",
    nr5g_rsrp: 0,
    nr5g_rsrq: 0,
    nr5g_rssi: 0,
    nr5g_snr: "",
    nr5g_pci: 0,
    nr5g_cell_id: 0,
    lte_rsrp: rsrp,
    lte_rsrq: rsrq,
    lte_rssi: rsrp + 27,
    lte_snr: sinr.toFixed(1),
    lte_pci: pcell.pci,
    cell_id: LTE_CELL_ID,
    nrca: "",
    lteca: caString(lteList, rsrp, rsrq, sinr, 0),
    ltecasig: caSig(scc, rsrp, rsrq, sinr),
  };
}

// ─── traffic model ───────────────────────────────────────────────────────────

/** Instantaneous WAN rates (bytes/s) at time t; smooth sines so charts move. */
function rates(ctx: Ctx, t: number): { rx: number; tx: number } {
  if (!wwanConnected(ctx)) return { rx: 0, tx: 0 };
  const k = ctx.has("weak") ? 0.08 : 1;
  const rx = 2_400_000 + 1_500_000 * Math.sin(t / 20) + 600_000 * Math.sin(t / 3.7) + 250_000 * Math.sin(t / 1.3);
  const tx = 320_000 + 180_000 * Math.sin(t / 17 + 1) + 80_000 * Math.sin(t / 2.9);
  return { rx: Math.max(0, rx * k), tx: Math.max(0, tx * k) };
}

/** Cumulative rmnet counters since "boot", advanced on every read. */
const counters = {
  at: Date.now() / 1000,
  rx: 18_734_220_113,
  tx: 1_902_448_871,
};

function advance(ctx: Ctx): void {
  const now = ctx.now / 1000;
  const dt = now - counters.at;
  if (dt <= 0) return;
  const r = rates(ctx, now - dt / 2);
  counters.rx += Math.round(r.rx * dt);
  counters.tx += Math.round(r.tx * dt);
  counters.at = now;
}

/** Bytes counted at mock start, for data-usage deltas. */
const startRx = counters.rx;
const startTx = counters.tx;

function iface(name: string, rx: number, tx: number, mtu = 1400): NetInterfaceTraffic {
  return {
    name,
    rx_bytes: Math.round(rx),
    tx_bytes: Math.round(tx),
    rx_packets: Math.round(rx / mtu),
    tx_packets: Math.round(tx / mtu),
  };
}

function traffic(ctx: Ctx): NetworkTraffic {
  advance(ctx);
  const { rx, tx } = counters;
  const list: NetworkTraffic = [
    iface("lo", 48_211_904, 48_211_904, 300),
    iface("rmnet_ipa0", rx * 0.021, tx * 0.018),
    iface("rmnet_data0", rx, tx),
    // Downloads leave the LAN side as tx.
    iface("br-lan", tx * 0.96, rx * 0.97),
    iface("eth0", 0, 0),
  ];
  if (shared.wifiOn) {
    list.push(iface("wlan0", tx * 0.2, rx * 0.19), iface("wlan1", tx * 0.76, rx * 0.78));
  }
  if (shared.tailscale.running) list.push(iface("tailscale0", 212_553_004, 96_410_225, 1280));
  return list;
}

function speed(ctx: Ctx): NetworkSpeed {
  advance(ctx);
  const now = ctx.now / 1000;
  const r = rates(ctx, now);
  const ipaRx = counters.rx * 0.021;
  const ipaTx = counters.tx * 0.018;
  return {
    rx_bytes: Math.round(counters.rx + ipaRx),
    tx_bytes: Math.round(counters.tx + ipaTx),
    rx_speed: Math.round(r.rx * 1.021 * 1000) / 1000,
    tx_speed: Math.round(r.tx * 1.018 * 1000) / 1000,
    elapsed_ms: 15000 + Math.round(3 * Math.sin(now)),
  };
}

// ─── netifd interfaces ───────────────────────────────────────────────────────

/** WAN came up this long before the mock started (seconds). */
const WAN_UP_SINCE = Date.now() / 1000 - 11_520;

function downIface(proto: string): NetifdStatus {
  return {
    up: false,
    pending: false,
    available: true,
    autostart: true,
    dynamic: false,
    proto,
    device: "rmnet_data0",
    metric: 0,
    dns_metric: 0,
    delegation: true,
    "ipv4-address": [],
    "ipv6-address": [],
    "ipv6-prefix": [],
    "ipv6-prefix-assignment": [],
    route: [],
    "dns-server": [],
    "dns-search": [],
    neighbors: [],
    inactive: { "ipv4-address": [], "ipv6-address": [], route: [], "dns-server": [], "dns-search": [], neighbors: [] },
    data: {},
  };
}

function wan(ctx: Ctx): NetifdStatus {
  if (!wwanConnected(ctx)) return downIface("static");
  return {
    ...downIface("static"),
    up: true,
    uptime: Math.floor(ctx.now / 1000 - WAN_UP_SINCE),
    l3_device: "rmnet_data0",
    "ipv4-address": [{ address: "10.94.137.21", mask: 30 }],
    route: [{ target: "0.0.0.0", mask: 0, nexthop: "10.94.137.22", source: "0.0.0.0/0" }],
    "dns-server": ["198.51.100.10", "198.51.100.11"],
  };
}

function wan6(ctx: Ctx): NetifdStatus {
  // Cellular IPv6 stays on (only LAN / proxy IPv6 are off on this device).
  if (!wwanConnected(ctx)) return downIface("dhcpv6");
  return {
    ...downIface("dhcpv6"),
    up: true,
    dynamic: true,
    uptime: Math.floor(ctx.now / 1000 - WAN_UP_SINCE),
    l3_device: "rmnet_data0",
    "ipv6-address": [{ address: "2001:db8:4f2a:1c07:a1b2:33ff:fe4d:9e10", mask: 64, preferred: 604800, valid: 1209600 }],
    route: [{ target: "::", mask: 0, nexthop: "fe80::5c3a:1ff:fe20:1", source: "2001:db8:4f2a:1c07::/64" }],
    "dns-server": ["2001:db8:100::53", "2001:db8:100::54"],
  };
}

const LAN = {
  up: true,
  pending: false,
  available: true,
  autostart: true,
  dynamic: false,
  uptime: 0,
  l3_device: "br-lan",
  proto: "static",
  device: "br-lan",
  metric: 0,
  dns_metric: 0,
  delegation: true,
  "ipv4-address": [{ address: "10.0.66.1", mask: 24 }],
  // LAN IPv6 is off (u60-guard keeps br-lan disable_ipv6=1).
  "ipv6-address": [],
  "ipv6-prefix": [],
  "ipv6-prefix-assignment": [],
  route: [],
  "dns-server": [],
  "dns-search": [],
  neighbors: [],
  inactive: { "ipv4-address": [], "ipv6-address": [], route: [], "dns-server": [], "dns-search": [], neighbors: [] },
  data: {},
} satisfies NetifdStatus;

/** LAN has been up since "boot" (a bit before the WAN). */
const LAN_UP_SINCE = WAN_UP_SINCE - 74;

// ─── clients ─────────────────────────────────────────────────────────────────

interface MockClient {
  mac: string;
  ip: string;
  hostname: string;
  /** Seconds of lease left when the mock started (negative = already expired). */
  left: number;
}

/**
 * Four active leases + one expired one; MACs in the made-up 02:00:00:00:00:xx
 * range (tools/privacy-scan.sh). MAC casing is the same for lease
 * macaddr and the hint keys (unconfirmed against the firmware).
 */
const CLIENTS: MockClient[] = [
  { mac: "02:00:00:00:00:11", ip: "10.0.66.101", hostname: "iPhone", left: 38_210 },
  { mac: "02:00:00:00:00:12", ip: "10.0.66.117", hostname: "MacBook-Air", left: 21_455 },
  { mac: "02:00:00:00:00:13", ip: "10.0.66.123", hostname: "iPad", left: 9_140 },
  { mac: "02:00:00:00:00:14", ip: "10.0.66.142", hostname: "Xiaomi-14", left: 41_030 },
  { mac: "02:00:00:00:00:15", ip: "10.0.66.155", hostname: "Nintendo-Switch", left: -3_600 },
];
const CLIENTS_T0 = Date.now() / 1000;

function clients(): NetworkClients {
  const devNowAtStart = Math.floor(CLIENTS_T0) + DEVICE_UTC_OFFSET;
  const leases: DhcpLease[] = CLIENTS.map((c) => ({
    expires: devNowAtStart + c.left,
    hostname: c.hostname,
    macaddr: c.mac,
    ipaddr: c.ip,
  }));
  const hosts: Record<string, HostHint> = {};
  for (const c of CLIENTS) {
    hosts[c.mac] = { ipaddrs: [c.ip], ip6addrs: [], name: c.hostname };
  }
  // Band / generation as the owner's B27 reports them (iw, 2026-09-25): the
  // first two on 5 GHz, one on 2.4 GHz, the Switch not on Wi-Fi.
  const wifi: WifiStation[] = [
    { mac: CLIENTS[0].mac, iface: "wlan2", band: "5 GHz", channel: 44, width_mhz: 160, wifi_gen: 6, link_down_mbps: 2402, link_up_mbps: 1201, signal: -39, connected_secs: 1260 },
    { mac: CLIENTS[1].mac, iface: "wlan2", band: "5 GHz", channel: 44, width_mhz: 160, wifi_gen: 6, link_down_mbps: 1201, link_up_mbps: 864, signal: -58, connected_secs: 3400 },
    { mac: CLIENTS[3].mac, iface: "wlan0", band: "2.4 GHz", channel: 11, width_mhz: 40, wifi_gen: 4, link_down_mbps: 144, link_up_mbps: 72, signal: -71, connected_secs: 800 },
  ];
  return { hosts, dhcp_leases: leases, wifi };
}

// ─── QoS / QCI ───────────────────────────────────────────────────────────────

const QOS_NOTE =
  "QCI is read from the modem (qci_device, same value as the device's About screen) when available; dedicated bearers may also report a measured 5QI via AT; otherwise it is inferred from the APN.";

function qos(ctx: Ctx): NetworkQos {
  const lines: string[] = ["AT+CGCONTRDP"];
  const contexts: QosContext[] = [];
  if (!radioDown(ctx) && servingRat() !== "none") {
    if (wwanConnected(ctx)) {
      lines.push('+CGCONTRDP: 1,5,"cmnet.mnc000.mcc460.gprs","10.94.137.21.255.255.255.252","10.94.137.22","198.51.100.10","198.51.100.11"');
      contexts.push({
        cid: 1,
        bearer_id: 5,
        apn: "cmnet.mnc000.mcc460.gprs",
        qci: null,
        qci_device: 6,
        qci_inferred: 9,
        dl_gbr_kbps: null,
        ul_gbr_kbps: null,
        dl_mbr_kbps: null,
        ul_mbr_kbps: null,
      });
    }
    // IMS stays registered even with mobile data off.
    lines.push('+CGCONTRDP: 2,6,"ims","32.1.13.184.0.100.0.0.0.0.0.0.0.0.0.1.255.255.255.255.255.255.255.255.0.0.0.0.0.0.0.0"');
    contexts.push({
      cid: 2,
      bearer_id: 6,
      apn: "ims",
      qci: null,
      qci_device: 5,
      qci_inferred: 5,
      dl_gbr_kbps: null,
      ul_gbr_kbps: null,
      dl_mbr_kbps: null,
      ul_mbr_kbps: null,
    });
  }
  lines.push("", "OK");
  return { contexts, raw_cgcontrdp: lines.join("\r\n"), note: QOS_NOTE };
}

// ─── data usage / CPU ────────────────────────────────────────────────────────

function dataUsage(ctx: Ctx): DataUsage {
  advance(ctx);
  const dRx = counters.rx - startRx;
  const dTx = counters.tx - startTx;
  const since = Math.floor(ctx.now / 1000 - CLIENTS_T0);
  const period = (rx: number, tx: number, secs: number) => ({
    tx_bytes: tx + dTx,
    rx_bytes: rx + dRx,
    time_secs: secs + since,
    tx_packets: Math.round((tx + dTx) / 1400),
    rx_packets: Math.round((rx + dRx) / 1400),
  });
  return {
    day: period(1_284_551_906, 152_330_417, 40_311),
    month: period(38_902_117_554, 3_411_093_280, 1_802_966),
    total: period(412_663_091_845, 36_004_517_730, 19_440_872),
  };
}

function cpu(ctx: Ctx): CpuUsage {
  const t = ctx.now / 1000;
  const load = wwanConnected(ctx) ? 1 : 0.6;
  const cores = [16, 9, 12, 7].map((b, i) => {
    const v = (b + 6 * Math.sin(t / (3 + i) + i) + 3 * Math.sin(t / 1.7 + i * 2)) * load;
    return Math.round(Math.max(0.5, Math.min(100, v)) * 10) / 10;
  });
  const overall = Math.round((cores.reduce((a, b) => a + b, 0) / cores.length) * 10) / 10;
  // Core 3 offline under "missing": /proc/stat drops it, sysfs still lists it.
  const offline = ctx.has("missing") ? 3 : -1;
  const per_core = cores.map((u, id) => ({
    id,
    online: id !== offline,
    usage: id === offline ? null : u,
    freq_mhz: id === offline ? null : id === 2 ? 825 : 1516,
    max_mhz: 2208,
  }));
  return { cores: cores.filter((_, i) => i !== offline), overall, per_core };
}

/** system.rs read_meminfo (/proc/meminfo). */
function memory(): MemInfo {
  return { total_kb: 1_852_000, free_kb: 402_000, available_kb: 1_010_000, buffers_kb: 12_000, cached_kb: 560_000, used_kb: 842_000, usage_pct: 45.5 };
}

/** netinfo.rs `get`: a mainland SIM roaming in Taiwan, proxy exit in Japan. Documentation-range IPs. */
function netinfo(ctx: Ctx) {
  const now = Math.floor(ctx.now / 1000);
  return {
    now,
    direct: { ip: "203.0.113.24", geo: "中国台湾 台北市", isp: "中华电信", node: null, source: "ip-api.com", fetched_at: now - 240, error: null },
    proxy: null,
    home_operator: { mcc: "460", mnc: "01", name: "中国联通", country: "中国" },
    serving_operator: { mcc: "466", mnc: "92", name: "中华电信", country: "中国台湾" },
    roaming: true,
    roaming_raw: "Roaming",
    network_type: "SA",
    data_connected: true,
    selection: { mode: "auto", checked_at: now - 60 },
    guard: modemPrivate.guardNow(ctx.now),
    neighbors: { state: "unsupported", scanned_at: 0, error: NBR_UNSUPPORTED, cells: [] },
    scan: modemPrivate.netinfoScanNow(ctx),
    clients: null,
  };
}

// ─── routes ──────────────────────────────────────────────────────────────────

/**
 * firmware-b27 (recorded 2026-09-25, SA): rmcc / rmnc are numbers; in SA the
 * LTE fields lte_pci / cell_id and wan_active_channel are absent (only
 * nr5g_pci / nr5g_cell_id); plus a few device-only strings, empty here.
 */
function b27Signal(sig: NetworkSignal): NetworkSignal {
  const out: NetworkSignal = { ...sig };
  if (typeof out.rmcc === "string") out.rmcc = Number(out.rmcc);
  if (typeof out.rmnc === "string") out.rmnc = Number(out.rmnc);
  if (out.network_type === "SA") {
    delete out.lte_pci;
    delete out.cell_id;
    delete out.wan_active_channel;
  }
  return { ...out, lte_neighbor_cell: "", nr_neighbor_cell: "", lteca_state: 0 };
}

export const routes: Route[] = [
  { method: "GET", path: "/api/network/signal", handler: (ctx) => ok(ctx.has("firmware-b27") ? b27Signal(buildNetinfo(ctx)) : buildNetinfo(ctx)) },
  { method: "GET", path: "/api/network/traffic", handler: (ctx) => ok(traffic(ctx)) },
  { method: "GET", path: "/api/network/speed", handler: (ctx) => ok(speed(ctx)) },
  { method: "GET", path: "/api/network/wan", handler: (ctx) => ok(wan(ctx)) },
  { method: "GET", path: "/api/network/wan6", handler: (ctx) => ok(wan6(ctx)) },
  {
    method: "GET",
    path: "/api/network/lan-status",
    handler: (ctx) => ok({ ...clone(LAN), uptime: Math.floor(ctx.now / 1000 - LAN_UP_SINCE) } satisfies NetifdStatus),
  },
  { method: "GET", path: "/api/network/clients", handler: () => ok(clients()) },
  // Real handler: ≥ 5.3 s + 3.3 s per PDP context on the AT port (qos.rs:19,52).
  // A short delay keeps the loading state visible without tripping the 9 s timeout.
  { method: "GET", path: "/api/network/qos", handler: (ctx) => ({ data: qos(ctx), delayMs: 1200 }) },
  { method: "GET", path: "/api/data-usage", handler: (ctx) => ok(dataUsage(ctx)) },
  // "missing" = core 3 offline (cpu() handles it), not stripped fields.
  { method: "GET", path: "/api/cpu", handler: (ctx) => ok(cpu(ctx)), ownMissing: true },
  { method: "GET", path: "/api/memory", handler: () => ok(memory()) },
  { method: "GET", path: "/api/netinfo", handler: (ctx) => ok(netinfo(ctx)) },
];
