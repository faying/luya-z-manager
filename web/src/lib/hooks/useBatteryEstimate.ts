"use client";
// Battery estimate shared by the home tile and /router/device. Samples live
// in the calling component, so leaving the page drops the window.
import { useRef, useState } from "react";
import type { ChargeControl, ChargerInfo, SysfsBattery } from "@/lib/api/schemas/device";
import { useApi } from "@/lib/hooks/useApi";
import { estimate, type Estimate, type EstimateSample } from "@/lib/batteryEstimate";
import { estimateInput, isPluggedIn, pushSample, sysfsBatteryValid } from "@/lib/batteryDetail";

export function useBatteryEstimate(intervalMs = 5000) {
  const [samples, setSamples] = useState<EstimateSample[]>([]);
  const pluggedRef = useRef<boolean | null>(null);
  const ch = useApi<ChargerInfo>("/api/device/charger", {
    refreshInterval: 30000,
    onSuccess: (d) => {
      pluggedRef.current = isPluggedIn(d);
    },
  });
  const cc = useApi<ChargeControl>("/api/device/charge-control", { refreshInterval: 30000 });
  const plugged = isPluggedIn(ch.data);
  // One sample per successful reply (SWR dedupes, so shared callers don't double up).
  const bat = useApi<SysfsBattery>("/api/battery", {
    refreshInterval: intervalMs,
    isValid: sysfsBatteryValid,
    onSuccess: (d) => {
      if (sysfsBatteryValid(d) !== true) return;
      const on = pluggedRef.current ?? !!d.charger?.online;
      setSamples((l) => pushSample(l, [Date.now() / 1000, d.current_ua, on]));
    },
  });
  const b = bat.data;

  let est: Estimate | null = null;
  let targetPct = 100;
  if (b) {
    const input = estimateInput(samples, b, cc.data, plugged);
    est = estimate(input);
    targetPct = input.target_pct;
  }
  return { bat, plugged, est, targetPct };
}
