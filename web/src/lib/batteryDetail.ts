// Pure helpers for the battery details on /router/device.
import type { ValidResult } from "@/lib/api/freshness";
import type { ChargeControl, ChargerInfo, SysfsBattery } from "@/lib/api/schemas/device";
import type { EstimateInput, EstimateSample } from "@/lib/batteryEstimate";
import { WINDOW_SECS } from "@/lib/batteryEstimate";

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

/** Appends a sample and drops the ones the estimate can no longer use. */
export function pushSample(list: EstimateSample[], s: EstimateSample): EstimateSample[] {
  const out = [...list, s];
  const cut = s[0] - WINDOW_SECS;
  while (out.length > 1 && out[0][0] < cut) out.shift();
  return out;
}

export function estimateInput(
  samples: EstimateSample[],
  b: SysfsBattery,
  cc: ChargeControl | null | undefined,
  plugged: boolean | null,
): EstimateInput {
  const limitOn = !!cc?.charge_limit_enabled;
  return {
    samples,
    soc: b.capacity,
    charge_full_uah: b.charge_full_uah,
    charge_counter_uah: b.charge_counter_uah,
    target_pct: limitOn ? cc!.charge_limit ?? 100 : 100,
    paused_at_limit: limitOn && !!cc?.charging_stopped && !cc?.manual_override && plugged !== false,
  };
}
