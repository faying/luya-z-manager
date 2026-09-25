// SMS inbox + SMS forwarding — mirrors zte-agent/src/sms.rs (a ubus
// passthrough to the firmware's zwrt_wms) and sms_forward.rs (agent-built
// JSON, config + log kept in agent memory / a JSON file).
//
// Model: `inbox` is the firmware's sms.db. Bodies are UCS2 (UTF-16BE) hex and
// dates "YY,MM,DD,HH,MM,SS,+TZquarters", exactly as the firmware stores them,
// so lib/sms.ts decodeSms / formatSmsDate work. `tag` is a STRING
// ("0" read, "1" unread, "2" sent, "3" failed) — that is what the companion
// apps parse (iOS SMSModels.swift:205 guards `as? String`); the web page
// compares `tag !== 0` and will therefore treat every row as unread. That is a
// page bug, deliberately not papered over here.
//
// shared.sms.unread (what /api/public/status reports) is recomputed from the
// inbox after every change: sms_dev_unread_num + sms_sim_unread_num.
//
// Persona: travelling in Taiwan (device clock = Taiwan wall time, +32 quarter
// hours), 6 received messages, 2 unread; one forwarding rule sends codes to an
// invented Bark-style webhook; the log has one failure from landing (no WAN
// yet) and one HTTP 502.

import type { Route, Ctx, Reply } from "../lib.ts";
import { ok, fail, bodyField, clone } from "../lib.ts";
import { shared } from "../shared.ts";
import type {
  SmsMessage,
  SmsList,
  SmsCapacity,
  SmsWmsResult,
  SmsFilter,
  ForwardDestination,
  HttpHeader,
  ForwardRule,
  SmsForwardConfig,
  SmsForwardConfigResponse,
  ForwardLogEntry,
  SmsForwardLog,
  SmsForwardSent,
} from "../../../src/lib/api/schemas/sms.ts";

// ── Encoding helpers (firmware formats) ─────────────────────────────────────

/** UTF-16BE hex, upper case — how the firmware stores bodies (sms_forward.rs encode_ucs2_hex). */
function ucs2(text: string): string {
  let out = "";
  for (let i = 0; i < text.length; i++) {
    out += text.charCodeAt(i).toString(16).toUpperCase().padStart(4, "0");
  }
  return out;
}

function isUcs2Hex(s: string): boolean {
  const t = s.trim();
  return t.length > 0 && t.length % 4 === 0 && /^[0-9a-fA-F]+$/.test(t);
}

function fromUcs2(hex: string): string {
  let out = "";
  for (let i = 0; i + 4 <= hex.length; i += 4) out += String.fromCharCode(parseInt(hex.slice(i, i + 4), 16));
  return out;
}

const pad2 = (n: number) => String(n).padStart(2, "0");

/** Taiwan is UTC+8 = +32 quarter hours. */
const TZ_QUARTERS = "+32";
const DEVICE_UTC_OFFSET_S = 8 * 3600;

/** Firmware date string for a device-local wall time. */
function smsDate(y: number, mo: number, d: number, h: number, mi: number, s: number): string {
  return [y % 100, mo, d, h, mi, s].map(pad2).join(",") + "," + TZ_QUARTERS;
}

/** Device epoch (local wall time labelled UTC) for a device-local wall time. */
function devEpoch(y: number, mo: number, d: number, h: number, mi: number, s: number): number {
  return Date.UTC(y, mo - 1, d, h, mi, s) / 1000;
}

/** Device epoch "now": real unix time + the Taiwan offset (see CLAUDE.md 当地时间). */
function devNow(ctx: Ctx): number {
  return Math.floor(ctx.now / 1000) + DEVICE_UTC_OFFSET_S;
}

function smsDateNow(ctx: Ctx): string {
  const t = new Date(devNow(ctx) * 1000);
  return smsDate(t.getUTCFullYear(), t.getUTCMonth() + 1, t.getUTCDate(), t.getUTCHours(), t.getUTCMinutes(), t.getUTCSeconds());
}

/** sms_forward.rs:901 preview — first `max` UTF-8 bytes on a char boundary, then "...". */
function preview(s: string, max: number): string {
  if (Buffer.byteLength(s, "utf8") <= max) return s;
  let out = "";
  let bytes = 0;
  for (const ch of s) {
    const b = Buffer.byteLength(ch, "utf8");
    if (bytes + b > max) break;
    out += ch;
    bytes += b;
  }
  return out + "...";
}

// ── Inbox state (the firmware's sms.db) ─────────────────────────────────────

type Store = "nv" | "sim";

interface Row {
  body: string; // decoded text; encoded on the way out
  msg: SmsMessage;
}

function row(id: number, store: Store, tag: string, number: string, when: [number, number, number, number, number, number], text: string): Row {
  return {
    body: text,
    msg: {
      id,
      number,
      content: ucs2(text),
      date: smsDate(...when),
      tag,
      mem_store: store,
      draft_group_id: "",
    } satisfies SmsMessage,
  };
}

const inbox: Row[] = [
  row(101, "sim", "0", "800", [2026, 9, 21, 14, 5, 33],
    "【中華電信】歡迎您來到台灣！您已登錄中華電信網路，漫遊服務已開通。撥打 800 可查詢漫遊資費，祝您旅途愉快。"),
  row(102, "nv", "0", "106580", [2026, 9, 21, 14, 6, 10],
    "【漫游提醒】尊敬的客户，您已抵达中国台湾地区。境外流量包 25元/天，达 3GB 后限速，当日有效。退订请回复 TD。"),
  row(103, "nv", "0", "+886955120334", [2026, 9, 22, 19, 47, 2],
    "【星海銀行】您的驗證碼為 482913，5 分鐘內有效。本行不會以任何方式向您索取驗證碼，請勿告知他人。"),
  row(104, "nv", "0", "+886912000451", [2026, 9, 23, 9, 12, 44],
    "Your Apple Account code is: 739204. Don't share it with anyone."),
  row(105, "nv", "1", "+886938000217", [2026, 9, 24, 8, 31, 20],
    "G-204918 is your Google verification code."),
  row(106, "nv", "1", "+886975331208", [2026, 9, 24, 10, 2, 55],
    "到机场了吗？我们在出境大厅 3 号门等你，车停在 P2 🙂"),
];

const NV_TOTAL = 100;
const SIM_TOTAL = 30;

function unreadIn(store: Store): number {
  return inbox.filter((r) => r.msg.mem_store === store && r.msg.tag === "1").length;
}

function syncUnread(): void {
  shared.sms.unread = unreadIn("nv") + unreadIn("sim");
}
syncUnread();

function boxCount(store: Store, tags: string[]): number {
  return inbox.filter((r) => r.msg.mem_store === store && tags.includes(String(r.msg.tag))).length;
}

function capacity(ctx: Ctx): SmsCapacity {
  // Per-box counts (B27 keys): tag 0/1 received, 2/3 sent, 4 draft.
  const perBox = {
    sms_nv_rev_total: boxCount("nv", ["0", "1"]),
    sms_nv_send_total: boxCount("nv", ["2", "3"]),
    sms_nv_draftbox_total: boxCount("nv", ["4"]),
    sms_sim_rev_total: boxCount("sim", ["0", "1"]),
    sms_sim_send_total: boxCount("sim", ["2", "3"]),
    sms_sim_draftbox_total: boxCount("sim", ["4"]),
  };
  if (ctx.has("firmware-b27")) {
    // B27 (recorded): no sms_simused_total; sms_nvused_total present but 0
    // even with received messages — the page must not trust it.
    return {
      sms_nv_total: NV_TOTAL,
      sms_sim_total: SIM_TOTAL,
      ...perBox,
      sms_nvused_total: 0,
      sms_dev_unread_num: unreadIn("nv"),
      sms_sim_unread_num: unreadIn("sim"),
    } satisfies SmsCapacity;
  }
  // Default persona: the per-box keys plus the older *used_total pair.
  return {
    sms_nv_total: NV_TOTAL,
    sms_nvused_total: inbox.filter((r) => r.msg.mem_store === "nv").length,
    sms_sim_total: SIM_TOTAL,
    sms_simused_total: inbox.filter((r) => r.msg.mem_store === "sim").length,
    ...perBox,
    sms_dev_unread_num: unreadIn("nv"),
    sms_sim_unread_num: unreadIn("sim"),
  } satisfies SmsCapacity;
}

/** WMS "success" (sms_forward.rs:473-482: result 3 = sent). */
const WMS_OK: SmsWmsResult = { result: 3 };

function isObj(b: unknown): b is Record<string, unknown> {
  return !!b && typeof b === "object" && !Array.isArray(b);
}

/** sms.rs:109 parse_ids — "3681;3682;". */
function parseIds(field: unknown): { ids: number[] } | { error: string } {
  if (field === undefined || field === null) return { error: "missing 'id' field" };
  if (typeof field !== "string") return { error: "'id' must be a string" };
  const ids: number[] = [];
  for (const part of field.split(";")) {
    const p = part.trim();
    if (!p) continue;
    if (!/^[+-]?\d+$/.test(p)) return { error: `invalid id '${p}' (must be integer)` };
    ids.push(Number(p));
  }
  return { ids };
}

function wanUp(ctx: Ctx): boolean {
  return !shared.airplane && shared.mobileData && !ctx.has("nosignal");
}

// ── Inbox handlers ──────────────────────────────────────────────────────────

/** POST /api/sms/list — sms.rs:10. */
function smsList(ctx: Ctx): Reply {
  if (!isObj(ctx.body)) return fail("invalid JSON", 400);
  const b = ctx.body;
  const ms = b.mem_store;
  const memInt = typeof ms === "number" ? Math.trunc(ms) : ms === "sim" ? 2 : 1;
  const store: Store = memInt === 2 ? "sim" : "nv";
  const page = typeof b.page === "number" ? b.page : 0;
  const perPage = typeof b.data_per_page === "number" && b.data_per_page > 0 ? b.data_per_page : 500;
  const tags = typeof b.tags === "number" ? b.tags : 10;
  const order = typeof b.order_by === "string" ? b.order_by : "order by id desc";

  // tags: 10 = everything; 1 = received (read + unread), 2 = sent (+failed).
  // The finer semantics of the firmware's tag filter are unknown (mock guess).
  let rows = inbox.filter((r) => r.msg.mem_store === store);
  if (tags === 1) rows = rows.filter((r) => r.msg.tag === "0" || r.msg.tag === "1");
  else if (tags === 2) rows = rows.filter((r) => r.msg.tag === "2" || r.msg.tag === "3");
  rows = [...rows].sort((a, c) => (/\basc\b/i.test(order) ? a.msg.id - c.msg.id : c.msg.id - a.msg.id));
  const slice = rows.slice(page * perPage, page * perPage + perPage);
  const data: SmsList = { messages: slice.map((r) => clone(r.msg)) };
  return ok(data);
}

/** GET /api/sms/capacity — sms.rs:42. */
function smsCapacity(ctx: Ctx): Reply {
  return ok(capacity(ctx));
}

/** "YYYYMMDDHHmmss" → firmware date; falls back to device now. */
function dateFromSmsTime(v: unknown, ctx: Ctx): string {
  if (typeof v === "string" && /^\d{14}$/.test(v)) {
    const n = (a: number, l: number) => Number(v.slice(a, a + l));
    return smsDate(n(0, 4), n(4, 2), n(6, 2), n(8, 2), n(10, 2), n(12, 2));
  }
  return smsDateNow(ctx);
}

/** POST /api/sms/send — sms.rs:49. The firmware stores the outgoing message (tag 2, or 3 on failure). */
function smsSend(ctx: Ctx): Reply {
  if (!isObj(ctx.body)) return fail("invalid JSON", 400);
  const b = ctx.body;
  const number = typeof b.number === "string" ? b.number.trim() : "";
  const raw = typeof b.message_body === "string" ? b.message_body : "";
  const text = b.encode_type === "ucs2" && isUcs2Hex(raw) ? fromUcs2(raw) : raw;
  // Firmware result on failure is not known; 4 is invented. The agent passes it
  // through as ok:true either way (no readback of `result`, sms.rs:54-57).
  // SMS needs cellular service, not mobile data. "fakesuccess": ok:true, message stored as failed.
  const sent = !shared.airplane && !ctx.has("nosignal") && !ctx.has("fakesuccess") && number !== "";
  const id = inbox.reduce((m, r) => Math.max(m, r.msg.id), 0) + 1;
  inbox.push({
    body: text,
    msg: {
      id,
      number,
      content: ucs2(text),
      date: dateFromSmsTime(b.sms_time, ctx),
      tag: sent ? "2" : "3",
      mem_store: "nv",
      draft_group_id: "",
    },
  });
  return ok(sent ? WMS_OK : { result: 4 });
}

/** POST /api/sms/delete — sms.rs:68. NV rows go via ubus; SIM rows survive ubus (firmware bug) and are sqlite-deleted. */
function smsDelete(ctx: Ctx): Reply {
  if (!isObj(ctx.body)) return fail("invalid JSON", 400);
  const parsed = parseIds(ctx.body.id);
  if ("error" in parsed) return fail(parsed.error, 400);
  if (parsed.ids.length === 0) return fail("no ids in 'id' field", 400);
  const want = new Set(parsed.ids);
  // ubus zwrt_wms_delete_sms: removes NV rows only.
  for (let i = inbox.length - 1; i >= 0; i--) {
    if (want.has(inbox[i].msg.id) && inbox[i].msg.mem_store === "nv") inbox.splice(i, 1);
  }
  const survivors = inbox.filter((r) => want.has(r.msg.id)).map((r) => r.msg.id);
  if (survivors.length === 0) {
    syncUnread();
    return { raw: { ok: true, data: WMS_OK, deleted_via: "ubus" } };
  }
  for (let i = inbox.length - 1; i >= 0; i--) {
    if (survivors.includes(inbox[i].msg.id)) inbox.splice(i, 1);
  }
  syncUnread();
  return { raw: { ok: true, deleted_via: "sqlite", ids: survivors } };
}

/** POST /api/sms/read — sms.rs:178, ubus zwrt_wms_modify_tag {id, tag}. */
function smsRead(ctx: Ctx): Reply {
  if (!isObj(ctx.body)) return fail("invalid JSON", 400);
  const parsed = parseIds(ctx.body.id);
  const tagRaw = ctx.body.tag;
  const tag = typeof tagRaw === "number" || typeof tagRaw === "string" ? String(tagRaw) : "0";
  if ("ids" in parsed) {
    const want = new Set(parsed.ids);
    for (const r of inbox) {
      // Only received messages flip between read/unread.
      if (want.has(r.msg.id) && (r.msg.tag === "0" || r.msg.tag === "1")) r.msg.tag = tag;
    }
    syncUnread();
  }
  return ok(WMS_OK);
}

// ── Forwarding state ────────────────────────────────────────────────────────

const RULE_CODES: ForwardRule = {
  id: 1,
  name: "验证码 → 手机推送",
  enabled: true,
  filter: { type: "content", keywords: ["验证码", "驗證碼", "code"] },
  destination: { type: "webhook", url: "https://push.example.net/bark/Q7mZr2x9Ke/", method: "POST", headers: [] },
};

const fwdConfig: SmsForwardConfig = {
  enabled: true,
  poll_interval_secs: 30,
  mark_read_after_forward: false,
  delete_after_forward: false,
  rules: [RULE_CODES],
} satisfies SmsForwardConfig;

const lastForwardedId = 106;

function logEntry(
  ts: number,
  smsId: number,
  sender: string,
  date: string,
  text: string,
  error?: string,
): ForwardLogEntry {
  const e: ForwardLogEntry = {
    timestamp: ts,
    sms_id: smsId,
    sender,
    content_preview: preview(text, 80),
    rule_name: RULE_CODES.name,
    destination_type: "webhook",
    success: error === undefined,
    rule_id: RULE_CODES.id,
    content: text,
    date,
  };
  if (error !== undefined) e.error = error;
  return e;
}

const byId = (id: number) => inbox.find((r) => r.msg.id === id);
function logFor(id: number, ts: number, error?: string): ForwardLogEntry {
  const r = byId(id);
  if (!r) throw new Error(`fixture: sms ${id} missing`);
  return logEntry(ts, id, r.msg.number, r.msg.date, r.body, error);
}

/** Stored oldest first, like ForwardState.log; GET reverses. */
const fwdLog: ForwardLogEntry[] = [
  // SMS 100 arrived right after landing, before data came up; it has since been deleted from the inbox.
  logEntry(devEpoch(2026, 9, 21, 13, 58, 41), 100, "+886955120334", smsDate(2026, 9, 21, 13, 58, 40),
    "【星海銀行】您的驗證碼為 663015，用於綁定新裝置，5 分鐘內有效。", "no WAN connectivity (permanent error)"),
  logFor(103, devEpoch(2026, 9, 22, 19, 47, 4)),
  logFor(104, devEpoch(2026, 9, 23, 9, 13, 21), "webhook: HTTP 502"),
  logFor(105, devEpoch(2026, 9, 24, 8, 31, 22)),
];

// ── Forwarding validation (serde-style messages) ────────────────────────────

const DEST_TYPES = ["telegram", "webhook", "sms", "ntfy", "discord", "slack"];
const FILTER_TYPES = ["all", "sender", "content", "sender_and_content"];

type Parsed<T> = { v: T } | { err: string };

function str(o: Record<string, unknown>, k: string): Parsed<string> {
  const v = o[k];
  if (v === undefined) return { err: `missing field \`${k}\`` };
  if (typeof v !== "string") return { err: `invalid type: ${typeName(v)}, expected a string` };
  return { v };
}

function strList(o: Record<string, unknown>, k: string): Parsed<string[]> {
  const v = o[k];
  if (v === undefined) return { err: `missing field \`${k}\`` };
  if (!Array.isArray(v)) return { err: `invalid type: ${typeName(v)}, expected a sequence` };
  if (v.some((x) => typeof x !== "string")) return { err: "invalid type: expected a string" };
  return { v: v as string[] };
}

function typeName(v: unknown): string {
  if (v === null) return "null";
  if (Array.isArray(v)) return "sequence";
  if (typeof v === "object") return "map";
  if (typeof v === "string") return `string ${JSON.stringify(v)}`;
  if (typeof v === "boolean") return `boolean \`${v}\``;
  if (typeof v === "number") return Number.isInteger(v) ? `integer \`${v}\`` : `floating point \`${v}\``;
  return typeof v;
}

function tagOf(v: unknown, kinds: string[]): Parsed<{ o: Record<string, unknown>; t: string }> {
  if (!isObj(v)) return { err: `invalid type: ${typeName(v)}, expected internally tagged enum` };
  const t = v.type;
  if (t === undefined) return { err: "missing field `type`" };
  if (typeof t !== "string" || !kinds.includes(t)) {
    return { err: `unknown variant \`${String(t)}\`, expected one of ${kinds.map((k) => `\`${k}\``).join(", ")}` };
  }
  return { v: { o: v, t } };
}

function parseFilter(v: unknown): Parsed<SmsFilter> {
  const tg = tagOf(v, FILTER_TYPES);
  if ("err" in tg) return tg;
  const { o, t } = tg.v;
  if (t === "all") return { v: { type: "all" } };
  if (t === "sender") {
    const p = strList(o, "patterns");
    return "err" in p ? p : { v: { type: "sender", patterns: p.v } };
  }
  if (t === "content") {
    const k = strList(o, "keywords");
    return "err" in k ? k : { v: { type: "content", keywords: k.v } };
  }
  const p = strList(o, "patterns");
  if ("err" in p) return p;
  const k = strList(o, "keywords");
  if ("err" in k) return k;
  return { v: { type: "sender_and_content", patterns: p.v, keywords: k.v } };
}

function parseDestination(v: unknown): Parsed<ForwardDestination> {
  const tg = tagOf(v, DEST_TYPES);
  if ("err" in tg) return tg;
  const { o, t } = tg.v;
  switch (t) {
    case "telegram": {
      const bt = str(o, "bot_token");
      if ("err" in bt) return bt;
      const ci = str(o, "chat_id");
      if ("err" in ci) return ci;
      return { v: { type: "telegram", bot_token: bt.v, chat_id: ci.v, silent: o.silent === true } };
    }
    case "webhook": {
      const url = str(o, "url");
      if ("err" in url) return url;
      const method = typeof o.method === "string" ? o.method : "POST";
      const headers: HttpHeader[] = Array.isArray(o.headers)
        ? o.headers.filter(isObj).map((h) => ({ name: String(h.name ?? ""), value: String(h.value ?? "") }))
        : [];
      return { v: { type: "webhook", url: url.v, method, headers } };
    }
    case "sms": {
      const n = str(o, "forward_number");
      return "err" in n ? n : { v: { type: "sms", forward_number: n.v } };
    }
    case "ntfy": {
      const url = str(o, "url");
      if ("err" in url) return url;
      const topic = str(o, "topic");
      if ("err" in topic) return topic;
      const d: ForwardDestination = { type: "ntfy", url: url.v, topic: topic.v };
      if (typeof o.token === "string") d.token = o.token;
      return { v: d };
    }
    default: {
      const w = str(o, "webhook_url");
      if ("err" in w) return w;
      return { v: { type: t as "discord" | "slack", webhook_url: w.v } };
    }
  }
}

const badJson = (msg: string) => fail(`invalid JSON: ${msg}`, 400);

/** Simulated delivery. Only the WAN matters here; SMS destinations go over the modem. */
function deliver(dest: ForwardDestination, ctx: Ctx): string | null {
  if (dest.type === "sms") {
    return shared.airplane || ctx.has("nosignal") ? "sms forward: device rejected (result=4)" : null;
  }
  if (!wanUp(ctx)) {
    return `${dest.type}: io: failed to lookup address information: Temporary failure in name resolution`;
  }
  return null;
}

// ── Forwarding handlers ─────────────────────────────────────────────────────

/** GET /api/sms/forward/config — sms_forward.rs:1079. */
function fwdConfigGet(): Reply {
  const data: SmsForwardConfigResponse = { config: clone(fwdConfig), last_forwarded_id: lastForwardedId };
  return ok(data);
}

/** PUT /api/sms/forward/config — sms_forward.rs:1095. Fields apply in order; a bad interval returns 400 AFTER `enabled` was already changed in memory (as the Rust does). */
function fwdConfigSet(ctx: Ctx): Reply {
  if (!isObj(ctx.body)) return badJson("EOF while parsing a value at line 1 column 0");
  const b = ctx.body;
  for (const k of ["enabled", "mark_read_after_forward", "delete_after_forward"]) {
    if (b[k] !== undefined && b[k] !== null && typeof b[k] !== "boolean") {
      return badJson(`invalid type: ${typeName(b[k])}, expected a boolean`);
    }
  }
  const pi = b.poll_interval_secs;
  if (pi !== undefined && pi !== null && !(typeof pi === "number" && Number.isInteger(pi) && pi >= 0)) {
    return badJson(`invalid type: ${typeName(pi)}, expected u64`);
  }
  if (typeof b.enabled === "boolean") fwdConfig.enabled = b.enabled;
  if (typeof pi === "number") {
    if (pi < 10) return fail("poll_interval_secs must be >= 10", 400);
    fwdConfig.poll_interval_secs = pi;
  }
  if (typeof b.mark_read_after_forward === "boolean") fwdConfig.mark_read_after_forward = b.mark_read_after_forward;
  if (typeof b.delete_after_forward === "boolean") fwdConfig.delete_after_forward = b.delete_after_forward;
  return ok(clone(fwdConfig));
}

function parseRuleBody(b: Record<string, unknown>, needId: boolean): Parsed<Omit<ForwardRule, "id"> & { id: number }> {
  let id = 0;
  if (needId) {
    if (b.id === undefined) return { err: "missing field `id`" };
    if (typeof b.id !== "number" || !Number.isInteger(b.id) || b.id < 0) return { err: `invalid type: ${typeName(b.id)}, expected u32` };
    id = b.id;
  }
  const name = str(b, "name");
  if ("err" in name) return name;
  let enabled = true;
  if (b.enabled === undefined) {
    if (needId) return { err: "missing field `enabled`" };
  } else if (typeof b.enabled !== "boolean") {
    return { err: `invalid type: ${typeName(b.enabled)}, expected a boolean` };
  } else {
    enabled = b.enabled;
  }
  if (b.filter === undefined) return { err: "missing field `filter`" };
  const filter = parseFilter(b.filter);
  if ("err" in filter) return filter;
  if (b.destination === undefined) return { err: "missing field `destination`" };
  const destination = parseDestination(b.destination);
  if ("err" in destination) return destination;
  return { v: { id, name: name.v, enabled, filter: filter.v, destination: destination.v } };
}

/** POST /api/sms/forward/rules — sms_forward.rs:1136. 201 + the new rule. */
function rulesCreate(ctx: Ctx): Reply {
  if (!isObj(ctx.body)) return badJson("EOF while parsing a value at line 1 column 0");
  const p = parseRuleBody(ctx.body, false);
  if ("err" in p) return badJson(p.err);
  const rule: ForwardRule = { ...p.v, id: fwdConfig.rules.reduce((m, r) => Math.max(m, r.id), 0) + 1 };
  fwdConfig.rules.push(rule);
  return { status: 201, data: clone(rule) };
}

/** PUT /api/sms/forward/rules — sms_forward.rs:1173. */
function rulesUpdate(ctx: Ctx): Reply {
  if (!isObj(ctx.body)) return badJson("EOF while parsing a value at line 1 column 0");
  const p = parseRuleBody(ctx.body, true);
  if ("err" in p) return badJson(p.err);
  const rule = fwdConfig.rules.find((r) => r.id === p.v.id);
  if (!rule) return fail("rule not found", 404);
  rule.name = p.v.name;
  rule.enabled = p.v.enabled;
  rule.filter = p.v.filter;
  rule.destination = p.v.destination;
  return ok(clone(rule));
}

function u64Field(b: unknown, k: string): number | null {
  const v = bodyField(b, k);
  return typeof v === "number" && Number.isInteger(v) && v >= 0 ? v : null;
}

/** DELETE /api/sms/forward/rules — sms_forward.rs:1205. `{ok:true}` with no data. */
function rulesDelete(ctx: Ctx): Reply {
  if (ctx.body === undefined) return fail("invalid JSON", 400);
  const id = u64Field(ctx.body, "id");
  if (id === null) return fail("missing 'id' field", 400);
  const i = fwdConfig.rules.findIndex((r) => r.id === id);
  if (i < 0) return fail("rule not found", 404);
  fwdConfig.rules.splice(i, 1);
  return ok();
}

/** PUT /api/sms/forward/rules/toggle — sms_forward.rs:1228. */
function rulesToggle(ctx: Ctx): Reply {
  if (ctx.body === undefined) return fail("invalid JSON", 400);
  const id = u64Field(ctx.body, "id");
  if (id === null) return fail("missing 'id' field", 400);
  const enabled = bodyField(ctx.body, "enabled");
  if (typeof enabled !== "boolean") return fail("missing 'enabled' field", 400);
  const rule = fwdConfig.rules.find((r) => r.id === id);
  if (!rule) return fail("rule not found", 404);
  rule.enabled = enabled;
  return ok(clone(rule));
}

/** POST /api/sms/forward/test — sms_forward.rs:1255. Body `{destination}`; really sends. */
function fwdTest(ctx: Ctx): Reply {
  if (!isObj(ctx.body)) return badJson("EOF while parsing a value at line 1 column 0");
  if (ctx.body.destination === undefined) return badJson("missing field `destination`");
  const d = parseDestination(ctx.body.destination);
  if ("err" in d) return badJson(d.err);
  const err = deliver(d.v, ctx);
  if (err) return fail(err, 502);
  const data: SmsForwardSent = { status: "sent" };
  return { data, delayMs: 700 };
}

/** GET /api/sms/forward/log — sms_forward.rs:1283, newest first. */
function fwdLogGet(): Reply {
  const data: SmsForwardLog = clone(fwdLog).reverse();
  return ok(data);
}

/** POST /api/sms/forward/log/clear — sms_forward.rs:1291. */
function fwdLogClear(): Reply {
  fwdLog.length = 0;
  return ok();
}

/** POST /api/sms/forward/retry — sms_forward.rs:1299. `{index}` into the newest-first log. */
function fwdRetry(ctx: Ctx): Reply {
  if (!isObj(ctx.body)) return badJson("EOF while parsing a value at line 1 column 0");
  const index = ctx.body.index;
  if (index === undefined) return badJson("missing field `index`");
  if (typeof index !== "number" || !Number.isInteger(index) || index < 0) {
    return badJson(`invalid type: ${typeName(index)}, expected usize`);
  }
  const internal = fwdLog.length - 1 - index;
  const entry = fwdLog[internal];
  if (internal < 0 || !entry) return fail("log entry not found", 404);
  if (entry.success) return fail("entry already succeeded", 400);
  const rule = fwdConfig.rules.find((r) => r.id === entry.rule_id);
  if (!rule) return fail("rule no longer exists", 404);
  const err = deliver(rule.destination, ctx);
  entry.timestamp = devNow(ctx);
  if (err) {
    entry.error = err;
    return fail(err, 502);
  }
  entry.success = true;
  delete entry.error;
  const data: SmsForwardSent = { status: "sent" };
  return { data, delayMs: 700 };
}

// ── Routes ──────────────────────────────────────────────────────────────────

export const routes: Route[] = [
  { method: "POST", path: "/api/sms/list", handler: smsList, kind: "read" },
  { method: "GET", path: "/api/sms/capacity", handler: smsCapacity },
  { method: "POST", path: "/api/sms/send", handler: smsSend },
  { method: "POST", path: "/api/sms/delete", handler: smsDelete },
  { method: "POST", path: "/api/sms/read", handler: smsRead },

  { method: "GET", path: "/api/sms/forward/config", handler: fwdConfigGet },
  { method: "PUT", path: "/api/sms/forward/config", handler: fwdConfigSet },
  { method: "POST", path: "/api/sms/forward/rules", handler: rulesCreate },
  { method: "PUT", path: "/api/sms/forward/rules", handler: rulesUpdate },
  { method: "DELETE", path: "/api/sms/forward/rules", handler: rulesDelete },
  { method: "PUT", path: "/api/sms/forward/rules/toggle", handler: rulesToggle },
  { method: "POST", path: "/api/sms/forward/test", handler: fwdTest },
  { method: "GET", path: "/api/sms/forward/log", handler: fwdLogGet },
  { method: "POST", path: "/api/sms/forward/log/clear", handler: fwdLogClear },
  { method: "POST", path: "/api/sms/forward/retry", handler: fwdRetry },
];
