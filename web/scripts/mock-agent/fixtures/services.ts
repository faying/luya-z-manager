// Tailscale — mirrors zte-agent/src/services.rs.
//
// Device clock = local wall time labelled UTC; the mock uses a +8 h offset
// (Taipei/Beijing), so device epoch = real epoch + 28800 and ISO strings
// carry local digits with a "Z".

import type { Route, Ctx, Reply } from "../lib.ts";
import { ok, clone, unixNow } from "../lib.ts";
import { shared } from "../shared.ts";
import type {
  TailscaleStatus,
  TailscalePeer,
  ServiceLog,
} from "../../../src/lib/api/schemas/services.ts";

// ── clock helpers ───────────────────────────────────────────────────────────

const DEVICE_OFFSET = 8 * 3600;
const deviceNow = (): number => unixNow() + DEVICE_OFFSET;
/** Device-clock ISO: "2026-09-24T14:02:11Z" (local digits). */
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
    at(6, "magicsock: endpoints changed: 1.34.201.17:41641 (stun), 192.168.0.1:41641 (local)"),
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
];
