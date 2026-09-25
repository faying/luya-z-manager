// Battery time estimate. Rules: docs/battery-estimate.md (shared with touch-ui).

/** [t seconds, battery current µA (+ = charging), charger online] */
export type EstimateSample = [number, number, boolean];

export interface EstimateInput {
  samples: EstimateSample[];
  soc: number;
  charge_full_uah: number | null;
  charge_counter_uah: number | null;
  target_pct: number;
  paused_at_limit: boolean;
}

export type EstimateKind =
  | "charging_eta"
  | "discharging_eta"
  | "reached_target"
  | "paused_at_limit"
  | "unknown";

export interface Estimate {
  kind: EstimateKind;
  minutes: number | null;
}

export const WINDOW_SECS = 180;
export const MIN_CURRENT_UA = 50_000;

/** Time-weighted average over the current window; null when there are no samples. */
export function averageCurrent(samples: EstimateSample[]): number | null {
  if (samples.length === 0) return null;
  const [tLast, iLast, onLast] = samples[samples.length - 1];
  const positive = iLast >= 0;
  let start = samples.length - 1;
  while (start > 0) {
    const [t, i, on] = samples[start - 1];
    if (t < tLast - WINDOW_SECS || i >= 0 !== positive || on !== onLast) break;
    start--;
  }
  let sum = 0;
  let weight = 0;
  for (let k = start; k < samples.length - 1; k++) {
    const w = samples[k + 1][0] - samples[k][0];
    sum += samples[k][1] * w;
    weight += w;
  }
  return weight > 0 ? sum / weight : iLast;
}

export function estimate(input: EstimateInput): Estimate {
  const unknown: Estimate = { kind: "unknown", minutes: null };
  const avg = averageCurrent(input.samples);
  if (avg === null) return unknown;
  const online = input.samples[input.samples.length - 1][2];
  if (input.paused_at_limit) return { kind: "paused_at_limit", minutes: null };
  if (online && input.soc >= input.target_pct && (avg >= 0 || Math.abs(avg) < MIN_CURRENT_UA)) {
    return { kind: "reached_target", minutes: null };
  }
  if (Math.abs(avg) < MIN_CURRENT_UA) return unknown;
  if (avg > 0) {
    if (input.charge_full_uah == null) return unknown;
    const need = (input.charge_full_uah * (input.target_pct - input.soc)) / 100;
    return { kind: "charging_eta", minutes: Math.round((need / avg) * 60) };
  }
  const left =
    input.charge_counter_uah ??
    (input.charge_full_uah == null ? null : (input.charge_full_uah * input.soc) / 100);
  if (left == null) return unknown;
  return { kind: "discharging_eta", minutes: Math.round((left / -avg) * 60) };
}

/** i18next-style translate: key, English default, interpolation values. */
export type Tr = (key: string, def: string, vars?: Record<string, string | number>) => string;

export function formatDuration(minutes: number, t: Tr): string {
  if (minutes < 60) return t("battery.dMin", "{{m}} min", { m: minutes });
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  return m === 0 ? t("battery.dHour", "{{h}} h", { h }) : t("battery.dHourMin", "{{h}} h {{m}} min", { h, m });
}

export function estimateText(e: Estimate, targetPct: number, t: Tr): string {
  switch (e.kind) {
    case "charging_eta": {
      const d = formatDuration(e.minutes!, t);
      return targetPct >= 100
        ? t("battery.eToFull", "Full in about {{d}}", { d })
        : t("battery.eToLimit", "{{pct}}% in about {{d}}", { d, pct: targetPct });
    }
    case "discharging_eta":
      return t("battery.eLeft", "About {{d}} left", { d: formatDuration(e.minutes!, t) });
    case "reached_target":
      return targetPct >= 100 ? t("battery.eFull", "Full") : t("battery.eAtLimit", "At the {{pct}}% limit", { pct: targetPct });
    case "paused_at_limit":
      return t("battery.ePaused", "At the limit, charging paused");
    default:
      return "—";
  }
}
