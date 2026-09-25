// Mock fixtures: device / battery / charger / USB (zte-agent device_ext.rs,
// network_ext.rs:55, charge_policy.rs, usb.rs).
//
// Persona: battery 76 % and charging from a wall charger (so the USB data link
// is down, CC is attached), uptime a little over 2 days, power-save and fast
// boot off, no charge limit.
//
// The ubus passthrough writes honour `fakesuccess`: 200 ok:true, but the
// firmware "ignored" the change, so the readback is unchanged.

import type { Route, Ctx } from "../lib.ts";
import { ok, fail, bodyField, clone, unixNow } from "../lib.ts";
import type {
  DeviceThermal,
  BatteryInfo,
  ChargerInfo,
  DeviceSystem,
  ChargeControl,
  FastBoot,
  PowerSave,
  UsbStatus,
} from "../../../src/lib/api/schemas/device.ts";

// ── Device clock ────────────────────────────────────────────────────────────

/** Seconds the device clock runs ahead of UTC (local wall time labelled UTC; Taiwan/Beijing +8). */
export const UTC_OFFSET = 8 * 3600;

/** "Now" on the device clock (epoch seconds carrying local wall-clock digits). */
export function deviceNow(): number {
  return unixNow() + UTC_OFFSET;
}

/** Uptime at mock start: 2 d 3 h 17 min. */
const INITIAL_UPTIME = 2 * 86400 + 3 * 3600 + 17 * 60;

/** Device epoch of the last boot. Reset by reboot / factory-reset. */
let bootAt = deviceNow() - INITIAL_UPTIME;

/** Device epoch of the current boot (for other areas that need boot-relative times). */
export function deviceBootAt(): number {
  return bootAt;
}

/** /proc/uptime seconds. */
export function deviceUptime(): number {
  return Math.max(0, deviceNow() - bootAt);
}

// ── State ───────────────────────────────────────────────────────────────────

const battery: BatteryInfo = {
  battery_capacity: 76,
  battery_online: 1,
  battery_time_to_full: 58,
  battery_time_to_empty: -1,
  battery_temperature: 31.5,
} satisfies BatteryInfo;

const charger: ChargerInfo = {
  charger_type: 2,
  charge_status: 1,
  charger_connect: 1,
  direct_power_supply_mode: "disable",
  otg_powerbank_state: 0,
} satisfies ChargerInfo;

/** Charge-limit enforcer (charge_policy.rs LimitState). Persisted part = enabled/limit/hysteresis. */
const limit = {
  enabled: false,
  limit: 80,
  hysteresis: 5,
  manual_override: false,
};

/** `zwrt_mc.device.manager get_device_info` store (power-save + fast-boot share it). */
const DEVICE_INFO_DEFAULTS: Record<string, string> = {
  power_saver_mode: "0",
  quicken_power_on: "0",
};
const deviceInfo: Record<string, string> = { ...DEVICE_INFO_DEFAULTS };

const USB_DEFAULTS: UsbStatus = {
  connect: 0,
  mode: "rndis",
  typec_cc: "cc1",
  usb2rj45: 0,
} satisfies UsbStatus;
let usb: UsbStatus = { ...USB_DEFAULTS };

// ── Helpers ─────────────────────────────────────────────────────────────────

function chargerConnected(): boolean {
  return charger.charger_connect === 1 || charger.charger_connect === "1";
}

function chargingStopped(): boolean {
  return charger.direct_power_supply_mode === "enable";
}

/** Mirror of charge_policy.rs set_charging (inverted ubus naming) + what the firmware reports after it. */
function setCharging(allow: boolean): void {
  charger.direct_power_supply_mode = allow ? "disable" : "enable";
  const charging = allow && chargerConnected() && (battery.battery_capacity ?? 0) < 100;
  charger.charge_status = charging ? 1 : 0;
  battery.battery_time_to_full = charging ? 58 : -1;
}

/** /sys/class/power_supply/battery/status. */
function batteryStatus(): string {
  if (!chargerConnected()) return "Discharging";
  if (chargingStopped()) return "Not charging";
  if ((battery.battery_capacity ?? 0) >= 100) return "Full";
  return "Charging";
}

/** charge_policy.rs:198 enforce (charger connected; capacity vs limit). */
function enforce(): void {
  if (batteryStatus() === "Discharging" && !chargingStopped()) return;
  const cap = battery.battery_capacity ?? 0;
  if (cap >= limit.limit && !chargingStopped()) setCharging(false);
  else if (cap <= limit.limit - limit.hysteresis && chargingStopped()) setCharging(true);
}

function chargeControl(): ChargeControl {
  return {
    charging_stopped: chargingStopped(),
    battery_status: batteryStatus(),
    capacity: battery.battery_capacity ?? 0,
    charge_limit_enabled: limit.enabled,
    charge_limit: limit.limit,
    hysteresis: limit.hysteresis,
    manual_override: limit.manual_override,
  } satisfies ChargeControl;
}

function isObject(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === "object" && !Array.isArray(v);
}

/** serde `as_u64()`: non-negative integer. */
function asU64(v: unknown): number | undefined {
  return typeof v === "number" && Number.isInteger(v) && v >= 0 ? v : undefined;
}

function systemInfo(): DeviceSystem {
  const up = deviceUptime();
  // A little wobble so the dashboard is not frozen.
  const wobble = Math.round(Math.sin(up / 30) * 4000);
  return {
    localtime: deviceNow(),
    uptime: up,
    load: [26214 + wobble, 30146, 28835],
    memory: {
      total: 1_003_560_960,
      free: 218_734_592 + wobble * 64,
      shared: 9_842_688,
      buffered: 11_403_264,
      available: 461_172_736 + wobble * 64,
      cached: 256_401_408,
    },
    root: { total: 76_800, free: 0, used: 76_800, avail: 0 },
    tmp: { total: 490_020, free: 461_388, used: 28_632, avail: 461_388 },
    swap: { total: 0, free: 0 },
    // hostname / kernel intentionally absent: OpenWrt `system info` has neither.
  } satisfies DeviceSystem;
}

/** Common reboot effect: the agent restarts (manual override is in memory only), uptime restarts. */
function rebootEffects(): void {
  bootAt = deviceNow();
  limit.manual_override = false;
}

// ── Routes ──────────────────────────────────────────────────────────────────

/** CPU temperature in milli-°C; drifts a little per read, hotter while charging. */
function thermalNow(ctx: Ctx): DeviceThermal {
  const base = 47000 + Math.round(Math.sin(ctx.now / 60000) * 1500);
  return { cpuss_temp: base } satisfies DeviceThermal;
}

export const routes: Route[] = [
  // handlers.rs:67 — /proc/sys/kernel/hostname, /proc/uptime, /proc/loadavg, /proc/version.
  {
    method: "GET",
    path: "/api/device",
    handler: () =>
      ok({
        hostname: "MU5250",
        uptime_secs: deviceUptime(),
        load_avg: [0.42, 0.38, 0.35],
        kernel: "Linux version 5.4.210 (builder@buildhost) (gcc version 9.3.0) #1 SMP PREEMPT",
      }),
  },
  // device_ext.rs:8 — ubus zwrt_bsp.thermal get_cpu_temp. `missing`: the key is absent ({}).
  {
    method: "GET",
    path: "/api/device/thermal",
    ownMissing: true,
    handler: (ctx) => ok(ctx.has("missing") ? ({} satisfies DeviceThermal) : thermalNow(ctx)),
  },
  {
    method: "GET",
    path: "/api/device/battery-info",
    handler: () => ok(clone(battery)),
  },
  {
    method: "GET",
    path: "/api/device/charger",
    handler: () => ok(clone(charger)),
  },
  {
    method: "GET",
    path: "/api/device/system",
    handler: () => ok(systemInfo()),
  },
  {
    method: "GET",
    path: "/api/device/charge-control",
    handler: () => ok(chargeControl()),
  },
  {
    // device_ext.rs:81 charge_control_set
    method: "PUT",
    path: "/api/device/charge-control",
    handler: (ctx: Ctx) => {
      if (!isObject(ctx.body)) return fail("invalid JSON", 400);
      const b = ctx.body;
      if (typeof b.charging_stopped === "boolean") {
        setCharging(!b.charging_stopped);
        limit.manual_override = b.charging_stopped;
      }
      if ("charge_limit_enabled" in b || "charge_limit" in b || "hysteresis" in b) {
        const enabled = typeof b.charge_limit_enabled === "boolean" ? b.charge_limit_enabled : limit.enabled;
        // Rust: as_u64().map(|v| v as u8) — truncates to 8 bits.
        const l = asU64(b.charge_limit);
        const h = asU64(b.hysteresis);
        const lim = l !== undefined ? l & 0xff : limit.limit;
        const hys = h !== undefined ? h & 0xff : limit.hysteresis;
        if (lim < 50 || lim > 100) return fail("limit must be 50-100", 400);
        if (hys < 1 || hys > 20) return fail("hysteresis must be 1-20", 400);
        limit.enabled = enabled;
        limit.limit = lim;
        limit.hysteresis = hys;
        limit.manual_override = false;
        if (enabled) enforce();
        else setCharging(true);
      }
      return ok(chargeControl());
    },
  },
  {
    // device_ext.rs:126 — a read served over POST; body passed to get_device_info.
    method: "POST",
    path: "/api/device/power-save",
    kind: "read",
    handler: (ctx: Ctx) => {
      if (ctx.body === undefined) return fail("invalid JSON", 400);
      const list = bodyField(ctx.body, "deviceInfoList");
      const out: PowerSave = {};
      if (Array.isArray(list)) {
        for (const k of list) {
          if (typeof k === "string" && k in deviceInfo) out[k] = deviceInfo[k];
        }
      }
      return ok(out);
    },
  },
  {
    // device_ext.rs:141 — body passed verbatim to set_device_info.
    method: "PUT",
    path: "/api/device/power-save",
    handler: (ctx: Ctx) => {
      if (ctx.body === undefined) return fail("invalid JSON", 400);
      if (ctx.has("fakesuccess")) return ok(null);
      const list = bodyField(ctx.body, "deviceInfoList");
      // The web page sends [{power_saver_mode}], the mobile apps {power_saver_mode}; accept both.
      const items = Array.isArray(list) ? list : [list];
      for (const item of items) {
        if (!isObject(item)) continue;
        const v = item.power_saver_mode;
        if (v === "0" || v === "1") deviceInfo.power_saver_mode = v;
      }
      return ok(null);
    },
  },
  {
    method: "GET",
    path: "/api/device/fast-boot",
    handler: () => ok({ fast_boot: deviceInfo.quicken_power_on ?? "0" } satisfies FastBoot),
  },
  {
    // device_ext.rs:167
    method: "PUT",
    path: "/api/device/fast-boot",
    handler: (ctx: Ctx) => {
      if (ctx.body === undefined) return fail("invalid JSON", 400);
      const v = bodyField(ctx.body, "fast_boot");
      if (v !== "0" && v !== "1") return fail('fast_boot must be "0" or "1"', 400);
      if (!ctx.has("fakesuccess")) deviceInfo.quicken_power_on = v;
      return ok({ fast_boot: v } satisfies FastBoot);
    },
  },
  {
    // device_ext.rs:29 — ubus `system reboot`. The server core implements the
    // reboot-token downtime; here only the post-boot state changes.
    method: "POST",
    path: "/api/device/reboot",
    handler: () => {
      rebootEffects();
      return ok(null);
    },
  },
  {
    // device_ext.rs:37 — ubus `zwrt_bsp.power factory_reset` (wipes settings, reboots).
    method: "POST",
    path: "/api/device/factory-reset",
    handler: () => {
      rebootEffects();
      Object.assign(deviceInfo, DEVICE_INFO_DEFAULTS);
      usb = { ...USB_DEFAULTS };
      limit.enabled = false;
      limit.limit = 80;
      limit.hysteresis = 5;
      setCharging(true);
      charger.otg_powerbank_state = 0;
      return ok(null);
    },
  },
  {
    method: "GET",
    path: "/api/usb/status",
    handler: () => ok(clone(usb)),
  },
  {
    // usb.rs:13 — body passed verbatim to `zwrt_bsp.usb set`; no validation in the agent.
    method: "PUT",
    path: "/api/usb/mode",
    handler: (ctx: Ctx) => {
      if (ctx.body === undefined) return fail("invalid JSON", 400);
      if (ctx.has("fakesuccess")) return ok(null);
      const mode = bodyField(ctx.body, "mode");
      if (typeof mode === "string" && ["debug", "mtp", "rndis"].includes(mode)) usb.mode = mode;
      return ok(null);
    },
  },
  {
    // usb.rs:24 — body passed verbatim to `zwrt_bsp.powerbank set`.
    method: "PUT",
    path: "/api/usb/powerbank",
    handler: (ctx: Ctx) => {
      if (ctx.body === undefined) return fail("invalid JSON", 400);
      if (ctx.has("fakesuccess")) return ok(null);
      // Web page sends {state}, Android {otg_powerbank_state}; accept both.
      const raw = bodyField(ctx.body, "state") ?? bodyField(ctx.body, "otg_powerbank_state");
      if (raw === 1 || raw === "1" || raw === true) charger.otg_powerbank_state = 1;
      else if (raw === 0 || raw === "0" || raw === false) charger.otg_powerbank_state = 0;
      return ok(null);
    },
  },
];
