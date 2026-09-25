// Response shapes for the Wi-Fi, guest Wi-Fi, AP radio and Home Mode endpoints
// of zte-agent (zte-agent/src/wifi.rs, wifi_radio.rs, homemode.rs).
//
// Every handler here builds its JSON itself, so the Rust shape is the source
// of truth; page-side disagreements are noted inline. Types only — no runtime
// code.

/**
 * GET /api/wifi/status — wifi.rs:80 `wifi_status`.
 *
 * Every string field is a raw `uci get` value; a missing key (or a failed
 * `uci get`) is "" — never absent, never null (wifi.rs:12-18).
 * `wifi_onoff` / `wifi6_switch` come from `zte_mbb.wifi.*`, falling back to
 * `ubus zwrt_wlan report`, then to "1" / "0" (wifi.rs:84-103).
 *
 * Page disagreements:
 * - router/wifi/page.tsx types this as `Record<string, string>`, but the three
 *   `clients_*` fields are numbers.
 * - router/wifi/page.tsx also falls back to `ssid`, `password`, `key`,
 *   `encryption`, `channel`, `bandwidth`, `tx_power` — the handler never
 *   returns any of those names.
 */
export interface WifiStatus {
  /** "1" = on, "0" = off (zte_mbb.wifi.wifi_onoff). */
  wifi_onoff: string;
  /** "1" = 802.11ax on (zte_mbb.wifi.wifi6_switch). */
  wifi6_switch: string;
  /** wireless.wifi0.disabled — "1" = 2.4 GHz radio off; "" = enabled. */
  radio2_disabled: string;
  /** wireless.wifi1.disabled — "1" = 5 GHz radio off. */
  radio5_disabled: string;
  /** Configured channel ("auto" or a number as a string). */
  channel_2g: string;
  channel_5g: string;
  /** wireless.wifiN.txpowerpercent: "25" | "50" | "100" on the page. */
  txpower_2g: string;
  txpower_5g: string;
  /** wireless.wifiN.htmode, e.g. "HT20", "VHT80", "auto". */
  htmode_2g: string;
  htmode_5g: string;
  /** wireless.wifi0.country (two letters, upper case). Written via PUT `country`. */
  country_code: string;
  ssid_2g: string;
  ssid_5g: string;
  /** Plain-text passphrases. */
  key_2g: string;
  key_5g: string;
  /** "psk2+ccmp" | "psk3+ccmp" | "psk2+psk3+ccmp" | "none". */
  encryption_2g: string;
  encryption_5g: string;
  /** "1" = hidden. */
  hidden_2g: string;
  hidden_5g: string;
  /** From `iw wlan0 info` ("channel N ..."); "" when the interface is down. */
  actual_channel_2g: string;
  /** From `iw wlan0 info` "width: 20 MHz" → "20 MHz"; "" when down. */
  actual_bw_2g: string;
  /** From `iw wlan2 info`. */
  actual_channel_5g: string;
  actual_bw_5g: string;
  /** `iw wlanN station dump | grep -c Station`; 0 on failure. */
  clients_2g: number;
  clients_5g: number;
  clients_total: number;
  /** wireless.guest_2g.disabled / guest_5g.disabled; "" if the section is absent. */
  guest_disabled_2g: string;
  guest_disabled_5g: string;
  /** wireless.guest_2g.ssid. */
  guest_ssid: string;
}

/**
 * PUT /api/wifi/settings body — wifi.rs:155 `wifi_set`.
 * Any subset; values may be strings, numbers or booleans (true→"1").
 * Unknown keys are ignored. `country` must be two letters or it is ignored.
 */
export interface WifiSettingsBody {
  ssid_2g?: string;
  ssid_5g?: string;
  key_2g?: string;
  key_5g?: string;
  encryption_2g?: string;
  encryption_5g?: string;
  hidden_2g?: string;
  hidden_5g?: string;
  channel_2g?: string;
  channel_5g?: string;
  txpower_2g?: string;
  txpower_5g?: string;
  htmode_2g?: string;
  htmode_5g?: string;
  radio2_disabled?: string;
  radio5_disabled?: string;
  wifi_onoff?: string;
  wifi6_switch?: string;
  country?: string;
}

/**
 * PUT /api/wifi/settings response data — wifi.rs:276/291/300.
 * `note: "no changes"` when nothing differed; `hot: true` when only txpower
 * changed (applied with `iw set txpower`); otherwise just `status` and the
 * reload runs in the background after the reply.
 */
export interface WifiSettingsResult {
  status: "ok";
  note?: "no changes";
  hot?: true;
}

/**
 * GET /api/wifi/radio — wifi_radio.rs:329 `radio_get` (→ `observe`, :268).
 * Read-only: uci get + `ps w` + `hostapd_cli status` per AP interface.
 */
export interface WifiRadio {
  /** AP interfaces as configured in uci (main_2g/main_5g.disabled != "1"). Not the radios. */
  configured: { ap_2g: boolean; ap_5g: boolean };
  /** Running hostapd processes; usize::MAX (18446744073709551615) if `ps` failed. */
  hostapd_processes: number;
  /** hostapd_cli reports state=ENABLED on that interface. */
  beaconing: { wlan0: boolean; wlan2: boolean };
}

/** PUT /api/wifi/radio body — wifi_radio.rs:339. Missing key = keep current. */
export interface WifiRadioSetBody {
  ap_2g?: boolean;
  ap_5g?: boolean;
}

/**
 * PUT /api/wifi/radio response data — wifi_radio.rs:310-317. Only returned
 * when verified; otherwise 503 with the observed state in the error string.
 */
export interface WifiRadioSetResult extends WifiRadio {
  verified: boolean;
  target: { ap_2g: boolean; ap_5g: boolean };
  /** Present only when `ubus zwrt_wlan reload` returned an error. */
  reload_error?: string;
}

/**
 * GET /api/wifi/guest — wifi.rs:307 `guest_status`.
 * All strings are raw `uci get wireless.guest_2g.*` ("" when absent).
 *
 * Page (router/wifi-guest/page.tsx) also declares `guest_onoff`, `guest_ssid`,
 * `guest_key`, `guest_encryption`, `guest_isolate`, `active_time`,
 * `guest_disabled_2g`, `guest_disabled_5g` — the handler returns none of
 * those names (they are the PUT body names); the page falls back correctly.
 */
export interface GuestWifi {
  ssid: string;
  key: string;
  encryption: string;
  /** "1" = guest 2.4 GHz AP disabled. */
  disabled_2g: string;
  disabled_5g: string;
  hidden: string;
  /** "1" = client isolation. */
  isolate: string;
  /** Minutes, "0" = unlimited (unit from the page label; unconfirmed). */
  guest_active_time: string;
  /**
   * `ubus zwrt_wlan wlan_get_guest_access_left_time` → guest_left_time, parsed
   * to an integer; -1 when the call fails. Seconds (page counts it down per
   * second). What the firmware returns while the guest AP is off is
   * unconfirmed.
   */
  remaining_seconds: number;
}

/** PUT /api/wifi/guest body — wifi.rs:333. Each key writes guest_2g and/or guest_5g. */
export interface GuestWifiBody {
  guest_ssid?: string;
  guest_key?: string;
  guest_encryption?: string;
  /** Sets both bands. */
  guest_disabled?: string;
  guest_disabled_2g?: string;
  guest_disabled_5g?: string;
  guest_hidden?: string;
  guest_isolate?: string;
  guest_active_time?: string;
}

/** PUT /api/wifi/guest response data — wifi.rs:381/391. */
export interface GuestWifiResult {
  status: "ok";
  note?: "no changes";
}

/**
 * GET /api/homemode — homemode.rs:103 `homemode_get`. Reads local files only.
 * Also the response of PUT /api/homemode (homemode.rs:192).
 */
export interface HomeMode {
  /** `/data/homemode/disabled` does not exist. */
  enabled: boolean;
  /** Configured list, or the built-in defaults when using_default. */
  ssids: string[];
  /** `/data/homemode/ssids` absent (an empty file is an explicit empty list). */
  using_default: boolean;
  default_ssids: string[];
  /** First word of `/data/homemode/state`; "normal" when absent. */
  mode: "home" | "normal";
  /** mode === "home" (the worker has turned Wi-Fi off). */
  wifi_off: boolean;
  /** Minutes between rechecks while at home, 1–60 (default 2). */
  check_every: number;
  /** Missed rechecks before Wi-Fi returns, 1–30 (default 2). */
  exit_misses: number;
}

/** PUT /api/homemode body — homemode.rs:132. Numbers may also be numeric strings. */
export interface HomeModeBody {
  enabled?: boolean;
  ssids?: string[];
  check_every?: number | string;
  exit_misses?: number | string;
}

/**
 * GET /api/homemode/scan — homemode.rs:235 `homemode_scan`.
 * One entry per SSID (strongest BSS kept), strongest first; hidden SSIDs
 * dropped. `signal` is dBm (f64, e.g. -48).
 *
 * page expects an optional `note` (router/home-mode/page.tsx:132); the
 * handler never returns it, and returns `woke_radio`, which the page ignores.
 */
export interface HomeModeScan {
  networks: { ssid: string; signal: number }[];
  /** 2.4 GHz was off (home mode) and was woken just for this scan. */
  woke_radio: boolean;
}

/**
 * GET /api/homemode/log — homemode.rs:314 `homemode_log`.
 * Last 200 lines of each file, oldest first, "\n"-joined; "" when absent.
 * Line format (scripts/homemode.sh:99): `<date +%Y-%m-%dT%H:%M:%S%z> <message>`.
 */
export interface HomeModeLog {
  /** /data/log/homemode.log — Wi-Fi on/off transitions. */
  events: string;
  /** /data/log/homemode-scan.log — periodic rechecks while at home. */
  scans: string;
}

/** GET endpoints of this area (path without query → response data). */
export interface WifiGetMap {
  "/api/wifi/status": WifiStatus;
  "/api/wifi/radio": WifiRadio;
  "/api/wifi/guest": GuestWifi;
  "/api/homemode": HomeMode;
  "/api/homemode/scan": HomeModeScan;
  "/api/homemode/log": HomeModeLog;
}
