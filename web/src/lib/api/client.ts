import { ApiError, ApiResp, TimeoutError, UnauthorizedError } from "./types";

// Request lifecycle (R7 timeouts, R11 token generations):
//
//   apiFetch(path, opts)
//     │ tokenAtSend = getToken()          (null for noAuth requests)
//     │ timer = timeoutMs ?? per-path override ?? (GET 8 s | other 15 s)
//     │ signal = caller signal ⊕ timer
//     ▼
//   fetch ──┬─ timer fired ─────────────────────────► TimeoutError (status 0)
//           ├─ other failure / caller abort ────────► ApiError("network error…", 0)
//           ├─ 401 ─┬─ tokenAtSend === getToken() ──► clear token, fire
//           │       │                                 UNAUTHORIZED_EVENT,
//           │       │                                 UnauthorizedError(staleToken=false)
//           │       └─ token changed since send ────► UnauthorizedError(staleToken=true)
//           │                                         (session untouched; caller's
//           │                                          resend rule applies — R2)
//           └─ 2xx/4xx/5xx ─► unwrap {ok,data,error} ─► data | ApiError(status)
//
//   The timer stays armed until the body has been read: a stalled body is a
//   hang too.

const TOKEN_KEY = "u60.token";
const BASE_KEY = "u60.agent_url";
const DEFAULT_BASE = "http://192.168.0.1:9090";

/** Default time limits (R7). */
export const READ_TIMEOUT_MS = 8000;
export const WRITE_TIMEOUT_MS = 15000;

/** Existing endpoints that legitimately block longer than the defaults, so
 *  pages that don't pass `timeoutMs` yet keep working. Keyed "METHOD path"
 *  (path without query string). Sources (zte-agent/src):
 *  - homemode/scan: Wi-Fi lock 5 s, scan, wake 2.4G + 4 s settle, 5 scan
 *    retries × 1.5 s, then reload_and_verify up to 45 s (homemode.rs:235-300;
 *    wifi_radio.rs:57-59,101)
 *  - scenario/scan: `iw` scan with retries (scenario.rs:1731, wifi_scan.rs)
 *  - scenario/apply, scenario/enabled (disable): run the scenario's Wi-Fi /
 *    CHILL actions inline (scenario.rs:1679-1726; wifi_radio::apply ≤ ~50 s)
 *  - wifi radio/settings/guest: Wi-Fi lock wait 5 s + reload verified up to
 *    45 s (wifi_radio.rs:57-59,101,339; wifi.rs:167) */
export const TIMEOUT_OVERRIDES: Readonly<Record<string, number>> = {
  "GET /api/homemode/scan": 120000,
  "GET /api/scenario/scan": 30000,
  "POST /api/scenario/apply": 120000,
  "PUT /api/scenario/enabled": 120000,
  "PUT /api/wifi/radio": 60000,
  "PUT /api/wifi/settings": 60000,
  "PUT /api/wifi/guest": 60000,
};

export function defaultTimeoutMs(method: string, path: string): number {
  const bare = path.split("?")[0];
  const o = TIMEOUT_OVERRIDES[`${method} ${bare}`];
  if (o !== undefined) return o;
  return method === "GET" ? READ_TIMEOUT_MS : WRITE_TIMEOUT_MS;
}

export function getApiBase(): string {
  if (typeof window === "undefined") return DEFAULT_BASE;
  const stored = window.localStorage.getItem(BASE_KEY);
  if (stored) return stored.replace(/\/$/, "");
  // Production: same origin (UI served by agent on :9090)
  // Dev (localhost:3000): fall back to default device IP
  if (window.location.hostname === "localhost" || window.location.hostname === "127.0.0.1") {
    return DEFAULT_BASE;
  }
  return window.location.origin;
}

export function setApiBase(url: string) {
  if (typeof window === "undefined") return;
  window.localStorage.setItem(BASE_KEY, url.replace(/\/$/, ""));
}

export function getToken(): string | null {
  if (typeof window === "undefined") return null;
  return window.localStorage.getItem(TOKEN_KEY);
}

/** Event fired when a request is rejected with 401, so the auth layer can
 *  drop the session and redirect to login (same tab — `storage` won't fire). */
export const UNAUTHORIZED_EVENT = "u60:unauthorized";

/** Event fired whenever a (new) token is stored — i.e. after a login. Write
 *  operations waiting in "relogin" and reads that failed with 401 listen for
 *  it (R1, R2). */
export const LOGIN_EVENT = "u60:login";

export function setToken(token: string | null) {
  if (typeof window === "undefined") return;
  if (token) {
    window.localStorage.setItem(TOKEN_KEY, token);
    window.dispatchEvent(new Event(LOGIN_EVENT));
  } else {
    window.localStorage.removeItem(TOKEN_KEY);
  }
}

/** Clear the session and notify listeners (AuthProvider) to redirect to login. */
function handleUnauthorized() {
  setToken(null);
  if (typeof window !== "undefined") {
    window.dispatchEvent(new Event(UNAUTHORIZED_EVENT));
  }
}

/** Apply the R11 rule to a 401 and return the error to throw.
 *  noAuth requests carry no token, so their 401 (e.g. wrong password on
 *  /api/auth/login) says nothing about the session: never cleared. */
function on401(tokenAtSend: string | null, noAuth: boolean | undefined): UnauthorizedError {
  if (noAuth) return new UnauthorizedError();
  if (tokenAtSend === getToken()) {
    handleUnauthorized();
    return new UnauthorizedError();
  }
  return new UnauthorizedError("unauthorized", true);
}

export interface FetchOptions {
  method?: "GET" | "POST" | "PUT" | "DELETE";
  body?: unknown;
  signal?: AbortSignal;
  /** Skip the {ok, data, error} envelope unwrap (for endpoints that return raw payloads). */
  raw?: boolean;
  /** Skip auth header (used by /api/auth/login). */
  noAuth?: boolean;
  /** Override the default time limit (GET 8 s, others 15 s). */
  timeoutMs?: number;
}

interface Deadline {
  signal: AbortSignal;
  timedOut: () => boolean;
  done: () => void;
}

/** A signal that aborts when the caller's signal aborts or `ms` elapses.
 *  Own AbortController + setTimeout (not AbortSignal.timeout) so fake timers
 *  drive it and a flag tells the two causes apart. */
function withDeadline(ms: number, caller?: AbortSignal): Deadline {
  const ctl = new AbortController();
  let fired = false;
  const timer = setTimeout(() => {
    fired = true;
    ctl.abort();
  }, ms);
  let signal: AbortSignal = ctl.signal;
  let unlink = () => {};
  if (caller) {
    const any = (AbortSignal as unknown as { any?: (s: AbortSignal[]) => AbortSignal }).any;
    if (typeof any === "function") {
      signal = any([caller, ctl.signal]);
    } else if (caller.aborted) {
      ctl.abort();
    } else {
      const onAbort = () => ctl.abort();
      caller.addEventListener("abort", onAbort, { once: true });
      unlink = () => caller.removeEventListener("abort", onAbort);
    }
  }
  return {
    signal,
    timedOut: () => fired,
    done: () => {
      clearTimeout(timer);
      unlink();
    },
  };
}

function failure(e: unknown, dl: Deadline, ms: number): ApiError {
  if (dl.timedOut()) return new TimeoutError(ms);
  if (e instanceof ApiError) return e;
  return new ApiError(`network error: ${(e as Error)?.message ?? String(e)}`, 0);
}

export async function apiFetch<T = unknown>(path: string, opts: FetchOptions = {}): Promise<T> {
  const url = `${getApiBase()}${path}`;
  const method = opts.method ?? "GET";
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  const tokenAtSend = opts.noAuth ? null : getToken();
  if (tokenAtSend) headers["Authorization"] = `Bearer ${tokenAtSend}`;

  const ms = opts.timeoutMs ?? defaultTimeoutMs(method, path);
  const dl = withDeadline(ms, opts.signal);
  try {
    let res: Response;
    try {
      res = await fetch(url, {
        method,
        headers,
        body: opts.body !== undefined ? JSON.stringify(opts.body) : undefined,
        signal: dl.signal,
      });
    } catch (e) {
      throw failure(e, dl, ms);
    }

    if (res.status === 401) throw on401(tokenAtSend, opts.noAuth);

    // LAN download streams binary; let caller handle via raw fetch instead.
    const ct = res.headers.get("content-type") ?? "";
    if (!ct.includes("application/json")) {
      if (!res.ok) throw new ApiError(`HTTP ${res.status}`, res.status);
      try {
        return (await res.text()) as unknown as T;
      } catch (e) {
        throw failure(e, dl, ms);
      }
    }

    let json: ApiResp<T>;
    try {
      json = (await res.json()) as ApiResp<T>;
    } catch (e) {
      if (dl.timedOut()) throw new TimeoutError(ms);
      throw new ApiError(`invalid JSON: ${(e as Error)?.message ?? String(e)}`, res.status);
    }
    if (opts.raw) return json as unknown as T;

    if (!json.ok) {
      throw new ApiError(json.error || `HTTP ${res.status}`, res.status);
    }
    return (json.data ?? (undefined as unknown)) as T;
  } finally {
    dl.done();
  }
}

export async function apiFetchBinary(
  path: string,
  opts: { signal?: AbortSignal; timeoutMs?: number } = {}
): Promise<ArrayBuffer> {
  const url = `${getApiBase()}${path}`;
  const headers: Record<string, string> = {};
  const tokenAtSend = getToken();
  if (tokenAtSend) headers["Authorization"] = `Bearer ${tokenAtSend}`;
  const ms = opts.timeoutMs ?? defaultTimeoutMs("GET", path);
  const dl = withDeadline(ms, opts.signal);
  try {
    let res: Response;
    try {
      res = await fetch(url, { headers, signal: dl.signal });
    } catch (e) {
      throw failure(e, dl, ms);
    }
    if (res.status === 401) throw on401(tokenAtSend, false);
    if (!res.ok) throw new ApiError(`HTTP ${res.status}`, res.status);
    try {
      return await res.arrayBuffer();
    } catch (e) {
      throw failure(e, dl, ms);
    }
  } finally {
    dl.done();
  }
}
