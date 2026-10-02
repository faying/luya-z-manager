// Text the agent words in two languages (docs/designs/ui-english.md §2, T7):
// the English field is used in English, and its absence (older agent, row not
// yet worded) falls back to the Chinese, never to a blank.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { pick, pickLang } from "@/lib/i18n/pick";
import { apiFetch } from "@/lib/api/client";
import { ApiError, errorText } from "@/lib/api/types";
import { INITIAL_WRITE_OP, writeOpReducer, type WriteOpEvent } from "@/lib/api/writeOp";
import { alertLabel } from "@/lib/alerts";
import { healthRows } from "@/lib/health";
import { guardReason } from "@/lib/guardReason";
import { jsonResponse, stubWindow } from "./windowStub";

describe("pick", () => {
  it("English when there is one, Chinese otherwise", () => {
    expect(pick("中国联通", "China Unicom", "en")).toBe("China Unicom");
    expect(pick("中国联通", "China Unicom", "zh")).toBe("中国联通");
    expect(pick("中国联通", null, "en")).toBe("中国联通");
    expect(pick("中国联通", undefined, "en")).toBe("中国联通");
    expect(pick("中国联通", "", "en")).toBe("中国联通");
    expect(pick(null, null, "en")).toBe("");
  });

  it("pickLang reads <field>_en", () => {
    const row = { label: "ZTE 自动升级", label_en: "ZTE auto-update" as string | null };
    expect(pickLang(row, "label", "en")).toBe("ZTE auto-update");
    expect(pickLang({ ...row, label_en: null }, "label", "en")).toBe("ZTE 自动升级");
    expect(pickLang<{ label: string }>(null, "label", "en")).toBe("");
  });
});

describe("errors carry error_en (O2)", () => {
  beforeEach(() => {
    stubWindow();
  });
  afterEach(() => vi.unstubAllGlobals());

  // The agent's own reply while an operator search runs (netinfo.rs claim).
  const BUSY = {
    ok: false,
    error: "正在搜索网络，搜完再操作",
    error_en: "Searching for networks; try again when it finishes",
  };

  it("a failed reply becomes an ApiError with both texts; message stays the agent's error", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse(409, BUSY)));
    const err = await apiFetch("/api/netinfo/scan", { method: "POST" }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    const e = err as ApiError;
    expect([e.status, e.message, e.messageEn]).toEqual([409, BUSY.error, BUSY.error_en]);
    expect(errorText(e, "en")).toBe(BUSY.error_en);
    expect(errorText(e, "zh")).toBe(BUSY.error);
  });

  it("an older agent without error_en shows the Chinese in English mode", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse(409, { ok: false, error: BUSY.error })));
    const e = await apiFetch("/api/netinfo/scan", { method: "POST" }).catch((x: unknown) => x);
    expect((e as ApiError).messageEn).toBeUndefined();
    expect(errorText(e, "en")).toBe(BUSY.error);
    expect(errorText(new Error("plain"), "en")).toBe("plain");
  });

  it("a failed write keeps both texts for OpResult", () => {
    const events: WriteOpEvent[] = [
      { type: "start", tier: 1, labels: ["Start scan"] },
      { type: "stepStart", index: 0 },
      { type: "stepError", index: 0, kind: "device", message: BUSY.error, messageEn: BUSY.error_en, now: 0 },
    ];
    const s = events.reduce(writeOpReducer, INITIAL_WRITE_OP);
    expect([s.phase, s.error, s.errorEn]).toEqual(["failed", BUSY.error, BUSY.error_en]);
    const again = writeOpReducer(s, { type: "resubmit" });
    expect(again.errorEn).toBeUndefined();
  });
});

describe("alert names come from the agent (R8)", () => {
  const ev = { kind: "agent-crash", label: "管理后台意外退出，已自动重启", label_en: "Admin backend (zte-agent) exited unexpectedly" };

  it("label_en in English, label in Chinese", () => {
    expect(alertLabel(ev, "en")).toBe(ev.label_en);
    expect(alertLabel(ev, "zh")).toBe(ev.label);
  });

  it("an SMS record from an older agent borrows the label of an event of the same kind, else shows the kind", () => {
    expect(alertLabel({ kind: "agent-crash" }, "en", [ev])).toBe(ev.label_en);
    expect(alertLabel({ kind: "wifi-takeover" }, "en", [ev])).toBe("wifi-takeover");
  });

  it("an event with only the Chinese label shows it in English mode", () => {
    expect(alertLabel({ kind: "x", label: "其他告警（x）" }, "en")).toBe("其他告警（x）");
  });
});

describe("health rows pick doctor's English", () => {
  it("--tsv2 rows in English; --tsv rows (no English) stay Chinese", () => {
    const rows = healthRows(
      [
        { level: "ok", id: "fota", label: "ZTE 自动升级", detail: "已关闭", label_en: "ZTE auto-update", detail_en: "Off" },
        { level: "ok", id: "standby", label: "待机", detail: "正常", label_en: null, detail_en: null },
      ],
      "en"
    );
    expect(rows.map((r) => [r.label, r.detail])).toEqual([
      ["ZTE auto-update", "Off"],
      ["待机", "正常"],
    ]);
  });
});

describe("guard reason: compare the code, not the text", () => {
  const auto = { reason: "手动恢复自动", reason_code: "manual_auto", reason_en: "Back to automatic on request" };
  const failed = { reason: "注册失败", reason_code: "register_failed", reason_en: "Registration failed" };

  it("back to automatic on request says nothing of its own, in either language", () => {
    expect(guardReason(auto, "en")).toBeNull();
    expect(guardReason(auto, "zh")).toBeNull();
    expect(guardReason({ ...auto, reason_code: null, reason_en: null }, "en")).toBeNull(); // older agent
  });

  it("any other reason is shown in the page language, Chinese when there is no English", () => {
    expect(guardReason(failed, "en")).toBe("Registration failed");
    expect(guardReason(failed, "zh")).toBe("注册失败");
    expect(guardReason({ reason: "注册失败" }, "en")).toBe("注册失败");
    expect(guardReason({ reason: "" }, "en")).toBeNull();
  });
});

