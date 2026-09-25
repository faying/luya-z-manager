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
