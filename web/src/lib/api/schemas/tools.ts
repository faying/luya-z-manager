// Response shapes for the tools endpoints of zte-agent: WAN speed test
// (speedtest.rs), AT terminal (at_terminal.rs + at_cmd.rs) and the LAN test
// ping (lan_test.rs). Every handler builds its JSON itself, so the Rust shape
// is the source of truth; page-side disagreements are noted inline. Types only.

/**
 * One server of GET /api/speedtest/servers — speedtest.rs:41 `TestServer`
 * (`base_url` is `#[serde(skip)]`). The list is speedtest.net's
 * `/api/js/servers?engine=js&limit=20`, geolocated to the device's public IP.
 */
export interface SpeedServer {
  id: number;
  name: string;
  sponsor: string;
  country: string;
  host: string;
  /** Upload URL (…/upload.php); latency.txt and random4000x4000.jpg live next to it. */
  url: string;
}

/**
 * GET /api/speedtest/servers — speedtest.rs:355 `servers`. Cached 5 min;
 * otherwise fetched from speedtest.net (10 s timeout, 503 on failure).
 */
export type SpeedServers = SpeedServer[];

/**
 * GET /api/speedtest/progress — speedtest.rs:425 `progress` (speedtest.rs:26
 * `SpeedTestProgress`). Progress bands: latency 0-20, download 20-60, upload 60-100.
 *
 * Page disagreement: tools/speedtest/page.tsx types the nullable numbers as
 * optional (`?: number`) — the handler sends explicit null; it does not type
 * `server`, `download_bytes`, `upload_bytes`.
 */
export interface SpeedProgress {
  phase: "idle" | "latency" | "download" | "upload" | "complete" | "cancelled" | "error";
  /** 0-100. */
  progress: number;
  live_speed_mbps: number;
  ping_ms: number | null;
  jitter_ms: number | null;
  download_mbps: number | null;
  upload_mbps: number | null;
  download_bytes: number;
  upload_bytes: number;
  /** "sponsor (name)"; "" before the first start. */
  server: string;
  error: string | null;
}

/** POST /api/speedtest/start body — speedtest.rs:362. No server_id = first server. */
export interface SpeedStartBody {
  server_id?: number;
}

/** POST /api/speedtest/start reply `data` — speedtest.rs:422. */
export interface SpeedStartResult {
  status: "started";
}

/** POST /api/speedtest/stop reply `data` — speedtest.rs:430. */
export interface SpeedStopResult {
  status: "stopping" | "not_running";
}

/**
 * GET /api/at/port — at_terminal.rs:63 `at_port`. Always 200.
 *
 * Page disagreement: tools/at/page.tsx types `port: string`; it is null when
 * no port answered.
 */
export interface AtPort {
  /** e.g. "/dev/at_mdm0"; null when none of the candidates answered "OK". */
  port: string | null;
  available: boolean;
}

/** POST /api/at/send body — at_terminal.rs:10. timeout: seconds, clamped 1-30, default 3. */
export interface AtSendBody {
  command: string;
  timeout?: number;
}

/**
 * POST /api/at/send reply `data` — at_terminal.rs:48-55. The call always takes
 * `timeout` seconds (+0.3 s): at_cmd.rs reads the port for exactly that long.
 */
export interface AtSendResult {
  /** The command actually sent (' ` $ ; | & stripped). */
  command: string;
  /** Raw text read from the port (echo, reply lines, OK/ERROR, CRLFs). */
  response: string;
  /** "" when detection failed after the send. */
  port: string;
  elapsed_ms: number;
}

/**
 * GET /api/lan/ping — lan_test.rs:25 `ping`. Body is `{"ok": true}` with no
 * `data`, so apiFetch returns undefined.
 */
export type LanPing = undefined;

/** GET endpoints of this area. */
export interface ToolsGetMap {
  "/api/speedtest/servers": SpeedServers;
  "/api/speedtest/progress": SpeedProgress;
  "/api/at/port": AtPort;
  "/api/lan/ping": LanPing;
}

/* ------------------------------------------------------------------ *
 *  Deep diagnosis (deep_diag.rs, docs/designs/slow-diagnosis.md §4.2)
 * ------------------------------------------------------------------ */

/** deep_diag.rs `Level`: pending / running while it checks; ok ● warn ▲ bad ■; na = can't tell; info = a value, no judgement (speed row). */
export type DiagLevel = "pending" | "running" | "ok" | "warn" | "bad" | "na" | "info";

/** One row: wifi / signal / limit / link / crowd / proxy, or the speed row ("speed"). */
export interface DiagLayer {
  id: string;
  level: DiagLevel;
  /** The value side of the row (numbers, or why it can't tell for na). */
  detail: string;
  detail_en: string;
  /** false = shown grey, not counted in "3/6". */
  counted: boolean;
}

/** deep_diag.rs `Main`: the headline and its one action. `layer` "" = nothing found. */
export interface DiagMain {
  layer: string;
  level: DiagLevel | null;
  text: string;
  text_en: string;
  action: string;
  action_en: string;
  /** "placement" (a touch-screen page) / "proxy" (the proxy page) / "". */
  action_to: "placement" | "proxy" | "";
  /** Other layers at warn or bad ("+N more"). */
  more: number;
}

/**
 * GET /api/diagnose, POST /api/diagnose (202, `joined: true` when one was
 * already going), POST /api/diagnose/speed (202). Times are device-clock
 * seconds. A run is kept 10 minutes after it finished; then GET says idle.
 */
export interface DiagRun {
  id: number;
  state: "waiting" | "running" | "done";
  /** What a waiting run waits for. */
  waiting_for?: "scan" | "register" | "speedtest";
  asked_at: number;
  started_at: number | null;
  finished_at: number | null;
  /** Layers finished / layers counted. */
  step: number;
  steps: number;
  layers: DiagLayer[];
  main: DiagMain | null;
  key?: unknown;
  /** "touch" / "client" / "other". */
  from: string;
  feedback: boolean | null;
  speed: DiagLayer | null;
  /** Seconds since it finished; null while it runs. */
  age_s: number | null;
}

export type DiagGet = DiagRun | { state: "idle" };
