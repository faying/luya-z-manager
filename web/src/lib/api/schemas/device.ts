// Response shapes for the device / battery / charger / USB endpoints of
// zte-agent (zte-agent/src/device_ext.rs, network_ext.rs, charge_policy.rs,
// usb.rs).
//
// Most of these handlers are ubus passthroughs: the firmware's JSON is put
// into `data` as-is (an empty ubus stdout becomes `null`). For those the page
// types, the inventory (web/docs/controls-inventory.md) and the mobile apps'
// parsers (mobile/*/DeviceModels) are the only evidence; fields the inventory
// flags 未确认 are marked `// unconfirmed`. charge-control and fast-boot are
// built by the agent itself, so the Rust shape wins there. Types only.

/**
 * GET /api/device/battery-info — network_ext.rs:55 `network_battery_ubus`.
 * ubus passthrough: `zwrt_bsp.battery list`, shape from page (dashboard) +
 * mobile DeviceParser.parseBattery.
 */
export interface BatteryInfo {
  /** Percent 0-100. */
  battery_capacity?: number;
  /** Dashboard treats 1 as "external power present". */ // unconfirmed
  battery_online?: number;
  /** Minutes (?) to full; -1 when not charging. Dashboard: online=1 && >=0 → "Charging". */ // unconfirmed
  battery_time_to_full?: number;
  /** Mobile apps only. */ // unconfirmed
  battery_time_to_empty?: number;
  /** °C. Mobile apps only. */ // unconfirmed
  battery_temperature?: number;
  [key: string]: unknown;
}

/**
 * GET /api/battery — system.rs `read_battery_at` (sysfs power_supply).
 * The first five fields are the original contract (0 when a node is missing);
 * the rest are null when the node is missing. 503 = no battery directory.
 * Current sign: + = into the battery.
 */
export interface SysfsBattery {
  status: string;
  capacity: number;
  voltage_uv: number;
  current_ua: number;
  /** Tenths of °C. */
  temperature: number;
  charge_full_uah: number | null;
  charge_full_design_uah: number | null;
  charge_counter_uah: number | null;
  cycle_count: number | null;
  health: string | null;
  voltage_max_uv: number | null;
  /** sysfs usb supply; null when that directory is missing. Reads 0 while
   *  charging is stopped (the firmware cuts the input). */
  charger: {
    online: boolean | null;
    voltage_uv: number | null;
    current_ua: number | null;
    input_current_limit_ua: number | null;
  } | null;
}

/**
 * GET /api/device/charger — device_ext.rs:15 `device_charger`.
 * ubus passthrough: `zwrt_bsp.charger list`, shape from page (usb) + mobile
 * DeviceParser.parseCharger + charge_policy.rs:43-66.
 */
export interface ChargerInfo {
  /** Raw charger type code; page shows the number. */ // unconfirmed
  charger_type?: number;
  /** 1 = charging (mobile apps). */ // unconfirmed
  charge_status?: number;
  /** 1 = charger connected. charge_policy.rs:63 also accepts it as a string ("0" = no). */
  charger_connect?: number | string;
  /**
   * Inverted naming: "enable" = charging STOPPED, "disable" = charging
   * (charge_policy.rs:42, device_ext.rs:59).
   */
  direct_power_supply_mode?: string;
  /** 1 = reverse-charging (power bank) on. */ // unconfirmed
  otg_powerbank_state?: number;
  [key: string]: unknown;
}

/**
 * GET /api/device/system — device_ext.rs:22 `device_system`.
 * ubus passthrough: `system info` (standard OpenWrt procd). Shape from the
 * OpenWrt procd `system info` reply + device-info / dashboard pages.
 *
 * Page disagreement: device-info/page.tsx reads `hostname` and `kernel`;
 * OpenWrt `system info` does not return them (they are in `system board`),
 * so they are normally absent and the page shows "—".
 */
export interface DeviceSystem {
  /** Device epoch seconds (local wall time labelled UTC). */
  localtime?: number;
  /** Seconds since boot. */
  uptime?: number;
  /** 1/5/15-min load averages, fixed point ×65536. */
  load?: number[];
  /** Bytes. */
  memory?: {
    total?: number;
    free?: number;
    shared?: number;
    buffered?: number;
    available?: number;
    cached?: number;
  };
  root?: { total?: number; free?: number; used?: number; avail?: number };
  tmp?: { total?: number; free?: number; used?: number; avail?: number };
  swap?: { total?: number; free?: number };
  /** page expects hostname — not in `system info`. */ // unconfirmed
  hostname?: string;
  /** page expects kernel — not in `system info`. */ // unconfirmed
  kernel?: string;
}

/**
 * GET /api/device/charge-control — device_ext.rs:44 `charge_control_get`.
 * Built by the agent: sysfs battery status/capacity + `zwrt_bsp.charger list`
 * + the charge-limit enforcer's in-memory state (charge_policy.rs:218).
 * Also the body of PUT /api/device/charge-control (device_ext.rs:123).
 */
export interface ChargeControl {
  /** true when charger `direct_power_supply_mode` == "enable"; false when the ubus read fails. */
  charging_stopped: boolean;
  /** /sys/class/power_supply/battery/status, e.g. "Charging", "Not charging", "Discharging", "Full"; "" when unreadable. */
  battery_status: string;
  /** /sys/class/power_supply/battery/capacity; 0 when unreadable. */
  capacity: number;
  charge_limit_enabled: boolean;
  /** 50-100. */
  charge_limit: number;
  /** 1-20 (page slider only offers 1-10). */
  hysteresis: number;
  /** true after a manual PUT {charging_stopped}; cleared by any limit change. */
  manual_override: boolean;
}

/** PUT /api/device/charge-control body — device_ext.rs:81. Every field optional. */
export interface ChargeControlSet {
  charging_stopped?: boolean;
  charge_limit_enabled?: boolean;
  charge_limit?: number;
  hysteresis?: number;
}

/**
 * GET /api/device/fast-boot — device_ext.rs:156 `device_fast_boot_get`.
 * Built by the agent from `zwrt_mc.device.manager get_device_info`
 * `quicken_power_on` ("0" when the key is missing). Also the body of PUT.
 */
export interface FastBoot {
  /** "1" = on, "0" = off. */
  fast_boot: "0" | "1" | string;
}

/**
 * POST /api/device/power-save (a READ served over POST) —
 * device_ext.rs:126 `device_power_save_get`.
 * ubus passthrough: `zwrt_mc.device.manager get_device_info` with the request
 * body passed through verbatim. Request: `{deviceInfoList: ["power_saver_mode"]}`;
 * the reply holds the requested keys. Shape from page (router/device).
 */
export interface PowerSave {
  /** "1" = on. */ // unconfirmed
  power_saver_mode?: string;
  [key: string]: unknown;
}

/**
 * PUT /api/device/power-save body — device_ext.rs:141, passed verbatim to
 * `set_device_info`. The web page sends an array of objects
 * (`[{power_saver_mode}]`); the agent's own fast-boot handler and both mobile
 * apps send an object (`{power_saver_mode}`). Which one the firmware accepts
 * is unconfirmed.
 */
export interface PowerSaveSet {
  deviceInfoList: { power_saver_mode: string } | { power_saver_mode: string }[];
}

/**
 * GET /api/usb/status — usb.rs:6 `usb_status`.
 * ubus passthrough: `zwrt_bsp.usb list`, shape from page (usb) + mobile
 * DeviceParser.parseUSBStatus.
 */
export interface UsbStatus {
  /** 1 = USB data link up (host attached). */ // unconfirmed
  connect?: number;
  /** "debug" | "mtp" | "rndis" (the three the page offers). */ // unconfirmed
  mode?: string;
  /** Type-C CC state; mobile apps treat "no_cc" as no cable. */ // unconfirmed
  typec_cc?: string;
  /** USB-to-RJ45 adapter present. */ // unconfirmed
  usb2rj45?: number;
  [key: string]: unknown;
}

/** PUT /api/usb/mode body — usb.rs:13, passed verbatim to `zwrt_bsp.usb set`. */
export interface UsbModeSet {
  mode: "debug" | "mtp" | "rndis" | string;
}

/**
 * PUT /api/usb/powerbank body — usb.rs:24, passed verbatim to
 * `zwrt_bsp.powerbank set`. The web page sends `{state}`; the Android app sends
 * `{otg_powerbank_state}`. Which key the firmware reads is unconfirmed.
 */
export interface UsbPowerbankSet {
  state?: number;
  otg_powerbank_state?: number | string;
}

/**
 * Reply `data` of the ubus passthrough writes (PUT power-save, PUT usb/mode,
 * PUT usb/powerbank, POST reboot, POST factory-reset): the firmware's reply,
 * `null` when ubus printed nothing.
 */
export type UbusWriteReply = Record<string, unknown> | null;

/**
 * GET /api/device/thermal — device_ext.rs:8 `device_thermal`.
 * ubus passthrough: `zwrt_bsp.thermal get_cpu_temp {}`; 503 on ubus failure.
 * On MU5250 only `cpuss_temp` is trusted (zwrt-datad state.rs:1174-1185):
 * milli-°C when >= 1000, else °C; negative = unreadable. Other keys the
 * firmware may add are unknown.
 */
export interface DeviceThermal {
  cpuss_temp?: number; // unconfirmed: exact type (number vs numeric string) not seen in this repo
  [key: string]: unknown;
}

/** GET (and POST-as-read) endpoints of this area. */
export interface DeviceGetMap {
  "/api/device/battery-info": BatteryInfo;
  "/api/device/thermal": DeviceThermal;
  "/api/device/charger": ChargerInfo;
  "/api/device/system": DeviceSystem;
  "/api/device/charge-control": ChargeControl;
  "/api/device/fast-boot": FastBoot;
  "/api/usb/status": UsbStatus;
  /** POST, not GET — a read that takes a body ({deviceInfoList:["power_saver_mode"]}). */
  "/api/device/power-save": PowerSave;
}
