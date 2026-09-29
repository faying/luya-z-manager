"use client";
// Battery estimate shared by the home tile and /router/device. The agent
// computes it (battery_eta.rs) from its own 5 s samples, so it is ready as soon
// as the page opens and matches the touch screen.
import type { ChargerInfo, SysfsBattery } from "@/lib/api/schemas/device";
import { useApi } from "@/lib/hooks/useApi";
import { fromReport } from "@/lib/batteryEstimate";
import { isPluggedIn, sysfsBatteryValid } from "@/lib/batteryDetail";

export function useBatteryEstimate(intervalMs = 5000) {
  const ch = useApi<ChargerInfo>("/api/device/charger", { refreshInterval: 30000 });
  const plugged = isPluggedIn(ch.data);
  const bat = useApi<SysfsBattery>("/api/battery", {
    refreshInterval: intervalMs,
    isValid: sysfsBatteryValid,
  });
  const { est, targetPct } = fromReport(bat.data?.estimate);
  return { bat, plugged, est, targetPct };
}
