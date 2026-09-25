// Data freshness — spec §5 rule, D1, R9.
//
//   each request for an endpoint (one per SWR key, deduped)
//      │
//      ├─ HTTP/envelope ok ─► isValid(data)? ──yes──► success: failures = 0,
//      │                          │                   lastOkAt = now, reason cleared
//      │                          no                  (SWR stores the new data)
//      │                          ▼
//      │                     failure(reason): fetcher throws InvalidDataError,
//      │                     so SWR keeps the last valid data
//      └─ timeout / network / 4xx / 5xx / 401 ─► failure: failures += 1
//
//   stale = failures >= 2
//        || (refreshInterval > 0 && lastOkAt != null
//            && now - lastOkAt > max(3 × refreshInterval, 10 s))
//   recovery: the next success clears it — no manual refresh.
//
//   One 1-second ticker for the whole app drives the time half of the rule;
//   it runs only while at least one component is subscribed.

import { InvalidDataError } from "./types";

export const STALE_FAILURES = 2;
export const STALE_MIN_MS = 10000;

export interface FreshState {
  /** Browser ms of the last valid response, null if none this session. */
  lastOkAt: number | null;
  /** Consecutive failures (errors or invalid payloads) since then. */
  failures: number;
  /** Set when the latest failure was an invalid payload. */
  invalidReason?: string;
}

export const INITIAL_FRESH: FreshState = { lastOkAt: null, failures: 0 };

export type FetchOutcome =
  | { kind: "ok" }
  | { kind: "invalid"; reason: string }
  | { kind: "error" };

export function nextFresh(s: FreshState, o: FetchOutcome, now: number): FreshState {
  switch (o.kind) {
    case "ok":
      return { lastOkAt: now, failures: 0 };
    case "invalid":
      return { lastOkAt: s.lastOkAt, failures: s.failures + 1, invalidReason: o.reason };
    case "error":
      return { lastOkAt: s.lastOkAt, failures: s.failures + 1 };
  }
}

export function staleAfterMs(refreshInterval: number): number {
  return Math.max(3 * refreshInterval, STALE_MIN_MS);
}

/** The §5 rule. The time half applies only to polled endpoints
 *  (refreshInterval > 0): a one-shot fetch doesn't age. */
export function isStale(s: FreshState, now: number, refreshInterval: number): boolean {
  if (s.failures >= STALE_FAILURES) return true;
  if (refreshInterval > 0 && s.lastOkAt !== null) {
    return now - s.lastOkAt > staleAfterMs(refreshInterval);
  }
  return false;
}

// ── validators (R9) ──────────────────────────────────────────────────────────

export type ValidResult = boolean | { ok: false; reason: string };
export type Validator<T> = (data: T) => ValidResult;

/** Normalise a validator result: null = valid, string = reason. */
export function checkValid<T>(isValid: Validator<T> | undefined, data: T): string | null {
  if (!isValid) return null;
  const r = isValid(data);
  if (r === true) return null;
  if (r === false) return "invalid data";
  return r.reason || "invalid data";
}

/** /api/services/tailscale answers 200 with `error` when `tailscale status` fails. */
export function tailscaleValid(data: { error?: unknown } | null | undefined): ValidResult {
  const e = data?.error;
  if (typeof e === "string" && e.trim() !== "") return { ok: false, reason: e };
  return true;
}


// ── shared store + 1 s ticker ────────────────────────────────────────────────

const states = new Map<string, FreshState>();
const listeners = new Set<() => void>();
let timer: ReturnType<typeof setInterval> | null = null;
let tickNow = Date.now();

function emit() {
  for (const l of Array.from(listeners)) l();
}

/** Current time as seen by subscribers: advances once a second while the
 *  ticker runs, so snapshots are stable within a tick. */
export function freshNow(): number {
  if (!timer) tickNow = Date.now();
  return tickNow;
}

/** Subscribe to ticks and freshness changes. Starts the ticker on the first
 *  subscriber and stops it after the last one leaves. */
export function subscribeFresh(cb: () => void): () => void {
  listeners.add(cb);
  if (!timer) {
    tickNow = Date.now();
    timer = setInterval(() => {
      tickNow = Date.now();
      emit();
    }, 1000);
  }
  return () => {
    listeners.delete(cb);
    if (listeners.size === 0 && timer) {
      clearInterval(timer);
      timer = null;
    }
  };
}

/** For tests. */
export function tickerRunning(): boolean {
  return timer !== null;
}

export function getFresh(key: string | null): FreshState {
  if (key === null) return INITIAL_FRESH;
  return states.get(key) ?? INITIAL_FRESH;
}

export function recordOutcome(key: string, o: FetchOutcome, now = Date.now()) {
  states.set(key, nextFresh(getFresh(key), o, now));
  tickNow = now;
  emit();
}

/** For tests. */
export function resetFreshStore() {
  states.clear();
}

/** Wrap a fetcher so every call records its outcome for `key`, and an invalid
 *  payload throws InvalidDataError — SWR then keeps the previous (valid) data
 *  instead of storing this one. */
export function validatingFetcher<T>(
  key: string,
  fetcher: () => Promise<T>,
  isValid: Validator<T> | undefined,
  record: (key: string, o: FetchOutcome) => void = recordOutcome
): () => Promise<T> {
  return async () => {
    let data: T;
    try {
      data = await fetcher();
    } catch (e) {
      record(key, { kind: "error" });
      throw e;
    }
    const reason = checkValid(isValid, data);
    if (reason !== null) {
      record(key, { kind: "invalid", reason });
      throw new InvalidDataError(reason);
    }
    record(key, { kind: "ok" });
    return data;
  };
}

// ── "numbers stopped at hh:mm" (§5) ─────────────────────────────────────────

const OFFSET_KEY = "u60.utc_offset";

/** Remember the latest clock.utc_offset so a later session can show device
 *  time before /api/public/status answers. */
export function saveUtcOffset(offset: number) {
  if (typeof window === "undefined" || !Number.isFinite(offset)) return;
  try {
    window.localStorage.setItem(OFFSET_KEY, String(offset));
  } catch {
    /* storage full / disabled */
  }
}

export function loadUtcOffset(): number | null {
  if (typeof window === "undefined") return null;
  try {
    const v = window.localStorage.getItem(OFFSET_KEY);
    if (v === null) return null;
    const n = Number(v);
    return Number.isFinite(n) ? n : null;
  } catch {
    return null;
  }
}

export type StoppedAt =
  | { kind: "device"; deviceEpoch: number } // format with fmtDevice(…, "time")
  | { kind: "relative"; minutesAgo: number }; // "N 分钟前"

/** How to label the last-good time. Offset priority: live value from this
 *  session, else the stored one, else fall back to a relative time. */
export function stoppedAt(lastOkAt: number, now: number, liveOffset: number | null, storedOffset: number | null): StoppedAt {
  const offset = liveOffset ?? storedOffset;
  if (offset === null) return { kind: "relative", minutesAgo: Math.max(0, Math.floor((now - lastOkAt) / 60000)) };
  return { kind: "device", deviceEpoch: Math.floor(lastOkAt / 1000) + offset };
}
