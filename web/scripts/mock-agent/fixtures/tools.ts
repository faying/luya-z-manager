// Mock fixtures: WAN speed test (speedtest.rs), AT terminal (at_terminal.rs +
// at_cmd.rs), LAN ping (lan_test.rs).
//
// Speed test: progress is computed lazily from the time since start (no
// timers), ~15 s end to end: latency 0-1.5 s, download 1.5-11.5 s, upload
// 11.5-15 s, with the same progress bands as the agent (0-20 / 20-60 / 60-100).
// The server list comes from speedtest.net on the real device (geolocated to
// the device's public IP), so here it is a Taiwan list; the hosts are made up.
// Offline (nosignal, airplane, mobile data off): the list fetch fails with 503
// unless a list is still cached (5 min), in which case start works and the test
// ends in phase "error" after the latency phase — like the agent.
//
// AT terminal: every send takes `timeout` seconds + 0.3 s (the agent reads the
// port for exactly that long). A few common commands get plausible replies;
// anything else answers OK. The reply text format (no echo, CRLFs) and the
// ZTE-specific commands are invented, not recorded.

import type { Route, Ctx } from "../lib.ts";
import { ok, fail, bodyField } from "../lib.ts";
import { shared } from "../shared.ts";
import type {
  SpeedServer,
  SpeedProgress,
  AtPort,
  AtSendResult,
} from "../../../src/lib/api/schemas/tools.ts";

// ── Speed test ──────────────────────────────────────────────────────────────

const SERVERS: SpeedServer[] = [
  { id: 18445, name: "Taipei", sponsor: "Chunghwa Telecom", country: "Taiwan", host: "speedtest-tpe1.example.net:8080", url: "http://speedtest-tpe1.example.net:8080/speedtest/upload.php" },
  { id: 27981, name: "Taipei", sponsor: "Taiwan Mobile", country: "Taiwan", host: "st-tpe.example.com.tw:8080", url: "http://st-tpe.example.com.tw:8080/speedtest/upload.php" },
  { id: 13506, name: "New Taipei", sponsor: "Far EasTone", country: "Taiwan", host: "speedtest.example-fet.tw:8080", url: "http://speedtest.example-fet.tw:8080/speedtest/upload.php" },
  { id: 40508, name: "Taoyuan", sponsor: "So-net Taiwan", country: "Taiwan", host: "sp-tyn.example.org:8080", url: "http://sp-tyn.example.org:8080/speedtest/upload.php" },
  { id: 22391, name: "Taichung", sponsor: "Chunghwa Telecom", country: "Taiwan", host: "speedtest-txg1.example.net:8080", url: "http://speedtest-txg1.example.net:8080/speedtest/upload.php" },
  { id: 36704, name: "Kaohsiung", sponsor: "Chunghwa Telecom", country: "Taiwan", host: "speedtest-khh1.example.net:8080", url: "http://speedtest-khh1.example.net:8080/speedtest/upload.php" },
] satisfies SpeedServer[];

const CACHE_TTL_MS = 300_000;
const LAT_MS = 1_500;
const DL_END_MS = 11_500;
const TOTAL_MS = 15_000;
const UPLOAD_ROUNDS = 10;
const UPLOAD_SIZE = 1_000_000;

let serversCachedAt: number | null = null;

interface RunState {
  startedAt: number;
  server: SpeedServer;
  /** Final results for this run (chosen at start from the scenario). */
  ping: number;
  jitter: number;
  dl: number;
  ul: number;
  /** Set when the network was down at start: ends in phase "error" after latency. */
  offline: boolean;
  cancelledAt: number | null;
}

let run: RunState | null = null;

function offline(ctx: Ctx): boolean {
  return ctx.has("nosignal") || shared.airplane || !shared.mobileData;
}

/** speedtest.rs:131 get_servers — cached 5 min, else fetched (fails offline). */
function getServers(ctx: Ctx): { list: SpeedServer[]; fetched: boolean } | { error: string } {
  if (serversCachedAt !== null && ctx.now - serversCachedAt < CACHE_TTL_MS) return { list: SERVERS, fetched: false };
  if (offline(ctx)) return { error: "fetch servers: io: Network is unreachable (os error 101)" };
  serversCachedAt = ctx.now;
  return { list: SERVERS, fetched: true };
}

const r2 = (v: number) => Math.round(v * 100) / 100;

function idleProgress(server = ""): SpeedProgress {
  return {
    phase: "idle",
    progress: 0,
    live_speed_mbps: 0,
    ping_ms: null,
    jitter_ms: null,
    download_mbps: null,
    upload_mbps: null,
    download_bytes: 0,
    upload_bytes: 0,
    server,
    error: null,
  };
}

/** The agent's progress struct `t` ms after start. */
function progressAt(s: RunState, t: number): SpeedProgress {
  const p = idleProgress(`${s.server.sponsor} (${s.server.name})`);
  if (t < LAT_MS) {
    p.phase = "latency";
    p.progress = Math.min(19, Math.floor((t / LAT_MS) * 20));
    return p;
  }
  if (s.offline) {
    p.phase = "error";
    p.progress = 20;
    p.error = "all ping attempts failed";
    return p;
  }
  p.ping_ms = s.ping;
  p.jitter_ms = s.jitter;
  if (t < DL_END_MS) {
    const secs = (t - LAT_MS) / 1000;
    const frac = (t - LAT_MS) / (DL_END_MS - LAT_MS);
    // TCP ramp-up, then a little wobble around the final rate.
    const live = s.dl * (1 - Math.exp(-secs / 1.2)) * (1 + 0.04 * Math.sin(secs * 3));
    p.phase = "download";
    p.progress = 20 + Math.min(40, Math.floor(frac * 40));
    p.live_speed_mbps = r2(live);
    p.download_bytes = Math.round(((s.dl * 1e6) / 8) * Math.max(0, secs - 1.2));
    return p;
  }
  const dlSecs = (DL_END_MS - LAT_MS) / 1000;
  p.download_mbps = s.dl;
  p.download_bytes = Math.round(((s.dl * 1e6) / 8) * dlSecs);
  if (t < TOTAL_MS) {
    const frac = (t - DL_END_MS) / (TOTAL_MS - DL_END_MS);
    const rounds = Math.min(UPLOAD_ROUNDS, Math.floor(frac * UPLOAD_ROUNDS));
    p.phase = "upload";
    p.progress = 60 + Math.floor((rounds * 40) / UPLOAD_ROUNDS);
    p.upload_bytes = rounds * UPLOAD_SIZE;
    p.live_speed_mbps = rounds === 0 ? 0 : r2(s.ul * (0.9 + 0.1 * Math.sin(rounds)));
    return p;
  }
  p.phase = "complete";
  p.progress = 100;
  p.upload_mbps = s.ul;
  p.upload_bytes = UPLOAD_ROUNDS * UPLOAD_SIZE;
  p.live_speed_mbps = 0;
  return p;
}

function currentProgress(now: number): SpeedProgress {
  if (!run) return idleProgress();
  if (run.cancelledAt !== null) {
    const p = progressAt(run, run.cancelledAt - run.startedAt);
    if (p.phase === "complete" || p.phase === "error") return p;
    p.phase = "cancelled";
    p.live_speed_mbps = 0;
    return p;
  }
  return progressAt(run, now - run.startedAt);
}

function isRunning(now: number): boolean {
  if (!run || run.cancelledAt !== null) return false;
  const t = now - run.startedAt;
  return run.offline ? t < LAT_MS : t < TOTAL_MS;
}

// ── AT terminal ─────────────────────────────────────────────────────────────

const AT_PORT = "/dev/at_mdm0";
let atPortDetected = false;

/** Invented identity strings (other fixtures may use different ones). */
const FW_REVISION = "BD_MU5250V1.0.0B27";
const IMEI = "350000000000017";
const IMSI = "460001234567890";

function atReply(cmdUpper: string, ctx: Ctx): string {
  const noService = ctx.has("nosignal") || shared.airplane;
  const weak = ctx.has("weak");
  const lines = (...l: string[]) => `\r\n${l.join("\r\n")}\r\n\r\nOK\r\n`;
  const c = cmdUpper.replace(/\s+/g, "");

  if (c === "AT" || c === "ATE0" || c === "ATE1") return "\r\nOK\r\n";
  if (c === "ATI" || c === "ATI0")
    return lines("Manufacturer: ZTE CORPORATION", "Model: MU5250", `Revision: ${FW_REVISION}`, "SVN: 01", `IMEI: ${IMEI}`, "+GCAP: +CGSM,+DS,+ES");
  if (c === "AT+CGMI" || c === "AT+GMI") return lines("ZTE CORPORATION");
  if (c === "AT+CGMM" || c === "AT+GMM") return lines("MU5250");
  if (c === "AT+CGMR" || c === "AT+GMR") return lines(FW_REVISION);
  if (c === "AT+CGSN" || c === "AT+GSN") return lines(IMEI);
  if (c === "AT+CIMI") return lines(IMSI);
  if (c === "AT+CPIN?") return lines("+CPIN: READY");
  if (c === "AT+CFUN?") return lines(`+CFUN: ${shared.airplane ? 4 : 1}`);
  if (c === "AT+CFUN=0" || c === "AT+CFUN=4") {
    shared.airplane = true;
    return "\r\nOK\r\n";
  }
  if (c === "AT+CFUN=1") {
    shared.airplane = false;
    return "\r\nOK\r\n";
  }
  if (c === "AT+CSQ") return lines(noService ? "+CSQ: 99,99" : weak ? "+CSQ: 6,99" : "+CSQ: 22,99");
  if (c === "AT+COPS?") return lines(noService ? "+COPS: 0" : '+COPS: 0,0,"Chunghwa Telecom",11');
  if (c === "AT+C5GREG?") return lines(noService ? "+C5GREG: 0,2" : "+C5GREG: 0,5");
  if (c === "AT+CEREG?") return lines(noService ? "+CEREG: 0,2" : "+CEREG: 0,5");
  if (c === 'AT+QENG="SERVINGCELL"') {
    if (noService) return lines('+QENG: "servingcell","SEARCH"');
    const [rsrp, rsrq, sinr] = weak ? [-115, -17, -2] : [-95, -11, 18];
    return lines(`+QENG: "servingcell","NOCONN","NR5G-SA","TDD",466,92,1A2B3C4D5,371,5A0C01,627264,78,12,${rsrp},${rsrq},${sinr},1,-`);
  }
  if (c === "AT+ZNWINFO?" || c === "AT+ZNWINFO") {
    if (noService) return lines('+ZNWINFO: "NO SERVICE"');
    const [rsrp, rsrq, sinr] = weak ? [-115, -17, -2] : [-95, -11, 18];
    return lines(`+ZNWINFO: "SA","46692","n78",627264,371,${rsrp},${rsrq},${sinr}`);
  }
  if (c === "AT+CGPADDR=1" || c === "AT+CGPADDR") return lines(noService ? '+CGPADDR: 1,"0.0.0.0"' : '+CGPADDR: 1,"10.123.45.67"');
  return "\r\nOK\r\n";
}

// ── Routes ──────────────────────────────────────────────────────────────────

export const routes: Route[] = [
  {
    // speedtest.rs:355 — outbound fetch on the device when the cache is cold.
    method: "GET",
    path: "/api/speedtest/servers",
    handler: (ctx: Ctx) => {
      const r = getServers(ctx);
      if ("error" in r) return { status: 503, error: r.error, delayMs: 300 };
      return { data: r.list.map((s) => ({ ...s })), delayMs: r.fetched ? 900 : 0 };
    },
  },
  {
    // speedtest.rs:362
    method: "POST",
    path: "/api/speedtest/start",
    handler: (ctx: Ctx) => {
      if (ctx.body === undefined) return fail("invalid JSON", 400);
      const sid = bodyField(ctx.body, "server_id");
      const r = getServers(ctx);
      if ("error" in r) return { status: 503, error: r.error, delayMs: 300 };
      let server: SpeedServer | undefined;
      if (typeof sid === "number" && Number.isInteger(sid) && sid >= 0) {
        server = r.list.find((s) => s.id === sid);
        if (!server) return fail("server not found", 404);
      } else {
        server = r.list[0];
        if (!server) return fail("no servers available", 503);
      }
      if (isRunning(ctx.now)) return fail("test already running", 409);
      const weak = ctx.has("weak");
      run = {
        startedAt: ctx.now,
        server,
        ping: weak ? 96.8 : 64.21,
        jitter: weak ? 18.4 : 3.12,
        dl: weak ? 6.83 : 182.47,
        ul: weak ? 1.92 : 41.73,
        offline: offline(ctx),
        cancelledAt: null,
      };
      return ok({ status: "started" });
    },
  },
  {
    method: "GET",
    path: "/api/speedtest/progress",
    handler: (ctx: Ctx) => ok(currentProgress(ctx.now)),
  },
  {
    // speedtest.rs:430
    method: "POST",
    path: "/api/speedtest/stop",
    handler: (ctx: Ctx) => {
      if (!run || !isRunning(ctx.now)) return ok({ status: "not_running" });
      run.cancelledAt = ctx.now;
      return ok({ status: "stopping" });
    },
  },
  {
    // at_terminal.rs:63 — first call probes the serial ports (~1.3 s each), then cached.
    method: "GET",
    path: "/api/at/port",
    handler: () => {
      const first = !atPortDetected;
      atPortDetected = true;
      return { data: { port: AT_PORT, available: true } satisfies AtPort, delayMs: first ? 1_300 : 0 };
    },
  },
  {
    // at_terminal.rs:10
    method: "POST",
    path: "/api/at/send",
    handler: (ctx: Ctx) => {
      if (ctx.body === undefined) return fail("invalid JSON", 400);
      const command = bodyField(ctx.body, "command");
      if (typeof command !== "string") return fail("missing 'command' field", 400);
      if (!command.toUpperCase().startsWith("AT")) return fail("command must start with 'AT'", 400);
      const t = bodyField(ctx.body, "timeout");
      const timeout = Math.min(30, Math.max(1, typeof t === "number" && Number.isInteger(t) && t >= 0 ? t : 3));
      const sanitized = command.replace(/['`$;|&]/g, "");
      if (sanitized === "") return fail("command is empty after sanitization", 400);
      const probe = atPortDetected ? 0 : 1_300;
      atPortDetected = true;
      const elapsed = timeout * 1000 + 300 + probe;
      const data: AtSendResult = {
        command: sanitized,
        response: atReply(sanitized.toUpperCase(), ctx),
        port: AT_PORT,
        elapsed_ms: elapsed + 12,
      };
      return { data, delayMs: elapsed };
    },
  },
  {
    // lan_test.rs:25 — `{"ok": true}` with no data.
    method: "GET",
    path: "/api/lan/ping",
    handler: () => ok(),
  },
];
