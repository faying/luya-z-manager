import { describe, expect, it } from "vitest";
import { estimateText, formatDuration, fromReport, type EstimateReport, type Tr } from "@/lib/batteryEstimate";
import { zh as batteryZh } from "@/lib/i18n/nd-zh/battery";

// The estimate itself is computed by zte-agent (battery_eta.rs) and tested
// there against docs/battery-estimate/fixtures.json; this side only names it.

const zhT: Tr = (key, _def, vars) => {
  const s = (batteryZh.battery as Record<string, string>)[key.replace("battery.", "")];
  return s.replace(/\{\{(\w+)\}\}/g, (_, k) => String(vars?.[k]));
};

const report = (o: Partial<EstimateReport>): EstimateReport => ({
  state: "ok",
  kind: "discharging_eta",
  minutes: 300,
  target_pct: 100,
  observed_at: 1,
  samples: 10,
  ...o,
});

describe("agent report", () => {
  it("shows only a fresh report", () => {
    expect(fromReport(report({}))).toEqual({ est: { kind: "discharging_eta", minutes: 300 }, targetPct: 100 });
    expect(fromReport(report({ state: "stale" })).est).toBeNull();
    expect(fromReport(report({ state: "estimating" })).est).toBeNull();
    expect(fromReport(report({ state: "unavailable" })).est).toBeNull();
  });
  it("keeps the limit even when the estimate is not shown", () => {
    expect(fromReport(report({ state: "stale", target_pct: 80 })).targetPct).toBe(80);
  });
  it("older agents without an estimate read as none", () => {
    expect(fromReport(undefined)).toEqual({ est: null, targetPct: 100 });
  });
});

describe("estimate text", () => {
  it("formats durations", () => {
    expect(formatDuration(45, zhT)).toBe("45 分钟");
    expect(formatDuration(120, zhT)).toBe("2 小时");
    expect(formatDuration(198, zhT)).toBe("3 小时 18 分");
  });

  it("names the limit when charging to it", () => {
    expect(estimateText({ kind: "charging_eta", minutes: 198 }, 80, zhT)).toBe("约 3 小时 18 分充到 80%");
    expect(estimateText({ kind: "reached_target", minutes: null }, 100, zhT)).toBe("已充满");
    expect(estimateText({ kind: "paused_at_limit", minutes: null }, 80, zhT)).toBe("已到上限，暂停充电");
    expect(estimateText({ kind: "unknown", minutes: null }, 100, zhT)).toBe("—");
  });
});
