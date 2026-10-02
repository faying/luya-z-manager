"use client";
// Diagnose / 网络诊断 (docs/designs/slow-diagnosis.md §4.2, §12.3, §12.5,
// §12.8 decision 11A: same content and states as the touch screen's page,
// only the placement differs). Reading order:
//
//   status block: main cause + one action (or 正在检查… N/M, 等另一个操作做完…,
//                 后台没回应) · 「HH:MM 测」 · 「再查一次」 in the title bar
//   layers: one row per layer (StatusMark: symbol + word + value), then the
//           speed row once 「加测速度」 ran
//   what you can do: 「加测速度」 (two presses when roaming) · 「对 / 不对」
//   (≥1024: layers and what-you-can-do side by side)
//
// The agent (deep_diag.rs) judges and words everything in both languages;
// lib/diagnose.ts maps levels to symbols and words. The run itself is the
// readback of every button here (start, speed, feedback): the page polls
// GET /api/diagnose every second while the run or its speed row is going.
// None of them changes a device setting, so they are plain requests, not
// write ops: start costs a few probe packets, the speed test ≤30 MB (and
// asks twice when roaming, like the touch screen).
//
// Entering from inside the app (or with ?start=1, which the home page's
// 「查原因 →」 adds) starts a run when there is no result from the last 10
// minutes; a hard load never does (see the effect below).
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ArrowClockwise, Play } from "@phosphor-icons/react";
import { apiFetch } from "@/lib/api/client";
import { ApiError, UnauthorizedError, errorText } from "@/lib/api/types";
import { useApi } from "@/lib/hooks/useApi";
import type { DiagGet, DiagLayer, DiagRun } from "@/lib/api/schemas/tools";
import type { NetInfo } from "@/lib/api/schemas/network";
import { Button, GroupTitle, StatusBlock, StatusMark, type Tone } from "@/components/nd";
import { fmtDevice } from "@/lib/deviceClock";
import { useLang } from "@/lib/i18n/pick";
import { arrivedInApp, headView, isLive, isRun, layerName, orderedLayers, pollMs, progress, rowView, waitingFor } from "@/lib/diagnose";

const PATH = "/api/diagnose";
/** How long the roaming confirm stays armed (touch screen: 5 s). */
const ARM_MS = 5000;

type StartReply = { ok: boolean; data?: DiagRun; joined?: boolean; error?: string; error_en?: string };

/** No answer, a timeout or a 5xx: the agent is not responding. A 401 is the
 *  session's business (apiFetch hands it to the login flow). */
function agentDown(e: unknown): boolean {
  if (!e || e instanceof UnauthorizedError) return false;
  return !(e instanceof ApiError) || e.status === 0 || e.status >= 500;
}

export default function DiagnosePage() {
  const { t } = useTranslation();
  const lang = useLang();

  const [starting, setStarting] = useState(false);
  const [startErr, setStartErr] = useState<unknown>(null);
  const [note, setNote] = useState<string | null>(null);

  const diag = useApi<DiagGet>(PATH, {
    refreshInterval: (d) => pollMs(d),
    refreshWhenHidden: true,
  });
  // Roaming decides the two-press speed test (same key as the home page).
  const ni = useApi<NetInfo>("/api/netinfo?lite=1", { refreshInterval: 30000 });
  const roaming = ni.data?.roaming === true;

  const run = isRun(diag.data) ? diag.data : null;
  const live = isLive(diag.data);
  const down = agentDown(startErr) || agentDown(diag.error);
  // An agent that answers but can't diagnose (older than deep_diag: 404).
  const noDiag = !down && !run && diag.error instanceof ApiError && diag.error.status >= 400 && diag.error.status < 500;

  // 「正在查，稍等」 belongs to the run in progress; gone once it is over.
  useEffect(() => {
    if (!live) setNote(null);
  }, [live]);

  async function start() {
    // Pressing again while it runs: say so, don't start a second one.
    if (run && run.state !== "done") {
      setNote(t("diagnose.alreadyRunning", "Checking already; one moment"));
      return;
    }
    setNote(null);
    setStartErr(null);
    setStarting(true);
    try {
      const r = await apiFetch<StartReply>(PATH, { method: "POST", body: {}, raw: true });
      if (!r.ok) throw new ApiError(r.error || "failed", 500, r.error_en);
      if (r.data) await diag.mutate(r.data, { revalidate: false });
      if (r.joined) setNote(t("diagnose.alreadyRunning", "Checking already; one moment"));
    } catch (e) {
      if (!(e instanceof UnauthorizedError)) setStartErr(e);
    } finally {
      setStarting(false);
    }
  }

  function retry() {
    if (startErr) void start();
    else {
      setStartErr(null);
      void diag.mutate();
    }
  }

  // Starting by itself (§12.5: no result from the last 10 minutes → check
  // again on entry), only when the user came here from inside the app (menu,
  // ⌘K, the home page's 「查原因 →」), or with ?start=1. A hard load — typed
  // URL, bookmark, reload, and every route sweep in the e2e suite — shows the
  // last result or 「开始」 instead, so opening a page never sends probes on
  // its own. The agent answers idle once its result is over 10 minutes old.
  const autoRef = useRef<"armed" | "done" | null>(null);
  useEffect(() => {
    if (autoRef.current === null) {
      const nav = performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming | undefined;
      const asked = new URLSearchParams(window.location.search).get("start") === "1";
      autoRef.current = asked || arrivedInApp(nav?.name, window.location.href) ? "armed" : "done";
    }
    if (autoRef.current !== "armed" || diag.data === undefined) return;
    autoRef.current = "done";
    if (!isRun(diag.data)) void start();
    // start() reads the latest state; this runs once, after the first read.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [diag.data]);

  // ── status block ──
  let tone: Tone = "neutral";
  let word: ReactNode;
  let reason: ReactNode = null;
  let meta: ReactNode = null;
  let actions: ReactNode = null;
  const checkingNote = t("diagnose.checkingNote", "About 10 s; sends a few probe packets");

  if (down) {
    tone = "bad";
    word = t("diagnose.agentDown", "Agent not responding");
    reason = run ? t("diagnose.agentDownReason", "Below is the last result received") : null;
    actions = (
      <Button variant="secondary" onPress={retry}>
        <ArrowClockwise size={18} weight="bold" aria-hidden />
        {t("diagnose.retry", "Retry")}
      </Button>
    );
  } else if (starting && !live) {
    word = t("diagnose.checking", "Checking…");
    reason = checkingNote;
  } else if (run?.state === "waiting") {
    word = t("diagnose.waiting", "Waiting…");
    const what = waitingFor(t, run.waiting_for);
    reason = what ? t("diagnose.waitingWhat", "{{what}}; starts when it's done", { what }) : null;
  } else if (run?.state === "running") {
    const p = progress(run);
    word = t("diagnose.checkingN", "Checking… {{step}}/{{steps}}", p);
    reason = checkingNote;
    meta = (
      <div
        role="progressbar"
        aria-label={t("diagnose.progressLabel", "Progress")}
        aria-valuemin={0}
        aria-valuemax={p.steps}
        aria-valuenow={p.step}
        className="h-2 w-full overflow-hidden rounded-full bg-nd-track"
      >
        <div
          className="h-full rounded-full transition-[width] duration-500"
          style={{ width: `${p.steps ? Math.round((p.step / p.steps) * 100) : 0}%`, background: "var(--nd-t1)" }}
        />
      </div>
    );
  } else if (run?.state === "done") {
    const h = headView(t, run.main, lang);
    tone = h.tone;
    word = h.text;
    reason = (
      <>
        {h.action && <ActionText to={h.to} text={h.action} />}
        {h.more && <>{h.action ? " · " : ""}{h.more}</>}
      </>
    );
    if (run.finished_at) meta = t("diagnose.testedAt", "Checked {{time}}", { time: fmtDevice(run.finished_at, "time") });
  } else if (noDiag) {
    word = t("diagnose.unsupported", "The agent on the device can't diagnose yet");
    reason = errorText(diag.error, lang);
  } else if (diag.data === undefined) {
    word = t("speedtest.reading", "Reading…");
  } else {
    word = t("diagnose.idle", "Not checked yet");
    reason = t("diagnose.idleReason", "Find out whether it's Wi-Fi, signal, a speed cap, the cellular link or cell load");
    actions = (
      <Button onPress={() => void start()} pending={starting} aria-describedby="dg-cost">
        <Play size={18} weight="fill" aria-hidden />
        {t("diagnose.start", "Start")}
      </Button>
    );
  }

  const layers = orderedLayers(run?.layers);
  const showRows = layers.length > 0;

  return (
    <div className="max-w-[1100px]">
      <div className="mb-4 mt-2 flex flex-wrap items-center gap-x-3 gap-y-2">
        <h1 className="nd-title flex-1">{t("diagnose.title", "Diagnose")}</h1>
        {run && (
          <Button variant="secondary" onPress={() => void start()} pending={starting}>
            <ArrowClockwise size={18} weight="bold" aria-hidden />
            {t("diagnose.again", "Check again")}
          </Button>
        )}
      </div>

      <div className="grid gap-6">
        <section aria-label={t("diagnose.statusLabel", "Diagnosis status")}>
          <StatusBlock tone={tone} state={word} reason={reason} meta={meta} actions={actions} />
          <div className="mt-2 grid gap-1 px-1">
            {!run && !down && (
              <p id="dg-cost" className="nd-aux">
                {checkingNote}
              </p>
            )}
            {note && (
              <p role="status" className="nd-aux">
                {note}
              </p>
            )}
            {/* A refused start that isn't "agent gone" (e.g. 404 from an older agent): say why. */}
            {!down && startErr != null && (
              <p role="alert">
                <StatusMark tone="bad">{errorText(startErr, lang)}</StatusMark>
              </p>
            )}
          </div>
        </section>

        {showRows && (
          <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
            <section aria-labelledby="dg-layers">
              <GroupTitle id="dg-layers">{t("diagnose.layersTitle", "Layers")}</GroupTitle>
              <div className={`nd-group${down ? " nd-stale" : ""}`} aria-live="polite">
                {layers.map((l) => (
                  <LayerRow key={l.id} layer={l} />
                ))}
                {run?.speed && <LayerRow layer={run.speed} />}
              </div>
            </section>

            {run?.state === "done" && <NextSteps run={run} roaming={roaming} onRun={(r) => void diag.mutate(r, { revalidate: false })} />}
          </div>
        )}
      </div>
    </div>
  );
}

/** The one action; "proxy" leads to the proxy page, "placement" is a touch-screen page (text only). */
function ActionText(p: { to: string; text: string }) {
  return <>{p.text}</>;
}


function LayerRow({ layer }: { layer: DiagLayer }) {
  const { t } = useTranslation();
  const lang = useLang();
  const v = rowView(t, layer, lang);
  const measuring = layer.id === "speed" && layer.level === "running";
  return (
    <div className="nd-row flex-wrap gap-y-1">
      <span className={`nd-row__label w-28 shrink-0 sm:w-32${v.muted ? " text-nd-t3" : ""}`}>{layerName(t, layer.id)}</span>
      <span className={`nd-row__value min-w-0 flex-1 text-start${v.muted ? " text-nd-t3" : ""}`}>
        {measuring ? <Elapsed /> : v.tone ? <StatusMark tone={v.tone}>{v.word}</StatusMark> : v.word}
        {v.detail && <span className="nd-aux ms-2">{v.detail}</span>}
      </span>
    </div>
  );
}

/** 「测试中… 3 s」: seconds since the speed row started measuring here. */
function Elapsed() {
  const { t } = useTranslation();
  const [t0] = useState(() => Date.now());
  const [now, setNow] = useState(t0);
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);
  const s = Math.floor((now - t0) / 1000);
  return <>{s > 0 ? t("diagnose.runningFor", "Testing… {{s}} s", { s }) : t("diagnose.running", "Testing…")}</>;
}

function NextSteps({ run, roaming, onRun }: { run: DiagRun; roaming: boolean; onRun: (r: DiagRun) => void }) {
  const { t } = useTranslation();
  const lang = useLang();

  // ── 加测速度 (two presses when roaming) ──
  const [armed, setArmed] = useState(false);
  const [speedBusy, setSpeedBusy] = useState(false);
  const [speedErr, setSpeedErr] = useState<unknown>(null);
  useEffect(() => {
    if (!armed) return;
    const id = setTimeout(() => setArmed(false), ARM_MS);
    return () => clearTimeout(id);
  }, [armed]);
  const measuring = run.speed?.level === "running";

  async function speed() {
    if (roaming && !armed) {
      setArmed(true);
      return;
    }
    setArmed(false);
    setSpeedErr(null);
    setSpeedBusy(true);
    try {
      const r = await apiFetch<DiagRun | null>(`${PATH}/speed`, { method: "POST", body: { id: run.id } });
      if (r && isRun(r)) onRun(r);
    } catch (e) {
      if (!(e instanceof UnauthorizedError)) setSpeedErr(e);
    } finally {
      setSpeedBusy(false);
    }
  }

  // ── 对 / 不对 ──
  const [fbSent, setFbSent] = useState(false);
  const [fbBusy, setFbBusy] = useState(false);
  const [fbErr, setFbErr] = useState<unknown>(null);
  async function feedback(right: boolean) {
    setFbErr(null);
    setFbBusy(true);
    try {
      await apiFetch(`${PATH}/feedback`, { method: "POST", body: { id: run.id, right } });
      setFbSent(true);
      onRun({ ...run, feedback: right });
    } catch (e) {
      if (!(e instanceof UnauthorizedError)) setFbErr(e);
    } finally {
      setFbBusy(false);
    }
  }
  const answered = fbSent || run.feedback != null;

  return (
    <section aria-labelledby="dg-next">
      <GroupTitle id="dg-next">{t("diagnose.nextTitle", "What you can do")}</GroupTitle>
      <div className="nd-group">
        <div className="grid gap-2 p-4">
          <div>
            <Button
              variant={armed ? "confirm" : "secondary"}
              onPress={() => void speed()}
              isDisabled={measuring || speedBusy}
              pending={speedBusy}
              aria-describedby="dg-speed-note"
            >
              {armed ? t("diagnose.speedRoaming", "Uses roaming data; press again") : t("diagnose.speedAdd", "Add speed test · 5 s, ≤30 MB")}
            </Button>
          </div>
          <p id="dg-speed-note" className="nd-aux">
            {roaming ? t("diagnose.speedRoamingNote", "Roaming: this test uses roaming data") : t("diagnose.speedNote", "Download only; the result names the route it took")}
          </p>
          {speedErr != null && (
            <p role="alert">
              <StatusMark tone="warn">{errorText(speedErr, lang)}</StatusMark>
            </p>
          )}
        </div>
        <div className="grid gap-2 border-t border-nd-hair p-4">
          {answered ? (
            <p role="status">
              <StatusMark tone="ok">{t("diagnose.thanks", "Noted, thanks")}</StatusMark>
            </p>
          ) : (
            <>
              <p className="nd-body">{t("diagnose.askRight", "Is this right?")}</p>
              <div className="flex flex-wrap gap-2">
                <Button variant="secondary" onPress={() => void feedback(true)} isDisabled={fbBusy} className="min-w-[88px]">
                  {t("diagnose.right", "Right")}
                </Button>
                <Button variant="secondary" onPress={() => void feedback(false)} isDisabled={fbBusy} className="min-w-[88px]">
                  {t("diagnose.wrong", "Wrong")}
                </Button>
              </div>
              {fbErr != null && (
                <p role="alert">
                  <StatusMark tone="bad">{errorText(fbErr, lang)}</StatusMark>
                </p>
              )}
            </>
          )}
        </div>
      </div>
    </section>
  );
}
