import { describe, expect, it } from "vitest";
import {
  INITIAL_WRITE_OP,
  WriteOpConfig,
  WriteOpController,
  WriteOpDeps,
  WriteOpEvent,
  WriteOpState,
  classifyError,
  summarizeSteps,
  writeOpReducer,
} from "@/lib/api/writeOp";
import { ApiError, TimeoutError, UnauthorizedError } from "@/lib/api/types";

// ── reducer ─────────────────────────────────────────────────────────────────

function run(events: WriteOpEvent[], s: WriteOpState = INITIAL_WRITE_OP): WriteOpState {
  return events.reduce(writeOpReducer, s);
}
const WAIT = { expectedSec: 90, timeoutSec: 180 };

describe("writeOpReducer", () => {
  it("tier 1 submits at once; tier 2/3 confirm first; cancel returns to idle", () => {
    expect(run([{ type: "start", tier: 1, labels: ["a"] }]).phase).toBe("submitting");
    const c = run([{ type: "start", tier: 3, labels: ["a"] }]);
    expect(c.phase).toBe("confirming");
    expect(writeOpReducer(c, { type: "cancel" }).phase).toBe("idle");
    expect(writeOpReducer(c, { type: "confirm" }).phase).toBe("submitting");
    expect(run([{ type: "start", tier: 2, labels: ["a"] }]).phase).toBe("confirming");
  });

  it("start is ignored while busy (no double submit)", () => {
    const s = run([{ type: "start", tier: 1, labels: ["a"] }, { type: "stepStart", index: 0 }]);
    expect(writeOpReducer(s, { type: "start", tier: 1, labels: ["a"] })).toBe(s);
  });

  it("all ok + verify → verifying → applied (R6)", () => {
    const s = run([
      { type: "start", tier: 1, labels: ["a"] },
      { type: "stepStart", index: 0 },
      { type: "stepOk", index: 0 },
      { type: "stepsDone", now: 0, hasVerify: true },
    ]);
    expect(s.phase).toBe("verifying");
    expect(writeOpReducer(s, { type: "verifyResult", ok: true }).phase).toBe("applied");
    const bad = writeOpReducer(s, { type: "verifyResult", ok: false });
    expect(bad.phase).toBe("failed");
    expect(bad.errorKind).toBe("mismatch");
  });

  it("last step drops while waiting for the device, earlier step done → partial once back (R10)", () => {
    const s = run([
      { type: "start", tier: 3, labels: ["NSA", "SA"] },
      { type: "confirm" },
      { type: "stepStart", index: 0 },
      { type: "stepOk", index: 0 },
      { type: "stepStart", index: 1 },
      { type: "stepError", index: 1, kind: "timeout", message: "timeout", now: 0, wait: WAIT },
    ]);
    expect(s.phase).toBe("waitDevice");
    const back = writeOpReducer(s, { type: "back", hasVerify: false });
    expect(back.phase).toBe("partial");
    expect(summarizeSteps(back)).toEqual({ done: ["NSA"], notDone: ["SA"] });
  });

  it("single step drops while waiting for the device → unknown once back", () => {
    const s = run([
      { type: "start", tier: 3, labels: ["reset"] },
      { type: "confirm" },
      { type: "stepStart", index: 0 },
      { type: "stepError", index: 0, kind: "timeout", message: "timeout", now: 0, wait: WAIT },
    ]);
    expect(writeOpReducer(s, { type: "back", hasVerify: false }).phase).toBe("unknown");
  });

  it("all ok, no verify → accepted (R6)", () => {
    const s = run([
      { type: "start", tier: 2, labels: ["a"] },
      { type: "confirm" },
      { type: "stepStart", index: 0 },
      { type: "stepOk", index: 0 },
      { type: "stepsDone", now: 0, hasVerify: false },
    ]);
    expect(s.phase).toBe("accepted");
  });

  it("first-step device error → failed; timeout/network → unknown", () => {
    const base: WriteOpEvent[] = [{ type: "start", tier: 1, labels: ["a"] }, { type: "stepStart", index: 0 }];
    const f = run([...base, { type: "stepError", index: 0, kind: "device", message: "bad band", now: 0 }]);
    expect(f.phase).toBe("failed");
    expect(f.error).toBe("bad band");
    expect(f.steps[0].status).toBe("failed");
    const u = run([...base, { type: "stepError", index: 0, kind: "timeout", message: "t", now: 0 }]);
    expect(u.phase).toBe("unknown");
    expect(u.steps[0].status).toBe("unconfirmed");
    expect(run([...base, { type: "stepError", index: 0, kind: "network", message: "n", now: 0 }]).phase).toBe("unknown");
    // unknown offers no resubmit
    expect(writeOpReducer(u, { type: "resubmit" })).toBe(u);
  });

  it("step 1 ok, step 2 fails/times out → partial; resubmit resets only unfinished steps (R10)", () => {
    const head: WriteOpEvent[] = [
      { type: "start", tier: 3, labels: ["NSA", "SA"] },
      { type: "confirm" },
      { type: "stepStart", index: 0 },
      { type: "stepOk", index: 0 },
      { type: "stepStart", index: 1 },
    ];
    for (const kind of ["timeout", "device", "network"] as const) {
      const p = run([...head, { type: "stepError", index: 1, kind, message: "x", now: 0 }]);
      expect(p.phase).toBe("partial");
      expect(summarizeSteps(p)).toEqual({ done: ["NSA"], notDone: ["SA"] });
      const r = writeOpReducer(p, { type: "resubmit" });
      expect(r.phase).toBe("submitting");
      expect(r.steps.map((s) => s.status)).toEqual(["done", "pending"]);
      expect(r.runId).toBe(p.runId + 1);
    }
  });

  it("401 → relogin; login: tier 1 auto-resends, tier 2/3 → resubmitReady (R2)", () => {
    for (const tier of [1, 2, 3] as const) {
      const s = run([
        { type: "start", tier, labels: ["a"] },
        ...(tier === 1 ? [] : [{ type: "confirm" } as WriteOpEvent]),
        { type: "stepStart", index: 0 },
        { type: "stepError", index: 0, kind: "unauthorized", message: "unauthorized", now: 0 },
      ]);
      expect(s.phase).toBe("relogin");
      expect(s.reloginFor).toBe("submit");
      expect(s.steps[0].status).toBe("pending");
      const after = writeOpReducer(s, { type: "login" });
      if (tier === 1) {
        expect(after.phase).toBe("submitting");
      } else {
        expect(after.phase).toBe("resubmitReady");
        const again = writeOpReducer(after, { type: "resubmit" });
        expect(again.phase).toBe("submitting");
      }
    }
  });

  it("stale 401 skips the login step: tier 1 re-sends, tier 2/3 resubmitReady (R11)", () => {
    const mk = (tier: 1 | 3) =>
      run([
        { type: "start", tier, labels: ["a"] },
        ...(tier === 1 ? [] : [{ type: "confirm" } as WriteOpEvent]),
        { type: "stepStart", index: 0 },
        { type: "stepError", index: 0, kind: "staleUnauthorized", message: "unauthorized", now: 0 },
      ]);
    const t1 = mk(1);
    expect(t1.phase).toBe("submitting");
    expect(t1.steps[0].status).toBe("pending");
    expect(mk(3).phase).toBe("resubmitReady");
  });

  it("waitDevice: stepsDone with wait → waitDevice; back → verifying / accepted; timeout → waitTimeout", () => {
    const s = run([
      { type: "start", tier: 3, labels: ["reboot"] },
      { type: "confirm" },
      { type: "stepStart", index: 0 },
      { type: "stepOk", index: 0 },
      { type: "stepsDone", now: 5, hasVerify: true, wait: WAIT },
    ]);
    expect(s.phase).toBe("waitDevice");
    expect(s.wait).toEqual({ startedAt: 5, expectedSec: 90, timeoutSec: 180, sawDown: false, submitUnconfirmed: false });
    expect(writeOpReducer(s, { type: "probe", ok: false }).wait?.sawDown).toBe(true);
    expect(writeOpReducer(s, { type: "back", hasVerify: true }).phase).toBe("verifying");
    expect(writeOpReducer(s, { type: "back", hasVerify: false }).phase).toBe("accepted");
    expect(writeOpReducer(s, { type: "verifyResult", ok: true }).phase).toBe("applied");
    const t = writeOpReducer(s, { type: "waitTimeout", recovery: "连 Wi-Fi「U60-new」" });
    expect(t.phase).toBe("waitTimeout");
    expect(t.recovery).toBe("连 Wi-Fi「U60-new」");
    const r = writeOpReducer(s, { type: "verifyError", kind: "unauthorized", message: "u", during: "wait" });
    expect(r.phase).toBe("relogin");
    expect(r.reloginFor).toBe("verify");
    expect(writeOpReducer(r, { type: "login" }).phase).toBe("verifying");
    // network error while waiting: keep waiting
    expect(writeOpReducer(s, { type: "verifyError", kind: "network", message: "n", during: "wait" })).toBe(s);
  });

  it("dropped connection on the last step of a waitDevice op → waitDevice, not unknown", () => {
    const s = run([
      { type: "start", tier: 3, labels: ["reboot"] },
      { type: "confirm" },
      { type: "stepStart", index: 0 },
      { type: "stepError", index: 0, kind: "network", message: "n", now: 7, wait: WAIT },
    ]);
    expect(s.phase).toBe("waitDevice");
    expect(s.wait?.submitUnconfirmed).toBe(true);
    // back without a readback: still can't confirm
    expect(writeOpReducer(s, { type: "back", hasVerify: false }).phase).toBe("unknown");
    // readback confirms: the unconfirmed step counts as done
    const ok = writeOpReducer(s, { type: "verifyResult", ok: true });
    expect(ok.phase).toBe("applied");
    expect(ok.steps[0].status).toBe("done");
  });

  it("verify network error → unknown; readback later can still resolve it", () => {
    const s = run([
      { type: "start", tier: 1, labels: ["a"] },
      { type: "stepStart", index: 0 },
      { type: "stepOk", index: 0 },
      { type: "stepsDone", now: 0, hasVerify: true },
      { type: "verifyError", kind: "timeout", message: "t", during: "verify" },
    ]);
    expect(s.phase).toBe("unknown");
    expect(writeOpReducer(s, { type: "verifyResult", ok: true }).phase).toBe("applied");
  });

  it("reset returns to idle and invalidates the run", () => {
    const s = run([{ type: "start", tier: 1, labels: ["a"] }]);
    const r = writeOpReducer(s, { type: "reset" });
    expect(r.phase).toBe("idle");
    expect(r.runId).toBeGreaterThan(s.runId);
  });
});

describe("classifyError", () => {
  it("maps error classes", () => {
    expect(classifyError(new UnauthorizedError())).toBe("unauthorized");
    expect(classifyError(new UnauthorizedError("unauthorized", true))).toBe("staleUnauthorized");
    expect(classifyError(new TimeoutError(15000))).toBe("timeout");
    expect(classifyError(new ApiError("network error: x", 0))).toBe("network");
    expect(classifyError(new ApiError("bad", 400))).toBe("device");
    expect(classifyError(new ApiError("bad", 500))).toBe("device");
    expect(classifyError(new Error("x"))).toBe("other");
  });
});

// ── controller (fake clock, probe and login) ────────────────────────────────

interface Harness {
  ctl: WriteOpController;
  clock: { t: number };
  probes: boolean[];
  login: () => void;
  flush: () => Promise<void>;
  states: string[];
}

function harness(cfg: () => WriteOpConfig, probeAnswers: boolean[] = []): Harness {
  const clock = { t: 1000 };
  const probes: boolean[] = [];
  let loginCb: (() => void) | null = null;
  const deps: WriteOpDeps = {
    now: () => clock.t,
    sleep: async (ms) => {
      clock.t += ms;
    },
    probe: async () => {
      const a = probeAnswers.length ? probeAnswers.shift()! : true;
      probes.push(a);
      return a;
    },
    onLogin: (cb) => {
      loginCb = cb;
      return () => (loginCb = null);
    },
  };
  const ctl = new WriteOpController(cfg, deps);
  ctl.attach();
  const states: string[] = [];
  ctl.subscribe(() => {
    const p = ctl.getState().phase;
    if (states[states.length - 1] !== p) states.push(p);
  });
  const flush = async () => {
    for (let i = 0; i < 50; i++) await Promise.resolve();
  };
  return { ctl, clock, probes, login: () => loginCb?.(), flush, states };
}

function step(label: string, impl: () => Promise<unknown>) {
  const calls = { n: 0 };
  return { calls, step: { label, run: () => ((calls.n += 1), impl()) } };
}

describe("WriteOpController", () => {
  it("applied with readback, accepted without", async () => {
    const a = step("a", async () => ({}));
    const h = harness(() => ({ tier: 1, steps: [a.step], verify: async () => true }));
    h.ctl.start();
    await h.flush();
    expect(h.ctl.getState().phase).toBe("applied");

    const b = step("b", async () => ({}));
    const h2 = harness(() => ({ tier: 1, steps: [b.step] }));
    h2.ctl.start();
    await h2.flush();
    expect(h2.ctl.getState().phase).toBe("accepted");
  });

  it("ignores a second start while submitting", async () => {
    let release!: () => void;
    const a = step("a", () => new Promise<void>((r) => (release = r)));
    const h = harness(() => ({ tier: 1, steps: [a.step] }));
    h.ctl.start();
    h.ctl.start();
    h.ctl.resubmit();
    await h.flush();
    expect(a.calls.n).toBe(1);
    release();
    await h.flush();
    expect(h.ctl.getState().phase).toBe("accepted");
  });

  it("tier 3 needs confirm before anything is sent", async () => {
    const a = step("a", async () => ({}));
    const h = harness(() => ({ tier: 3, steps: [a.step] }));
    h.ctl.start();
    await h.flush();
    expect(h.ctl.getState().phase).toBe("confirming");
    expect(a.calls.n).toBe(0);
    h.ctl.confirm();
    await h.flush();
    expect(a.calls.n).toBe(1);
  });

  it("partial on step-2 timeout; resubmit sends only step 2 (R10)", async () => {
    const nsa = step("NSA", async () => ({}));
    let saFails = true;
    const sa = step("SA", async () => {
      if (saFails) throw new TimeoutError(15000);
    });
    const h = harness(() => ({ tier: 3, steps: [nsa.step, sa.step], verify: async () => true }));
    h.ctl.start();
    h.ctl.confirm();
    await h.flush();
    expect(h.ctl.getState().phase).toBe("partial");
    expect(summarizeSteps(h.ctl.getState())).toEqual({ done: ["NSA"], notDone: ["SA"] });
    saFails = false;
    h.ctl.resubmit();
    await h.flush();
    expect(nsa.calls.n).toBe(1);
    expect(sa.calls.n).toBe(2);
    expect(h.ctl.getState().phase).toBe("applied");
  });

  it("partial on step-2 device error; step-2 401 → relogin then only step 2 after login", async () => {
    const one = step("1", async () => ({}));
    const two = step("2", async () => {
      throw new ApiError("rejected", 400);
    });
    const h = harness(() => ({ tier: 2, steps: [one.step, two.step] }));
    h.ctl.start();
    h.ctl.confirm();
    await h.flush();
    expect(h.ctl.getState().phase).toBe("partial");

    let first = true;
    const x = step("x", async () => ({}));
    const y = step("y", async () => {
      if (first) {
        first = false;
        throw new UnauthorizedError();
      }
    });
    const h2 = harness(() => ({ tier: 1, steps: [x.step, y.step] }));
    h2.ctl.start();
    await h2.flush();
    expect(h2.ctl.getState().phase).toBe("relogin");
    h2.login();
    await h2.flush();
    expect(h2.ctl.getState().phase).toBe("accepted");
    expect(x.calls.n).toBe(1);
    expect(y.calls.n).toBe(2);
  });

  it("unknown on timeout, no resubmit offered", async () => {
    const a = step("a", async () => {
      throw new TimeoutError(15000);
    });
    const h = harness(() => ({ tier: 1, steps: [a.step] }));
    h.ctl.start();
    await h.flush();
    expect(h.ctl.getState().phase).toBe("unknown");
    h.ctl.resubmit();
    await h.flush();
    expect(a.calls.n).toBe(1);
  });

  it("unknown with a readback reconfirms once the device answers (5.1)", async () => {
    const a = step("a", async () => {
      throw new ApiError("network error: x", 0);
    });
    const h = harness(() => ({ tier: 1, steps: [a.step], verify: async () => true }), [false, true]);
    h.ctl.start();
    await h.flush();
    expect(h.states).toContain("unknown");
    expect(h.ctl.getState().phase).toBe("applied");
    expect(h.probes).toEqual([false, true]);
  });

  it("readback mismatch → failed; resubmit sends the write again", async () => {
    const a = step("a", async () => ({}));
    let verifyOk = false;
    const h = harness(() => ({ tier: 1, steps: [a.step], verify: async () => verifyOk }));
    h.ctl.start();
    await h.flush();
    expect(h.ctl.getState().phase).toBe("failed");
    expect(h.ctl.getState().errorKind).toBe("mismatch");
    verifyOk = true;
    h.ctl.resubmit();
    await h.flush();
    expect(a.calls.n).toBe(2);
    expect(h.ctl.getState().phase).toBe("applied");
  });

  it("failed on device error keeps state for another try", async () => {
    const a = step("a", async () => {
      throw new ApiError("SSID too long", 400);
    });
    const h = harness(() => ({ tier: 2, steps: [a.step] }));
    h.ctl.start();
    h.ctl.confirm();
    await h.flush();
    const s = h.ctl.getState();
    expect(s.phase).toBe("failed");
    expect(s.error).toBe("SSID too long");
  });

  it("401: tier 1 auto-resends after login; tier 2/3 wait in resubmitReady (R2)", async () => {
    for (const tier of [1, 2, 3] as const) {
      let first = true;
      const a = step("a", async () => {
        if (first) {
          first = false;
          throw new UnauthorizedError();
        }
      });
      const h = harness(() => ({ tier, steps: [a.step] }));
      h.ctl.start();
      if (tier !== 1) h.ctl.confirm();
      await h.flush();
      expect(h.ctl.getState().phase).toBe("relogin");
      h.login();
      await h.flush();
      if (tier === 1) {
        expect(h.ctl.getState().phase).toBe("accepted");
        expect(a.calls.n).toBe(2);
      } else {
        expect(h.ctl.getState().phase).toBe("resubmitReady");
        expect(a.calls.n).toBe(1);
        h.ctl.resubmit(); // one click, no confirm
        await h.flush();
        expect(h.ctl.getState().phase).toBe("accepted");
        expect(a.calls.n).toBe(2);
      }
    }
  });

  it("login events outside relogin are ignored", async () => {
    const a = step("a", async () => ({}));
    const h = harness(() => ({ tier: 1, steps: [a.step] }));
    h.ctl.start();
    await h.flush();
    h.login();
    await h.flush();
    expect(a.calls.n).toBe(1);
  });

  it("stale 401 on tier 1 re-sends immediately with the new token", async () => {
    let first = true;
    const a = step("a", async () => {
      if (first) {
        first = false;
        throw new UnauthorizedError("unauthorized", true);
      }
    });
    const h = harness(() => ({ tier: 1, steps: [a.step] }));
    h.ctl.start();
    await h.flush();
    expect(h.ctl.getState().phase).toBe("accepted");
    expect(a.calls.n).toBe(2);
    expect(h.states).not.toContain("relogin");
  });

  it("waitDevice: device drops, comes back, readback applied (R1)", async () => {
    const reboot = step("reboot", async () => ({}));
    let verifyCalls = 0;
    const h = harness(
      () => ({
        tier: 3,
        steps: [reboot.step],
        verify: async () => (verifyCalls++, true),
        waitDevice: { expectedSec: 90, expectDown: true },
      }),
      [true, false, false, true] // still up for a moment, then down, then back
    );
    h.ctl.start();
    h.ctl.confirm();
    await h.flush();
    expect(h.states).toContain("waitDevice");
    expect(h.ctl.getState().phase).toBe("applied");
    expect(h.probes).toEqual([true, false, false, true]);
    expect(verifyCalls).toBe(1); // not verified while it was still going down
  });

  it("waitDevice: without a readback → accepted when back", async () => {
    const s = step("apn", async () => ({}));
    const h = harness(() => ({ tier: 3, steps: [s.step], waitDevice: { expectedSec: 30 } }), [false, true]);
    h.ctl.start();
    h.ctl.confirm();
    await h.flush();
    expect(h.ctl.getState().phase).toBe("accepted");
  });

  it("waitDevice: never comes back → waitTimeout with the caller's recovery hint", async () => {
    const s = step("lan", async () => ({}));
    const h = harness(
      () => ({
        tier: 3,
        steps: [s.step],
        verify: async () => true,
        waitDevice: { expectedSec: 30, timeoutSec: 60, probeEveryMs: 3000, recovery: "打开 http://10.0.99.1:9090" },
      }),
      Array(100).fill(false)
    );
    h.ctl.start();
    h.ctl.confirm();
    await h.flush();
    await h.flush();
    const st = h.ctl.getState();
    expect(st.phase).toBe("waitTimeout");
    expect(st.recovery).toBe("打开 http://10.0.99.1:9090");
    expect(h.probes.length).toBe(20); // 60 s / 3 s
  });

  it("waitDevice: readback 401 after reboot → relogin, then readback after login (R1)", async () => {
    const reboot = step("reboot", async () => ({}));
    let tokenValid = false;
    let verifyCalls = 0;
    const h = harness(
      () => ({
        tier: 3,
        steps: [reboot.step],
        verify: async () => {
          verifyCalls++;
          if (!tokenValid) throw new UnauthorizedError();
          return true;
        },
        waitDevice: { expectedSec: 90 },
      }),
      [false, true]
    );
    h.ctl.start();
    h.ctl.confirm();
    await h.flush();
    let st = h.ctl.getState();
    expect(st.phase).toBe("relogin");
    expect(st.reloginFor).toBe("verify");
    tokenValid = true;
    h.login();
    await h.flush();
    st = h.ctl.getState();
    expect(st.phase).toBe("applied");
    expect(verifyCalls).toBe(2);
    expect(reboot.calls.n).toBe(1); // never re-sends the reboot
  });

  it("waitDevice: the reboot request itself drops → still waits, then readback", async () => {
    const reboot = step("reboot", async () => {
      throw new ApiError("network error: Failed to fetch", 0);
    });
    const h = harness(
      () => ({ tier: 3, steps: [reboot.step], verify: async () => true, waitDevice: { expectedSec: 90 } }),
      [false, true]
    );
    h.ctl.start();
    h.ctl.confirm();
    await h.flush();
    expect(h.states).not.toContain("unknown");
    expect(h.ctl.getState().phase).toBe("applied");
  });

  it("detach abandons a running wait", async () => {
    const s = step("reboot", async () => ({}));
    const clock = { t: 0 };
    let probes = 0;
    const ctl = new WriteOpController(() => ({ tier: 3, steps: [s.step], waitDevice: { expectedSec: 90 } }), {
      now: () => clock.t,
      sleep: async (ms) => void (clock.t += ms),
      probe: async () => (probes++, false),
      onLogin: () => () => {},
    });
    const detach = ctl.attach();
    ctl.start();
    ctl.confirm();
    for (let i = 0; i < 5; i++) await Promise.resolve();
    detach();
    const before = probes;
    for (let i = 0; i < 50; i++) await Promise.resolve();
    expect(probes).toBeLessThanOrEqual(before + 1);
  });
});
