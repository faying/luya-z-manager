import { cachedDeviceOffset, fmtDevice } from "@/lib/deviceClock";

/**
 * A browser timestamp (ms) shown as device-local wall time, using the last
 * clock offset this browser saw. Never seen → minutes ago instead.
 */
export function formatDeviceTime(browserMs: number): { kind: "clock"; text: string } | { kind: "ago"; minutes: number } {
  const off = cachedDeviceOffset();
  if (off == null) return { kind: "ago", minutes: Math.max(0, Math.round((Date.now() - browserMs) / 60000)) };
  return { kind: "clock", text: fmtDevice(Math.floor(browserMs / 1000) + off, "time") };
}
