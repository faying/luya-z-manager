// The write-op layer (E4 T9): GET /api/ops, POST /api/ops/act, POST
// /api/ops/write, GET /api/ops/journal. Shapes copied from datad's real view
// (STATE_V2.md §12; touch-ui tests/op_view_test.c) and journal (V2-41).
//
// Off by default ("supported": false, like an older datad) so the network-mode
// page keeps its own readback in the other specs. Tests pick a state with
// GET /__mock/ops?set=<preset> (server.ts); the same reply lists what the
// page sent (`acts`, `writes`).

import { ok, fail, bodyField, type Route } from "../lib.ts";

type Json = Record<string, unknown>;

const STEPS = (a: boolean, r: boolean, d: boolean) => [
  { key: "applied", zh: "设置已生效", en: "Setting applied", done: a },
  { key: "registered", zh: "已注册", en: "Registered", done: r },
  { key: "data", zh: "数据", en: "Data", done: d },
];

const base = (o: Json): Json => ({
  action: "network.set_mode",
  item: "network.mode",
  source: "web",
  source_zh: "网页",
  source_en: "Web",
  undo: null,
  what_zh: "制式",
  what_en: "Network mode",
  target: "Only_5G",
  target_zh: "只用 5G SA",
  target_en: "5G SA only",
  old: "WL_AND_5G",
  old_zh: "自动",
  old_en: "Auto",
  rollback_to: "WL_AND_5G",
  rollback_to_zh: "自动",
  rollback_to_en: "Auto",
  readback_zh: null,
  readback_en: null,
  note_zh: null,
  note_en: null,
  next_zh: null,
  next_en: null,
  reason: null,
  remaining_ms: null,
  can_revert: false,
  can_keep: false,
  revert_label_zh: "退回自动",
  revert_label_en: "Revert to Auto",
  keep_label_zh: "保留只用 5G SA",
  keep_label_en: "Keep 5G SA only",
  steps: STEPS(true, true, true),
  ...o,
});

const verifying = (opId: string, rollback = true): Json =>
  base({
    op_id: opId,
    phase: "verifying",
    say_zh: "正在确认",
    say_en: "Checking",
    next_zh: rollback ? "{t} 后没通就退回到自动" : "还剩 {t} · 自动退回没开",
    next_en: rollback ? "Back to Auto in {t} if no data" : "{t} left · auto revert off",
    remaining_ms: 102000,
    mark: null,
    stay: "live",
    can_revert: true,
    can_keep: true,
    readback_zh: "只用 5G SA",
    readback_en: "5G SA only",
    steps: STEPS(true, true, false),
  });

const result = (opId: string, o: Json): Json => base({ op_id: opId, acked: false, needs_ack: true, ...o });

const PRESETS: Record<string, () => { supported: boolean; datad: string; op: Json | null }> = {
  off: () => ({ supported: false, datad: "up", op: null }),
  idle: () => ({ supported: true, datad: "up", op: { rollback_enabled: true, active: null, last: null } }),
  "idle-rollback-off": () => ({ supported: true, datad: "up", op: { rollback_enabled: false, active: null, last: null } }),
  // auto revert just switched on, nobody pressed "Got it" yet (DD18)
  notice: () => ({ supported: true, datad: "up", op: { rollback_enabled: true, active: null, last: null, notice: "rollback_on" } }),
  // started on the touch screen: the web shows it too (DD14: source 触屏)
  verifying: () => ({
    supported: true,
    datad: "up",
    op: { rollback_enabled: true, active: { ...verifying("screen-12-3"), source: "screen", source_zh: "触屏", source_en: "Screen" }, last: null },
  }),
  "rollback-off": () => ({ supported: true, datad: "up", op: { rollback_enabled: false, active: verifying("screen-12-4", false), last: null } }),
  sticky: () => ({
    supported: true,
    datad: "up",
    op: {
      rollback_enabled: true,
      active: null,
      last: result("web-7", {
        phase: "rolled_back",
        reason: "timeout",
        say_zh: "没通 · 已退回自动",
        say_en: "No data · back to Auto",
        mark: "warn",
        stay: "sticky",
        undo: { ok: false, label_zh: "撤销", label_en: "Undo", why_zh: "设置没变 · 不用撤销", why_en: "Nothing to undo", value: "WL_AND_5G" },
      }),
    },
  }),
  "rollback-failed": () => ({
    supported: true,
    datad: "up",
    op: {
      rollback_enabled: true,
      active: null,
      last: result("web-8", {
        phase: "rollback_failed",
        reason: "rollback_timeout",
        say_zh: "退回也没通",
        say_en: "Revert failed",
        mark: "bad",
        stay: "alert",
        readback_zh: null,
        readback_en: null,
        steps: STEPS(true, false, false),
      }),
    },
  }),
  "not-applied": () => ({
    supported: true,
    datad: "up",
    op: {
      rollback_enabled: true,
      active: null,
      last: result("web-9", {
        phase: "not_applied",
        reason: "ignored",
        say_zh: "没切成 · 还是自动",
        say_en: "Didn't apply · still Auto",
        mark: "warn",
        stay: "sticky",
        steps: STEPS(false, true, true),
      }),
    },
  }),
  stuck: () => ({ supported: true, datad: "stuck", op: { rollback_enabled: true, active: verifying("screen-12-5"), last: null } }),
};

// journal.list as datad words it (V2-41): newest first, one row per kind
const ENTRIES: Json[] = [
  { source: "screen", action: "op.ack", result: "ok", hide: true, what_zh: "op.ack", what_en: "op.ack" },
  {
    op_id: "web-20", action: "network.set_mode", item: "network.mode", source: "web", undo: false, new: "Only_LTE", old: "WL_AND_5G",
    t: "2026-10-03 14:32:07", what_zh: "制式", what_en: "Network mode", change_zh: "自动 → 只用 4G", change_en: "Auto → 4G only",
    result_zh: "已切到只用 4G", result_en: "Now 4G only", mark: "ok", source_zh: "网页", source_en: "Web", hide: false,
    undo_view: { ok: true, label_zh: "撤销", label_en: "Undo", why_zh: null, why_en: null, request: { action: "network.set_mode", undo: true, params: { mode: "WL_AND_5G" } } },
  },
  {
    action: "cellular.set", source: "screen", params: { enabled: 0 }, result: "failed", t: "2026-10-03 09:01:00",
    what_zh: "移动数据", what_en: "Mobile data", change_zh: "关掉数据", change_en: "Turn data off", result_zh: "没改成", result_en: "Not changed",
    mark: "bad", source_zh: "触屏", source_en: "Screen", hide: false, undo_view: null,
  },
  {
    action: "wifi.apply", item: "wifi", source: "scenario", result: "skipped", skip: "end", count: 5, t: "2026-10-02 22:10:00",
    what_zh: "Wi-Fi", what_en: "Wi-Fi", change_zh: "", change_en: "", result_zh: "情景跳过 ×5（你手动改过）", result_en: "Scene skipped ×5 (you changed it by hand)",
    mark: "warn", source_zh: "情景", source_en: "Scene", hide: false, undo_view: null,
  },
  {
    action: "esim.switch", source: "web", journal_append: true, result: "ok", old: "CMLink", new: "Ubigi", t: "2026-10-02 08:00:00",
    what_zh: "eSIM", what_en: "eSIM", change_zh: "CMLink → Ubigi", change_en: "CMLink → Ubigi", result_zh: "已改", result_en: "Done",
    mark: "ok", source_zh: "网页", source_en: "Web", hide: false, undo_view: null,
  },
  {
    op_id: "screen-3-1", action: "network.set_mode", item: "network.mode", source: "screen", undo: false, new: "WL_AND_5G", old: "Only_5G",
    t: "2026-10-01 19:45:00", what_zh: "制式", what_en: "Network mode", change_zh: "只用 5G SA → 自动", change_en: "5G SA only → Auto",
    result_zh: "已切到自动", result_en: "Now Auto", mark: "ok", source_zh: "触屏", source_en: "Screen", hide: false,
    undo_view: { ok: false, label_zh: "撤销", label_en: "Undo", why_zh: "之后又改过", why_en: "Changed since", request: { action: "network.set_mode", undo: true, params: { mode: "Only_5G" } } },
  },
];
const OWNERS: Json = {
  "network.mode": { source: "web", user: true, undo: false, value: "Only_LTE", op_id: "web-20", ts: 1791117127, t: "2026-10-03 14:32:07" },
};
let journal: "full" | "empty" | "down" = "full";
let entries: Json[] = ENTRIES.slice();
/** The vendor call errors after datad started the transaction (503 + op). */
let applyFails = false;

let state = PRESETS.off();
let acts: Json[] = [];
let writes: Json[] = [];
let n = 0;

/** /__mock/ops?set=<preset>: pick a state, clear the logs. Returns the logs (accepted acts only). */
export function mockOps(set: string | null): { ok: boolean; error?: string; data?: Json } {
  if (set !== null) {
    if (set === "journal-push") {
      // a newer change lands (from the touch screen), nothing else changes
      entries = [
        {
          action: "cellular.set", source: "screen", params: { roaming: 0 }, result: "ok", t: "2026-10-03 15:00:00",
          what_zh: "数据漫游", what_en: "Data roaming", change_zh: "关掉漫游", change_en: "Turn roaming off", result_zh: "已改", result_en: "Done",
          mark: "ok", source_zh: "触屏", source_en: "Screen", hide: false, undo_view: null,
        },
        ...entries,
      ];
      return { ok: true, data: { supported: state.supported, datad: state.datad, acts, writes } };
    }
    entries = ENTRIES.slice();
    applyFails = set === "idle-apply-fails";
    if (set === "idle-apply-fails") {
      state = PRESETS.idle();
      acts = [];
      writes = [];
      return { ok: true, data: { supported: true, datad: "up", acts, writes } };
    }
    if (set === "journal-empty" || set === "journal-down") {
      state = PRESETS.idle();
      journal = set === "journal-empty" ? "empty" : "down";
      acts = [];
      writes = [];
      return { ok: true, data: { supported: true, datad: "up", acts, writes } };
    }
    journal = "full";
    const p = PRESETS[set];
    if (!p) return { ok: false, error: `unknown ops preset: ${set} (known: ${Object.keys(PRESETS).join(", ")})` };
    state = p();
    acts = [];
    writes = [];
  }
  return { ok: true, data: { supported: state.supported, datad: state.datad, acts, writes } };
}

/** Network mode written while the layer is on: a transaction starts (modem.ts). */
export function opsApplyFails(): boolean {
  return applyFails;
}

export function opsStartNetworkMode(target: string): Json | null {
  if (!state.supported || !state.op) return null;
  const v = verifying(`web-${Date.now()}-${++n}`, !!state.op.rollback_enabled);
  v.target = target;
  v.source = "web";
  state.op = { ...state.op, active: v };
  return { op_id: v.op_id, phase: "applying" };
}

export const routes: Route[] = [
  {
    method: "GET",
    path: "/api/ops",
    handler: () => ok({ op: state.op, datad: state.datad, stalled: state.datad === "stuck", supported: state.supported }),
  },
  {
    method: "POST",
    path: "/api/ops/act",
    handler: (ctx) => {
      const act = String(bodyField(ctx.body, "act") ?? "");
      const opId = String(bodyField(ctx.body, "op_id") ?? "");
      if (act === "notice_ack") {
        acts.push({ act });
        if (state.op) state.op = { ...state.op, notice: null };
        return ok({ notice: "rollback_on", acked: true });
      }
      if (!["revert", "keep", "ack"].includes(act) || !opId) return fail("bad request", 400);
      const op = state.op as Json | null;
      const active = op?.active as Json | null;
      const last = op?.last as Json | null;
      if (act === "ack") {
        if (!last || last.op_id !== opId) return { status: 409, error: "not the last result" };
        acts.push({ act, op_id: opId });
        state.op = { ...op, last: { ...last, acked: true, needs_ack: false } };
        return ok(null);
      }
      if (!active || active.op_id !== opId || !active.can_revert) return { status: 409, error: "不在等确认" };
      acts.push({ act, op_id: opId });
      const done =
        act === "revert"
          ? result(opId, { phase: "rolled_back", reason: "user_revert", say_zh: "已退回自动", say_en: "Back to Auto", mark: "ok", stay: "brief", needs_ack: false })
          : result(opId, { phase: "confirmed", reason: "user_keep", say_zh: "保留只用 5G SA · 没确认通", say_en: "Kept 5G SA only · unconfirmed", mark: "warn", stay: "sticky" });
      state.op = { ...op, active: null, last: done };
      return ok(null);
    },
  },
  {
    method: "POST",
    path: "/api/ops/write",
    handler: (ctx) => {
      const request = bodyField(ctx.body, "request") as Json | undefined;
      if (!request || request.action !== "network.set_mode") return fail("not a transaction action", 400);
      writes.push({ op_id: bodyField(ctx.body, "op_id"), ...request });
      return ok({ result: "success" });
    },
  },
  {
    method: "GET",
    path: "/api/ops/journal",
    handler: () => {
      if (!state.supported || journal === "down") return fail("数据服务没回答，不知道改没改成；稍后看一下当前设置", 503);
      return journal === "empty" ? ok({ entries: [], owners: {} }) : ok({ entries, owners: OWNERS });
    },
  },
];
