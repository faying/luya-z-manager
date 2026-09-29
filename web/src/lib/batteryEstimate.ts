// Battery time estimate: computed by zte-agent (battery_eta.rs, rules in
// docs/battery-estimate.md); this file only names and formats it.

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

/** `/api/battery`'s `estimate` and `/api/screen`'s `battery` (zte-agent battery_eta.rs). */
export interface EstimateReport {
  /** ok · estimating (agent just started) · stale (its sampler stopped) · unavailable (no battery) */
  state: "ok" | "estimating" | "stale" | "unavailable";
  kind: EstimateKind;
  minutes: number | null;
  /** 100, or the charge limit while it is on. */
  target_pct: number;
  observed_at: number;
  samples: number;
}

/** Only a fresh report is shown; anything else reads as "—". */
export function fromReport(r: EstimateReport | null | undefined): { est: Estimate | null; targetPct: number } {
  if (!r) return { est: null, targetPct: 100 };
  const targetPct = typeof r.target_pct === "number" ? r.target_pct : 100;
  if (r.state !== "ok") return { est: null, targetPct };
  return { est: { kind: r.kind, minutes: r.minutes }, targetPct };
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
