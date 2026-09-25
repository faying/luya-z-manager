// Wi-Fi, guest Wi-Fi, AP radio and Home Mode — mirrors zte-agent/src/wifi.rs,
// wifi_radio.rs and homemode.rs.
//
// Model: `uci` holds what the handlers read back with `uci get` (config);
// what is actually broadcasting (`live()`) is derived from it, plus the Wi-Fi
// master switch in shared.wifiOn. Under the "fakesuccess" scenario a write
// commits the config (status reads it back) but the reload "fails", so the
// live state (actual_*, clients, /api/wifi/radio) stays where it was.
//
// Persona: travelling in Taiwan, Wi-Fi on, 4 clients (1 on 2.4 GHz, 3 on
// 5 GHz), guest off, Home Mode installed but paused.

import type { Route, Ctx, Reply } from "../lib.ts";
import { ok, fail, bodyField, clone } from "../lib.ts";
import { shared } from "../shared.ts";
import type {
  WifiStatus,
  WifiSettingsResult,
  WifiRadio,
  WifiRadioSetResult,
  GuestWifi,
  GuestWifiResult,
  HomeMode,
  HomeModeScan,
  HomeModeLog,
} from "../../../src/lib/api/schemas/wifi.ts";

// ── uci state ───────────────────────────────────────────────────────────────

interface WifiUci {
  // keys of wifi.rs uci_map (the PUT body names)
  ssid_2g: string;
  ssid_5g: string;
  key_2g: string;
  key_5g: string;
  encryption_2g: string;
  encryption_5g: string;
  hidden_2g: string;
  hidden_5g: string;
  channel_2g: string;
  channel_5g: string;
  txpower_2g: string;
  txpower_5g: string;
  htmode_2g: string;
  htmode_5g: string;
  radio2_disabled: string;
  radio5_disabled: string;
  /** wireless.wifi0/wifi1.country */
  country: string;
  /** zte_mbb.wifi.wifi6_switch (wifi_onoff lives in shared.wifiOn) */
  wifi6_switch: string;
  /** wireless.main_2g/main_5g.disabled — the AP interfaces PUT /api/wifi/radio toggles. */
  ap2g_disabled: string;
  ap5g_disabled: string;
}

const uci: WifiUci = {
  ssid_2g: "U60-Pro-A1B2",
  ssid_5g: "U60-Pro-A1B2-5G",
  key_2g: "u60pro-Tw2026",
  key_5g: "u60pro-Tw2026",
  encryption_2g: "psk2+ccmp",
  encryption_5g: "psk2+psk3+ccmp",
  hidden_2g: "0",
  hidden_5g: "0",
  channel_2g: "auto",
  channel_5g: "149",
  txpower_2g: "100",
  txpower_5g: "100",
  htmode_2g: "HT20",
  htmode_5g: "VHT80",
  radio2_disabled: "0",
  radio5_disabled: "0",
  country: "CN",
  wifi6_switch: "1",
  ap2g_disabled: "0",
  ap5g_disabled: "0",
};

/** wifi.rs uci_map keys, in handler order. */
const UCI_KEYS = [
  "ssid_2g",
  "ssid_5g",
  "key_2g",
  "key_5g",
  "encryption_2g",
  "encryption_5g",
  "hidden_2g",
  "hidden_5g",
  "channel_2g",
  "channel_5g",
  "txpower_2g",
  "txpower_5g",
  "htmode_2g",
  "htmode_5g",
  "radio2_disabled",
  "radio5_disabled",
] as const;

interface GuestUci {
  ssid: string;
  key: string;
  encryption: string;
  disabled_2g: string;
  disabled_5g: string;
  hidden: string;
  isolate: string;
  guest_active_time: string;
}

const guest: GuestUci = {
  ssid: "U60-Pro-A1B2-Guest",
  key: "guest-A1B2-88",
  encryption: "psk2+ccmp",
  disabled_2g: "1",
  disabled_5g: "1",
  hidden: "0",
  isolate: "1",
  guest_active_time: "0",
};
/** Unix ms when the guest timer started (null = no timer running). */
let guestTimerStart: number | null = null;

// ── helpers ─────────────────────────────────────────────────────────────────

/** wifi.rs:70 sanitize_uci_value */
function sanitize(v: string): string {
  return v.replace(/['";$`\\|<>&]/g, "");
}

/** wifi.rs:223-228 — string / number / bool → uci string; anything else skipped. */
function uciValue(v: unknown): string | null {
  if (typeof v === "string") return sanitize(v);
  if (typeof v === "number") return sanitize(String(v));
  if (typeof v === "boolean") return v ? "1" : "0";
  return null;
}

/** 400s the Rust handlers return before touching anything. */
function objectBody(ctx: Ctx): Record<string, unknown> | Reply {
  if (ctx.body === undefined) return fail("invalid JSON", 400);
  if (!ctx.body || typeof ctx.body !== "object" || Array.isArray(ctx.body)) {
    return fail("expected JSON object", 400);
  }
  return ctx.body as Record<string, unknown>;
}

function isReply(v: unknown): v is Reply {
  return !!v && typeof v === "object" && ("error" in v || "status" in v);
}

interface Live {
  ap2g: boolean;
  ap5g: boolean;
  guest: boolean;
}

/** What the radios would be doing if the last reload had worked. */
function computeLive(): Live {
  const on2 = shared.wifiOn && uci.radio2_disabled !== "1";
  const on5 = shared.wifiOn && uci.radio5_disabled !== "1";
  return {
    ap2g: on2 && uci.ap2g_disabled !== "1",
    ap5g: on5 && uci.ap5g_disabled !== "1",
    guest: (on2 && guest.disabled_2g === "0") || (on5 && guest.disabled_5g === "0"),
  };
}

/** Set by a "fakesuccess" write: the reload failed, radios kept the old state. */
let frozenLive: Live | null = null;

function live(): Live {
  return frozenLive ?? computeLive();
}

/** Call before applying a write. */
function beginWrite(ctx: Ctx): void {
  if (ctx.has("fakesuccess")) {
    if (!frozenLive) frozenLive = computeLive();
  } else {
    frozenLive = null;
  }
}

function actualChannel(band: "2g" | "5g"): string {
  const ch = band === "2g" ? uci.channel_2g : uci.channel_5g;
  if (ch && ch !== "auto") return ch;
  return band === "2g" ? "6" : "149"; // ACS picks
}

function actualBw(band: "2g" | "5g"): string {
  const ht = (band === "2g" ? uci.htmode_2g : uci.htmode_5g).toUpperCase();
  const m = ht.match(/(\d+)$/);
  if (m) return `${m[1]} MHz`; // iw info "width: 80 MHz, center1: ..." → "80 MHz"
  return band === "2g" ? "20 MHz" : "80 MHz";
}

function observe(): WifiRadio {
  const l = live();
  const ap = (l.ap2g ? 1 : 0) + (l.ap5g ? 1 : 0) + (l.guest ? 1 : 0);
  return {
    configured: { ap_2g: uci.ap2g_disabled !== "1", ap_5g: uci.ap5g_disabled !== "1" },
    hostapd_processes: ap,
    beaconing: { wlan0: l.ap2g, wlan2: l.ap5g },
  };
}

function guestRemaining(ctx: Ctx): number {
  const mins = parseInt(guest.guest_active_time, 10) || 0;
  if (!live().guest || mins <= 0 || guestTimerStart === null) return -1; // unconfirmed
  return Math.max(0, mins * 60 - Math.floor((ctx.now - guestTimerStart) / 1000));
}

// ── /api/wifi/status + settings ─────────────────────────────────────────────

function wifiStatus(): WifiStatus {
  const l = live();
  const c2 = l.ap2g ? 1 : 0;
  const c5 = l.ap5g ? 3 : 0;
  const s = {
    wifi_onoff: shared.wifiOn ? "1" : "0",
    wifi6_switch: uci.wifi6_switch,
    radio2_disabled: uci.radio2_disabled,
    radio5_disabled: uci.radio5_disabled,
    channel_2g: uci.channel_2g,
    channel_5g: uci.channel_5g,
    txpower_2g: uci.txpower_2g,
    txpower_5g: uci.txpower_5g,
    htmode_2g: uci.htmode_2g,
    htmode_5g: uci.htmode_5g,
    country_code: uci.country,
    ssid_2g: uci.ssid_2g,
    ssid_5g: uci.ssid_5g,
    key_2g: uci.key_2g,
    key_5g: uci.key_5g,
    encryption_2g: uci.encryption_2g,
    encryption_5g: uci.encryption_5g,
    hidden_2g: uci.hidden_2g,
    hidden_5g: uci.hidden_5g,
    actual_channel_2g: l.ap2g ? actualChannel("2g") : "",
    actual_bw_2g: l.ap2g ? actualBw("2g") : "",
    actual_channel_5g: l.ap5g ? actualChannel("5g") : "",
    actual_bw_5g: l.ap5g ? actualBw("5g") : "",
    clients_2g: c2,
    clients_5g: c5,
    clients_total: c2 + c5,
    guest_disabled_2g: guest.disabled_2g,
    guest_disabled_5g: guest.disabled_5g,
    guest_ssid: guest.ssid,
  } satisfies WifiStatus;
  return s;
}

/** wifi.rs:155 wifi_set — diff against uci, commit, hot txpower or background reload. */
function wifiSet(ctx: Ctx): Reply {
  const obj = objectBody(ctx);
  if (isReply(obj)) return obj;

  const next: WifiUci = { ...uci };
  let nextWifiOn = shared.wifiOn;
  let wirelessChanged = false;
  let mbbChanged = false;
  let onlyTxpower = true;

  const country = obj.country;
  if (typeof country === "string") {
    const cc = sanitize(country).toUpperCase();
    if (/^[A-Z]{2}$/.test(cc) && cc !== uci.country) {
      next.country = cc;
      wirelessChanged = true;
      onlyTxpower = false;
    }
  }

  for (const [key, raw] of Object.entries(obj)) {
    const val = uciValue(raw);
    if (val === null) continue;
    if ((UCI_KEYS as readonly string[]).includes(key)) {
      const k = key as (typeof UCI_KEYS)[number];
      if (next[k] !== val) {
        next[k] = val;
        wirelessChanged = true;
        if (k !== "txpower_2g" && k !== "txpower_5g") onlyTxpower = false;
      }
      continue;
    }
    if (key === "wifi_onoff") {
      const cur = shared.wifiOn ? "1" : "0";
      if (val !== cur) {
        nextWifiOn = val !== "0"; // uci stores the string; the mock keeps a bool
        mbbChanged = true;
        onlyTxpower = false;
      }
    } else if (key === "wifi6_switch" && val !== uci.wifi6_switch) {
      next.wifi6_switch = val;
      mbbChanged = true;
      onlyTxpower = false;
    }
  }

  if (!wirelessChanged && !mbbChanged) {
    return ok({ status: "ok", note: "no changes" } satisfies WifiSettingsResult);
  }

  // Hot txpower apply cannot "fail" in a way the reply shows; no freeze needed.
  if (!(wirelessChanged && onlyTxpower)) beginWrite(ctx);
  Object.assign(uci, next);
  shared.wifiOn = nextWifiOn;

  if (wirelessChanged && onlyTxpower) {
    return ok({ status: "ok", hot: true } satisfies WifiSettingsResult);
  }
  return ok({ status: "ok" } satisfies WifiSettingsResult);
}

// ── /api/wifi/radio ─────────────────────────────────────────────────────────

/** wifi_radio.rs:339 radio_set → apply_locked: unconditional write + reload + poll. */
function radioSet(ctx: Ctx): Reply {
  if (ctx.body === undefined) return fail("invalid JSON", 400);
  const cur = observe().configured;
  const want = (k: string, c: boolean): boolean => {
    const v = bodyField(ctx.body, k);
    return typeof v === "boolean" ? v : c;
  };
  const ap2g = want("ap_2g", cur.ap_2g);
  const ap5g = want("ap_5g", cur.ap_5g);

  frozenLive = null; // apply_locked always reloads and verifies for real
  uci.ap2g_disabled = ap2g ? "0" : "1";
  uci.ap5g_disabled = ap5g ? "0" : "1";

  const obs = observe();
  const anyOn = ap2g || ap5g;
  const verified = anyOn
    ? obs.hostapd_processes > 0 && (!ap2g || obs.beaconing.wlan0) && (!ap5g || obs.beaconing.wlan2)
    : obs.hostapd_processes === 0;
  // Real device: off settles in < 10 s, on takes ~30 s (deadlines 20 s / 45 s).
  const delayMs = verified ? (anyOn ? 8000 : 3000) : anyOn ? 45000 : 20000;
  if (!verified) {
    const observed = { ...obs, verified: false, target: { ap_2g: ap2g, ap_5g: ap5g } };
    return {
      ...fail(
        `AP state not observable within deadline; target ap_2g=${ap2g} ap_5g=${ap5g}, observed ${JSON.stringify(observed)}`,
        503,
      ),
      delayMs,
    };
  }
  const data = {
    ...obs,
    verified: true,
    target: { ap_2g: ap2g, ap_5g: ap5g },
  } satisfies WifiRadioSetResult;
  return { data, delayMs };
}

// ── /api/wifi/guest ─────────────────────────────────────────────────────────

function guestStatus(ctx: Ctx): GuestWifi {
  return {
    ...clone(guest),
    remaining_seconds: guestRemaining(ctx),
  } satisfies GuestWifi;
}

/** wifi.rs:343 guest_map: body key → guest fields (both bands share one mock field for ssid/key/…). */
const GUEST_MAP: Record<string, (keyof GuestUci)[]> = {
  guest_ssid: ["ssid"],
  guest_key: ["key"],
  guest_encryption: ["encryption"],
  guest_disabled: ["disabled_2g", "disabled_5g"],
  guest_disabled_2g: ["disabled_2g"],
  guest_disabled_5g: ["disabled_5g"],
  guest_hidden: ["hidden"],
  guest_isolate: ["isolate"],
  guest_active_time: ["guest_active_time"],
};

/** wifi.rs:333 guest_set — no diffing: any known key counts as a change. */
function guestSet(ctx: Ctx): Reply {
  const obj = objectBody(ctx);
  if (isReply(obj)) return obj;

  const wasOn = computeLive().guest;
  const oldTime = guest.guest_active_time;
  const next: GuestUci = { ...guest };
  let changed = false;
  for (const [key, raw] of Object.entries(obj)) {
    const val = uciValue(raw);
    if (val === null) continue;
    const fields = GUEST_MAP[key];
    if (!fields) continue;
    for (const f of fields) next[f] = val;
    changed = true;
  }
  if (!changed) return ok({ status: "ok", note: "no changes" } satisfies GuestWifiResult);

  beginWrite(ctx);
  Object.assign(guest, next);
  const nowOn = computeLive().guest;
  const mins = parseInt(guest.guest_active_time, 10) || 0;
  if (!nowOn || mins <= 0) guestTimerStart = null;
  else if (!wasOn || oldTime !== guest.guest_active_time || guestTimerStart === null) {
    guestTimerStart = ctx.now; // firmware restarts the countdown (unconfirmed)
  }
  return ok({ status: "ok" } satisfies GuestWifiResult);
}

// ── Home Mode ───────────────────────────────────────────────────────────────

// The real agent's built-in defaults (homemode.rs:35) are the original
// author's home networks; these are made-up stand-ins.
const DEFAULT_SSIDS = ["HOME-WIFI", "HOME-WIFI-5G", "HOME-IOT"];

const home: { ssids: string[] | null; check_every: number; exit_misses: number } = {
  ssids: ["Home-Mesh", "Home-Mesh-5G"], // null = no ssids file → defaults
  check_every: 2,
  exit_misses: 2,
};

function homeModeGet(): HomeMode {
  const mode = shared.homeMode.mode === "home" ? "home" : "normal";
  return {
    enabled: shared.homeMode.enabled,
    ssids: clone(home.ssids ?? DEFAULT_SSIDS),
    using_default: home.ssids === null,
    default_ssids: clone(DEFAULT_SSIDS),
    mode,
    wifi_off: mode === "home",
    check_every: home.check_every,
    exit_misses: home.exit_misses,
  } satisfies HomeMode;
}

/** homemode.rs:177 — accept a non-negative integer or a numeric string. */
function asU32(v: unknown): number | null {
  if (typeof v === "number" && Number.isInteger(v) && v >= 0) return v;
  if (typeof v === "string" && /^\d+$/.test(v)) return parseInt(v, 10);
  return null;
}

/** homemode.rs:132 homemode_set — returns the new homemode_get. */
function homeModeSet(ctx: Ctx): Reply {
  const obj = objectBody(ctx);
  if (isReply(obj)) return obj;

  if ("ssids" in obj) {
    const arr = obj.ssids;
    if (!Array.isArray(arr)) return fail("'ssids' must be an array", 400);
    const list: string[] = [];
    for (const v of arr) {
      if (typeof v !== "string") return fail("'ssids' must be strings", 400);
      const s = v.trim();
      if (!s || s.includes("\n") || s.includes("\r")) continue;
      if (!list.includes(s)) list.push(s); // write_ssids de-dupes, keeps order
    }
    home.ssids = list; // even [] writes the file → using_default false
  }

  if ("enabled" in obj) {
    shared.homeMode.enabled = typeof obj.enabled === "boolean" ? obj.enabled : true;
  }

  const ce = asU32(obj.check_every);
  const em = asU32(obj.exit_misses);
  if (ce !== null) home.check_every = Math.min(60, Math.max(1, ce));
  if (em !== null) home.exit_misses = Math.min(30, Math.max(1, em));

  return ok(homeModeGet());
}

/** Nearby networks as seen from a hotel room in Taipei (strongest first). */
const NEARBY: HomeModeScan["networks"] = [
  { ssid: "Hotel-Guest-12F", signal: -47 },
  { ssid: "CHT Wi-Fi(HiNet)", signal: -58 },
  { ssid: "iTaiwan", signal: -63 },
  { ssid: "TPE-Free", signal: -66 },
  { ssid: "7-ELEVEN Free WiFi", signal: -71 },
  { ssid: "CHT3120", signal: -74 },
  { ssid: "TP-Link_8C2E", signal: -79 },
  { ssid: "ASUS_58_2G", signal: -83 },
  { ssid: "Fami_Free_WiFi", signal: -86 },
];

/**
 * homemode.rs:235 homemode_scan. wlan0 up → plain scan. wlan0 down and
 * wifi0.disabled=1 → wake 2.4 GHz, scan, put it back (woke_radio true, slow).
 * wlan0 down for another reason → empty list.
 */
function homeModeScan(ctx: Ctx): Reply {
  if (ctx.has("missing")) {
    // Realistic absence: the scan came back empty.
    return { data: { networks: [], woke_radio: false } satisfies HomeModeScan, delayMs: 3500 };
  }
  if (live().ap2g) {
    return { data: { networks: clone(NEARBY), woke_radio: false } satisfies HomeModeScan, delayMs: 3500 };
  }
  if (uci.radio2_disabled === "1") {
    return { data: { networks: clone(NEARBY), woke_radio: true } satisfies HomeModeScan, delayMs: 14000 };
  }
  return { data: { networks: [], woke_radio: false } satisfies HomeModeScan, delayMs: 1000 };
}

// Last nights at home before flying out on 2026-09-21. Device clock is local
// time labelled UTC, hence "+0000". Message texts are scripts/homemode.sh's.
const LOG_EVENTS = [
  "2026-09-17T19:42:10+0000 HOME detected (Home-Mesh) → Wi-Fi OFF (both radios disabled)",
  "2026-09-18T08:14:37+0000 HOME gone (2 misses) → Wi-Fi ON, back to normal",
  "2026-09-18T20:05:52+0000 HOME detected (Home-Mesh) → Wi-Fi OFF (both radios disabled)",
  "2026-09-19T09:31:04+0000 HOME gone (2 misses) → Wi-Fi ON, back to normal",
  "2026-09-19T18:57:21+0000 HOME detected (Home-Mesh-5G) → Wi-Fi OFF (both radios disabled)",
  "2026-09-20T12:10:46+0000 HOME gone (2 misses) → Wi-Fi ON, back to normal",
  "2026-09-20T21:48:13+0000 HOME detected (Home-Mesh) → Wi-Fi OFF (both radios disabled)",
  "2026-09-21T06:52:40+0000 HOME gone (2 misses) → Wi-Fi ON, back to normal",
].join("\n");

const LOG_SCANS = [
  "2026-09-21T06:30:12+0000 still HOME (Home-Mesh) → stay OFF",
  "2026-09-21T06:32:14+0000 still HOME (Home-Mesh) → stay OFF",
  "2026-09-21T06:34:13+0000 still HOME (Home-Mesh) → stay OFF",
  "2026-09-21T06:36:15+0000 still HOME (Home-Mesh) → stay OFF",
  "2026-09-21T06:38:14+0000 still HOME (Home-Mesh-5G) → stay OFF",
  "2026-09-21T06:40:12+0000 still HOME (Home-Mesh) → stay OFF",
  "2026-09-21T06:42:16+0000 still HOME (Home-Mesh) → stay OFF",
  "2026-09-21T06:44:13+0000 still HOME (Home-Mesh) → stay OFF",
  "2026-09-21T06:46:15+0000 still HOME (Home-Mesh) → stay OFF",
  "2026-09-21T06:48:38+0000 HOME not seen (miss 1/2) → stay OFF, recheck soon",
  "2026-09-21T06:50:39+0000 HOME not seen (miss 1/2) → stay OFF, recheck soon",
].join("\n");

function homeModeLog(): HomeModeLog {
  return { events: LOG_EVENTS, scans: LOG_SCANS } satisfies HomeModeLog;
}

// ── routes ──────────────────────────────────────────────────────────────────

export const routes: Route[] = [
  { method: "GET", path: "/api/wifi/status", handler: () => ok(wifiStatus()) },
  { method: "PUT", path: "/api/wifi/settings", handler: wifiSet },
  { method: "GET", path: "/api/wifi/radio", handler: () => ok(observe()) },
  { method: "PUT", path: "/api/wifi/radio", handler: radioSet },
  { method: "GET", path: "/api/wifi/guest", handler: (ctx) => ok(guestStatus(ctx)) },
  { method: "PUT", path: "/api/wifi/guest", handler: guestSet },
  { method: "GET", path: "/api/homemode", handler: () => ok(homeModeGet()) },
  { method: "PUT", path: "/api/homemode", handler: homeModeSet },
  { method: "GET", path: "/api/homemode/scan", handler: homeModeScan, ownMissing: true },
  { method: "GET", path: "/api/homemode/log", handler: () => ok(homeModeLog()) },
];
