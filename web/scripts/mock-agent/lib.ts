// Shared contract for the mock agent: route table entries, request context,
// scenario names and small reply helpers. Area fixture modules
// (fixtures/<area>.ts) export `routes: Route[]` built on these types.
//
// Node runs this file directly (type stripping), so only erasable TypeScript
// syntax is allowed here and in every file that imports it: no enums, no
// namespaces, no parameter properties; `import type` for types; explicit
// `.ts` extensions on relative imports.

export const SCENARIOS = [
  "normal",
  "weak", // poor signal (RSRP around -115, SINR around -2)
  "nosignal", // no service: disconnected, radio values null
  "stale", // signal + speed endpoints hang past the client's 9 s timeout
  "missing", // GET payloads with fields absent or null
  "cmdfail", // every write returns 500 {ok:false,error:"mock failure"}
  "fakesuccess", // 200 ok:true but downstream failed (e.g. tailscale data.error)
  "reboot-token", // POST /api/device/reboot -> 20 s of dropped sockets, then all tokens invalid
  "step2-timeout", // second write of a multi-step op hangs 20 s
  "down", // every socket destroyed (except /__mock/*)
  "carriers8", // signal page carrier table gets 8 carriers
  "firmware-b27", // shapes recorded from a real U60 Pro on firmware B27 (2026-09-25): STC Method-not-found 503s, {} lists, numeric APN, SMS per-box counts…
  "old-agent-names", // agent before 2026-09-25: calls router_get_upnp_switch / router_get_qos_switch, which B27 lacks → 503 Method not found
  "apn-manual-pick", // auto APN mode, first manual profile still isEnable (owner's device 2026-09-25: isEnable = manual mode's pick, not in use)
  "nbrscan", // POST /api/cell/neighbors/scan keeps the old simulated scan (the real device answers 410; cell-lock tests use this)
] as const;

export type Scenario = (typeof SCENARIOS)[number];

export type Method = "GET" | "POST" | "PUT" | "DELETE";

export interface Ctx {
  method: Method;
  /** Path without query string, e.g. "/api/network/signal". */
  path: string;
  query: URLSearchParams;
  /** Parsed JSON body (undefined when empty or not JSON). */
  body: unknown;
  /** Active scenarios for this request. */
  scenarios: ReadonlySet<Scenario>;
  has(s: Scenario): boolean;
  /** Date.now() at request start. */
  now: number;
}

/** What a handler returns. The server wraps `data` in {ok:true,data}. */
export interface Reply {
  status?: number; // default 200 (or 500 when error is set)
  data?: unknown;
  /** When set, envelope is {ok:false,error}. */
  error?: string;
  /** Send this value as the whole JSON body, no envelope (rare). */
  raw?: unknown;
  /** Delay the reply this many ms before sending. */
  delayMs?: number;
}

export type Handler = (ctx: Ctx) => Reply | Promise<Reply>;

export interface Route {
  method: Method;
  path: string;
  handler: Handler;
  /**
   * "read" routes are exempt from cmdfail / step2-timeout and get the
   * `missing` transform. Default: GET = read, everything else = write.
   * Set explicitly for POST endpoints the agent uses as reads
   * (e.g. POST /api/sms/list, POST /api/device/power-save).
   */
  kind?: "read" | "write";
  /** Opt out of the generic `missing` transform (handler does its own). */
  ownMissing?: boolean;
}

export function ok(data?: unknown): Reply {
  return { data };
}

export function fail(error: string, status = 500): Reply {
  return { status, error };
}

/**
 * The agent's reply when the firmware has no such ubus method, as recorded on
 * B27 (router.rs / cell.rs passthrough: 503 + ubus.rs error text).
 */
export function methodNotFound(obj: string, method: string): Reply {
  return fail(`ubus call ${obj} ${method} failed: Command failed: ubus call ${obj} ${method} {} (Method not found)`, 503);
}

/** Read a field from an unknown JSON body. */
export function bodyField(body: unknown, key: string): unknown {
  if (body && typeof body === "object" && !Array.isArray(body)) {
    return (body as Record<string, unknown>)[key];
  }
  return undefined;
}

/** Shallow-merge known keys of `body` into `target` (only keys already present). */
export function mergeKnown<T extends object>(target: T, body: unknown): T {
  if (body && typeof body === "object" && !Array.isArray(body)) {
    const src = body as Record<string, unknown>;
    const t = target as Record<string, unknown>;
    for (const k of Object.keys(src)) {
      if (k in t) t[k] = src[k];
    }
  }
  return target;
}

/** Deep clone JSON-shaped fixture data so handlers never hand out live state. */
export function clone<T>(v: T): T {
  return structuredClone(v);
}

/** Unix seconds. */
export function unixNow(): number {
  return Math.floor(Date.now() / 1000);
}

/**
 * Generic `missing` transform: walks the payload and, for plain objects,
 * alternately deletes a primitive field or sets it to null. Arrays keep their
 * length (their object elements are transformed). Deterministic.
 */
export function stripFields(v: unknown): unknown {
  let n = 0;
  const walk = (x: unknown): unknown => {
    if (Array.isArray(x)) return x.map(walk);
    if (x && typeof x === "object") {
      const out: Record<string, unknown> = {};
      for (const [k, val] of Object.entries(x as Record<string, unknown>)) {
        if (val !== null && typeof val === "object") {
          out[k] = walk(val);
          continue;
        }
        n++;
        if (n % 3 === 0) continue; // absent
        out[k] = n % 3 === 1 ? null : val; // null, or keep
      }
      return out;
    }
    return x;
  };
  return walk(v);
}
