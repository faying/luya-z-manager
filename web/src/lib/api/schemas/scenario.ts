// Response shapes for the scenario engine (zte-agent/src/scenario.rs).
// Types only — no runtime code. All shapes are built by the agent (serde
// structs + json!), so Rust wins; `// page expects …` marks disagreements
// with app/(panel)/router/scenario/page.tsx.
//
// Unix times are device-clock seconds (local wall time labelled UTC).

/** Params — scenario.rs:129 (defaults scenario.rs:147-158). */
export interface ScenarioParams {
  scan_interval_home_secs: number;
  scan_interval_away_battery_secs: number;
  scan_interval_away_charging_secs: number;
  enter_hits: number;
  exit_misses: number;
  min_rssi_dbm: number;
}

/** SsidEntry — scenario.rs:162. `bssid` omitted when unset. */
export interface ScenarioSsidEntry {
  ssid: string;
  bssid?: string;
}

/** Detect — scenario.rs:172, serde tag "type", snake_case. */
export type ScenarioDetect =
  | { type: "ssid"; entries: ScenarioSsidEntry[] }
  | { type: "mcc"; mccs: string[] }
  | { type: "abroad" }
  | { type: "fallback" };

/** FieldCheck — scenario.rs:194. */
export interface ScenarioFieldCheck {
  path: string;
  /** JSON pointer, e.g. "/data/region/active". */
  pointer: string;
  equals: unknown;
}

/** RestoreOnExit — scenario.rs:231. */
export interface ScenarioRestoreOnExit {
  read_path: string;
  read_pointer: string;
  field: string;
}

/**
 * ScenarioAction — scenario.rs:203, with Action (action.rs:34) flattened in.
 * Optional fields are omitted when empty/false/None (skip_serializing_if).
 */
export interface ScenarioAction {
  method: string;
  /** One of `allowed_paths`. */
  path: string;
  body?: Record<string, unknown>; // Rust: any JSON Value
  snapshot?: string[];
  verify_job?: string;
  verify_field?: ScenarioFieldCheck;
  best_effort?: boolean;
  restore_on_exit?: ScenarioRestoreOnExit;
}

/** Scenario — scenario.rs:240. `actions`/`inhibit_sleep` always serialised. */
export interface ScenarioDef {
  id: string;
  name: string;
  detect: ScenarioDetect;
  actions: ScenarioAction[]; // page expects optional
  inhibit_sleep: boolean; // page expects optional
}

/**
 * Config — scenario.rs:256. Also the body of PUT /api/scenario and the reply
 * of GET /api/scenario/template (scenario.rs:1638, built by `template()` at
 * scenario.rs:354: home / away / abroad).
 */
export interface ScenarioConfig {
  version: number;
  home_mcc: string; // page expects optional
  params: ScenarioParams;
  scenarios: ScenarioDef[];
}

/** One `pending_restore[]` entry, reduced in state_json (scenario.rs:1459-1462). */
export interface ScenarioPendingRestore {
  /** e.g. `/api/services/chill/regions {"group":"🚀 节点选择"}`, "chill-on-after-abroad", "chill-exit-after-abroad". */
  key: string;
  /** Body of the action that puts the value back (null for chill-on). */
  body: Record<string, unknown> | null; // page expects `{ member?: string } | null`
  saved_at: number;
}

/**
 * GET /api/scenario — scenario.rs:1614 (`scenario_get` → `state_json`,
 * scenario.rs:1440). Also the reply data of PUT /api/scenario, POST
 * /api/scenario/pin, PUT /api/scenario/enabled and POST /api/scenario/apply
 * (the latter 503 with ok:false but the same data when the switch failed).
 */
export interface ScenarioState {
  enabled: boolean;
  pin: string | null;
  guard_takeover: boolean; // page expects optional
  /** Applied scenario id; "" before anything was applied. */
  current: string;
  candidate: string;
  hits: number;
  misses: number;
  /** uci key → value the engine last wrote. Not used by the page. */
  applied: Record<string, string>;
  last_switch: number | null;
  /** 0 before the first scan. */
  last_scan: number;
  last_error: string | null;
  config: ScenarioConfig;
  /** Home MCC from the SIM IMSI (ubus zwrt_zte_mdm.api get_sim_info); null when not ready. */
  sim_mcc: string | null;
  pending_restore: ScenarioPendingRestore[]; // page expects optional
  /** ALLOWED_PATHS (scenario.rs:113). Not used by the page. */
  allowed_paths: string[];
}

/** One network from wifi_scan (wifi_scan.rs:49). `signal` in dBm. */
export interface ScenarioScanNetwork {
  ssid: string;
  bssid: string;
  signal: number;
}

/** GET /api/scenario/scan — scenario.rs:1731. 503 `{ok:false,error}` when the scan fails. */
export interface ScenarioScan {
  networks: ScenarioScanNetwork[];
}

/** GET /api/scenario/log — scenario.rs:1742. Last 300 lines joined with "\n"; each "YYYY-MM-DDTHH:MM:SS msg" (device local time). */
export interface ScenarioLog {
  log: string;
}

/* ── request bodies ────────────────────────────────────────────────── */

/** POST /api/scenario/pin — `{id:null}` clears. */
export interface ScenarioPinBody {
  id: string | null;
}
/** PUT /api/scenario/enabled — missing `enabled` counts as true (scenario.rs:1684). */
export interface ScenarioEnabledBody {
  enabled: boolean;
}
/** POST /api/scenario/apply */
export interface ScenarioApplyBody {
  id: string;
}

/** GET endpoints of this area (paths without query string). */
export interface ScenarioGetMap {
  "/api/scenario": ScenarioState;
  "/api/scenario/template": ScenarioConfig;
  "/api/scenario/scan": ScenarioScan;
  "/api/scenario/log": ScenarioLog;
}
