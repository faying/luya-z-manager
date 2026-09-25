// Write-operation lifecycle — spec 3.1 / 5.1 (7A), R1, R2, R6, R7, R10, R11.
//
//                 ┌──────── cancel ◄────────┐
//   idle ─start─► confirming (tier 2/3) ─confirm─┐
//     │ tier 1                                    ▼
//     └──────────────────────────────► submitting(step i of n) ◄───────────────┐
//                                          │ (double submit ignored)            │
//   ┌───────────────┬────────────────┬─────┴──────────┬─────────────────┐       │
//   ▼               ▼                ▼                ▼                 ▼       │
// failed         partial          unknown          relogin          all steps ok│
// (device 4xx/  (≥1 step done,   (timeout/network, (401, token      │           │
//  5xx on the    then error/      nothing done;     still current)  │           │
//  first step;   timeout)         no retry prompt)     │ login event │           │
//  inputs kept)     │                │ verify given:   ├─ tier 1 ────┼───────────┤ auto-resend
//     │             │                │ probe+verify    └─ tier 2/3 ─► resubmitReady
//     └─resubmit────┴── resubmit ────┼─ bounded ──┐        │                     │
//       (only steps not done — R10)  │            │        └─ resubmit ──────────┘
//                                    ▼            ▼
//                                 (stays      verifying ──true──► applied  「已生效」
//                                  unknown)       │     ──false─► failed (mismatch)
//                                                 │     ──401──► relogin(verify) ─login─► verifying
//                                                 │     ──net──► unknown
//   all steps ok ─┬─ waitDevice option ──► waitDevice ──probe /api/public/status every 3 s──┐
//                 │                           │  (runs while the tab is hidden)            │
//                 │                           ├─ back + verify ─► applied / failed / relogin(verify)
//                 │                           ├─ back, no verify ─► accepted (unknown if the
//                 │                           │                     last request itself dropped)
//                 │                           └─ timeoutSec passed ─► waitTimeout (+ recovery hint)
//                 ├─ verify given ────────► verifying
//                 └─ no verify ───────────► accepted 「设备已接受」 (R6)
//
// A stale 401 (token changed while the request was in flight, R11) means a
// login already happened: tier 1 re-sends at once, tier 2/3 → resubmitReady.
// A timeout/network error on the LAST step of a waitDevice operation goes to
// waitDevice (reboot/APN may drop the connection before answering).

import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { apiFetch, LOGIN_EVENT } from "./client";
import { freshNow, subscribeFresh } from "./freshness";
import { ApiError, TimeoutError, UnauthorizedError } from "./types";

export type Tier = 1 | 2 | 3;

export type Phase =
  | "idle"
  | "confirming"
  | "submitting"
  | "verifying"
  | "waitDevice"
  | "applied"
  | "accepted"
  | "failed"
  | "partial"
  | "unknown"
  | "relogin"
  | "resubmitReady"
  | "waitTimeout";

export type StepStatus = "pending" | "running" | "done" | "failed" | "unconfirmed";

export type ErrorKind =
  | "device"
  | "timeout"
  | "network"
  | "unauthorized"
  | "staleUnauthorized"
  | "mismatch"
  | "other";

export interface StepState {
  label: string;
  status: StepStatus;
  error?: string;
}

export interface WaitInfo {
  /** Browser ms when waiting started. */
  startedAt: number;
  expectedSec: number;
  timeoutSec: number;
  /** A probe has failed at least once (the device went away). */
  sawDown: boolean;
  /** The last request dropped without an answer; without a readback the
   *  result stays unknown even after the device is back. */
  submitUnconfirmed: boolean;
}

export interface WriteOpState {
  phase: Phase;
  tier: Tier;
  steps: StepState[];
  /** Index of the running step while submitting. */
  current: number | null;
  error?: string;
  errorKind?: ErrorKind;
  reloginFor?: "submit" | "verify";
  wait?: WaitInfo;
  recovery?: string;
  /** Bumped whenever a new run starts or the op is reset; async work from an
   *  older run is discarded. */
  runId: number;
}

export const INITIAL_WRITE_OP: WriteOpState = { phase: "idle", tier: 1, steps: [], current: null, runId: 0 };

export type WriteOpEvent =
  | { type: "start"; tier: Tier; labels: string[] }
  | { type: "confirm" }
  | { type: "cancel" }
  | { type: "stepStart"; index: number }
  | { type: "stepOk"; index: number }
  | { type: "stepError"; index: number; kind: ErrorKind; message: string; now: number; wait?: { expectedSec: number; timeoutSec: number } }
  | { type: "stepsDone"; now: number; hasVerify: boolean; wait?: { expectedSec: number; timeoutSec: number } }
  | { type: "probe"; ok: boolean }
  | { type: "back"; hasVerify: boolean }
  | { type: "verifyResult"; ok: boolean }
  | { type: "verifyError"; kind: ErrorKind; message: string; during: "verify" | "wait" | "reconfirm" }
  | { type: "waitTimeout"; recovery?: string }
  | { type: "login" }
  | { type: "resubmit" }
  | { type: "reset" };

/** Phases in which a submission is in flight (start/resubmit are ignored). */
export const BUSY_PHASES: ReadonlySet<Phase> = new Set<Phase>(["submitting", "verifying", "waitDevice"]);

const RESUBMITTABLE: ReadonlySet<Phase> = new Set<Phase>(["partial", "resubmitReady", "failed"]);

function setStep(steps: StepState[], i: number, patch: Partial<StepState>): StepState[] {
  return steps.map((s, j) => (j === i ? { ...s, ...patch } : s));
}

function startRun(s: WriteOpState): WriteOpState {
  return {
    ...s,
    phase: "submitting",
    steps: s.steps.map((st) => (st.status === "done" ? st : { label: st.label, status: "pending" })),
    current: null,
    error: undefined,
    errorKind: undefined,
    reloginFor: undefined,
    wait: undefined,
    recovery: undefined,
    runId: s.runId + 1,
  };
}

export function writeOpReducer(s: WriteOpState, e: WriteOpEvent): WriteOpState {
  switch (e.type) {
    case "start": {
      if (BUSY_PHASES.has(s.phase)) return s;
      const fresh: WriteOpState = {
        ...INITIAL_WRITE_OP,
        tier: e.tier,
        steps: e.labels.map((label) => ({ label, status: "pending" })),
        runId: s.runId + 1,
      };
      return e.tier === 1 ? { ...fresh, phase: "submitting" } : { ...fresh, phase: "confirming" };
    }
    case "confirm":
      return s.phase === "confirming" ? { ...s, phase: "submitting" } : s;
    case "cancel":
      return s.phase === "confirming" ? { ...s, phase: "idle" } : s;
    case "stepStart":
      if (s.phase !== "submitting") return s;
      return { ...s, current: e.index, steps: setStep(s.steps, e.index, { status: "running", error: undefined }) };
    case "stepOk":
      if (s.phase !== "submitting") return s;
      return { ...s, steps: setStep(s.steps, e.index, { status: "done" }) };
    case "stepError": {
      if (s.phase !== "submitting") return s;
      const anyDone = s.steps.some((st, j) => j !== e.index && st.status === "done");
      const base = { ...s, current: null, error: e.message, errorKind: e.kind };
      switch (e.kind) {
        case "unauthorized":
          // Rejected before it ran: the step is simply not done yet.
          return { ...base, phase: "relogin", reloginFor: "submit", steps: setStep(s.steps, e.index, { status: "pending" }) };
        case "staleUnauthorized": {
          const steps = setStep(s.steps, e.index, { status: "pending" });
          if (s.tier === 1) return startRun({ ...s, steps });
          return { ...base, phase: "resubmitReady", steps };
        }
        case "timeout":
        case "network": {
          const steps = setStep(s.steps, e.index, { status: "unconfirmed", error: e.message });
          if (e.wait && e.index === s.steps.length - 1) {
            return {
              ...base,
              phase: "waitDevice",
              steps,
              wait: { startedAt: e.now, ...e.wait, sawDown: true, submitUnconfirmed: true },
            };
          }
          return { ...base, phase: anyDone ? "partial" : "unknown", steps };
        }
        default: {
          const steps = setStep(s.steps, e.index, { status: "failed", error: e.message });
          return { ...base, phase: anyDone ? "partial" : "failed", steps };
        }
      }
    }
    case "stepsDone":
      if (s.phase !== "submitting") return s;
      if (e.wait) {
        return {
          ...s,
          current: null,
          phase: "waitDevice",
          wait: { startedAt: e.now, ...e.wait, sawDown: false, submitUnconfirmed: false },
        };
      }
      return { ...s, current: null, phase: e.hasVerify ? "verifying" : "accepted" };
    case "probe":
      if (s.phase !== "waitDevice" || !s.wait || e.ok || s.wait.sawDown) return s;
      return { ...s, wait: { ...s.wait, sawDown: true } };
    case "back":
      if (s.phase !== "waitDevice") return s;
      if (e.hasVerify) return { ...s, phase: "verifying" };
      if (!s.wait?.submitUnconfirmed) return { ...s, phase: "accepted" };
      // The last request dropped: if earlier steps went through, this is a
      // partial result (R10), and resubmit sends only the unconfirmed step.
      return { ...s, phase: s.steps.some((st) => st.status === "done") ? "partial" : "unknown" };
    case "verifyResult": {
      if (s.phase !== "verifying" && s.phase !== "waitDevice" && s.phase !== "unknown") return s;
      if (e.ok) {
        return {
          ...s,
          phase: "applied",
          error: undefined,
          errorKind: undefined,
          steps: s.steps.map((st) => (st.status === "unconfirmed" ? { label: st.label, status: "done" } : st)),
        };
      }
      // The device answered but doesn't show the change: mark the steps
      // unconfirmed so a resubmit sends them again (not just the readback).
      return {
        ...s,
        phase: "failed",
        errorKind: "mismatch",
        error: "readback does not show the change",
        steps: s.steps.map((st) => (st.status === "done" ? { label: st.label, status: "unconfirmed" } : st)),
      };
    }
    case "verifyError": {
      if (s.phase !== "verifying" && s.phase !== "waitDevice" && s.phase !== "unknown") return s;
      if (e.kind === "unauthorized") {
        return { ...s, phase: "relogin", reloginFor: "verify", error: e.message, errorKind: e.kind };
      }
      // Waiting/reconfirming: keep going, the loop tries again.
      if (e.during !== "verify") return s;
      if (e.kind === "staleUnauthorized") return s; // controller retries once with the new token
      return { ...s, phase: "unknown", error: e.message, errorKind: e.kind };
    }
    case "waitTimeout":
      if (s.phase !== "waitDevice") return s;
      return { ...s, phase: "waitTimeout", recovery: e.recovery };
    case "login":
      if (s.phase !== "relogin") return s;
      if (s.reloginFor === "verify") {
        return { ...s, phase: "verifying", reloginFor: undefined, error: undefined, errorKind: undefined, runId: s.runId + 1 };
      }
      // R2: tier 1 is re-sent automatically; tier 2/3 need one explicit click.
      if (s.tier === 1) return startRun(s);
      return { ...s, phase: "resubmitReady", reloginFor: undefined };
    case "resubmit":
      return RESUBMITTABLE.has(s.phase) ? startRun(s) : s;
    case "reset":
      return { ...INITIAL_WRITE_OP, runId: s.runId + 1 };
  }
}

/** Labels of finished and not-yet-confirmed steps, for 「部分生效：…，…」. */
export function summarizeSteps(s: WriteOpState): { done: string[]; notDone: string[] } {
  return {
    done: s.steps.filter((st) => st.status === "done").map((st) => st.label),
    notDone: s.steps.filter((st) => st.status !== "done").map((st) => st.label),
  };
}

export function classifyError(e: unknown): ErrorKind {
  if (e instanceof UnauthorizedError) return e.staleToken ? "staleUnauthorized" : "unauthorized";
  if (e instanceof TimeoutError) return "timeout";
  if (e instanceof ApiError) {
    if (e.status === 0) return "network";
    if (e.status >= 400) return "device";
  }
  return "other";
}

function messageOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

// ── controller: runs the async side of the machine ─────────────────────────

export interface WriteStep {
  label: string;
  run: () => Promise<unknown>;
}

export interface WaitDeviceOptions {
  /** Shown as the countdown (reboot ≈ 90, network changes ≈ 30). */
  expectedSec: number;
  probeEveryMs?: number;
  /** Give up after this long. Default max(2 × expectedSec, expectedSec + 30). */
  timeoutSec?: number;
  /** Require one failed probe before "back" counts (reboot: the agent still
   *  answers for a moment right after the request). */
  expectDown?: boolean;
  /** What to do if it never comes back (new SSID, new LAN address, …). */
  recovery?: string;
}

export interface WriteOpConfig {
  tier: Tier;
  steps: WriteStep[];
  /** Readback: true only if the device now shows the change (R6). */
  verify?: () => Promise<boolean>;
  waitDevice?: WaitDeviceOptions;
  /** After "unknown", keep probing + verifying this long (5.1 「重连后自动读取」). Default 120. */
  reconfirmSec?: number;
}

export interface WriteOpDeps {
  now: () => number;
  sleep: (ms: number) => Promise<void>;
  /** Does the agent answer /api/public/status (no auth)? Never throws. */
  probe: (timeoutMs: number) => Promise<boolean>;
  onLogin: (cb: () => void) => () => void;
}

export const DEFAULT_PROBE_MS = 3000;
export const DEFAULT_RECONFIRM_SEC = 120;

export const browserDeps: WriteOpDeps = {
  now: () => Date.now(),
  sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
  probe: async (timeoutMs) => {
    try {
      await apiFetch("/api/public/status", { noAuth: true, raw: true, timeoutMs });
      return true;
    } catch {
      return false;
    }
  },
  onLogin: (cb) => {
    if (typeof window === "undefined") return () => {};
    window.addEventListener(LOGIN_EVENT, cb);
    return () => window.removeEventListener(LOGIN_EVENT, cb);
  },
};

function waitTimes(w: WaitDeviceOptions) {
  return { expectedSec: w.expectedSec, timeoutSec: w.timeoutSec ?? Math.max(2 * w.expectedSec, w.expectedSec + 30) };
}

export class WriteOpController {
  private state: WriteOpState = INITIAL_WRITE_OP;
  private listeners = new Set<() => void>();
  private cfg: WriteOpConfig | null = null;
  private attached = 0;
  private unLogin: (() => void) | null = null;

  constructor(
    private getConfig: () => WriteOpConfig,
    private deps: WriteOpDeps = browserDeps
  ) {}

  /** Start listening for logins; async work only runs while attached (so an
   *  unmounted page stops probing). Returns the detach function. */
  attach = (): (() => void) => {
    this.attached++;
    if (!this.unLogin) this.unLogin = this.deps.onLogin(() => this.handleLogin());
    let done = false;
    return () => {
      if (done) return;
      done = true;
      this.attached--;
      if (this.attached === 0) {
        this.unLogin?.();
        this.unLogin = null;
        // Abandon in-flight loops of the current run.
        this.state = { ...this.state, runId: this.state.runId + 1 };
      }
    };
  };

  getState = (): WriteOpState => this.state;

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  private dispatch(e: WriteOpEvent) {
    const next = writeOpReducer(this.state, e);
    if (next === this.state) return;
    this.state = next;
    for (const l of Array.from(this.listeners)) l();
  }

  /** First click. Tier 1 submits; tier 2/3 open the confirm step. */
  start = () => {
    if (BUSY_PHASES.has(this.state.phase)) return;
    const cfg = this.getConfig();
    this.cfg = cfg; // closures captured now; resubmit re-runs these
    this.dispatch({ type: "start", tier: cfg.tier, labels: cfg.steps.map((s) => s.label) });
    if (this.state.phase === "submitting") void this.runSteps();
  };

  confirm = () => {
    if (this.state.phase !== "confirming") return;
    this.dispatch({ type: "confirm" });
    void this.runSteps();
  };

  cancel = () => this.dispatch({ type: "cancel" });

  /** partial / resubmitReady / failed: send only the steps not done (R10),
   *  without asking to confirm again (R2). */
  resubmit = () => {
    if (!this.cfg) return;
    const before = this.state.phase;
    this.dispatch({ type: "resubmit" });
    if (this.state.phase === "submitting" && before !== "submitting") void this.runSteps();
  };

  reset = () => this.dispatch({ type: "reset" });

  private handleLogin() {
    if (this.state.phase !== "relogin") return;
    this.dispatch({ type: "login" });
    const next: Phase = this.getState().phase;
    if (next === "submitting") void this.runSteps();
    else if (next === "verifying") void this.runVerify();
  }

  private alive(runId: number) {
    return this.attached > 0 && this.state.runId === runId;
  }

  private waitArg() {
    const w = this.cfg?.waitDevice;
    return w ? waitTimes(w) : undefined;
  }

  private async runSteps(): Promise<void> {
    const cfg = this.cfg;
    if (!cfg) return;
    const runId = this.state.runId;
    for (let i = 0; i < cfg.steps.length; i++) {
      if (this.state.steps[i]?.status === "done") continue;
      this.dispatch({ type: "stepStart", index: i });
      try {
        await cfg.steps[i].run();
      } catch (e) {
        if (!this.alive(runId)) return;
        this.dispatch({
          type: "stepError",
          index: i,
          kind: classifyError(e),
          message: messageOf(e),
          now: this.deps.now(),
          wait: this.waitArg(),
        });
        return this.afterSettle(runId);
      }
      if (!this.alive(runId)) return;
      this.dispatch({ type: "stepOk", index: i });
    }
    this.dispatch({ type: "stepsDone", now: this.deps.now(), hasVerify: !!cfg.verify, wait: this.waitArg() });
    return this.afterSettle(runId);
  }

  /** Continue with whatever the reducer moved us to. */
  private afterSettle(prevRunId: number): Promise<void> | void {
    const p = this.state.phase;
    if (p === "submitting" && this.state.runId !== prevRunId) return this.runSteps(); // stale 401, tier 1
    if (p === "verifying") return this.runVerify();
    if (p === "waitDevice") return this.probeLoop("wait");
    if (p === "unknown") return this.probeLoop("reconfirm");
  }

  private async runVerify(): Promise<void> {
    const verify = this.cfg?.verify;
    if (!verify) return;
    const runId = this.state.runId;
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        const ok = await verify();
        if (!this.alive(runId)) return;
        this.dispatch({ type: "verifyResult", ok });
        return;
      } catch (e) {
        if (!this.alive(runId)) return;
        const kind = classifyError(e);
        if (kind === "staleUnauthorized" && attempt === 0) continue; // newer token: try once more
        this.dispatch({ type: "verifyError", kind, message: messageOf(e), during: "verify" });
        if (this.state.phase === "unknown") return this.probeLoop("reconfirm");
        return;
      }
    }
  }

  /** wait: after a device-dropping write (R1) — ends in a result or waitTimeout.
   *  reconfirm: after "unknown" — ends in a result or silently stays unknown. */
  private async probeLoop(mode: "wait" | "reconfirm"): Promise<void> {
    const cfg = this.cfg;
    if (!cfg) return;
    if (mode === "reconfirm" && (!cfg.verify || this.state.steps.some((s) => s.status === "pending" || s.status === "failed"))) return;
    const runId = this.state.runId;
    const w = cfg.waitDevice;
    const every = w?.probeEveryMs ?? DEFAULT_PROBE_MS;
    const started = mode === "wait" && this.state.wait ? this.state.wait.startedAt : this.deps.now();
    const limitMs = (mode === "wait" && this.state.wait ? this.state.wait.timeoutSec : cfg.reconfirmSec ?? DEFAULT_RECONFIRM_SEC) * 1000;
    const inPhase = mode === "wait" ? "waitDevice" : "unknown";

    for (;;) {
      await this.deps.sleep(every);
      if (!this.alive(runId) || this.state.phase !== inPhase) return;
      if (this.deps.now() - started > limitMs) {
        if (mode === "wait") this.dispatch({ type: "waitTimeout", recovery: w?.recovery });
        return;
      }
      const up = await this.deps.probe(Math.min(every, 8000));
      if (!this.alive(runId) || this.state.phase !== inPhase) return;
      if (!up) {
        this.dispatch({ type: "probe", ok: false });
        continue;
      }
      if (mode === "wait" && w?.expectDown && !this.state.wait?.sawDown) continue;
      if (!cfg.verify) {
        this.dispatch({ type: "back", hasVerify: false });
        return;
      }
      try {
        const ok = await cfg.verify();
        if (!this.alive(runId)) return;
        this.dispatch({ type: "verifyResult", ok });
        return;
      } catch (e) {
        if (!this.alive(runId)) return;
        this.dispatch({ type: "verifyError", kind: classifyError(e), message: messageOf(e), during: mode });
        if (this.state.phase !== inPhase) return; // relogin(verify)
        // network / stale token / device error: try again next round
      }
    }
  }
}

// ── React hook ───────────────────────────────────────────────────────────────

export interface UseWriteOp extends WriteOpState {
  start: () => void;
  confirm: () => void;
  cancel: () => void;
  resubmit: () => void;
  reset: () => void;
  busy: boolean;
  /** waitDevice countdown against expectedSec (0 once exceeded). */
  remainingSec: number | null;
  done: string[];
  notDone: string[];
}

const noopSubscribe = () => () => {};

/** Tracks one write operation (the confirm UI itself is ConfirmInline /
 *  ConfirmDialog). Pass the latest config every render; start() snapshots it. */
export function useWriteOp(config: WriteOpConfig): UseWriteOp {
  // Latest config for start(), which only runs from event handlers, i.e.
  // after the layout effect of the render that produced the press.
  const cfgRef = useRef(config);
  useLayoutEffect(() => {
    cfgRef.current = config;
  });
  // The getter is only called from start(), never during render.
  // eslint-disable-next-line react-hooks/refs
  const [ctl] = useState(() => new WriteOpController(() => cfgRef.current));
  useEffect(() => ctl.attach(), [ctl]);

  const s = useSyncExternalStore(ctl.subscribe, ctl.getState, ctl.getState);
  const waiting = s.phase === "waitDevice" && s.wait ? s.wait : null;
  const remainingSec = useSyncExternalStore(
    waiting ? subscribeFresh : noopSubscribe,
    () => (waiting ? Math.max(0, Math.ceil((waiting.startedAt + waiting.expectedSec * 1000 - freshNow()) / 1000)) : null),
    () => null
  );
  const { done, notDone } = summarizeSteps(s);
  return {
    ...s,
    start: ctl.start,
    confirm: ctl.confirm,
    cancel: ctl.cancel,
    resubmit: ctl.resubmit,
    reset: ctl.reset,
    busy: BUSY_PHASES.has(s.phase),
    remainingSec,
    done,
    notDone,
  };
}
