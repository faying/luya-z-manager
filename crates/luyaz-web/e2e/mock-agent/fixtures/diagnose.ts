// Deep diagnosis — deep_diag.rs (`POST/GET /api/diagnose`, `/speed`,
// `/feedback`; slow-diagnosis.md §4.2). A run here is a timeline: an
// optional wait (scenario diag-waiting), then one layer every STEP_MS, then
// done. The judgement follows the persona: normal → nothing found; weak →
// weak signal (bad) + a slow cellular link (warn), i.e. "+1 more". The proxy
// layer is there while the proxy service runs, like the agent's `proxy_on()`.
//
// While a run is going, the speed test, the network search and manual
// register answer the agent's 409 (`refusal`): 正在诊断，约 N 秒后再试.

import type { Ctx, Reply, Route } from "../lib.ts";
import { bodyField, fail, ok } from "../lib.ts";
import { DEVICE_UTC_OFFSET } from "./network.ts";
import type { DiagLayer, DiagLevel, DiagMain, DiagRun } from "../../../src/lib/api/schemas/tools.ts";

const STEP_MS = 800;
const SLOW_STEP_MS = 4000;
const WAIT_MS = 3000;
const SPEED_MS = 3000;
const KEEP_S = 600;

interface MockRun {
  id: number;
  askedAt: number;
  waitMs: number;
  stepMs: number;
  weak: boolean;
  proxy: boolean;
  feedback: boolean | null;
  speedAt: number | null;
}

let current: MockRun | null = null;

const devSecs = (ms: number) => Math.floor(ms / 1000) + DEVICE_UTC_OFFSET;

type Final = Omit<DiagLayer, "counted"> & { counted?: boolean };

function finals(r: MockRun): Final[] {
  const out: Final[] = [
    { id: "wifi", level: "ok", detail: "−52 dBm · 866 Mbps · 重传 2%", detail_en: "−52 dBm · 866 Mbps · 2% retries" },
    r.weak
      ? { id: "signal", level: "bad", detail: "信号弱 · RSRP −115 dBm · SINR −2 dB", detail_en: "Weak signal · RSRP −115 dBm · SINR −2 dB" }
      : { id: "signal", level: "ok", detail: "RSRP −95 dBm · SINR 18 dB", detail_en: "RSRP −95 dBm · SINR 18 dB" },
    { id: "limit", level: "na", detail: "QoS 读不到", detail_en: "QoS unavailable" },
    r.weak
      ? { id: "link", level: "warn", detail: "延迟 180 ms · 丢包 4%", detail_en: "180 ms · 4% loss" }
      : { id: "link", level: "ok", detail: "延迟 28 ms · 丢包 0%", detail_en: "28 ms · 0% loss" },
    { id: "crowd", level: "na", detail: "历史不够", detail_en: "Not enough history" },
  ];
  if (r.proxy) out.push({ id: "proxy", level: "ok", detail: "TW 01 · 120 ms（直连 95 ms）", detail_en: "TW 01 · 120 ms (direct 95 ms)" });
  return out;
}

function doneAt(r: MockRun): number {
  return r.askedAt + r.waitMs + finals(r).length * r.stepMs;
}

function live(now: number): boolean {
  return current !== null && now < doneAt(current);
}

/** deep_diag.rs `pick_main`, for the two personas the mock has. */
function pickMain(layers: DiagLayer[]): DiagMain {
  const m = layers.find((l) => l.level === "bad") ?? layers.find((l) => l.level === "warn");
  if (!m) {
    return {
      layer: "",
      level: null,
      text: "没查到问题",
      text_en: "No problem found",
      action: "可能是对方网站慢；也可以加测速度",
      action_en: "The site itself may be slow; you can also add a speed test",
      action_to: "",
      more: 0,
    };
  }
  const more = layers.filter((l) => l.id !== m.id && (l.level === "warn" || l.level === "bad")).length;
  const byLayer: Record<string, [string, string, string, string, DiagMain["action_to"]]> = {
    signal: ["信号弱", "Weak signal", "固定位置时用摆放模式；在路上只能等", "Use Placement if you're staying put; on the move, wait", "placement"],
    link: ["蜂窝链路不稳", "Cellular link unstable", "过几分钟再试，或换个地方", "Try again in a few minutes, or move", ""],
    proxy: ["代理节点慢或不通", "Proxy node slow or down", "换节点", "Switch node", "proxy"],
  };
  const [text, text_en, action, action_en, action_to] = byLayer[m.id] ?? ["", "", "", "", ""];
  return { layer: m.id, level: m.level, text, text_en, action, action_en, action_to, more };
}

function view(r: MockRun, now: number): DiagRun {
  const fs = finals(r);
  const t = now - r.askedAt - r.waitMs;
  const waiting = t < 0;
  const done = now >= doneAt(r);
  const layers: DiagLayer[] = fs.map((f, i) => {
    const counted = f.counted ?? true;
    if (done || (!waiting && t >= (i + 1) * r.stepMs)) return { ...f, counted };
    const level: DiagLevel = !waiting && t >= i * r.stepMs ? "running" : "pending";
    return { id: f.id, level, detail: "", detail_en: "", counted };
  });
  const counted = layers.filter((l) => l.counted);
  let speed: DiagLayer | null = null;
  if (r.speedAt !== null) {
    speed =
      now - r.speedAt < SPEED_MS
        ? { id: "speed", level: "running", detail: "", detail_en: "", counted: true }
        : { id: "speed", level: "info", detail: r.weak ? "直连 ↓ 4 Mbps" : "直连 ↓ 86 Mbps", detail_en: r.weak ? "Direct ↓ 4 Mbps" : "Direct ↓ 86 Mbps", counted: true };
  }
  const finished = done ? devSecs(doneAt(r)) : null;
  return {
    id: r.id,
    state: waiting ? "waiting" : done ? "done" : "running",
    ...(waiting ? { waiting_for: "speedtest" as const } : {}),
    asked_at: devSecs(r.askedAt),
    started_at: waiting ? null : devSecs(r.askedAt + r.waitMs),
    finished_at: finished,
    step: counted.filter((l) => l.level !== "pending" && l.level !== "running").length,
    steps: counted.length,
    layers,
    main: done ? pickMain(layers) : null,
    key: { plmn: "46692", cell: 1234567, ch: 627264, hour: 14 },
    from: "client",
    feedback: r.feedback,
    speed,
    age_s: finished === null ? null : Math.max(0, devSecs(now) - finished),
  };
}

/**
 * The 409 a speed test, network search or manual register gets while a
 * diagnosis runs (deep_diag.rs `refusal`), or null when none runs.
 */
export function diagRefusal(now: number): Reply | null {
  if (!current || !live(now)) return null;
  const n = Math.max(1, Math.ceil((doneAt(current) - now) / 1000));
  return {
    status: 409,
    raw: { ok: false, error: `正在诊断，约 ${n} 秒后再试`, error_en: `Diagnosing; try again in about ${n} s`, busy: "diagnose", retry_after_s: n },
  };
}

function finishedRun(ctx: Ctx): MockRun | Reply {
  const id = bodyField(ctx.body, "id");
  if (typeof id !== "number") return fail("id is required", 400);
  if (!current || current.id !== id || live(ctx.now)) return fail("no such finished run", 404);
  return current;
}

export const routes: Route[] = [
  {
    method: "POST",
    path: "/api/diagnose",
    handler: (ctx) => {
      if (current && live(ctx.now)) return { status: 202, raw: { ok: true, data: view(current, ctx.now), joined: true } };
      current = {
        id: (current?.id ?? 0) + 1,
        askedAt: ctx.now,
        waitMs: ctx.has("diag-waiting") ? WAIT_MS : 0,
        stepMs: ctx.has("diag-slow") ? SLOW_STEP_MS : STEP_MS,
        weak: ctx.has("weak"),
        proxy: false,
        feedback: null,
        speedAt: null,
      };
      return { status: 202, data: view(current, ctx.now) };
    },
  },
  {
    method: "GET",
    path: "/api/diagnose",
    ownMissing: true,
    handler: (ctx) => {
      if (!current) return ok({ state: "idle" });
      const v = view(current, ctx.now);
      if (v.state === "done" && (v.age_s ?? 0) > KEEP_S) return ok({ state: "idle" });
      return ok(v);
    },
  },
  {
    method: "POST",
    path: "/api/diagnose/speed",
    handler: (ctx) => {
      const r = finishedRun(ctx);
      if (!("askedAt" in r)) return r;
      if (r.speedAt === null || ctx.now - r.speedAt >= SPEED_MS) r.speedAt = ctx.now;
      return { status: 202, data: view(r, ctx.now) };
    },
  },
  {
    method: "POST",
    path: "/api/diagnose/feedback",
    handler: (ctx) => {
      const right = bodyField(ctx.body, "right");
      if (typeof right !== "boolean") return fail("id and right are required", 400);
      const r = finishedRun(ctx);
      if (!("askedAt" in r)) return r;
      if (r.feedback !== null) return ok({ already: true });
      r.feedback = right;
      return ok({ recorded: true });
    },
  },
];
