// Cross-area mock state. Values that more than one endpoint reports (e.g.
// /api/public/status summarises Wi-Fi, CHILL, Tailscale and SMS) live here so
// a write in one area is visible in another area's readback. Area-private
// state stays inside fixtures/<area>.ts.

export const shared = {
  /** Wi-Fi master switch (wifi/status wifi_onoff, public/status wifi.on). */
  wifiOn: true,
  /** Airplane / low-power mode (modem). When true the modem is offline. */
  airplane: false,
  /** Mobile data switch (modem/data). */
  mobileData: true,
  /** CHILL state as the touch screen / public status sees it. */
  chill: {
    /** "running" | "direct" | "stopped" | "starting" | "unknown" */
    state: "running" as string,
    /** "proxy" | "global" | "direct_keep_ai" | "direct_all" (chill.rs exit_set) */
    exit: "proxy" as string,
    region: "TW" as string,
    profile: "standard" as string,
  },
  tailscale: {
    running: true,
    node: "u60-pro",
  },
  sms: {
    unread: 2,
  },
  homeMode: {
    present: true,
    enabled: false,
    mode: "normal" as string, // "home" | "normal" (homemode.rs)
  },
  /** Band lock readback. null = not locked (all bands). Strings are what the page sent, e.g. "78" or "1,3,7". */
  bandLock: {
    nr: null as string | null,
    lte: null as string | null,
  },
  alertsUnread: 1,
};

export type SharedState = typeof shared;
