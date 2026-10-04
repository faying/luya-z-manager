import { beforeEach, describe, expect, it } from "vitest";
import {
  countdownFrozen,
  fillNext,
  fmtClock,
  markTone,
  noteAcked,
  noteMine,
  noteNoticeAcked,
  observeOps,
  opNotice,
  opShow,
  opText,
  remainingNow,
  resetOpsSeen,
  shortWhen,
  type OpBlock,
  type OpView,
  type OpsState,
} from "@/lib/ops";

// shapes as datad sends them (touch-ui tests/op_view_test.c, STATE_V2.md §12)
const view = (o: Partial<OpView>): OpView => ({
  op_id: "web-1",
  action: "network.set_mode",
  item: "network.mode",
  source: "web",
  phase: "verifying",
  reason: null,
  target: "Only_LTE",
  old: "WL_AND_5G",
  rollback_to: "WL_AND_5G",
  remaining_ms: 102000,
  say_zh: "正在确认",
  say_en: "Checking",
  next_zh: "{t} 后没通就退回到自动",
  next_en: "Back to Auto in {t} if no data",
  note_zh: null,
  note_en: null,
  what_zh: "制式",
  what_en: "Network mode",
  source_zh: "网页",
  source_en: "Web",
  old_zh: "自动",
  old_en: "Auto",
  target_zh: "只用 4G",
  target_en: "4G only",
  mark: null,
  stay: "live",
  steps: [],
  can_revert: true,
  can_keep: true,
  undo: null,
  ...o,
});
const block = (active: OpView | null, last: OpView | null): OpBlock => ({ rollback_enabled: true, active, last });
const brief = (id: string) => view({ op_id: id, phase: "confirmed", reason: "verified", stay: "brief", mark: "ok", remaining_ms: null, needs_ack: false, acked: false });
const sticky = (id: string, o: Partial<OpView> = {}) =>
  view({ op_id: id, phase: "rolled_back", reason: "timeout", stay: "sticky", mark: "warn", remaining_ms: null, needs_ack: true, acked: false, ...o });

beforeEach(() => resetOpsSeen());

describe("countdown", () => {
  it("formats m:ss and fills {t}", () => {
    expect(fmtClock(102000)).toBe("1:42");
    expect(fmtClock(500)).toBe("0:01");
    expect(fmtClock(-5)).toBe("0:00");
    expect(fillNext("{t} 后没通就退回到自动", 61000)).toBe("1:01 后没通就退回到自动");
    expect(fillNext(null, 1)).toBeNull();
  });
  it("counts down between reads, stops when frozen", () => {
    const v = view({});
    expect(remainingNow(v, 1000, false, 4000)).toBe(99000);
    expect(remainingNow(v, 1000, true, 4000)).toBe(102000);
  });
  it("freezes when datad is stuck, the block is stale or reads stop for 20 s", () => {
    const s = (o: Partial<OpsState>): OpsState => ({ op: block(view({}), null), datad: "up", stalled: false, supported: true, ...o });
    expect(countdownFrozen(s({}), 1000, 5000)).toBe(false);
    expect(countdownFrozen(s({ datad: "stuck" }), 1000, 5000)).toBe(true);
    expect(countdownFrozen(s({ op: block(view({ frozen: true }), null) }), 1000, 5000)).toBe(true);
    expect(countdownFrozen(s({}), 1000, 22000)).toBe(true);
    expect(countdownFrozen(undefined, 1000, 2000)).toBe(true);
  });
});

describe("what to show (V2-35, DD16)", () => {
  it("shows the running one", () => {
    observeOps(block(view({}), null), 0);
    expect(opShow(block(view({}), null), 0)?.kind).toBe("live");
  });
  it("never replays a 3-second result on the first read", () => {
    const op = block(null, brief("web-0"));
    observeOps(op, 0);
    expect(opShow(op, 10)).toBeNull();
  });
  it("shows a 3-second result it saw end, for 3 s", () => {
    observeOps(block(null, null), 0);
    observeOps(block(view({ op_id: "web-1" }), null), 1000);
    const done = block(null, brief("web-1"));
    observeOps(done, 5000);
    expect(opShow(done, 6000)?.kind).toBe("brief");
    expect(opShow(done, 8100)).toBeNull();
  });
  it("counts this page's own write as seen even if it ended between reads", () => {
    observeOps(block(null, brief("web-0")), 0);
    noteMine("web-2");
    const done = block(null, brief("web-2"));
    observeOps(done, 2000);
    expect(opShow(done, 2500)?.kind).toBe("brief");
  });
  it("keeps sticky results until Got it, here or elsewhere", () => {
    const op = block(null, sticky("web-3"));
    observeOps(op, 0);
    expect(opShow(op, 0)?.kind).toBe("sticky");
    expect(opShow(block(null, sticky("web-3", { needs_ack: false, acked: true })), 0)).toBeNull();
    noteAcked("web-3");
    expect(opShow(op, 0)).toBeNull();
  });
  it("a failed revert is an alert", () => {
    const op = block(null, sticky("web-4", { phase: "rollback_failed", reason: "rollback_timeout", stay: "alert", mark: "bad" }));
    expect(opShow(op, 0)?.kind).toBe("alert");
    expect(markTone("bad", "alert")).toBe("bad");
  });
  it("superseded results are not shown on their own", () => {
    expect(opShow(block(null, sticky("web-5", { stay: "none", needs_ack: false })), 0)).toBeNull();
  });
});

describe("text", () => {
  it("picks the language, Chinese when English is missing", () => {
    const v = view({});
    expect(opText(v, "say", "zh")).toBe("正在确认");
    expect(opText(v, "say", "en")).toBe("Checking");
    expect(opText(view({ note_zh: "重启过 · 重新确认", note_en: null }), "note", "en")).toBe("重启过 · 重新确认");
  });
  it("shortens change log times (device wall time, no conversion)", () => {
    expect(shortWhen("2026-10-03 14:32:07")).toBe("10-03 14:32");
    expect(shortWhen(undefined)).toBe("");
  });
});

describe("the auto-revert notice (DD18)", () => {
  it("shows only rollback_on, until Got it went through here", () => {
    expect(opNotice(null)).toBeNull();
    expect(opNotice(block(null, null))).toBeNull();
    expect(opNotice({ ...block(null, null), notice: null })).toBeNull();
    expect(opNotice({ ...block(null, null), notice: "something_else" })).toBeNull();
    const b = { ...block(null, null), notice: "rollback_on" };
    expect(opNotice(b)).toBe("rollback_on");
    noteNoticeAcked();
    expect(opNotice(b)).toBeNull();
    resetOpsSeen();
    expect(opNotice(b)).toBe("rollback_on");
  });
});
