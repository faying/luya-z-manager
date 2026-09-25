// Response shapes for the SMS inbox and SMS forwarding endpoints of zte-agent
// (zte-agent/src/sms.rs, sms_forward.rs).
//
// sms.rs is a thin ubus passthrough to the firmware's `zwrt_wms` object: the
// firmware JSON is returned as-is in `data`. Nothing in the agent pins those
// shapes down, so they come from the pages plus the two companion apps, which
// parse them strictly (mobile/ios/OpenU60/Features/SMS/SMSModels.swift:194-235,
// mobile/android/.../core/model/SMSModels.kt:175-197) and from what
// sms_forward.rs / public.rs read out of the same ubus calls.
//
// sms_forward.rs builds its JSON itself from serde structs, so the Rust shape
// is the source of truth there.
//
// Types only — no runtime code.

// ── Inbox (ubus passthrough) ────────────────────────────────────────────────

/**
 * One message in `zte_libwms_get_sms_data`'s `messages` array.
 * ubus passthrough: zwrt_wms zte_libwms_get_sms_data, shape from page + apps.
 */
export interface SmsMessage {
  /** Row id in /etc_rw/ztembb/ztesms/sms_db/sms.db. A JSON number (iOS parses `as? Int`). */
  id: number;
  /** Sender / recipient as plain text (not UCS2), e.g. "+886912345678" or a short code. */
  number: string;
  /** Body as UCS2 (UTF-16BE) hex — decode with lib/sms.ts decodeSms. */
  content: string;
  /** "YY,MM,DD,HH,MM,SS,+TZ" — TZ in quarter hours (+32 = UTC+8). Device-local wall time. */
  date: string;
  /**
   * "0" read, "1" unread, "2" sent, "3" send failed, "4" draft.
   * A STRING per the iOS app (guard `item["tag"] as? String`, SMSModels.swift:205;
   * sms_forward.rs:1004-1008 accepts number or string).
   * page expects number (sms/page.tsx `tag: number`, unread = `tag !== 0`). // unconfirmed
   */
  tag: string;
  /** "nv" (device) or "sim". page expects optional string; apps default to "nv". */ // unconfirmed
  mem_store?: string;
  /** Concatenated-SMS / draft group; apps read it, the web page doesn't. */ // unconfirmed
  draft_group_id?: string;
}

/**
 * POST /api/sms/list (a read) — sms.rs:10 `sms_list`.
 * ubus passthrough: zwrt_wms zte_libwms_get_sms_data, shape from page + apps.
 * The handler coerces `mem_store` ("sim" → 2, anything else → 1) and fills
 * defaults page 0 / data_per_page 500 / tags 10 / order_by "order by id desc"
 * (sms.rs:21-34). Each store only returns its own rows (sms_forward.rs:944-
 * 970 queries both and dedups), so the page's "All" and "Device" filters show
 * the same NV-only list.
 */
export interface SmsList {
  messages: SmsMessage[];
  /** page expects `total`; neither app reads it. */ // unconfirmed
  total?: number;
}

/** POST /api/sms/list body (what sms/page.tsx sends). */
export interface SmsListRequest {
  page?: number;
  data_per_page?: number;
  /** Friendly string or the firmware's integer (1 = NV, 2 = SIM). */
  mem_store?: "all" | "sim" | "device" | number;
  tags?: number;
  order_by?: string;
}

/**
 * GET /api/sms/capacity — sms.rs:42 `sms_capacity`.
 * ubus passthrough: zwrt_wms zwrt_wms_get_wms_capacity, key names from the
 * apps (SMSModels.swift:227-235) and public.rs:107-113 / sms_forward.rs:975-985.
 * Numbers (apps parse `as? Int`; the agent also accepts numeric strings).
 *
 * B27 (recorded 2026-09-25) sends per-store totals and per-box counts:
 * sms_{sim,nv}_total, sms_{sim,nv}_{rev,send,draftbox}_total,
 * sms_{dev,sim}_unread_num, and sms_nvused_total. There is NO
 * sms_simused_total, and sms_nvused_total read 0 while sms_nv_rev_total was 6,
 * so it is not trusted: used = rev + send + draftbox per store
 * (lib/sms.ts `storeUsage`). `*used_total` is only a fallback for firmware
 * without the per-box keys.
 */
export interface SmsCapacity {
  sms_nv_total?: number;
  sms_sim_total?: number;
  /** Received / sent / draft counts per store (B27). */
  sms_nv_rev_total?: number;
  sms_nv_send_total?: number;
  sms_nv_draftbox_total?: number;
  sms_sim_rev_total?: number;
  sms_sim_send_total?: number;
  sms_sim_draftbox_total?: number;
  /** Present on B27 but unreliable (0 with 6 received); fallback only. */
  sms_nvused_total?: number;
  /** Not sent by B27; kept for firmware that has it (fallback only). */
  sms_simused_total?: number;
  /** Unread on the device store; public.rs adds sms_sim_unread_num for status `sms.unread`. */
  sms_dev_unread_num?: number;
  sms_sim_unread_num?: number;
}

/**
 * The firmware's generic `{result}` reply for zte_libwms_send_sms,
 * zwrt_wms_modify_tag and zwrt_wms_delete_sms. result 3 = success
 * (sms_forward.rs:473-482; sms.rs:64-67 notes delete returns 3 even when it
 * did nothing). ubus passthrough — the agent never checks it.
 */
export interface SmsWmsResult {
  result: number | string; // unconfirmed
}

/** POST /api/sms/send — sms.rs:49 `sms_send`. Body passed to zte_libwms_send_sms as-is. */
export interface SmsSendRequest {
  number: string;
  /** "YYYYMMDDHHmmss", browser local time. */
  sms_time: string;
  /** UCS2 hex when encode_type is "ucs2", else plain text. */
  message_body: string;
  id: string;
  encode_type: "ucs2" | "gsm7";
}

/** POST /api/sms/read — sms.rs:178 `sms_mark_read`. Passed to zwrt_wms_modify_tag as-is. */
export interface SmsMarkReadRequest {
  /** "3681;3682;" — semicolon-joined with trailing ";". */
  id: string;
  tag: number;
}

/**
 * POST /api/sms/delete — sms.rs:68 `sms_delete`. NOT the standard envelope:
 * extra keys sit beside `data`, and the sqlite branch has no `data` at all.
 * - all rows gone after ubus: `{ok:true, data:<ubus result>, deleted_via:"ubus"}`
 * - some rows survived (SIM store bug) and were sqlite-deleted: `{ok:true, deleted_via:"sqlite", ids:[…]}`
 * - db check failed: `{ok:true, data:<ubus result>, warning:"db check skipped: …"}`
 */
export type SmsDeleteBody =
  | { ok: true; data: SmsWmsResult | null; deleted_via: "ubus" }
  | { ok: true; deleted_via: "sqlite"; ids: number[] }
  | { ok: true; data: SmsWmsResult; warning: string };

// ── Forwarding (agent-built JSON) ───────────────────────────────────────────

/** sms_forward.rs:64 `SmsFilter` (serde tag = "type"). */
export type SmsFilter =
  | { type: "all" }
  | { type: "sender"; patterns: string[] }
  | { type: "content"; keywords: string[] }
  | { type: "sender_and_content"; patterns: string[]; keywords: string[] };

/** sms_forward.rs:116 `HttpHeader`. */
export interface HttpHeader {
  name: string;
  value: string;
}

/** sms_forward.rs:80 `ForwardDestination` (serde tag = "type"). */
export type ForwardDestination =
  | { type: "telegram"; bot_token: string; chat_id: string; silent: boolean }
  /** page expects no `headers` (WebhookDest); the agent always serialises it (default []). */
  | { type: "webhook"; url: string; method: string; headers: HttpHeader[] }
  | { type: "sms"; forward_number: string }
  /** `token` is omitted when None (skip_serializing_if). */
  | { type: "ntfy"; url: string; topic: string; token?: string }
  | { type: "discord"; webhook_url: string }
  | { type: "slack"; webhook_url: string };

/** sms_forward.rs:54 `ForwardRule`. */
export interface ForwardRule {
  id: number;
  name: string;
  enabled: boolean;
  filter: SmsFilter;
  destination: ForwardDestination;
}

/** sms_forward.rs:25 `SmsForwardConfig`. Also the `data` of PUT /api/sms/forward/config. */
export interface SmsForwardConfig {
  enabled: boolean;
  /** >= 10 enforced on PUT (sms_forward.rs:1118-1122); page input has min=5. */
  poll_interval_secs: number;
  mark_read_after_forward: boolean;
  delete_after_forward: boolean;
  rules: ForwardRule[];
}

/** GET /api/sms/forward/config — sms_forward.rs:1079 `config_get`. */
export interface SmsForwardConfigResponse {
  config: SmsForwardConfig;
  /** Highest SMS id the forwarder has processed (page: optional). */
  last_forwarded_id: number;
}

/** PUT /api/sms/forward/config body — every field optional (sms_forward.rs:1096-1106). */
export interface SmsForwardConfigRequest {
  enabled?: boolean;
  poll_interval_secs?: number;
  mark_read_after_forward?: boolean;
  delete_after_forward?: boolean;
}

/** sms_forward.rs:129 `ForwardLogEntry`. */
export interface ForwardLogEntry {
  /** Unix seconds, device clock (local wall time labelled UTC) — show with fmtDevice. */
  timestamp: number;
  sms_id: number;
  sender: string;
  /** DECODED text (not UCS2), first 80 bytes + "..." (sms_forward.rs:901-907). */
  content_preview: string;
  rule_name: string;
  /** "telegram" | "webhook" | "sms" | "ntfy" | "discord" | "slack". */
  destination_type: string;
  success: boolean;
  /** Omitted when None. */
  error?: string;
  rule_id: number;
  /** Full decoded body (page doesn't read it; used by retry). */
  content: string;
  /** Raw firmware date "YY,MM,DD,HH,MM,SS,+TZ". */
  date: string;
  // page expects an optional `id?: string` — the agent never sends one.
}

/**
 * GET /api/sms/forward/log — sms_forward.rs:1283 `log_get`. Newest first
 * (stored oldest first, reversed on read). Max 200 entries.
 */
export type SmsForwardLog = ForwardLogEntry[];

/** `data` of POST /api/sms/forward/test and /retry on success. */
export interface SmsForwardSent {
  status: "sent";
}

// ── Endpoint maps ───────────────────────────────────────────────────────────

/** GET endpoints of this area. */
export interface SmsGetMap {
  "/api/sms/capacity": SmsCapacity;
  "/api/sms/forward/config": SmsForwardConfigResponse;
  "/api/sms/forward/log": SmsForwardLog;
}

/** POST endpoints that are reads (body: SmsListRequest). Not GET-able. */
export interface SmsReadPostMap {
  "/api/sms/list": SmsList;
}
