// Response shapes for the sidecar-service endpoints: Tailscale (services.rs)
// and CHILL (chill.rs, chill_proxy.rs). Types only — no runtime code.
//
// Every shape below is built by the agent itself (json! in Rust), so the Rust
// handler is the source of truth; `// page expects …` marks where a page's
// inline type disagrees.
//
// Timestamps: ISO strings ending in "Z" come from the device clock, which is
// local wall time labelled UTC (see lib/deviceClock.ts). Unix seconds likewise.

/* ------------------------------------------------------------------ *
 *  Tailscale
 * ------------------------------------------------------------------ */

/** One entry of `peers[]` (at most 32). Built in services.rs:141-153. */
export interface TailscalePeer {
  id: string | null;
  hostname: string | null;
  dns_name: string | null;
  os: string | null;
  ips: string[];
  online: boolean;
  exit_node: boolean;
  rx_bytes: number | null;
  tx_bytes: number | null;
  /** tailscale's own RFC 3339 time; clock basis (device clock vs real UTC) unconfirmed. "0001-01-01T00:00:00Z" = never. */
  last_seen: string | null;
  last_handshake: string | null;
}

export interface TailscaleSelf {
  hostname: string | null;
  dns_name: string | null;
  ips: string[];
  online: boolean;
  /** DERP region code, e.g. "tok". */
  relay: string | null;
  exit_node_option: boolean;
}

export interface TailscaleExitNode {
  hostname: string | null;
  ips: string[];
  online: boolean;
}

/**
 * GET /api/services/tailscale — services.rs:34 (`tailscale_status`).
 *
 * Three shapes, all ok:true:
 *  - not installed: `{installed:false, running:false}` (services.rs:35-43)
 *  - `tailscale status --json` failed / unparseable:
 *    `{installed:true, running, error}` (services.rs:50-86) — a fake success;
 *    lib/api/freshness.ts `tailscaleValid` flags it.
 *  - normal: every field below (services.rs:158-178).
 */
export interface TailscaleStatus {
  installed: boolean;
  running: boolean;
  /** Only in the failure shape. */
  error?: string;
  /** "Running" | "NeedsLogin" | "Stopped" | "Starting" | … ; "Unknown" when absent. */
  backend_state?: string;
  version?: string | null;
  /** null when empty; page expects `string | null` (optional) — matches. */
  auth_url?: string | null;
  self?: TailscaleSelf; // page expects every field optional; handler always sends all six (possibly null)
  exit_node?: TailscaleExitNode | null;
  peer_count?: number;
  peer_online?: number;
  peers?: TailscalePeer[];
}

/** Shared by both log endpoints. */
export interface ServiceLog {
  path: string;
  lines: string[];
  /** The clamped `lines` query value (1..2000, default 200). */
  limit: number;
}

/** GET /api/services/tailscale/log?lines=N — services.rs:182 (`tailscale_log`); path /data/tailscaled.log. */
export type TailscaleLog = ServiceLog;

/* ------------------------------------------------------------------ *
 *  CHILL
 * ------------------------------------------------------------------ */

export type ChillExit = "proxy" | "direct_keep_ai" | "direct_all" | "global";
export type ChillProfile = "eco" | "standard" | "perf";

/** One of the five whitelisted select groups (chill.rs:689-704). */
export interface ChillGroupSummary {
  name: string;
  now: string | null; // page expects `now?: string`
  size: number;
}

/** group_now() — chill.rs:706-713. */
export interface ChillGroupChoice {
  active: string | null; // page expects `active?: string`
  options: string[];
}

/**
 * GET /api/services/chill — chill.rs:114 (`status`).
 *
 * `/tmp/chill.state` (written by scripts/chill/chill.sh `write_state`, line
 * 131) passed through as-is, plus mihomo fields merged in only when
 * `state == "running"` (chill.rs:121-141). No state file yet → `{state:"unknown"}`.
 *
 * When running but mihomo is unreachable (fake success, chill.rs:122-140):
 * `version`, `region`, `ai_exit`, `mode` are null, `groups` is `[]` (an empty
 * Vec, not null) and `exit` is absent. freshness.ts `chillValid` flags that.
 */
export interface ChillStatus {
  /** chill.sh writes "running" | "direct"; the agent adds "unknown". */
  state: "running" | "direct" | "unknown";
  // ── chill.state fields (absent in the "unknown" shape) ──
  /** Direct reason: disabled | gaveup | paused | ruleset_missing | captive_wan | render_failed | overheat | lowmem; null while running. */
  reason?: string | null;
  cpuss_c?: number;
  mem_avail_mb?: number;
  /** 0 when no core (chill.sh `|| echo 0`); page expects `number | null`. */
  core_pid?: number;
  /** Device-clock ISO ("Z" but local digits); "" before the first start. */
  started_at?: string;
  updated_at?: string;
  bypass_stale?: string[];
  rules_drift?: boolean;
  mem_pressure?: boolean;
  profile?: ChillProfile;
  profile_effective?: ChillProfile;
  thermal_eco?: boolean;
  // ── mihomo fields (only when state == "running") ──
  version?: string | null; // page expects `version?: string`
  groups?: ChillGroupSummary[];
  /** Main group "🚀 节点选择". */
  region?: ChillGroupChoice | null;
  /** "🤖 AI" group. */
  ai_exit?: ChillGroupChoice | null;
  /** mihomo configs.mode: "rule" | "global" | "direct". */
  mode?: string | null;
  /** Only when both mode and main-group `now` are readable (chill.rs:137-139). */
  exit?: ChillExit;
  /** Manual-first (manual_first.rs `summary`): the agent moved 🚀 off 🎯 to a region group while the hand-picked node is down. */
  manual_first?: ChillManualFirst;
}

export interface ChillManualFirst {
  on_backup: boolean;
  manual: string | null;
  backup: string | null;
  /** One ready-made line, e.g. "手选的 JP 03 不通，已临时换到 🇯🇵 日本（自动）"; null when there is nothing to say. */
  notice: string | null;
  notice_at: number | null;
}

/** chill.sh log: GET /api/services/chill/log?lines=N — chill.rs:146 (`log`); path /tmp/chill.log. */
export type ChillLog = ServiceLog;

/** mihomo `subscriptionInfo`, passed through as-is (bytes; Expire = unix seconds). */
export interface ChillSubscription {
  Upload: number;
  Download: number;
  Total: number;
  Expire: number;
}

export interface ChillProvider {
  name: string;
  vehicle_type: string | null; // "HTTP" | "File"; page expects `vehicle_type?: string`
  /** mihomo's own RFC 3339 time. */
  updated_at: string | null; // page expects `updated_at?: string`
  node_count: number;
  subscription: ChillSubscription | null;
  /** Only "shouhou" (chill.rs:64). */
  editable: boolean;
}

/**
 * GET /api/services/chill/providers — chill.rs:160 (`providers_list`).
 * mihomo `/providers/proxies` minus "Compatible" providers, sorted by name.
 * mihomo unreachable → `{providers: []}` with ok:true (fake success).
 */
export interface ChillProviders {
  providers: ChillProvider[];
}

/**
 * GET /api/services/chill/bypass — chill.rs:417 (`bypass_get`).
 * `ips` from chill.env CHILL_BYPASS_IP; `stale` from chill.state `bypass_stale`.
 */
export interface ChillBypass {
  ips: string[];
  stale: string[];
}

/**
 * GET /api/services/chill/job — chill.rs:542 (`job`). Serialised JobState
 * (chill.rs:79-86). id 0 / status "idle" before the first job since agent start.
 */
export interface ChillJob {
  id: number;
  /** "enable" | "disable" | "reload" | "profile" | "" */
  kind: string;
  status: "idle" | "running" | "done" | "error";
  message: string;
  started_unix: number;
  /** 0 while running. */
  finished_unix: number;
}

/**
 * GET /api/services/chill/dashboard — chill_proxy.rs:68 (`dashboard_info`).
 * `secret` is a live credential for /chill-api/*. 500 when it cannot be created.
 */
export interface ChillDashboard {
  secret: string;
  /** "/chill-ui/" */
  ui: string;
  /** "/chill-api" */
  api: string;
}

/* ── write replies ─────────────────────────────────────────────────── */

/** Reply of POST enable / disable, PUT profile, PUT providers (chill.rs:568). 409 "operation in progress" when busy. */
export interface ChillJobStarted {
  job_id: number;
  status: "running";
}

/** PUT /api/services/chill/regions reply data (chill.rs:305). */
export interface ChillRegionSet {
  group: string;
  active: string;
}

/** PUT /api/services/chill/exit reply data (chill.rs:405). */
export interface ChillExitSet {
  exit: ChillExit;
}

/** PUT /api/services/chill/bypass reply data (chill.rs:467) — sorted, deduped. */
export interface ChillBypassSet {
  ips: string[];
}

/* ── request bodies ────────────────────────────────────────────────── */

export interface ChillRegionsBody {
  group: string;
  member: string;
}
export interface ChillExitBody {
  state: ChillExit;
}
export interface ChillProfileBody {
  profile: ChillProfile;
}
export interface ChillProvidersBody {
  name: string;
  url: string;
}
export interface ChillProviderRefreshBody {
  name: string;
}
export interface ChillBypassBody {
  ips: string[];
}

/** GET endpoints of this area (paths without query string). */
export interface ServicesGetMap {
  "/api/services/tailscale": TailscaleStatus;
  "/api/services/tailscale/log": TailscaleLog;
  "/api/services/chill": ChillStatus;
  "/api/services/chill/dashboard": ChillDashboard;
  "/api/services/chill/providers": ChillProviders;
  "/api/services/chill/bypass": ChillBypass;
  "/api/services/chill/job": ChillJob;
  "/api/services/chill/log": ChillLog;
}
