// Mock fixtures: WAN speed test (the core's /api/speedtest/*, crate luyaz-speedtest)
// and AT terminal (/api/at/*, luyaz-core api/device_admin/phone.rs).
//
// Speed test: progress is computed lazily from the time since start (no
// timers), ~15 s end to end: latency 0-1.5 s, download 1.5-11.5 s, upload
// 11.5-15 s, with luyaz-speedtest's progress bands (0-20 / 20-60 / 60-100).
// It answers as the v2 engine (core 1.48.0, method_version 2): download is
// single 1.5-4.5 s then multi 4.5-11.5 s (with path "chill" the last 2.5 s are
// the CHILL pair), stage/step/steps, the v2 result fields and a verdict. While
// roaming (netinfo roaming, i.e. unless scenario home-network) the core runs
// coarse: download only, 5 s, no verdict. Scenario speed-v1 answers as an old
// core (no v2 fields, the old four numbers); speed-short ends sample_short.
// The server list comes from speedtest.net on the real device (geolocated to
// the device's public IP), so here it is a Taiwan list; the hosts are made up.
// Offline (nosignal, airplane, mobile data off): the list fetch fails with 503
// unless a list is still cached (5 min), in which case start works and the test
// ends in phase "error" after the latency phase, as on the device.
//
// AT terminal: every send takes `timeout` seconds + 0.3 s (the core reads the
// port for exactly that long). A few common commands get plausible replies;
// anything else answers OK. The reply text format (no echo, CRLFs) and the
// ZTE-specific commands are invented, not recorded.

import type { Route, Ctx } from "../lib.ts";
import { ok, fail, bodyField } from "../lib.ts";
import { shared } from "../shared.ts";
import { diagRefusal } from "./diagnose.ts";
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
  /** old core: no v2 fields */
  v1: boolean;
  /** roaming: coarse, download only */
  coarse: boolean;
  chill: boolean;
  short: boolean;
}

const COARSE_MS = 5_000;
const SINGLE_END_MS = 4_500;
const CHILL_FROM_MS = 9_000;
const MB = 1_000_000;

/** The mock's v2 fields (luyaz-speedtest SpeedTestProgress after `server`). */
function v2Fields(s: RunState | null): Record<string, unknown> {
  return {
    method_version: 2, mode: s?.coarse ? "coarse" : "full", roaming_capped: !!s?.coarse, path: s?.chill ? "chill" : "direct",
    stage: null, step: 0, steps: s?.coarse ? 1 : s?.chill ? 6 : 5, connections: 6,
    download_whole_mbps: null, single_mbps: null, single_whole_mbps: null, upload_whole_mbps: null,
    chill_mbps: null, chill_whole_mbps: null, chill_region: null, idle_ms: null,
    loaded_down_ms: null, loaded_up_ms: null, loaded_chill_ms: null, sample_short: false,
    server_name: s?.server.name ?? "", server_sponsor: s?.server.sponsor ?? "", net_tune: s ? "off" : null,
    radio: s ? { sinr_db: 14, band: "n78", ca: 2 } : null, cpu_peak_pct: null, mihomo_cpu_pct: null, verdict: null,
  };
}

/** luyaz-speedtest verdict.rs `judge` for the mock's numbers. */
function verdictOf(s: RunState): Record<string, unknown> {
  const single = r2(s.dl / 3.1);
  const ratio = (s.dl / single).toFixed(1);
  const loaded = s.ping + 150;
  const layers = [
    { key: "single", state: "warn", value_zh: `${single.toFixed(0)} Mbps，多连接的 1/${ratio}`, value_en: `${single.toFixed(0)} Mbps, 1/${ratio} of multi` },
    s.chill
      ? { key: "proxy", state: "ok", value_zh: `经 CHILL ${(s.dl * 0.82).toFixed(0)} Mbps，直连的 82%`, value_en: `${(s.dl * 0.82).toFixed(0)} Mbps via CHILL, 82% of direct` }
      : { key: "proxy", state: "untested", value_zh: "没开「经 CHILL」", value_en: "\"Via CHILL\" off", reason: "not_requested" },
    { key: "queue", state: "warn", value_zh: `下载时延迟 +${(loaded - s.ping).toFixed(0)} ms`, value_en: `Latency +${(loaded - s.ping).toFixed(0)} ms downloading` },
    { key: "radio", state: "ok", value_zh: "SINR 14 dB · n78 · 2CA（只供参考）", value_en: "SINR 14 dB · n78 · 2CA (reference only)" },
    { key: "cell_wifi", state: "untested", value_zh: "没测", value_en: "Not tested", reason: "phase_two" },
  ];
  return { bottlenecks: ["single", "queue"], layers, summary_zh: "慢在单连接、下载时卡", summary_en: "Slow at single connection, lag under load" };
}

let run: RunState | null = null;

function offline(ctx: Ctx): boolean {
  return ctx.has("nosignal") || shared.airplane || !shared.mobileData;
}

/** luyaz-speedtest `servers` — cached 5 min, else fetched (fails offline). */
function getServers(ctx: Ctx): { list: SpeedServer[]; fetched: boolean } | { error: string } {
  if (serversCachedAt !== null && ctx.now - serversCachedAt < CACHE_TTL_MS) return { list: SERVERS, fetched: false };
  if (offline(ctx)) return { error: "fetch servers: io: Network is unreachable (os error 101)" };
  serversCachedAt = ctx.now;
  return { list: SERVERS, fetched: true };
}

const r2 = (v: number) => Math.round(v * 100) / 100;

function idleProgress(server = "", s: RunState | null = null): SpeedProgress {
  const base = {
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
  return (s?.v1 || (s === null && v1Now) ? base : { ...base, ...v2Fields(s) }) as SpeedProgress;
}

/** The last request's speed-v1 (an idle reply has no run to say which core it is). */
let v1Now = false;

/** luyaz-speedtest's progress reply `t` ms after start. */
function progressAt(s: RunState, t: number): SpeedProgress {
  if (s.coarse) return coarseAt(s, t);
  const p = progressV1(s, t);
  if (s.v1) return p;
  const q = p as unknown as Record<string, unknown>;
  const stage = (st: string, step: number) => { q.stage = st; q.step = step; };
  if (p.phase === "latency") stage("idle_latency", 1);
  if (p.phase === "download") {
    if (t < SINGLE_END_MS) stage("single_download", 2);
    else if (s.chill && t >= CHILL_FROM_MS) stage("chill_download", 4);
    else stage("multi_download", 3);
  }
  if (p.phase === "upload") stage("multi_upload", s.chill ? 5 : 4);
  if (p.ping_ms !== null) q.idle_ms = p.ping_ms;
  if (t >= SINGLE_END_MS) { q.single_mbps = r2(s.dl / 3.1); q.single_whole_mbps = r2(s.dl / 3.4); }
  if (p.download_mbps !== null) { q.download_whole_mbps = r2(s.dl * 0.9); q.loaded_down_ms = r2(s.ping + 150); }
  if (s.chill && p.download_mbps !== null) { q.chill_mbps = r2(s.dl * 0.82); q.chill_whole_mbps = r2(s.dl * 0.75); q.chill_region = "日本"; }
  if (p.phase === "complete") {
    q.step = q.steps;
    q.upload_whole_mbps = r2(s.ul * 0.92);
    q.loaded_up_ms = r2(s.ping + 40);
    q.cpu_peak_pct = 38;
    if (s.short) q.sample_short = true;
    else q.verdict = verdictOf(s);
  }
  return p;
}

/** The coarse run (roaming): download only, 5 s, ≤ 30 MB, no ping, no verdict. */
function coarseAt(s: RunState, t: number): SpeedProgress {
  const p = idleProgress(`Ookla · ${s.server.sponsor} (${s.server.name})`, s);
  const q = p as unknown as Record<string, unknown>;
  if (s.offline) {
    p.phase = "error";
    p.error = "all ping attempts failed";
    return p;
  }
  const dl = Math.min(s.dl, 40);
  if (t < COARSE_MS) {
    const secs = t / 1000;
    p.phase = "download";
    q.stage = "multi_download";
    q.step = 1;
    p.progress = Math.floor((t / COARSE_MS) * 100);
    p.live_speed_mbps = r2(dl * (1 - Math.exp(-secs / 0.6)));
    p.download_bytes = Math.min(30 * MB, Math.round(((dl * 1e6) / 8) * secs));
    return p;
  }
  p.phase = "complete";
  p.progress = 100;
  q.step = 1;
  p.download_mbps = dl;
  q.download_whole_mbps = r2(dl * 0.85);
  p.download_bytes = Math.min(30 * MB, Math.round(((dl * 1e6) / 8) * (COARSE_MS / 1000)));
  if (s.short) q.sample_short = true;
  return p;
}

/** The old engine's shape (still the v2 one's skeleton). */
function progressV1(s: RunState, t: number): SpeedProgress {
  const p = idleProgress(s.v1 ? `${s.server.sponsor} (${s.server.name})` : `Ookla · ${s.server.sponsor} (${s.server.name})`, s);
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
  if (run.v1 !== v1Now) return idleProgress();
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
  if (run.coarse) return t < COARSE_MS;
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
    // luyaz-speedtest servers — outbound fetch on the device when the cache is cold.
    method: "GET",
    path: "/api/speedtest/servers",
    handler: (ctx: Ctx) => {
      const r = getServers(ctx);
      if ("error" in r) return { status: 503, error: r.error, delayMs: 300 };
      return { data: r.list.map((s) => ({ ...s })), delayMs: r.fetched ? 900 : 0 };
    },
  },
  {
    // luyaz-speedtest start
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
      const busy = diagRefusal(ctx.now);
      if (busy) return busy;
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
        v1: ctx.has("speed-v1"),
        // the core decides roaming at admission (an old core never runs coarse)
        coarse: !ctx.has("speed-v1") && !ctx.has("home-network"),
        chill: !ctx.has("speed-v1") && bodyField(ctx.body, "path") === "chill",
        short: ctx.has("speed-short"),
      };
      return ok({ status: "started" });
    },
  },
  {
    method: "GET",
    path: "/api/speedtest/progress",
    handler: (ctx: Ctx) => {
      v1Now = ctx.has("speed-v1");
      return ok(currentProgress(ctx.now));
    },
  },
  {
    // luyaz-speedtest stop
    method: "POST",
    path: "/api/speedtest/stop",
    handler: (ctx: Ctx) => {
      if (!run || !isRunning(ctx.now)) return ok({ status: "not_running" });
      run.cancelledAt = ctx.now;
      return ok({ status: "stopping" });
    },
  },
  {
    // phone.rs at_port — first call probes the serial ports (~1.3 s each), then cached.
    method: "GET",
    path: "/api/at/port",
    handler: () => {
      const first = !atPortDetected;
      atPortDetected = true;
      return { data: { port: AT_PORT, available: true } satisfies AtPort, delayMs: first ? 1_300 : 0 };
    },
  },
  {
    // phone.rs at_send
    method: "POST",
    path: "/api/at/send",
    handler: (ctx: Ctx) => {
      if (ctx.body === undefined) return fail("invalid JSON", 400);
      const command = bodyField(ctx.body, "command");
      if (typeof command !== "string") return fail("missing 'command' field", 400);
      if (!command.toUpperCase().startsWith("AT")) return fail("not an AT command (it has to begin with AT)", 400);
      const t = bodyField(ctx.body, "timeout");
      const timeout = Math.min(30, Math.max(1, typeof t === "number" && Number.isInteger(t) && t >= 0 ? t : 3));
      const sanitized = command.replace(/['`$;|&]/g, "");
      if (sanitized === "") return fail("nothing left to send once unsafe characters are removed", 400);
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
];
