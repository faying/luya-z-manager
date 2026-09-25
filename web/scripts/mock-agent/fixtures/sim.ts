// SIM endpoints (sim.rs): info, IMEI, NCK trials, PIN verify / change / mode,
// NCK unlock. All are ubus passthroughs to zwrt_zte_mdm.api; payloads follow
// the page types (see schemas/sim.ts for what is unconfirmed).
//
// Persona: a China Mobile (46000) home SIM roaming in Taiwan, PIN lock off,
// 3 PIN / 10 PUK attempts left. IMSI / ICCID / IMEI are synthetic.
// Mock secrets: PIN "1234", PUK "12345678", NCK "1234567890123456".

import type { Ctx, Reply, Route } from "../lib.ts";
import { ok, fail, clone, bodyField } from "../lib.ts";
import type { SimInfo, SimImei, SimLockTrials } from "../../../src/lib/api/schemas/sim.ts";

const OBJ = "zwrt_zte_mdm.api";

function ubusFail(method: string, why: string): Reply {
  return fail(`ubus call ${OBJ} ${method} failed: ${why}`, 503);
}

function needJson(ctx: Ctx): Reply | null {
  return ctx.body === undefined ? fail("invalid JSON", 400) : null;
}

function str(v: unknown): string {
  return typeof v === "string" ? v : typeof v === "number" ? String(v) : "";
}

const PUK = "12345678";
const NCK = "1234567890123456";
const PIN_MAX = 3;
const PUK_MAX = 10;
const NCK_MAX = 5;

const secret = { pin: "1234" };

const info: SimInfo = {
  sim_states: "modem_init_complete",
  modem_main_state: "modem_init_complete",
  pin_status: "0",
  sim_imsi: "460000000000001",
  sim_iccid: "89860000000000000001",
  pinnumber: String(PIN_MAX),
  puknumber: String(PUK_MAX),
} satisfies SimInfo;

const imei: SimImei = { imei: "860000000000001" } satisfies SimImei;

const trials: SimLockTrials = { available_trials: String(NCK_MAX), ret: 0 } satisfies SimLockTrials;

function setState(s: "modem_init_complete" | "modem_waitpin" | "modem_waitpuk") {
  info.sim_states = s;
  info.modem_main_state = s;
}

/** Wrong PIN: burn an attempt; at 0 the SIM drops to PUK. */
function wrongPin() {
  const left = Math.max(0, Number(info.pinnumber ?? PIN_MAX) - 1);
  info.pinnumber = String(left);
  if (left === 0) setState("modem_waitpuk");
}

export const routes: Route[] = [
  { method: "GET", path: "/api/sim/info", handler: () => ok(clone(info)) },
  { method: "GET", path: "/api/sim/imei", handler: () => ok(clone(imei)) },
  { method: "GET", path: "/api/sim/lock-trials", handler: () => ok(clone(trials)) },
  {
    method: "POST",
    path: "/api/sim/pin/verify",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      const pin = str(bodyField(ctx.body, "pin_num"));
      const puk = str(bodyField(ctx.body, "puk_num"));
      if (ctx.has("fakesuccess")) return ok(null);
      if (info.sim_states === "modem_waitpuk") {
        if (puk !== PUK) {
          const left = Math.max(0, Number(info.puknumber ?? PUK_MAX) - 1);
          info.puknumber = String(left);
          return ubusFail("sim_verify_pin_puk", "Unknown error");
        }
        if (pin.length < 4) return ubusFail("sim_verify_pin_puk", "Invalid argument");
        secret.pin = pin;
        info.pinnumber = String(PIN_MAX);
        info.puknumber = String(PUK_MAX);
        setState("modem_init_complete");
        return ok(null);
      }
      if (info.sim_states !== "modem_waitpin") {
        // Nothing to verify; firmware behaviour unknown — treat as a plain check.
        return pin === secret.pin ? ok(null) : ubusFail("sim_verify_pin_puk", "Unknown error");
      }
      if (pin !== secret.pin) {
        wrongPin();
        return ubusFail("sim_verify_pin_puk", "Unknown error");
      }
      info.pinnumber = String(PIN_MAX);
      setState("modem_init_complete");
      return ok(null);
    },
  },
  {
    method: "POST",
    path: "/api/sim/pin/change",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      const oldPin = str(bodyField(ctx.body, "pin_num"));
      const newPin = str(bodyField(ctx.body, "new_pin_num"));
      if (ctx.has("fakesuccess")) return ok(null);
      if (info.sim_states !== "modem_init_complete") return ubusFail("sim_change_pin", "Operation not permitted");
      // 3GPP: changing the PIN requires the PIN lock to be enabled.
      if (info.pin_status !== "1") return ubusFail("sim_change_pin", "Operation not permitted");
      if (oldPin !== secret.pin) {
        wrongPin();
        return ubusFail("sim_change_pin", "Unknown error");
      }
      if (!/^\d{4,8}$/.test(newPin)) return ubusFail("sim_change_pin", "Invalid argument");
      secret.pin = newPin;
      info.pinnumber = String(PIN_MAX);
      return ok(null);
    },
  },
  {
    method: "POST",
    path: "/api/sim/pin/mode",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      const pin = str(bodyField(ctx.body, "pin_num_m"));
      const mode = str(bodyField(ctx.body, "pin_mode"));
      if (mode !== "0" && mode !== "1") return ubusFail("sim_change_pin_mode", "Invalid argument");
      if (ctx.has("fakesuccess")) return ok(null);
      if (info.sim_states !== "modem_init_complete") return ubusFail("sim_change_pin_mode", "Operation not permitted");
      if (pin !== secret.pin) {
        wrongPin();
        return ubusFail("sim_change_pin_mode", "Unknown error");
      }
      info.pin_status = mode;
      info.pinnumber = String(PIN_MAX);
      return ok(null);
    },
  },
  {
    method: "POST",
    path: "/api/sim/unlock",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      const nck = str(bodyField(ctx.body, "nck"));
      if (ctx.has("fakesuccess")) return ok(null);
      const left = Number(trials.available_trials ?? NCK_MAX);
      if (left <= 0) return ubusFail("set_simlock_nck", "Operation not permitted");
      if (nck !== NCK) {
        trials.available_trials = String(left - 1);
        return ubusFail("set_simlock_nck", "Unknown error");
      }
      // Device is not network-locked; a correct code is accepted and changes nothing.
      return ok(null);
    },
  },
];
