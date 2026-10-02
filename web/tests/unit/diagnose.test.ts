// Diagnose page mapping (slow-diagnosis.md §12.3, §12.5): level → symbol and
// word, the headline and its "+N more", the N/M count, when to poll; and the
// busy 409 other pages get while a diagnosis runs.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DiagLayer, DiagMain, DiagRun } from "@/lib/api/schemas/tools";
import { arrivedInApp, headView, isLive, isRun, layerName, orderedLayers, pollMs, progress, rowView, waitingFor, wantsDiagnose } from "@/lib/diagnose";
import { apiFetch } from "@/lib/api/client";
import { ApiError, errorText } from "@/lib/api/types";
import { classifyError, INITIAL_WRITE_OP, writeOpReducer, type WriteOpEvent } from "@/lib/api/writeOp";
import { jsonResponse, stubWindow } from "./windowStub";

// t() that returns the default with {{x}} filled in, like i18next with no dictionary.
const t = (_k: string, d: string, o?: Record<string, unknown>) => d.replace(/\{\{(\w+)\}\}/g, (_, k: string) => String(o?.[k] ?? ""));

const layer = (id: string, level: DiagLayer["level"], detail = "", detail_en = "", counted = true): DiagLayer => ({ id, level, detail, detail_en, counted });

const run = (over: Partial<DiagRun> = {}): DiagRun => ({
  id: 1,
  state: "done",
  asked_at: 100,
  started_at: 100,
  finished_at: 110,
  step: 5,
  steps: 5,
  layers: [layer("wifi", "ok"), layer("signal", "ok"), layer("limit", "na"), layer("link", "ok"), layer("crowd", "ok")],
  main: null,
  from: "client",
  feedback: null,
  speed: null,
  age_s: 3,
  ...over,
});

describe("rowView: level → symbol + word", () => {
  it("ok ● 正常, warn ▲ 疑点, bad ■ 差 with the agent's numbers in the page language", () => {
    expect(rowView(t, layer("signal", "ok", "RSRP −95 dBm", "RSRP −95 dBm"), "en")).toEqual({ tone: "ok", word: "OK", detail: "RSRP −95 dBm", muted: false });
    expect(rowView(t, layer("link", "warn", "延迟 180 ms", "180 ms"), "en")).toMatchObject({ tone: "warn", word: "Suspect", detail: "180 ms" });
    expect(rowView(t, layer("link", "warn", "延迟 180 ms", "180 ms"), "zh")).toMatchObject({ detail: "延迟 180 ms" });
    expect(rowView(t, layer("signal", "bad", "信号弱", "Weak signal"), "en")).toMatchObject({ tone: "bad", word: "Poor" });
  });

  it("na is grey 「测不了 · 原因」, never 正常", () => {
    const v = rowView(t, layer("crowd", "na", "历史不够", "Not enough history"), "en");
    expect(v).toEqual({ tone: "neutral", word: "Can't check · Not enough history", detail: "", muted: true });
    expect(rowView(t, layer("crowd", "na"), "en").word).toBe("Can't check");
  });

  it("pending 等待 and running 测试中… carry no symbol; info is the plain value", () => {
    expect(rowView(t, layer("link", "pending"), "en")).toMatchObject({ tone: null, word: "Waiting", muted: true });
    expect(rowView(t, layer("link", "running"), "en")).toMatchObject({ tone: null, word: "Testing…", muted: false });
    expect(rowView(t, layer("speed", "info", "直连 ↓ 86 Mbps", "Direct ↓ 86 Mbps"), "en")).toMatchObject({ tone: null, word: "Direct ↓ 86 Mbps" });
  });

  it("an older agent with no English shows the Chinese; counted:false rows are grey", () => {
    expect(rowView(t, layer("wifi", "ok", "−52 dBm", ""), "en").detail).toBe("−52 dBm");
    expect(rowView(t, layer("wifi", "ok", "", "", false), "en")).toMatchObject({ tone: "neutral", muted: true });
  });
});

describe("headView: the status block", () => {
  const main = (over: Partial<DiagMain>): DiagMain => ({
    layer: "signal", level: "bad", text: "信号弱", text_en: "Weak signal",
    action: "固定位置时用摆放模式；在路上只能等", action_en: "Use Placement if you're staying put; on the move, wait",
    action_to: "placement", more: 0, ...over,
  });

  it("tone follows the main cause: bad red, warn orange", () => {
    expect(headView(t, main({}), "en")).toEqual({ tone: "bad", text: "Weak signal", action: "Use Placement if you're staying put; on the move, wait", more: "", to: "placement" });
    expect(headView(t, main({ layer: "crowd", level: "warn", text: "疑似基站拥挤", text_en: "Cell likely busy", action_to: "" }), "zh")).toMatchObject({ tone: "warn", text: "疑似基站拥挤" });
  });

  it("other problems → 「另有 N 处疑点 / +N more」", () => {
    expect(headView(t, main({ more: 2 }), "en").more).toBe("+2 more");
  });

  it("nothing found → green 没查到问题 with the agent's action", () => {
    const h = headView(t, main({ layer: "", level: null, text: "没查到问题", text_en: "No problem found", action: "可能是对方网站慢；也可以加测速度", action_en: "The site itself may be slow; you can also add a speed test", action_to: "" }), "en");
    expect(h).toEqual({ tone: "ok", text: "No problem found", action: "The site itself may be slow; you can also add a speed test", more: "", to: "" });
    expect(headView(t, null, "en")).toMatchObject({ tone: "ok", text: "No problem found" });
  });

  it("a headline without an action (no service) has none", () => {
    expect(headView(t, main({ text: "无服务", text_en: "No service", action: "", action_en: "", action_to: "" }), "en").action).toBe("");
  });
});

describe("progress: 正在检查… N/M", () => {
  it("uses the agent's counts", () => {
    expect(progress(run({ state: "running", step: 3, steps: 6 }))).toEqual({ step: 3, steps: 6 });
  });
  it("without them, counts only counted rows that are finished", () => {
    const layers = [layer("wifi", "na", "", "", false), layer("signal", "ok"), layer("limit", "running"), layer("link", "pending"), layer("crowd", "pending")];
    expect(progress({ layers })).toEqual({ step: 1, steps: 4 });
  });
});

describe("run state and polling", () => {
  it("idle is not a run", () => {
    expect(isRun({ state: "idle" })).toBe(false);
    expect(isRun(undefined)).toBe(false);
    expect(isRun(run())).toBe(true);
  });
  it("polls every second while waiting, running or measuring speed; stops when done", () => {
    expect(pollMs(run({ state: "waiting" }))).toBe(1000);
    expect(pollMs(run({ state: "running" }))).toBe(1000);
    expect(pollMs(run({ speed: layer("speed", "running") }))).toBe(1000);
    expect(pollMs(run())).toBe(0);
    expect(pollMs(run({ speed: layer("speed", "info", "直连 ↓ 86 Mbps") }))).toBe(0);
    expect(pollMs({ state: "idle" })).toBe(0);
    expect(isLive(run({ state: "running" }))).toBe(true);
  });
  it("layers keep the fixed order", () => {
    const ls = [layer("proxy", "ok"), layer("crowd", "ok"), layer("wifi", "ok"), layer("signal", "ok")];
    expect(orderedLayers(ls).map((l) => l.id)).toEqual(["wifi", "signal", "crowd", "proxy"]);
    expect(orderedLayers(null)).toEqual([]);
  });
  it("names and what a waiting run waits for", () => {
    expect(["wifi", "signal", "limit", "link", "crowd", "proxy", "speed"].map((id) => layerName(t, id))).toEqual([
      "Wi-Fi", "Signal", "Speed cap", "Cellular link", "Cell load", "Proxy", "Speed",
    ]);
    expect(waitingFor(t, "scan")).toBe("Searching for networks");
    expect(waitingFor(t, undefined)).toBe("");
  });
});

describe("home 「查原因 →」", () => {
  it("datad's verdict decides: slow kinds, nodata and stall", () => {
    for (const v of ["limit", "weak", "noise", "crowd", "narrow", "nodata", "stall"]) expect(wantsDiagnose(v, "good"), v).toBe(true);
    for (const v of ["ok", "nosim", "airplane", "sos", "nosvc", "only2g", "only3g"]) expect(wantsDiagnose(v, "weak"), v).toBe(false);
  });
  it("no verdict (datad silent, older agent): the web's weak signal / no service", () => {
    expect(wantsDiagnose(null, "weak")).toBe(true);
    expect(wantsDiagnose(undefined, "none")).toBe(true);
    expect(wantsDiagnose(undefined, "good")).toBe(false);
    expect(wantsDiagnose(null, "nosim")).toBe(false);
  });
  it("never while connecting or stale", () => {
    expect(wantsDiagnose("stall", "loading")).toBe(false);
    expect(wantsDiagnose("stall", "stale")).toBe(false);
  });
});

describe("arrivedInApp: auto re-check only after in-app navigation", () => {
  it("hard load of this page (typed, bookmark, reload) is not in-app", () => {
    expect(arrivedInApp("http://h:9090/tools/diagnose/", "http://h:9090/tools/diagnose/")).toBe(false);
    expect(arrivedInApp("http://h:9090/tools/diagnose", "http://h:9090/tools/diagnose/?x=1")).toBe(false);
    expect(arrivedInApp(undefined, "http://h:9090/tools/diagnose/")).toBe(false);
  });
  it("loaded another page, then navigated here", () => {
    expect(arrivedInApp("http://h:9090/", "http://h:9090/tools/diagnose/")).toBe(true);
    expect(arrivedInApp("http://h:9090/charts/", "http://h:9090/tools/diagnose/?start=1")).toBe(true);
  });
});

describe("busy 409 while a diagnosis runs (D10)", () => {
  beforeEach(() => stubWindow());
  afterEach(() => vi.unstubAllGlobals());

  const BUSY = { ok: false, error: "正在诊断，约 8 秒后再试", error_en: "Diagnosing; try again in about 8 s", busy: "diagnose", retry_after_s: 8 };

  it("becomes an ApiError that knows it is busy", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse(409, BUSY)));
    const e = (await apiFetch("/api/speedtest/start", { method: "POST", body: {} }).catch((x: unknown) => x)) as ApiError;
    expect([e.status, e.busy, e.retryAfterS]).toEqual([409, "diagnose", 8]);
    expect(errorText(e, "en")).toBe(BUSY.error_en);
    expect(errorText(e, "zh")).toBe(BUSY.error);
    expect(classifyError(e)).toBe("busy");
  });

  it("a plain 409 (search running) stays a device error", () => {
    expect(classifyError(new ApiError("正在搜索网络，搜完再操作", 409))).toBe("device");
  });

  it("a busy step fails the op with the agent's sentence (OpResult shows it as is)", () => {
    const events: WriteOpEvent[] = [
      { type: "start", tier: 2, labels: ["Start"] },
      { type: "confirm" },
      { type: "stepStart", index: 0 },
      { type: "stepError", index: 0, kind: "busy", message: BUSY.error, messageEn: BUSY.error_en, now: 0 },
    ];
    const s = events.reduce(writeOpReducer, INITIAL_WRITE_OP);
    expect([s.phase, s.errorKind, s.error, s.errorEn]).toEqual(["failed", "busy", BUSY.error, BUSY.error_en]);
  });
});

