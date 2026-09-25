// The device clock is local wall time labelled UTC.
//
// ZTE's SNTP client sets the system clock to local time and leaves the time
// zone at UTC, so every timestamp the device produces — epochs, and ISO
// strings ending in "Z" — carries local wall-clock digits and runs ahead of
// real UTC by the device's zone (Beijing: 8 h). zte-agent/src/clock.rs has the
// details; the agent reports the gap as `clock.utc_offset` in
// /api/public/status.
//
// Two rules follow:
//  * To SHOW a device time, print its digits as-is (format in UTC). That is the
//    device's local time, whatever zone the browser is in.
//  * To COMPARE a device time with the browser's clock (ages, countdowns) or to
//    turn something the user picked into a device time, go through the offset.

import { useApi } from "@/lib/hooks/useApi";

interface PublicClock {
  clock?: { utc_offset?: number };
}

const OFFSET_KEY = "u60.utc_offset";

/** Last device offset this browser has seen, or null if never. */
export function cachedDeviceOffset(): number | null {
  if (typeof window === "undefined") return null;
  const v = window.localStorage.getItem(OFFSET_KEY);
  const n = v == null ? NaN : Number(v);
  return Number.isFinite(n) ? n : null;
}

/** Seconds the device clock runs ahead of real UTC (0 until known). */
export function useDeviceOffset(): number {
  // Same key as the header status strip, so SWR shares one request.
  const { data } = useApi<PublicClock>("/api/public/status", { refreshInterval: 10000 });
  const off = data?.clock?.utc_offset;
  // Keep it for pages opened while the device is unreachable (stale
  // timestamps still need device-local digits).
  if (typeof off === "number" && typeof window !== "undefined") {
    window.localStorage.setItem(OFFSET_KEY, String(off));
  }
  return off ?? cachedDeviceOffset() ?? 0;
}

const FMT: Record<"datetime" | "date" | "time", Intl.DateTimeFormatOptions> = {
  datetime: { year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false },
  date: { year: "numeric", month: "2-digit", day: "2-digit" },
  time: { hour: "2-digit", minute: "2-digit", hour12: false },
};

/** A device epoch (seconds) as the device's local wall time. */
export function fmtDevice(ts: number, style: keyof typeof FMT = "datetime"): string {
  return new Intl.DateTimeFormat(undefined, { ...FMT[style], timeZone: "UTC" }).format(ts * 1000);
}

/** "Now" on the device clock, in seconds. */
export function deviceNow(offset: number): number {
  return Math.floor(Date.now() / 1000) + offset;
}

/** A device ISO string ("…Z" with local digits) → device epoch seconds, or NaN. */
export function deviceIso(iso: string): number {
  return Math.floor(Date.parse(iso) / 1000);
}

/** Device epoch → value for <input type="datetime-local"> (device wall time). */
export function toWallInput(ts: number): string {
  return new Date(ts * 1000).toISOString().slice(0, 16);
}

/** <input type="datetime-local"> value (device wall time) → device epoch. */
export function fromWallInput(v: string): number {
  return Math.floor(Date.parse(`${v}:00Z`) / 1000);
}
