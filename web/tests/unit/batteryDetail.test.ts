import { describe, expect, it } from "vitest";
import type { SysfsBattery } from "@/lib/api/schemas/device";
import { healthPct, isPluggedIn, powerView } from "@/lib/batteryDetail";

const bat = (o: Partial<SysfsBattery> = {}): SysfsBattery => ({
  status: "Charging",
  capacity: 60,
  voltage_uv: 4_000_000,
  current_ua: 1_000_000,
  temperature: 350,
  charge_full_uah: 11_011_000,
  charge_full_design_uah: 10_214_000,
  charge_counter_uah: null,
  cycle_count: 66,
  health: "Good",
  voltage_max_uv: 4_500_000,
  charger: { online: true, voltage_uv: 9_000_000, current_ua: 1_000_000, input_current_limit_ua: null },
  ...o,
});


describe("power view", () => {
  it("splits input, battery and the rest", () => {
    const p = powerView(bat());
    expect(p.inputW).toBeCloseTo(9);
    expect(p.batteryW).toBeCloseTo(4);
    expect(p.systemW).toBeCloseTo(5);
  });
  it("has no input or system figure when the charger reports no current", () => {
    const p = powerView(bat({ current_ua: -400_000, charger: { online: false, voltage_uv: 31_000, current_ua: 0, input_current_limit_ua: null } }));
    expect(p.inputW).toBeNull();
    expect(p.systemW).toBeNull();
    expect(p.batteryW).toBeCloseTo(-1.6);
  });
});

describe("health", () => {
  it("is full over design and may pass 100", () => {
    expect(healthPct(bat())).toBe(108);
  });
  it("is unknown without the design capacity", () => {
    expect(healthPct(bat({ charge_full_design_uah: null }))).toBeNull();
  });
});

describe("charger", () => {
  it("reads charger_connect as number or string", () => {
    expect(isPluggedIn({ charger_connect: 1 })).toBe(true);
    expect(isPluggedIn({ charger_connect: "0" })).toBe(false);
    expect(isPluggedIn(null)).toBeNull();
  });
});
