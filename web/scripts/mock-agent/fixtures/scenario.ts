// Scenario engine — mirrors zte-agent/src/scenario.rs (HTTP surface + the
// parts of switch_to / run_restores / chill hooks the pages can observe).
//
// Persona: travelling in Taiwan. The engine is on, the config is the shipped
// template with two home networks filled in, and the device is in the
// "abroad" scenario. NOTE: scenario.rs judges "abroad" by the SIM's own home
// MCC (IMSI), not the serving cell, so this requires a local SIM / eSIM
// profile reporting MCC 466; a Chinese SIM roaming on 中華電信 would report
// 460 and stay in "away". The mock uses sim_mcc "466".
//
// On arrival the abroad scenario put CHILL's main group on DIRECT and saved
// the pre-trip member (🇯🇵 日本) as a pending restore; the owner then put the
// main group back on 🇹🇼 台湾 by hand (regions_set has no scenario hook, so
// nothing else was recorded). Coming home would restore 🇯🇵 日本.
//
// Unix times are device-clock seconds (real epoch + 8 h, see services.ts).

import type { Route, Ctx, Reply } from "../lib.ts";
import { ok, fail, clone, unixNow } from "../lib.ts";
import { shared } from "../shared.ts";
import { chillSetGroup, chillMainGroupNow, chillSetExit, chillEnableJob } from "./services.ts";
import type {
  ScenarioState,
  ScenarioConfig,
  ScenarioDef,
  ScenarioAction,
  ScenarioPendingRestore,
  ScenarioScan,
  ScenarioLog,
} from "../../../src/lib/api/schemas/scenario.ts";

const DEVICE_OFFSET = 8 * 3600;
const deviceNow = (): number => unixNow() + DEVICE_OFFSET;
/** scenario.rs log_line stamp: "2026-09-24T14:02:11" (localtime_r, no zone). */
const stamp = (devSec: number): string => new Date(devSec * 1000).toISOString().slice(0, 19);

const BOOT = deviceNow();

// ── template (scenario.rs:354 template()) ───────────────────────────────────

const CHILL_MAIN_GROUP = "🚀 节点选择";
const CHILL_STATUS = "/api/services/chill";
const CHILL_ACTIVE = "/data/region/active";
const WIFI_KEYS = ["wireless.main_2g.disabled", "wireless.main_5g.disabled"];
const CHILL_ON_KEY = "chill-on-after-abroad";
const CHILL_EXIT_KEY = "chill-exit-after-abroad";

const ALLOWED_PATHS = [
  "/api/wifi/radio",
  "/api/wifi/guest",
  "/api/services/chill/enable",
  "/api/services/chill/disable",
  "/api/services/chill/regions",
  "/api/services/chill/bypass",
  "/api/services/chill/exit",
  "/api/device/power-save",
  "/api/device/thermal",
  "/api/router/apn/profiles/activate",
];

function wifiAction(on: boolean): ScenarioAction {
  return { method: "PUT", path: "/api/wifi/radio", body: { ap_2g: on, ap_5g: on }, snapshot: [...WIFI_KEYS] };
}

function mainGroupAction(member: string): ScenarioAction {
  return {
    method: "PUT",
    path: "/api/services/chill/regions",
    body: { group: CHILL_MAIN_GROUP, member },
    verify_field: { path: CHILL_STATUS, pointer: CHILL_ACTIVE, equals: member },
    best_effort: true,
    restore_on_exit: { read_path: CHILL_STATUS, read_pointer: CHILL_ACTIVE, field: "member" },
  };
}

function template(): ScenarioConfig {
  return {
    version: 1,
    home_mcc: "460",
    params: {
      scan_interval_home_secs: 60,
      scan_interval_away_battery_secs: 300,
      scan_interval_away_charging_secs: 60,
      enter_hits: 2,
      exit_misses: 2,
      min_rssi_dbm: -70.0,
    },
    scenarios: [
      { id: "home", name: "在家", detect: { type: "ssid", entries: [] }, actions: [wifiAction(false)], inhibit_sleep: true },
      { id: "away", name: "外出", detect: { type: "fallback" }, actions: [wifiAction(true)], inhibit_sleep: false },
      {
        id: "abroad",
        name: "国外",
        detect: { type: "abroad" },
        actions: [wifiAction(true), mainGroupAction("DIRECT")],
        inhibit_sleep: false,
      },
    ],
  };
}

// ── engine state ────────────────────────────────────────────────────────────

interface PendingRestore {
  key: string;
  action: ScenarioAction;
  saved_at: number;
  while_abroad?: boolean;
}

const ARRIVED = BOOT - (1 * 86400 + 6 * 3600 + 23 * 60); // arrived in Taipei ~30 h ago

let config: ScenarioConfig = (() => {
  const c = template();
  const home = c.scenarios.find((s) => s.id === "home");
  if (home) home.detect = { type: "ssid", entries: [{ ssid: "Home-5G", bssid: "02:00:00:00:00:11" }, { ssid: "Home-2.4G" }] };
  return c;
})();

let enabled = true;
let pin: string | null = null;
const guardTakeover = false;

const run = {
  current: "abroad",
  candidate: "",
  hits: 0,
  misses: 0,
  applied: { "wireless.main_2g.disabled": "0", "wireless.main_5g.disabled": "0" } as Record<string, string>,
  last_switch: ARRIVED as number | null,
  last_scan: BOOT - 40,
  last_error: null as string | null,
};

const SIM_MCC = "466";

function restoreKey(a: ScenarioAction): string | null {
  const r = a.restore_on_exit;
  if (!r) return null;
  const body: Record<string, unknown> = { ...(a.body ?? {}) };
  delete body[r.field];
  return `${a.path} ${JSON.stringify(body)}`;
}

function restoreAction(a: ScenarioAction, saved: unknown): ScenarioAction {
  const r = a.restore_on_exit!;
  const back: ScenarioAction = clone(a);
  back.body = { ...(back.body ?? {}), [r.field]: saved };
  delete back.restore_on_exit;
  back.best_effort = true;
  if (back.verify_field) back.verify_field = { ...back.verify_field, equals: saved };
  return back;
}

let pending: PendingRestore[] = (() => {
  const a = mainGroupAction("DIRECT");
  return [{ key: restoreKey(a)!, action: restoreAction(a, "🇯🇵 日本"), saved_at: ARRIVED + 12 }];
})();

const log: string[] = (() => {
  const t = (s: number, m: string) => `${stamp(s)} ${m}`;
  const a = ARRIVED;
  return [
    t(a - 3 * 86400, "configuration replaced"),
    t(a - 3 * 86400 + 5, "engine enabled"),
    t(a - 7200, 'entering scenario "外出" (away)'),
    t(a - 7190, 'scenario "away" applied'),
    t(a - 900, "scan failed (1 in a row): no wiphy present — the radios are fully torn down, nothing can scan until they return"),
    t(a, 'entering scenario "国外" (abroad)'),
    t(a + 12, 'saved "🇯🇵 日本" to restore on leaving'),
    t(a + 14, 'scenario "abroad" applied'),
    t(BOOT - 6 * 3600, "[bootsafe] Wi-Fi already up; nothing to repair"),
  ];
})();

function logLine(msg: string): void {
  log.push(`${stamp(deviceNow())} ${msg}`);
  if (log.length > 300) log.splice(0, log.length - 300);
}

// ── helpers ─────────────────────────────────────────────────────────────────

function findScenario(id: string): ScenarioDef | undefined {
  return config.scenarios.find((s) => s.id === id);
}

function isAbroad(s: ScenarioDef | undefined): boolean {
  return !!s && (s.detect.type === "mcc" || s.detect.type === "abroad");
}

function inAbroadScenario(): boolean {
  return enabled && isAbroad(findScenario(run.current));
}

function stateJson(ctx?: Ctx): ScenarioState {
  const now = deviceNow();
  // The engine scans every 60 s abroad on charger (battery 76 % charging).
  const interval = config.params.scan_interval_away_charging_secs || 60;
  if (enabled && now - run.last_scan > interval) run.last_scan = now - ((now - BOOT) % interval);
  const data: ScenarioState = {
    enabled,
    pin,
    guard_takeover: guardTakeover,
    current: run.current,
    candidate: run.candidate,
    hits: run.hits,
    misses: run.misses,
    applied: { ...run.applied },
    last_switch: run.last_switch,
    last_scan: run.last_scan,
    last_error: run.last_error,
    config: clone(config),
    sim_mcc: SIM_MCC,
    pending_restore: pending.map((p) => ({
      key: p.key,
      body: (p.action.body as ScenarioPendingRestore["body"]) ?? null,
      saved_at: p.saved_at,
    })),
    allowed_paths: [...ALLOWED_PATHS],
  };
  if (ctx?.has("missing")) {
    // Realistic absences: SIM not ready, never switched, no scan yet.
    data.sim_mcc = null;
    data.last_switch = null;
    data.last_scan = 0;
    data.pending_restore = [];
    data.applied = {};
  }
  return data;
}

// ── running actions ─────────────────────────────────────────────────────────

/** Run one action the way run_action → route would. Throws the error text. */
function runAction(a: ScenarioAction): void {
  const body = (a.body ?? {}) as Record<string, unknown>;
  switch (a.path) {
    case "/api/wifi/radio": {
      const on = body.ap_2g !== false || body.ap_5g !== false;
      shared.wifiOn = on;
      for (const k of WIFI_KEYS) run.applied[k] = on ? "0" : "1";
      return;
    }
    case "/api/services/chill/regions": {
      const err = chillSetGroup(String(body.group ?? ""), String(body.member ?? ""));
      if (err) throw new Error(`${a.method} ${a.path} → ${err[0]}: ${err[1]}`);
      return;
    }
    case "/api/services/chill/exit": {
      const err = chillSetExit(String(body.state ?? ""));
      if (err) throw new Error(`${a.method} ${a.path} → ${err[0]}: ${err[1]}`);
      return;
    }
    case "/api/services/chill/enable": {
      const r = chillEnableJob();
      if (r.error) throw new Error(`${a.method} ${a.path} → ${r.status}: ${r.error}`);
      // verify_job: the real engine polls /job for up to 40 s; the mock job
      // settles in ~3 s on its own, so treat a started job as success.
      return;
    }
    default:
      // Other allowed paths belong to other areas; the mock treats them as applied.
      return;
  }
}

function dueRestores(next: ScenarioDef | undefined): PendingRestore[] {
  const owned = (next?.actions ?? []).map(restoreKey).filter((k): k is string => !!k);
  const abroadNext = isAbroad(next);
  const order = (k: string) => (k === CHILL_ON_KEY ? 0 : k === CHILL_EXIT_KEY ? 2 : 1);
  return pending
    .filter((p) => !owned.includes(p.key) && !(p.while_abroad && abroadNext))
    .sort((a, b) => order(a.key) - order(b.key));
}

function runRestores(next: ScenarioDef | undefined): void {
  const done: string[] = [];
  for (const p of dueRestores(next)) {
    try {
      runAction(p.action);
      logLine(`restored ${p.action.path} ${JSON.stringify(p.action.body ?? null)}`);
      done.push(p.key);
    } catch (e) {
      logLine(`restore failed, will retry: ${e instanceof Error ? e.message : String(e)}`);
    }
  }
  pending = pending.filter((p) => !done.includes(p.key));
}

function rememberForRestore(a: ScenarioAction): void {
  const key = restoreKey(a);
  if (!key || pending.some((p) => p.key === key)) return;
  const r = a.restore_on_exit!;
  const current = r.read_pointer === CHILL_ACTIVE ? chillMainGroupNow() : null;
  if (current == null) {
    logLine(`cannot read ${r.read_path}${r.read_pointer} before changing it; nothing to restore later`);
    return;
  }
  logLine(`saved ${JSON.stringify(current)} to restore on leaving`);
  pending.push({ key, action: restoreAction(a, current), saved_at: deviceNow() });
}

/** scenario.rs switch_to. */
function switchTo(id: string): void {
  const scen = findScenario(id);
  if (!scen) return;
  logLine(`entering scenario ${JSON.stringify(scen.name)} (${scen.id})`);
  runRestores(scen);
  let err: string | null = null;
  for (const a of scen.actions) {
    rememberForRestore(a);
    try {
      runAction(a);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      if (a.best_effort) {
        logLine(`best-effort action failed (${a.method} ${a.path}): ${msg} — continuing`);
        continue;
      }
      logLine(`action failed (${a.method} ${a.path}): ${msg} — rolling back`);
      err = msg;
      break;
    }
  }
  if (err == null) {
    run.current = scen.id;
    run.last_switch = deviceNow();
    run.last_error = null;
    logLine(`scenario ${JSON.stringify(scen.id)} applied`);
  } else {
    run.current = "away";
    run.last_error = err;
    shared.wifiOn = true;
    logLine(`scenario ${JSON.stringify(scen.id)} failed: ${err}; fell back to away`);
  }
  run.candidate = "";
  run.hits = 0;
  run.misses = 0;
}

// ── hooks called from fixtures/services.ts (server.rs:389-412) ───────────────

/** scenario.rs:1475 chill_toggled. */
export function scenarioChillToggled(on: boolean, wasOn: boolean): void {
  if (on) {
    const before = pending.length;
    pending = pending.filter((p) => p.key !== CHILL_ON_KEY);
    if (pending.length !== before) logLine("CHILL switched on by hand; no longer switching it on when home");
    return;
  }
  if (!wasOn || !inAbroadScenario() || pending.some((p) => p.key === CHILL_ON_KEY)) return;
  pending.push({
    key: CHILL_ON_KEY,
    action: { method: "POST", path: "/api/services/chill/enable", verify_job: "/api/services/chill/job", best_effort: true },
    saved_at: deviceNow(),
    while_abroad: true,
  });
  logLine("CHILL switched off abroad; will switch it back on when home");
}

/** scenario.rs:1500 chill_exit_changed. */
export function scenarioChillExitChanged(): void {
  if (!inAbroadScenario() || pending.some((p) => p.key === CHILL_EXIT_KEY)) return;
  pending.push({
    key: CHILL_EXIT_KEY,
    action: {
      method: "PUT",
      path: "/api/services/chill/exit",
      body: { state: "proxy" },
      verify_field: { path: CHILL_STATUS, pointer: "/data/exit", equals: "proxy" },
      best_effort: true,
    },
    saved_at: deviceNow(),
    while_abroad: true,
  });
  logLine("CHILL exit changed abroad; will go back to proxy when home");
}

// ── validation (scenario.rs:374 validate) ───────────────────────────────────

const isMcc = (v: unknown) => typeof v === "string" && /^\d{3}$/.test(v);

function parseConfig(body: unknown): ScenarioConfig | string {
  if (!body || typeof body !== "object" || Array.isArray(body)) {
    return `invalid config: invalid type: ${typeof body === "string" ? `string ${JSON.stringify(body)}` : String(body)}, expected struct Config at line 1 column 1`;
  }
  const b = body as Record<string, unknown>;
  if (typeof b.version !== "number") return "invalid config: missing field `version` at line 1 column 2";
  const scenarios = Array.isArray(b.scenarios) ? (b.scenarios as ScenarioDef[]) : [];
  const params = { ...template().params, ...((b.params as object) ?? {}) };
  const cfg: ScenarioConfig = {
    version: b.version,
    home_mcc: typeof b.home_mcc === "string" ? b.home_mcc : "460",
    params,
    scenarios: scenarios.map((s) => ({
      id: String(s.id ?? ""),
      name: String(s.name ?? ""),
      detect: s.detect,
      actions: Array.isArray(s.actions) ? s.actions : [],
      inhibit_sleep: !!s.inhibit_sleep,
    })),
  };
  for (const s of cfg.scenarios) {
    const t = (s.detect as { type?: unknown } | undefined)?.type;
    if (t !== "ssid" && t !== "mcc" && t !== "abroad" && t !== "fallback") {
      return "invalid config: unknown variant, expected one of `ssid`, `mcc`, `abroad`, `fallback`";
    }
  }
  if (cfg.scenarios.filter((s) => s.detect.type === "fallback").length > 1) {
    return "at most one scenario may use detect.type = fallback";
  }
  if (cfg.scenarios.filter((s) => s.detect.type === "abroad").length > 1) {
    return "at most one scenario may use detect.type = abroad";
  }
  if (!isMcc(cfg.home_mcc)) return `home_mcc ${JSON.stringify(cfg.home_mcc)} must be three digits`;
  const seen: string[] = [];
  for (const s of cfg.scenarios) {
    if (!s.id.trim()) return "scenario id must not be empty";
    if (seen.includes(s.id)) return `duplicate scenario id ${JSON.stringify(s.id)}`;
    seen.push(s.id);
    if (s.detect.type === "mcc") {
      const mccs = s.detect.mccs ?? [];
      if (mccs.length === 0) return `scenario ${JSON.stringify(s.id)}: mccs must not be empty`;
      const bad = mccs.find((m) => !isMcc(m));
      if (bad !== undefined) return `scenario ${JSON.stringify(s.id)}: MCC ${JSON.stringify(bad)} must be three digits`;
      if (mccs.includes(cfg.home_mcc)) {
        return `scenario ${JSON.stringify(s.id)} lists the home MCC ${cfg.home_mcc}; it would fire at home`;
      }
    }
    if (
      s.detect.type === "fallback" &&
      s.actions.some((a) => a.path === "/api/wifi/radio" && (a.body?.ap_2g === false || a.body?.ap_5g === false))
    ) {
      return `scenario ${JSON.stringify(s.id)} is the fallback and must not turn Wi-Fi off`;
    }
    for (const a of s.actions) {
      if (a.best_effort && a.path === "/api/wifi/radio") return "best_effort is not allowed on /api/wifi/radio";
      if (!ALLOWED_PATHS.includes(a.path)) {
        return `path ${JSON.stringify(a.path)} is not allowed in a scenario; permitted: ${ALLOWED_PATHS.join(", ")}`;
      }
      if (!["GET", "POST", "PUT", "DELETE"].includes(String(a.method).toUpperCase())) {
        return `unsupported method ${JSON.stringify(a.method)}`;
      }
    }
  }
  if (cfg.params.enter_hits === 0 || cfg.params.exit_misses === 0) {
    return "enter_hits and exit_misses must be at least 1";
  }
  return cfg;
}

// ── handlers ────────────────────────────────────────────────────────────────

function isObj(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === "object" && !Array.isArray(v);
}

function scenarioPut(ctx: Ctx): Reply {
  const r = parseConfig(ctx.body);
  if (typeof r === "string") return fail(r, 400);
  config = r;
  logLine("configuration replaced");
  return ok(stateJson());
}

function scenarioPin(ctx: Ctx): Reply {
  if (ctx.body === undefined) return fail("invalid JSON", 400);
  // `.get("id")` on a non-object (e.g. a double-encoded string) is None → clear.
  const id = isObj(ctx.body) ? ctx.body.id : undefined;
  if (typeof id === "string") {
    if (!findScenario(id)) return fail(`unknown scenario ${JSON.stringify(id)}`, 400);
    pin = id;
    logLine(`pinned to ${JSON.stringify(id)}`);
    // The engine tick (every 15 s) switches to the pinned scenario.
    const t = setTimeout(() => {
      if (enabled && pin === id && run.current !== id) switchTo(id);
    }, 3000);
    t.unref?.();
  } else {
    pin = null;
    logLine("pin cleared");
  }
  return ok(stateJson());
}

function scenarioEnabled(ctx: Ctx): Reply {
  if (ctx.body === undefined) return fail("invalid JSON", 400);
  const v = isObj(ctx.body) ? ctx.body.enabled : undefined;
  const on = typeof v === "boolean" ? v : true; // unwrap_or(true)
  if (on) {
    enabled = true;
    logLine("engine enabled");
  } else {
    enabled = false;
    logLine("engine disabled — restoring away scenario");
    if (!shared.wifiOn) {
      shared.wifiOn = true;
      logLine("[bootsafe] Wi-Fi was down; restored away scenario");
    } else {
      logLine("[bootsafe] Wi-Fi already up; nothing to repair");
    }
    runRestores(undefined);
    // bootsafe sleeps 5 s + 4 s before checking beaconing, then the restores
    // poll verify_field — the real call takes 9-20 s (client override: 120 s).
    // Next engine tick (scenario.rs:1199) lands on away.
    const t = setTimeout(() => {
      if (!enabled && run.current !== "away") {
        logLine("engine disabled while away from the away scenario — restoring it");
        switchTo("away");
      }
    }, 3000);
    t.unref?.();
    return { data: stateJson(), delayMs: 9500 };
  }
  return ok(stateJson());
}

function scenarioApply(ctx: Ctx): Reply {
  if (ctx.body === undefined) return fail("invalid JSON", 400);
  const id = isObj(ctx.body) ? ctx.body.id : undefined;
  if (typeof id !== "string") return fail('expected {"id": "..."}', 400);
  if (!findScenario(id)) return fail(`unknown scenario ${JSON.stringify(id)}`, 404);
  logLine(`manual apply requested: ${JSON.stringify(id)}`);
  switchTo(id);
  const data = stateJson();
  const failed = data.last_error != null && data.current !== id;
  // scenario.rs:1725: 503 {ok:false, data} — the envelope carries data, so raw.
  if (failed) return { status: 503, raw: { ok: false, data } };
  // Real: wifi_radio::apply polls until the change is visible (often 8-50 s).
  return { data, delayMs: 6000 };
}

const SCAN: ScenarioScan = {
  networks: [
    { ssid: "U60-Travel-5G", bssid: "02:00:00:00:00:21", signal: -31 },
    { ssid: "U60-Travel", bssid: "02:00:00:00:00:22", signal: -33 },
    { ssid: "CHT Wi-Fi(HiNet)", bssid: "02:00:00:00:00:31", signal: -58 },
    { ssid: "iTaiwan", bssid: "02:00:00:00:00:32", signal: -64 },
    { ssid: "Hotel-Guest", bssid: "02:00:00:00:00:41", signal: -67 },
    { ssid: "Hotel-Guest", bssid: "02:00:00:00:00:42", signal: -74 },
    { ssid: "7-ELEVEN_Free", bssid: "02:00:00:00:00:51", signal: -79 },
    { ssid: "", bssid: "02:00:00:00:00:61", signal: -83 },
  ],
} satisfies ScenarioScan;

export const routes: Route[] = [
  { method: "GET", path: "/api/scenario", handler: (ctx) => ok(stateJson(ctx)), ownMissing: true },
  { method: "PUT", path: "/api/scenario", handler: scenarioPut },
  // Generic stripping would break the page's "create defaults" (it PUTs this back).
  { method: "GET", path: "/api/scenario/template", handler: () => ok(template()), ownMissing: true },
  { method: "POST", path: "/api/scenario/pin", handler: scenarioPin },
  { method: "PUT", path: "/api/scenario/enabled", handler: scenarioEnabled },
  { method: "POST", path: "/api/scenario/apply", handler: scenarioApply },
  // Real scan: off-channel sweep (or temporary vdev) — takes a few seconds.
  { method: "GET", path: "/api/scenario/scan", handler: () => ({ data: clone(SCAN), delayMs: 4000 }) },
  {
    method: "GET",
    path: "/api/scenario/log",
    handler: () => {
      const data: ScenarioLog = { log: log.join("\n") };
      return ok(data);
    },
  },
];
