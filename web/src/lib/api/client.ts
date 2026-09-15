import { ApiError, ApiResp, UnauthorizedError } from "./types";

const TOKEN_KEY = "u60.token";
const BASE_KEY = "u60.agent_url";
const DEFAULT_BASE = "http://192.168.0.1:9090";

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

export function setToken(token: string | null) {
  if (typeof window === "undefined") return;
  if (token) window.localStorage.setItem(TOKEN_KEY, token);
  else window.localStorage.removeItem(TOKEN_KEY);
}

/** Event fired when a request is rejected with 401, so the auth layer can
 *  drop the session and redirect to login (same tab — `storage` won't fire). */
export const UNAUTHORIZED_EVENT = "u60:unauthorized";

/** Clear the session and notify listeners (AuthProvider) to redirect to login. */
function handleUnauthorized() {
  setToken(null);
  if (typeof window !== "undefined") {
    window.dispatchEvent(new Event(UNAUTHORIZED_EVENT));
  }
}

export interface FetchOptions {
  method?: "GET" | "POST" | "PUT" | "DELETE";
  body?: unknown;
  signal?: AbortSignal;
  /** Skip the {ok, data, error} envelope unwrap (for endpoints that return raw payloads). */
  raw?: boolean;
  /** Skip auth header (used by /api/auth/login). */
  noAuth?: boolean;
}

export async function apiFetch<T = unknown>(path: string, opts: FetchOptions = {}): Promise<T> {
  const url = `${getApiBase()}${path}`;
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (!opts.noAuth) {
    const tok = getToken();
    if (tok) headers["Authorization"] = `Bearer ${tok}`;
  }

  let res: Response;
  try {
    res = await fetch(url, {
      method: opts.method ?? "GET",
      headers,
      body: opts.body !== undefined ? JSON.stringify(opts.body) : undefined,
      signal: opts.signal,
    });
  } catch (e) {
    throw new ApiError(`network error: ${(e as Error).message}`, 0);
  }

  if (res.status === 401) {
    handleUnauthorized();
    throw new UnauthorizedError();
  }

  // LAN download streams binary; let caller handle via raw fetch instead.
  const ct = res.headers.get("content-type") ?? "";
  if (!ct.includes("application/json")) {
    if (!res.ok) throw new ApiError(`HTTP ${res.status}`, res.status);
    return (await res.text()) as unknown as T;
  }

  const json = (await res.json()) as ApiResp<T>;
  if (opts.raw) return json as unknown as T;

  if (!json.ok) {
    throw new ApiError(json.error || `HTTP ${res.status}`, res.status);
  }
  return (json.data ?? (undefined as unknown)) as T;
}

export async function apiFetchBinary(path: string, opts: { signal?: AbortSignal } = {}): Promise<ArrayBuffer> {
  const url = `${getApiBase()}${path}`;
  const headers: Record<string, string> = {};
  const tok = getToken();
  if (tok) headers["Authorization"] = `Bearer ${tok}`;
  const res = await fetch(url, { headers, signal: opts.signal });
  if (res.status === 401) {
    handleUnauthorized();
    throw new UnauthorizedError();
  }
  if (!res.ok) throw new ApiError(`HTTP ${res.status}`, res.status);
  return res.arrayBuffer();
}
