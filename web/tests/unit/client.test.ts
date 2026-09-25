import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  apiFetch,
  apiFetchBinary,
  getToken,
  LOGIN_EVENT,
  READ_TIMEOUT_MS,
  setToken,
  UNAUTHORIZED_EVENT,
  WRITE_TIMEOUT_MS,
  defaultTimeoutMs,
} from "@/lib/api/client";
import { ApiError, TimeoutError, UnauthorizedError } from "@/lib/api/types";
import { deferred, hangingFetch, jsonResponse, stubWindow } from "./windowStub";

let win: ReturnType<typeof stubWindow>;

beforeEach(() => {
  win = stubWindow();
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

async function settle<T>(p: Promise<T>): Promise<{ ok: T } | { err: unknown }> {
  try {
    return { ok: await p };
  } catch (err) {
    return { err };
  }
}

describe("apiFetch basics", () => {
  it("unwraps the envelope and sends the bearer token", async () => {
    setToken("t1");
    const f = vi.fn(async () => jsonResponse(200, { ok: true, data: { a: 1 } }));
    vi.stubGlobal("fetch", f);
    await expect(apiFetch("/api/x")).resolves.toEqual({ a: 1 });
    const init = (f.mock.calls[0] as unknown as [string, RequestInit])[1];
    expect((init.headers as Record<string, string>)["Authorization"]).toBe("Bearer t1");
  });

  it("device error keeps its status", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse(500, { ok: false, error: "boom" })));
    const r = await settle(apiFetch("/api/x", { method: "POST", body: {} }));
    expect("err" in r && r.err instanceof ApiError && r.err.status === 500 && r.err.message === "boom").toBe(true);
  });

  it("network failure is ApiError status 0, not TimeoutError", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => { throw new TypeError("Failed to fetch"); }));
    const r = await settle(apiFetch("/api/x"));
    expect("err" in r && r.err instanceof ApiError).toBe(true);
    expect("err" in r && r.err instanceof TimeoutError).toBe(false);
    expect("err" in r && (r.err as ApiError).status).toBe(0);
  });
});

describe("timeouts (R7)", () => {
  beforeEach(() => vi.useFakeTimers());

  it("GET times out after 8 s", async () => {
    vi.stubGlobal("fetch", hangingFetch());
    const p = settle(apiFetch("/api/network/signal"));
    await vi.advanceTimersByTimeAsync(READ_TIMEOUT_MS - 1);
    let done = false;
    void p.then(() => (done = true));
    await Promise.resolve();
    expect(done).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    const r = await p;
    expect("err" in r && r.err instanceof TimeoutError).toBe(true);
    expect("err" in r && (r.err as TimeoutError).status).toBe(0);
    expect("err" in r && (r.err as TimeoutError).timeoutMs).toBe(8000);
  });

  it("non-GET times out after 15 s", async () => {
    vi.stubGlobal("fetch", hangingFetch());
    const p = settle(apiFetch("/api/network/apn", { method: "POST", body: {} }));
    await vi.advanceTimersByTimeAsync(READ_TIMEOUT_MS + 1);
    let done = false;
    void p.then(() => (done = true));
    await Promise.resolve();
    expect(done).toBe(false);
    await vi.advanceTimersByTimeAsync(WRITE_TIMEOUT_MS - READ_TIMEOUT_MS);
    const r = await p;
    expect("err" in r && r.err instanceof TimeoutError && r.err.timeoutMs === 15000).toBe(true);
  });

  it("timeoutMs overrides the default", async () => {
    vi.stubGlobal("fetch", hangingFetch());
    const p = settle(apiFetch("/api/speedtest/start", { method: "POST", body: {}, timeoutMs: 60000 }));
    await vi.advanceTimersByTimeAsync(WRITE_TIMEOUT_MS + 1000);
    let done = false;
    void p.then(() => (done = true));
    await Promise.resolve();
    expect(done).toBe(false);
    await vi.advanceTimersByTimeAsync(60000);
    const r = await p;
    expect("err" in r && r.err instanceof TimeoutError && r.err.timeoutMs === 60000).toBe(true);
  });

  it("a short override fires early", async () => {
    vi.stubGlobal("fetch", hangingFetch());
    const p = settle(apiFetch("/api/x", { timeoutMs: 500 }));
    await vi.advanceTimersByTimeAsync(500);
    const r = await p;
    expect("err" in r && r.err instanceof TimeoutError).toBe(true);
  });

  it("per-path overrides for existing slow endpoints", () => {
    expect(defaultTimeoutMs("GET", "/api/homemode/scan")).toBe(120000);
    expect(defaultTimeoutMs("POST", "/api/scenario/apply")).toBe(120000);
    expect(defaultTimeoutMs("PUT", "/api/wifi/settings")).toBe(60000);
    expect(defaultTimeoutMs("GET", "/api/wifi/settings")).toBe(READ_TIMEOUT_MS);
    expect(defaultTimeoutMs("GET", "/api/homemode/scan?x=1")).toBe(120000);
  });

  it("caller abort is a network error, not a timeout", async () => {
    vi.stubGlobal("fetch", hangingFetch());
    const ctl = new AbortController();
    const p = settle(apiFetch("/api/x", { signal: ctl.signal }));
    ctl.abort();
    const r = await p;
    expect("err" in r && r.err instanceof ApiError && !(r.err instanceof TimeoutError)).toBe(true);
  });

  it("a stalled body read also times out", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_u: string, init?: RequestInit) => {
        const body = new ReadableStream({
          start(c) {
            init?.signal?.addEventListener("abort", () => c.error(new DOMException("aborted", "AbortError")));
          },
        });
        return new Response(body, { status: 200, headers: { "content-type": "application/json" } });
      })
    );
    const p = settle(apiFetch("/api/x"));
    await vi.advanceTimersByTimeAsync(READ_TIMEOUT_MS);
    const r = await p;
    expect("err" in r && r.err instanceof TimeoutError).toBe(true);
  });

  it("apiFetchBinary uses the read timeout", async () => {
    vi.stubGlobal("fetch", hangingFetch());
    const p = settle(apiFetchBinary("/api/lan/file"));
    await vi.advanceTimersByTimeAsync(READ_TIMEOUT_MS);
    const r = await p;
    expect("err" in r && r.err instanceof TimeoutError).toBe(true);
  });
});

describe("401 token generations (R11)", () => {
  function listen(name: string) {
    const seen = { n: 0 };
    win.addEventListener(name, () => seen.n++);
    return seen;
  }

  it("401 for the current token clears it and fires the event", async () => {
    setToken("old");
    const unauth = listen(UNAUTHORIZED_EVENT);
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse(401, { ok: false, error: "unauthorized" })));
    const r = await settle(apiFetch("/api/x"));
    expect("err" in r && r.err instanceof UnauthorizedError).toBe(true);
    expect("err" in r && (r.err as UnauthorizedError).staleToken).toBe(false);
    expect(getToken()).toBeNull();
    expect(unauth.n).toBe(1);
  });

  it("a late 401 for an old token does not log out the new login", async () => {
    setToken("old");
    const d = deferred<Response>();
    vi.stubGlobal("fetch", vi.fn(() => d.promise));
    const p = settle(apiFetch("/api/x"));
    setToken("new"); // user logged in again while the old request was in flight
    const unauth = listen(UNAUTHORIZED_EVENT);
    d.resolve(jsonResponse(401, { ok: false, error: "unauthorized" }));
    const r = await p;
    expect("err" in r && r.err instanceof UnauthorizedError).toBe(true);
    expect("err" in r && (r.err as UnauthorizedError).staleToken).toBe(true);
    expect(getToken()).toBe("new");
    expect(unauth.n).toBe(0);
  });

  it("apiFetchBinary follows the same rule", async () => {
    setToken("old");
    const d = deferred<Response>();
    vi.stubGlobal("fetch", vi.fn(() => d.promise));
    const p = settle(apiFetchBinary("/api/lan/file"));
    setToken("new");
    d.resolve(new Response("", { status: 401 }));
    const r = await p;
    expect("err" in r && (r.err as UnauthorizedError).staleToken).toBe(true);
    expect(getToken()).toBe("new");

    vi.stubGlobal("fetch", vi.fn(async () => new Response("", { status: 401 })));
    const r2 = await settle(apiFetchBinary("/api/lan/file"));
    expect("err" in r2 && (r2.err as UnauthorizedError).staleToken).toBe(false);
    expect(getToken()).toBeNull();
  });

  it("a noAuth 401 (wrong password) leaves the session alone", async () => {
    setToken("keep");
    const unauth = listen(UNAUTHORIZED_EVENT);
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse(401, { ok: false, error: "invalid password" })));
    const r = await settle(apiFetch("/api/auth/login", { method: "POST", body: { password: "x" }, noAuth: true }));
    expect("err" in r && r.err instanceof UnauthorizedError).toBe(true);
    expect(getToken()).toBe("keep");
    expect(unauth.n).toBe(0);
  });

  it("storing a token fires LOGIN_EVENT", () => {
    const login = listen(LOGIN_EVENT);
    setToken("t");
    setToken(null);
    expect(login.n).toBe(1);
  });
});
