// Response shapes for the unauthenticated public status endpoint.
// Types only — no runtime code (see web/scripts/mock-agent).

/**
 * GET /api/public/status — no login needed (server.rs:98 allow-list).
 * Handler: zte-agent/src/public.rs:29 `public_status`. The agent builds this
 * JSON itself, so this shape follows the Rust code; the pages (login,
 * dashboard, StatusStrip, deviceClock) each read a subset.
 */
export interface PublicStatus {
  network: {
    /** `zwrt_data get_wwaniface` connect_status contains "connected" (public.rs:50). */
    connected: boolean;
    /** nwinfo `network_type` as-is: "SA" | "NSA" | "LTE" | "NO_SERVICE" | "LIMITED_SERVICE…" | "" */
    type: string;
    /** `network_provider_fullname`, falling back to `network_provider`; "" when unknown. */
    operator: string;
    /** `signalbar` parsed as int; -1 when missing/unparseable. */
    bar: number;
    /** `lte_rsrp` when type == "LTE", otherwise `nr5g_rsrp`; 0 when missing. */
    rsrp: number;
  };
  wifi: {
    /** `zwrt_wlan report` wifi_onoff == "1"; false when the ubus call fails. */
    on: boolean;
    /** main2g_ssid, else main5g_ssid (still reported when Wi-Fi is off). */
    ssid: string;
  };
  battery: {
    /** sysfs capacity; -1 when unreadable. */
    percent: number;
    charging: boolean;
  };
  sms: {
    /** sms_dev_unread_num + sms_sim_unread_num; 0 when the capacity call fails. */
    unread: number; // page types this as optional (login, dashboard, StatusStrip)
  };
  services: {
    tailscale: {
      /** `pidof tailscaled` non-empty. */
      running: boolean;
      /** /data/tailscale/tailscale exists. */
      installed: boolean; // page types this as optional
      /**
       * First two columns of the first `tailscale status` line:
       * "<100.x.y.z> <hostname>" — not just the hostname. "" when not running.
       */
      node: string;
    };
    chill: {
      /** /tmp/chill.state `state`; "unknown" when the file is missing/unparseable. */
      state: string;
      /** /tmp/chill.state `reason`, null when absent (e.g. "overheat", "lowmem"). */
      reason: string | null; // page types this as optional
      /** Owner's on/off switch: /data/chill/disabled absent (chill.rs:492). Pages don't read it. */
      on: boolean;
    };
    home_mode: {
      /** /data/homemode.sh exists. */
      present: boolean;
      /** present && /data/homemode/disabled absent. */
      enabled: boolean;
      /** First word of /data/homemode/state, default "normal" (pages compare with "home"). */
      mode: string;
    };
  };
  /** scenario.rs:1582 `public_summary`. Not read by any page today. */
  scenario: {
    configured: boolean;
    enabled: boolean;
    /** Current scenario id, e.g. "abroad", "home", or "" before the first decision. */
    current: string;
    name: string;
    wifi_off: boolean;
    abroad: boolean;
    chill_on_when_home: boolean;
    auto_direct: boolean;
    /** Pinned scenario id, null when not pinned. */
    pin: string | null;
    guard_takeover: boolean;
    /** Device-clock unix seconds of the last switch, null if never. */
    last_switch: number | null;
  };
  /** alerts.rs:257 — count only. Not read by any page (AlertBanner uses /api/alerts). */
  alerts: { unread: number };
  /** clock.rs:57 — seconds the device clock runs ahead of real UTC (lib/deviceClock.ts reads it). */
  clock: { utc_offset: number };
  /**
   * `zwrt_common_info.common_config` read once: model_name ("MU5250") and
   * device_market_name ("U60 Pro"; "TOPFLOW" becomes "TopFlow"). "" when unreadable.
   */
  device: { model: string; name: string };
  /** health.rs:133 — counts only. */
  health: { checked: boolean; bad: number; warn: number };
}

/** Endpoint map for record.ts. */
export interface PublicGetMap {
  "/api/public/status": PublicStatus;
}
