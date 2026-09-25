import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  FreshState,
  INITIAL_FRESH,
  checkValid,
  freshNow,
  getFresh,
  isStale,
  nextFresh,
  recordOutcome,
  resetFreshStore,
  staleAfterMs,
  stoppedAt,
  subscribeFresh,
  tailscaleValid,
  tickerRunning,
  validatingFetcher,
  FetchOutcome,
} from "@/lib/api/freshness";
import { InvalidDataError } from "@/lib/api/types";

const T0 = 1_000_000;

describe("staleness rule", () => {
  const ok = nextFresh(INITIAL_FRESH, { kind: "ok" }, T0);

  it("never loaded, no failures → not stale (loading)", () => {
    expect(isStale(INITIAL_FRESH, T0, 2000)).toBe(false);
  });

  it("first failure stays fresh, second is stale", () => {
    const f1 = nextFresh(ok, { kind: "error" }, T0 + 2000);
    expect(isStale(f1, T0 + 2000, 2000)).toBe(false);
    const f2 = nextFresh(f1, { kind: "error" }, T0 + 4000);
    expect(isStale(f2, T0 + 4000, 2000)).toBe(true);
  });

  it("two failures before any success → stale (never connected)", () => {
    const f2 = nextFresh(nextFresh(INITIAL_FRESH, { kind: "error" }, T0), { kind: "error" }, T0);
    expect(isStale(f2, T0, 2000)).toBe(true);
  });

  it("time threshold: max(3 × interval, 10 s)", () => {
    expect(staleAfterMs(2000)).toBe(10000);
    expect(staleAfterMs(15000)).toBe(45000);
    expect(isStale(ok, T0 + 10000, 2000)).toBe(false);
    expect(isStale(ok, T0 + 10001, 2000)).toBe(true);
    expect(isStale(ok, T0 + 45000, 15000)).toBe(false);
    expect(isStale(ok, T0 + 45001, 15000)).toBe(true);
  });

  it("one-shot endpoints (no refreshInterval) don't age", () => {
    expect(isStale(ok, T0 + 3_600_000, 0)).toBe(false);
  });

  it("next success recovers", () => {
    const f2 = nextFresh(nextFresh(ok, { kind: "error" }, T0), { kind: "error" }, T0);
    const back = nextFresh(f2, { kind: "ok" }, T0 + 9000);
    expect(isStale(back, T0 + 9000, 2000)).toBe(false);
    expect(back).toEqual({ lastOkAt: T0 + 9000, failures: 0 });
  });

  it("invalid counts as a failure, keeps lastOkAt and records the reason", () => {
    const i1 = nextFresh(ok, { kind: "invalid", reason: "tailscaled not running" }, T0 + 5000);
    expect(i1).toEqual({ lastOkAt: T0, failures: 1, invalidReason: "tailscaled not running" });
    const i2 = nextFresh(i1, { kind: "invalid", reason: "x" }, T0 + 6000);
    expect(isStale(i2, T0 + 6000, 2000)).toBe(true);
    const e = nextFresh(i2, { kind: "error" }, T0 + 7000);
    expect(e.invalidReason).toBeUndefined();
  });
});

describe("validators", () => {
  it("tailscaleValid", () => {
    expect(tailscaleValid({})).toBe(true);
    expect(tailscaleValid({ error: "" })).toBe(true);
    expect(tailscaleValid({ error: null })).toBe(true);
    expect(tailscaleValid(undefined)).toBe(true);
    expect(tailscaleValid({ error: "failed to connect to local tailscaled" })).toEqual({
      ok: false,
      reason: "failed to connect to local tailscaled",
    });
  });


  it("checkValid normalises results", () => {
    expect(checkValid(undefined, 1)).toBeNull();
    expect(checkValid(() => true, 1)).toBeNull();
    expect(checkValid(() => false, 1)).toBe("invalid data");
    expect(checkValid(() => ({ ok: false as const, reason: "r" }), 1)).toBe("r");
  });
});

describe("validatingFetcher (invalid data does not overwrite)", () => {
  it("valid → returns data, records ok", async () => {
    const rec: FetchOutcome[] = [];
    const f = validatingFetcher("/k", async () => ({ error: "" }), tailscaleValid, (_k, o) => rec.push(o));
    await expect(f()).resolves.toEqual({ error: "" });
    expect(rec).toEqual([{ kind: "ok" }]);
  });

  it("invalid → throws InvalidDataError so SWR keeps the previous data", async () => {
    const rec: FetchOutcome[] = [];
    const f = validatingFetcher("/k", async () => ({ error: "down" }), tailscaleValid, (_k, o) => rec.push(o));
    const err = await f().catch((e) => e);
    expect(err).toBeInstanceOf(InvalidDataError);
    expect((err as InvalidDataError).reason).toBe("down");
    expect(rec).toEqual([{ kind: "invalid", reason: "down" }]);
  });

  it("fetch error → rethrows, records error", async () => {
    const rec: FetchOutcome[] = [];
    const boom = new Error("net");
    const f = validatingFetcher("/k", async () => { throw boom; }, undefined, (_k, o) => rec.push(o));
    await expect(f()).rejects.toBe(boom);
    expect(rec).toEqual([{ kind: "error" }]);
  });
  it("with the shared store, last valid state survives an invalid reply (tailscale)", async () => {
    resetFreshStore();
    let reply: { error: string } = { error: "" };
    const f = validatingFetcher("/api/services/tailscale", async () => reply, tailscaleValid);
    await f();
    const okAt = getFresh("/api/services/tailscale").lastOkAt;
    expect(okAt).not.toBeNull();
    reply = { error: "down" };
    await expect(f()).rejects.toBeInstanceOf(InvalidDataError);
    const s: FreshState = getFresh("/api/services/tailscale");
    expect(s.lastOkAt).toBe(okAt);
    expect(s.failures).toBe(1);
    expect(s.invalidReason).toBe("down");
  });

});

describe("shared ticker", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetFreshStore();
  });
  afterEach(() => vi.useRealTimers());

  it("one timer for all subscribers, started lazily and stopped at zero", () => {
    expect(tickerRunning()).toBe(false);
    const a = vi.fn();
    const b = vi.fn();
    const ua = subscribeFresh(a);
    const ub = subscribeFresh(b);
    expect(tickerRunning()).toBe(true);
    expect(vi.getTimerCount()).toBe(1);
    vi.advanceTimersByTime(3000);
    expect(a).toHaveBeenCalledTimes(3);
    expect(b).toHaveBeenCalledTimes(3);
    ua();
    expect(tickerRunning()).toBe(true);
    ub();
    expect(tickerRunning()).toBe(false);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("freshNow advances per tick; recordOutcome notifies", () => {
    const cb = vi.fn();
    const u = subscribeFresh(cb);
    const t = freshNow();
    vi.advanceTimersByTime(1000);
    expect(freshNow()).toBe(t + 1000);
    recordOutcome("/x", { kind: "ok" }, t + 1500);
    expect(cb).toHaveBeenCalledTimes(2);
    expect(getFresh("/x").lastOkAt).toBe(t + 1500);
    u();
  });
});

describe("stoppedAt label", () => {
  it("uses the live offset, else the stored one, else relative minutes", () => {
    const lastOk = 1_700_000_000_000;
    expect(stoppedAt(lastOk, lastOk + 60_000, 28800, 0)).toEqual({ kind: "device", deviceEpoch: 1_700_000_000 + 28800 });
    expect(stoppedAt(lastOk, lastOk + 60_000, null, 28800)).toEqual({ kind: "device", deviceEpoch: 1_700_000_000 + 28800 });
    expect(stoppedAt(lastOk, lastOk + 5 * 60_000 + 5, null, null)).toEqual({ kind: "relative", minutesAgo: 5 });
  });
});
