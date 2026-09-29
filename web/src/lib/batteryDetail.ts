// Pure helpers for the battery details on /router/device.
import type { ValidResult } from "@/lib/api/freshness";
import type { ChargerInfo, SysfsBattery } from "@/lib/api/schemas/device";

export function sysfsBatteryValid(d: SysfsBattery | null | undefined): ValidResult {
  if (!d || typeof d.status !== "string" || typeof d.current_ua !== "number") {
    return { ok: false, reason: "no battery status" };
  }
  return true;
}

/** µV × µA → W. */
export function watts(uv: number | null | undefined, ua: number | null | undefined): number | null {
  if (uv == null || ua == null) return null;
  return (uv / 1e6) * (ua / 1e6);
}

export interface PowerView {
  /** Charger input, only when it reports a positive current. */
  inputW: number | null;
  /** + = into the battery. */
  batteryW: number | null;
  /** Input minus battery power: the device's own draw plus conversion loss. */
  systemW: number | null;
}

export function powerView(b: SysfsBattery): PowerView {
  const batteryW = watts(b.voltage_uv, b.current_ua);
  const c = b.charger;
  const inputW = c?.online && (c.current_ua ?? 0) > 0 ? watts(c.voltage_uv, c.current_ua) : null;
  const systemW = inputW != null && batteryW != null ? Math.max(0, inputW - batteryW) : null;
  return { inputW, batteryW, systemW };
}

/** charge_full / charge_full_design in percent; null when either is missing. */
export function healthPct(b: SysfsBattery): number | null {
  const full = b.charge_full_uah;
  const design = b.charge_full_design_uah;
  if (full == null || design == null || design <= 0) return null;
  return Math.round((full / design) * 100);
}

export function isPluggedIn(ch: ChargerInfo | null | undefined): boolean | null {
  if (!ch || ch.charger_connect == null) return null;
  return String(ch.charger_connect) !== "0";
}

