// Response shapes for the SIM endpoints of zte-agent (zte-agent/src/sim.rs).
//
// Every handler is a ubus passthrough to `zwrt_zte_mdm.api` — the firmware's
// JSON comes back as-is inside {ok,data}; ubus failure = 503
// {ok:false,error:"ubus call zwrt_zte_mdm.api <method> failed: <stderr>"}.
// Shapes come from the pages (router/sim, device-info) + controls-inventory.
// Types only — no runtime code.

/**
 * GET /api/sim/info — sim.rs:6 `sim_info`.
 * ubus passthrough: `zwrt_zte_mdm.api get_sim_info`, shape from pages
 * (router/sim `SIMInfo` ∪ device-info `SimInfo`).
 * router/sim: PIN-locked when sim_states or modem_main_state (lower-cased) is
 * "wait pin" / "modem_waitpin"; PUK-locked for "wait puk" / "modem_waitpuk".
 */
export interface SimInfo {
  sim_states?: string; // unconfirmed (value set)
  modem_main_state?: string; // unconfirmed (value set)
  /** "1" = PIN lock enabled, "0" = disabled. */
  pin_status?: string; // unconfirmed
  sim_imsi?: string;
  sim_iccid?: string;
  /** PIN attempts left (shown only when truthy). */
  pinnumber?: string; // unconfirmed
  /** PUK attempts left (shown only when truthy). */
  puknumber?: string; // unconfirmed
}

/**
 * GET /api/sim/imei — sim.rs:13 `sim_imei`.
 * ubus passthrough: `zwrt_zte_mdm.api get_imei`, shape from device-info page.
 */
export interface SimImei {
  imei?: string;
}

/**
 * GET /api/sim/lock-trials — sim.rs:64 `sim_lock_trials`.
 * ubus passthrough: `zwrt_zte_mdm.api get_simlock_available_trials`, shape from router/sim page.
 * Remaining NCK (network-lock) attempts.
 */
export interface SimLockTrials {
  available_trials?: string; // unconfirmed
  ret?: number; // unconfirmed
}

/** POST /api/sim/pin/verify body (sim.rs:20, `sim_verify_pin_puk`). PUK path: pin_num = new PIN. */
export interface SimPinVerifyBody {
  pin_num: string;
  puk_num: string;
  pin_encode_flag: string;
}

/** POST /api/sim/pin/change body (sim.rs:31, `sim_change_pin`). */
export interface SimPinChangeBody {
  pin_num: string;
  new_pin_num: string;
  pin_encode_flag: string;
}

/** POST /api/sim/pin/mode body (sim.rs:42, `sim_change_pin_mode`). pin_mode "1" = enable PIN lock. */
export interface SimPinModeBody {
  pin_num_m: string;
  pin_mode: "0" | "1";
  pin_encode_flag: string;
}

/** POST /api/sim/unlock body (sim.rs:53, `set_simlock_nck`). */
export interface SimUnlockBody {
  nck: string;
}

/** GET endpoints of this file, keyed by exact path (record.ts). */
export interface SimGetMap {
  "/api/sim/info": SimInfo;
  "/api/sim/imei": SimImei;
  "/api/sim/lock-trials": SimLockTrials;
}
