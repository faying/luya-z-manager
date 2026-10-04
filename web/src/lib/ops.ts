// The write-op layer on the web (E4 T9; write-op-layer.md DD4–DD16).
// Everything a transaction says — the sentence, old → new, the three steps,
// the countdown line, button labels, what can be undone — is datad's
// (STATE_V2.md §12), passed through by the agent's /api/ops. This file only
// picks the language, fills the countdown and decides what is still worth
// showing (a 3-second result only if this page saw it end).
import type { Tone } from "@/components/nd/tone";
import { pick } from "@/lib/i18n/pick";
import type { Lang } from "@/lib/i18n/config";

export const OPS_PATH = "/api/ops";
export const OPS_JOURNAL_PATH = "/api/ops/journal";

export type OpMark = "ok" | "warn" | "bad" | null;
export type OpStay = "live" | "brief" | "sticky" | "alert" | "none";

export interface OpStep {
  key: string;
  zh: string;
  en: string;
  done: boolean;
}

export interface OpUndo {
  ok: boolean;
  label_zh: string;
  label_en: string;
  why_zh: string | null;
  why_en: string | null;
  value?: string | null;
}

/** One transaction as datad shows it (`op.active` / `op.last`). */
export interface OpView {
  op_id: string;
  action: string;
  item: string;
  source: string;
  phase: string;
  reason: string | null;
  target?: string;
  old?: string;
  rollback_to?: string;
  remaining_ms: number | null;
  /** Added by the agent: the block went stale, the countdown is a guess. */
  frozen?: boolean;
  say_zh: string;
  say_en: string;
  next_zh: string | null;
  next_en: string | null;
  note_zh: string | null;
  note_en: string | null;
  what_zh: string;
  what_en: string;
  source_zh: string;
  source_en: string;
  old_zh?: string | null;
  old_en?: string | null;
  target_zh?: string | null;
  target_en?: string | null;
  rollback_to_zh?: string | null;
  rollback_to_en?: string | null;
  readback_zh?: string | null;
  readback_en?: string | null;
  mark: OpMark;
  stay: OpStay;
  steps: OpStep[];
  can_revert: boolean;
  can_keep: boolean;
  revert_label_zh?: string;
  revert_label_en?: string;
  keep_label_zh?: string;
  keep_label_en?: string;
  undo: OpUndo | null;
  acked?: boolean;
  needs_ack?: boolean;
}

export interface OpBlock {
  rollback_enabled: boolean;
  active: OpView | null;
  last: OpView | null;
  /** `rollback_on`: auto revert is on and nobody has pressed "Got it" on
   *  either side yet (DD18; acked with `op.notice_ack`). Absent on older datad. */
  notice?: string | null;
}

/** GET /api/ops. */
export interface OpsState {
  op: OpBlock | null;
  datad: "up" | "stuck" | "down";
  stalled: boolean;
  /** datad has the write-op layer (an older one has no `op`). */
  supported: boolean;
}

/** Where each item's page is (the bar links there; DD4). */
export const ITEM_ROUTE: Record<string, string> = {
  "network.mode": "/router/network-mode",
};

export const isFinal = (phase: string) =>
  ["confirmed", "unverified", "rolled_back", "not_applied", "rollback_failed", "cancelled"].includes(phase);

/** `m:ss`, never negative. */
export function fmtClock(ms: number): string {
  const s = Math.max(0, Math.ceil(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** datad's countdown line with `{t}` filled in (null when there is none). */
export function fillNext(next: string | null | undefined, ms: number | null | undefined): string | null {
  if (!next) return null;
  return next.replace("{t}", fmtClock(ms ?? 0));
}

export function markTone(mark: OpMark, stay?: OpStay): Tone {
  if (stay === "alert") return "bad";
  if (mark === "ok") return "ok";
  if (mark === "warn") return "warn";
  if (mark === "bad") return "bad";
  return "neutral";
}

/** One of datad's paired texts in the page language. */
export function opText(v: OpView | OpUndo | null | undefined, field: string, lang: Lang): string {
  if (!v) return "";
  const o = v as unknown as Record<string, unknown>;
  const s = (x: unknown) => (typeof x === "string" ? x : null);
  return pick(s(o[`${field}_zh`]), s(o[`${field}_en`]), lang);
}

// ── what this page has seen (V2-35) ─────────────────────────────────────────
// A "brief" result (● Now Y, 3 s) shows only if this page saw the transaction
// end: its op_id was active here, or `last` changed while the page was open.
// The first read after loading never replays one. Module-level: one per page
// load, shared by the bar and every page, kept across navigation.

const BRIEF_MS = 3000;
const seenActive = new Set<string>();
const endedAt = new Map<string, number>();
const ackedHere = new Set<string>();
let noticeAckedHere = false;
let lastAtLoad: string | null | undefined; // undefined = nothing read yet

/** Feed every /api/ops read through this. */
export function observeOps(op: OpBlock | null | undefined, now = Date.now()): void {
  if (!op) return;
  if (op.active) seenActive.add(op.active.op_id);
  const lastId = op.last?.op_id ?? null;
  if (lastAtLoad === undefined) {
    lastAtLoad = lastId;
    // something we started ourselves and saw end in the very first read
    if (lastId && seenActive.has(lastId) && !op.active) endedAt.set(lastId, now);
    return;
  }
  if (lastId && !endedAt.has(lastId) && (lastId !== lastAtLoad || seenActive.has(lastId))) {
    endedAt.set(lastId, now);
  }
}

/** A write this page sent: its result counts as seen even if it ends between two reads. */
export function noteMine(opId: string | null | undefined): void {
  if (opId) seenActive.add(opId);
}

/** "Got it" pressed here: hide at once, before datad's next read says so. */
export function noteAcked(opId: string): void {
  ackedHere.add(opId);
}

/** "Got it" on the auto-revert notice went through here: hide it before the next read. */
export function noteNoticeAcked(): void {
  noticeAckedHere = true;
}

/** For tests. */
export function resetOpsSeen(): void {
  seenActive.clear();
  endedAt.clear();
  ackedHere.clear();
  noticeAckedHere = false;
  lastAtLoad = undefined;
}

/** The one-time notice to show (DD18): only the one this page knows, and not
 *  once "Got it" went through here. */
export function opNotice(op: OpBlock | null | undefined): "rollback_on" | null {
  if (!op || noticeAckedHere) return null;
  return op.notice === "rollback_on" ? "rollback_on" : null;
}

export type OpShow =
  | { kind: "live"; v: OpView }
  | { kind: "brief" | "sticky" | "alert"; v: OpView }
  | null;

/** What to show now: the running one, else a result still worth showing. */
export function opShow(op: OpBlock | null | undefined, now = Date.now()): OpShow {
  if (!op) return null;
  if (op.active) return { kind: "live", v: op.active };
  const l = op.last;
  if (!l || ackedHere.has(l.op_id)) return null;
  const waiting = l.needs_ack ?? !l.acked;
  if (l.stay === "alert" && waiting) return { kind: "alert", v: l };
  if (l.stay === "sticky" && waiting) return { kind: "sticky", v: l };
  if (l.stay === "brief") {
    const at = endedAt.get(l.op_id);
    if (at !== undefined && now - at < BRIEF_MS) return { kind: "brief", v: l };
  }
  return null;
}

/** The countdown can't be trusted: datad stuck, the block stale, or no read for 20 s (DD12). */
export const STALL_MS = 20000;
export function countdownFrozen(s: OpsState | undefined, lastOkAt: number | null, now = Date.now()): boolean {
  if (!s) return true;
  if (s.datad !== "up" || s.op?.active?.frozen) return true;
  return lastOkAt === null || now - lastOkAt > STALL_MS;
}

/** Time left now, from what the agent said at `lastOkAt`. */
export function remainingNow(v: OpView, lastOkAt: number | null, frozen: boolean, now = Date.now()): number {
  const ms = v.remaining_ms ?? 0;
  if (frozen || lastOkAt === null) return ms;
  return Math.max(0, ms - (now - lastOkAt));
}

/** A change log time ("2026-10-03 14:32:07", device wall time) as "10-03 14:32". */
export function shortWhen(t: string | null | undefined): string {
  if (!t || t.length < 16) return t ?? "";
  return `${t.slice(5, 10)} ${t.slice(11, 16)}`;
}

// ── the change log (journal.list, V2-41; DD5, DD10, DD11) ───────────────────

export const JOURNAL_LIMIT = 50;
export const JOURNAL_PATH = `${OPS_JOURNAL_PATH}?limit=${JOURNAL_LIMIT}`;

export interface UndoView {
  ok: boolean;
  label_zh: string;
  label_en: string;
  why_zh: string | null;
  why_en: string | null;
  /** Send as it is (the agent adds `source: web`). */
  request: { action: string; undo: boolean; params: Record<string, unknown> } | null;
}

/** One row as datad words it; the raw journal fields are kept too. */
export interface JournalEntry {
  op_id?: string | null;
  action?: string;
  item?: string;
  source?: string;
  t?: string;
  undo?: boolean;
  reason?: string | null;
  what_zh?: string;
  what_en?: string;
  change_zh?: string;
  change_en?: string;
  result_zh?: string;
  result_en?: string;
  source_zh?: string;
  source_en?: string;
  mark?: OpMark;
  hide?: boolean;
  undo_view?: UndoView | null;
}

export interface JournalOwner {
  source: string;
  user: boolean;
  undo: boolean;
  value: string;
  op_id: string | null;
  ts: number;
  t: string;
}

export interface Journal {
  entries: JournalEntry[];
  owners: Record<string, JournalOwner>;
}

/** Source display names (DD14) for `owners`, which carry only the raw source. */
export const SOURCE_NAMES: Record<string, [string, string]> = {
  screen: ["触屏", "Screen"],
  legacy: ["触屏", "Screen"],
  web: ["网页", "Web"],
  scenario: ["情景", "Scene"],
  scheduler: ["定时任务", "Schedule"],
  auto: ["自动", "Auto"],
  guard: ["自动恢复", "Auto-recovery"],
};

export function sourceName(source: string | null | undefined, lang: Lang): string {
  const n = source ? SOURCE_NAMES[source] : undefined;
  if (!n) return source ?? "";
  return lang === "en" ? n[1] : n[0];
}

/** The rows to list: datad marks the ones that are not rows of their own. */
export const visibleEntries = (j: Journal | undefined): JournalEntry[] => (j?.entries ?? []).filter((e) => !e.hide);

/** A key that stays with the entry when newer rows push it down the list
 *  (never the row's position: the detail's undo must not move to another row). */
export function entryKey(e: JournalEntry): string {
  return e.op_id ? `op:${e.op_id}` : `t:${e.t ?? ""}|${e.action ?? ""}|${e.source ?? ""}|${e.result_zh ?? ""}`;
}

/** The second line: "10-03 14:32 · 网页 · 已切到只用 4G", without repeating a
 *  source the result already starts with ("情景跳过 ×5 …"). */
export function entryLine2(e: JournalEntry, lang: Lang): string {
  const result = pick(e.result_zh, e.result_en, lang);
  const source = pick(e.source_zh, e.source_en, lang);
  const parts = [shortWhen(e.t)];
  if (source && !result.startsWith(source)) parts.push(source);
  if (result) parts.push(result);
  return parts.filter(Boolean).join(" · ");
}

/** Undo/redo that cuts the mobile link gets the tier-3 dialog (DD10). */
export const CUTS_UPLINK = new Set(["network.set_mode"]);
