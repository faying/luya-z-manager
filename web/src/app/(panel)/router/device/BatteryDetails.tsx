"use client";
// Battery details for /router/device: estimate, power split, health.
// Polls /api/battery every 5 s while the page is open and visible (SWR pauses
// hidden tabs); the sample window lives in this component, so leaving the
// page drops it. Rules: docs/battery-estimate.md.
import { useTranslation } from "react-i18next";
import { Button, GroupTitle, Row } from "@/components/nd";
import { ApiError } from "@/lib/api/types";
import { useBatteryEstimate } from "@/lib/hooks/useBatteryEstimate";
import { estimateText, type Tr } from "@/lib/batteryEstimate";
import { healthPct, powerView } from "@/lib/batteryDetail";

const fmt1 = (n: number) => (Math.round(n * 10) / 10).toFixed(1);

export function BatteryDetails() {
  const { t } = useTranslation();
  const tr: Tr = (k, d, v) => t(k, d, v);
  const { bat, plugged, est: e, targetPct } = useBatteryEstimate();
  const b = bat.data;

  const title = <GroupTitle id="dc-battery">{t("battery.title", "Battery details")}</GroupTitle>;

  if (!b) {
    if (!bat.error) return null;
    const unsupported = bat.error instanceof ApiError && bat.error.status === 503;
    return (
      <section aria-labelledby="dc-battery">
        {title}
        <div className="nd-group">
          <Row
            label={
              unsupported
                ? t("battery.unsupported", "This device doesn't report battery details")
                : t("battery.failed", "Can't read battery details right now")
            }
            control={
              unsupported ? undefined : (
                <Button variant="secondary" size="sm" onPress={() => bat.mutate()}>
                  {t("common.retry", "Retry")}
                </Button>
              )
            }
          />
        </div>
      </section>
    );
  }

  const est = e ? estimateText(e, targetPct, tr) : "—";
  const p = powerView(b);
  const health = healthPct(b);
  const mah = (uah: number | null) => (uah == null ? "—" : `${Math.round(uah / 1000)}`);

  let batteryW = "—";
  if (p.batteryW != null && Math.abs(p.batteryW) >= 0.05) {
    batteryW =
      p.batteryW > 0
        ? t("battery.charging", "In {{w}} W", { w: fmt1(p.batteryW) })
        : t("battery.discharging", "Out {{w}} W", { w: fmt1(-p.batteryW) });
  }
  let inputW: string;
  if (plugged === false || (plugged == null && !b.charger?.online)) inputW = t("battery.notPlugged", "Not plugged in");
  else inputW = p.inputW != null ? `${fmt1(p.inputW)} W` : t("battery.noInput", "No input");

  return (
    <section aria-labelledby="dc-battery">
      {title}
      <div className={`nd-group${bat.stale ? " nd-stale" : ""}`}>
        <Row
          label={t("battery.estimate", "Estimate")}
          sub={t("battery.estimateSub", "Average current over the last 3 minutes; restarts when the charger is plugged or unplugged")}
          value={est}
        />
        <Row label={t("battery.batteryPower", "Battery")} value={batteryW} mono />
        <Row label={t("battery.input", "Charger input")} value={inputW} mono />
        {p.systemW != null && (
          <Row
            label={t("battery.systemLabel", "Device")}
            value={t("battery.system", "About {{w}} W (incl. conversion loss)", { w: fmt1(p.systemW) })}
            mono
          />
        )}
        <Row label={t("battery.voltage", "Voltage")} value={`${(b.voltage_uv / 1e6).toFixed(2)} V`} mono />
        <Row label={t("battery.current", "Current")} value={`${Math.round(b.current_ua / 1000)} mA`} mono />
        <Row label={t("battery.temperature", "Temperature")} value={`${fmt1(b.temperature / 10)} °C`} mono />
        <Row
          label={t("battery.health", "Health")}
          sub={t("battery.healthSub", "Current full capacity ÷ design capacity")}
          value={health == null ? "—" : health > 100 ? t("battery.healthOver", "100%+ (fuel-gauge estimate)") : `${health}%`}
        />
        <Row label={t("battery.cycles", "Cycles")} value={b.cycle_count == null ? "—" : String(b.cycle_count)} mono />
        <Row
          label={t("battery.capacity", "Full / design capacity")}
          value={`${mah(b.charge_full_uah)} / ${mah(b.charge_full_design_uah)} mAh`}
          mono
        />
      </div>
    </section>
  );
}
