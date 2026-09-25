"use client";
// Signal detect (tool page, design doc §5.1 tool row). The page loads
// nothing on entry (as before): the button is available and the result
// area says there is no result yet.
//
//   Start = tier 2: POST start, then readback = progress starts moving
//           (polled like a bounded runJob).
//   Stop  = tier 2, two steps (R10): ① POST stop ② GET results.
//
// While a detection runs, progress is polled every 2 s and keeps polling
// when the tab is hidden (long job). At 100 % the results are read once.
// The results endpoint is a ubus passthrough whose shape is unconfirmed:
// rows come from cells[] / results[] / list[], else the object is one row.
// B27 (recorded 2026-09-25): idle progress is {progress: "", quality: ""}
// (strings, "" = nothing running) and results are `{}` when there are none;
// an object whose values are all empty counts as no result. A non-empty
// `quality` string is shown as the firmware's own quality word.
import { useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Play, Stop } from "@phosphor-icons/react";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { useWriteOp } from "@/lib/api/writeOp";
import type {
  CellSignalDetectProgress,
  CellSignalDetectRecord,
  CellSignalDetectResults,
} from "@/lib/api/schemas/modem";
import {
  Button,
  ConfirmInline,
  Freshness,
  GroupTitle,
  OpResult,
  StatusBlock,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

const PROGRESS = "/api/cell/signal-detect/progress";
const RESULTS = "/api/cell/signal-detect/results";
const POLL_MS = 2000;
const VERIFY_TRIES = 10; // × 2 s

type Phase = "idle" | "running" | "done" | "stopped";
type Rows = Record<string, string>[];

/** progress as a number, or null when the device sent nothing usable. */
function pctOf(d: CellSignalDetectProgress | undefined): number | null {
  if (!d || d.progress == null || d.progress === "") return null;
  const n = parseFloat(String(d.progress));
  return Number.isFinite(n) ? n : null;
}

function cell(v: unknown): string {
  if (v == null || v === "") return "—";
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

/** Unconfirmed shape: cells[] / results[] / list[], else one row. An empty
 *  object is no result (the old page drew a table with no columns). */
function toRows(data: CellSignalDetectResults | null | undefined): Rows {
  if (!data || typeof data !== "object") return [];
  const arr = data.cells ?? data.results ?? data.list;
  if (Array.isArray(arr)) {
    return arr
      .filter((x): x is CellSignalDetectRecord => !!x && typeof x === "object")
      .map((item) => Object.fromEntries(Object.entries(item).map(([k, v]) => [k, cell(v)])));
  }
  const entries = Object.entries(data);
  if (entries.length === 0 || entries.every(([, v]) => v == null || v === "")) return [];
  return [Object.fromEntries(entries.map(([k, v]) => [k, cell(v)]))];
}

/** The firmware's quality word next to progress (B27: string, "" when idle). */
function qualityOf(d: CellSignalDetectProgress | undefined): string | null {
  const q = d?.quality;
  if (q == null) return null;
  const s = String(q).trim();
  return s === "" ? null : s;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export default function SignalDetectPage() {
  const { t } = useTranslation();
  const [phase, setPhase] = useState<Phase>("idle");
  const running = phase === "running";

  // results: null = never loaded this visit
  const [rows, setRows] = useState<Rows | null>(null);
  const [resultsErr, setResultsErr] = useState<string | null>(null);
  const [resultsLoading, setResultsLoading] = useState(false);

  const loadResults = useCallback(async () => {
    setResultsLoading(true);
    try {
      const d = await apiFetch<CellSignalDetectResults>(RESULTS);
      setRows(toRows(d));
      setResultsErr(null);
    } catch (e) {
      // keep the old rows, say why
      setResultsErr(e instanceof Error ? e.message : String(e));
      throw e;
    } finally {
      setResultsLoading(false);
    }
  }, []);

  // Progress of this run only: SWR keeps the last run's cached value for the
  // key, and right after start the device may still report 100 from before,
  // so completion counts only after a reading below 100 (or 60 s of 100s).
  const phaseRef = useRef(phase);
  phaseRef.current = phase;
  const [livePct, setLivePct] = useState<number | null>(null);
  const lastPct = useRef<number | null>(null);
  const sawBelow = useRef(false);
  const startedAt = useRef(0);
  const progress = useApi<CellSignalDetectProgress>(running ? PROGRESS : null, {
    refreshInterval: POLL_MS,
    refreshWhenHidden: true,
    revalidateOnFocus: false,
    onSuccess: (d) => {
      const p = pctOf(d);
      if (p == null || phaseRef.current !== "running") return;
      lastPct.current = p;
      if (p < 100) sawBelow.current = true;
      if (p >= 100 && !sawBelow.current && Date.now() - startedAt.current < 60_000) return;
      setLivePct(p);
      if (p >= 100) {
        setPhase("done");
        void loadResults().catch(() => {});
      }
    },
  });
  const pct = running ? livePct : phase === "done" ? 100 : null;
  const quality = running || phase === "done" ? qualityOf(progress.data) : null;

  // ── writes ──
  const [confirmStart, setConfirmStart] = useState(false);
  const [confirmStop, setConfirmStop] = useState(false);
  const startInline = useConfirmInline(confirmStart);
  const stopInline = useConfirmInline(confirmStop);
  const baseline = useRef<number | null>(null);

  const startOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("sigdetect.startDetection", "Start Detection"),
        run: async () => {
          await apiFetch("/api/cell/signal-detect/start", { method: "POST" });
          setRows(null);
          setResultsErr(null);
          setLivePct(null);
          sawBelow.current = false;
          startedAt.current = Date.now();
          setPhase("running");
        },
      },
    ],
    // Readback (inventory): progress starts changing.
    verify: async () => {
      for (let i = 0; i < VERIFY_TRIES; i++) {
        await sleep(POLL_MS);
        const p = pctOf(await apiFetch<CellSignalDetectProgress>(PROGRESS));
        if (p == null) continue;
        if ((p > 0 && p < 100) || (baseline.current != null && p !== baseline.current)) return true;
      }
      return false;
    },
  });

  const stopOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("sigdetect.stepStop", "Stop"),
        run: async () => {
          await apiFetch("/api/cell/signal-detect/stop", { method: "POST" });
          setPhase("stopped");
        },
      },
      { label: t("sigdetect.stepResults", "Load results"), run: () => loadResults() },
    ],
  });

  const busy = startOp.busy || stopOp.busy;

  function openStart() {
    baseline.current = lastPct.current;
    setConfirmStart((o) => !o);
  }

  // ── status ──
  let tone: Tone = "neutral";
  let word: string;
  let reason: string | null = null;
  if (running) {
    tone = progress.stale ? "stale" : "neutral";
    word = pct != null
      ? t("sigdetect.runningPct", "Detecting · {{pct}}%", { pct: Math.min(100, Math.round(pct)) })
      : t("sigdetect.running", "Detecting…");
    reason = progress.error && !progress.data
      ? t("sigdetect.progressErr", "Can't read progress: {{e}}. Still trying every 2 seconds.", { e: String(progress.error.message ?? progress.error) })
      : t("sigdetect.runningReason", "The modem is measuring signal quality. You can leave this tab; it keeps checking.");
  } else if (phase === "done") {
    tone = "ok";
    word = t("sigdetect.detectionComplete", "Detection complete");
  } else if (phase === "stopped") {
    word = t("sigdetect.detectionStopped", "Detection stopped");
    reason = t("sigdetect.stoppedReason", "Results collected so far are shown below.");
  } else {
    word = t("sigdetect.idle", "Not detecting");
    reason = t("sigdetect.idleReason", "Press “Start Detection” to measure signal quality.");
  }

  const actionStart = t("sigdetect.startDetection", "Start Detection");
  const actionStop = t("sigdetect.stopDetection", "Stop Detection");
  const keys = rows && rows.length > 0 ? Array.from(new Set(rows.flatMap((r) => Object.keys(r)))) : [];

  return (
    <div className="max-w-[960px]">
      <h1 className="nd-title mb-4 mt-2">{t("sigdetect.title", "Signal Detection")}</h1>

      <div className="grid gap-6">
        <section aria-label={t("sigdetect.statusLabel", "Detection status")}>
          <StatusBlock
            tone={tone}
            state={word}
            reason={reason}
            meta={
              running || phase === "done" ? (
                <div className="grid gap-2">
                  <div
                    role="progressbar"
                    aria-label={t("sigdetect.progress", "Progress:")}
                    aria-valuemin={0}
                    aria-valuemax={100}
                    aria-valuenow={pct != null ? Math.min(100, Math.round(pct)) : undefined}
                    className="h-2 w-full overflow-hidden rounded-full bg-nd-track"
                  >
                    <div
                      className="h-full rounded-full transition-[width] duration-500"
                      style={{ width: `${Math.max(0, Math.min(100, pct ?? 0))}%`, background: "var(--nd-blue)" }}
                    />
                  </div>
                  {quality && (
                    <span className="nd-aux">
                      {t("sigdetect.quality", "Quality reported by the modem:")} <span className="nd-mono">{quality}</span>
                    </span>
                  )}
                  {running && <Freshness stale={progress.stale} lastOkAt={progress.lastOkAt} what={t("sigdetect.progressWhat", "Progress")} />}
                </div>
              ) : undefined
            }
            actions={
              running ? (
                <Button
                  variant="secondary"
                  onPress={() => setConfirmStop((o) => !o)}
                  isDisabled={stopOp.busy}
                  pending={stopOp.busy}
                  {...stopInline.triggerProps}
                >
                  <Stop size={18} weight="bold" aria-hidden />
                  {actionStop}
                </Button>
              ) : (
                <Button onPress={openStart} isDisabled={busy} pending={startOp.busy} {...startInline.triggerProps}>
                  <Play size={18} weight="fill" aria-hidden />
                  {actionStart}
                </Button>
              )
            }
          />
          <ConfirmInline
            id={startInline.id}
            open={confirmStart && !running}
            actionLabel={actionStart}
            consequence={t(
              "sigdetect.startConsequence",
              "The modem starts measuring signal quality. It can take a few minutes; you can stop it at any time."
            )}
            onCancel={() => setConfirmStart(false)}
            onConfirm={() => {
              setConfirmStart(false);
              startOp.start();
              startOp.confirm();
            }}
          />
          <ConfirmInline
            id={stopInline.id}
            open={confirmStop && running}
            actionLabel={actionStop}
            consequence={t("sigdetect.stopConsequence", "The measurement ends now and the results collected so far are loaded.")}
            onCancel={() => setConfirmStop(false)}
            onConfirm={() => {
              setConfirmStop(false);
              startOp.reset(); // abandon a start readback still in progress
              stopOp.start();
              stopOp.confirm();
            }}
          />
          <div className="mt-2 grid gap-1 px-1">
            <OpResult op={startOp} />
            <OpResult op={stopOp} />
          </div>
        </section>

        <section aria-labelledby="sd-results">
          <GroupTitle id="sd-results">{t("sigdetect.detectionResults", "Detection Results")}</GroupTitle>
          {resultsErr && (
            <div role="alert" className="mb-3 flex flex-wrap items-center gap-3 px-1">
              <span className="nd-body text-nd-badT">
                {t("sigdetect.resultsErr", "Couldn't read the results: {{e}}", { e: resultsErr })}
              </span>
              <Button variant="secondary" size="sm" onPress={() => void loadResults().catch(() => {})} pending={resultsLoading}>
                {t("common.retry", "Retry")}
              </Button>
            </div>
          )}
          <div className="nd-group">
            {rows === null ? (
              <p className="nd-body p-4 text-nd-t2 lg:p-5" role="status">
                {resultsLoading
                  ? t("sigdetect.loadingResults", "Reading results…")
                  : running
                    ? t("sigdetect.resultsPending", "Results appear here when the detection finishes.")
                    : t("sigdetect.noResultYet", "No result yet · press “Start Detection”")}
              </p>
            ) : rows.length === 0 ? (
              <p className="nd-body p-4 text-nd-t2 lg:p-5" role="status">
                {t("sigdetect.noResults", "No results available.")} {t("sigdetect.noResultsNext", "Try detecting again.")}
              </p>
            ) : (
              <div className="overflow-x-auto p-2 lg:p-3">
                <table className="w-full text-[14px] leading-5">
                  <thead>
                    <tr className="text-left text-nd-t2">
                      {keys.map((k) => (
                        <th key={k} scope="col" className="px-2 py-2 font-semibold">
                          {k}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map((row, i) => (
                      <tr key={i} className="border-t border-nd-sep">
                        {keys.map((k) => (
                          <td key={k} className="nd-mono whitespace-nowrap px-2 py-2 tabular-nums">
                            {row[k] ?? "—"}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </div>
        </section>
      </div>
    </div>
  );
}
