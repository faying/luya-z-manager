// Response shapes for the sidecar-service endpoints (Tailscale and the other
// add-on services). Types only — no runtime code.
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
/** GET endpoints of this area (paths without query string). */
export interface ServicesGetMap {
  "/api/services/tailscale": TailscaleStatus;
  "/api/services/tailscale/log": TailscaleLog;
}
