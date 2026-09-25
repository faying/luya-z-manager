// Response shapes for the call / USSD / SIM Toolkit endpoints of zte-agent
// (zte-agent/src/telephony.rs). Every handler talks AT commands via
// at_cmd::send (fixed sleep, no port mutex) and builds its JSON in Rust, so the
// Rust shape is the source of truth; page disagreements are noted inline.
// Types only — no runtime code.

/** One entry of AT+CLCC (telephony.rs:115 `parse_clcc`). */
export interface CallEntry {
  id: number;
  dir: "mo" | "mt" | "unknown";
  stat: "active" | "held" | "dialing" | "alerting" | "incoming" | "waiting" | "releasing" | "unknown";
  mode: number;
  number: string;
}

/**
 * GET /api/call/status — telephony.rs:334 `call_status`. Sends AT+CLCC (~2.3 s).
 * No web page calls it.
 */
export interface CallStatus {
  calls: CallEntry[];
}

/**
 * POST /api/ussd/send (telephony.rs:384) and POST /api/ussd/respond
 * (telephony.rs:419) — data of a successful reply (`format_ussd_response`,
 * telephony.rs:280, or the no-+CUSD fallback with status -1).
 * send: reply containing "ERROR" = 500 "USSD failed". respond: never checks
 * ERROR — an ERROR reply comes back as 200 with response "…ERROR…", status -1.
 * Page (router/stk `USSDResponse`) types `status` as string — handler sends a
 * number; page does not declare raw_response / dcs.
 */
export interface UssdResponse {
  /** Decoded text (GSM7 / UCS2 / ASCII per DCS). */
  response: string;
  /** Undecoded body from +CUSD. */
  raw_response: string;
  /** +CUSD status: 0 done, 1 further action required, 2 terminated, -1 no +CUSD line. page expects string */
  status: number;
  dcs: number;
  /** status === 1 */
  session_active: boolean;
}

/** POST /api/ussd/send body. Agent keeps only digits and `*#+`. */
export interface UssdSendBody {
  code: string;
}

/** POST /api/ussd/respond body. Agent keeps only digits and `*#+`. */
export interface UssdRespondBody {
  reply: string;
}

/** One STK menu item. Handler sends `id` as a number (page types string | number). */
export interface StkItem {
  id: number;
  label: string;
}

/**
 * GET /api/stk/menu — telephony.rs:457 `stk_menu`.
 * Three shapes: unsupported ({supported:false, items:[], reason}); menu
 * ({supported:true, title, items, source}); supported but nothing pending
 * ({supported:true, items:[], reason:"no proactive command pending", diagnostics}).
 * Page reads only supported / title / items.
 */
export interface StkMenu {
  supported: boolean;
  title?: string;
  items: StkItem[];
  source?: "at_cusatd" | "at_stgi";
  reason?: string;
  diagnostics?: string[];
}

/** POST /api/stk/select body. `item_id` must be a JSON integer (as_u64), else 400 "missing item_id". */
export interface StkSelectBody {
  item_id: number;
}

/**
 * POST /api/stk/select — telephony.rs:524 `stk_select` (data).
 * Unsupported SIM: {supported:false, reason} (checked BEFORE the body is parsed).
 * Sub-menu: {type:"menu", title, items, source:"at_cusate"}; otherwise
 * {type:"display", data:<AT reply without control chars>}. AT failure = 500
 * "item selection failed".
 */
export type StkSelectResult =
  | { supported: false; reason: string }
  | { type: "menu"; title: string; items: StkItem[]; source: "at_cusate" }
  | { type: "display"; data: string };

/** GET endpoints of this file, keyed by exact path (record.ts). */
export interface TelephonyGetMap {
  "/api/call/status": CallStatus;
  "/api/stk/menu": StkMenu;
}
