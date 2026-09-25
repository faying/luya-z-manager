"use client";
// Speed test (tool page, design doc §5.1 tool row; §2: the result is this
// page's one hero number). Order: status (phase · reason, start/stop) →
// readings (download as hero, upload, ping, jitter) → server choice →
// details (server used, data used).
//
//   Start = tier 2 (inventory; not in the closed tier-1 list): the inline
//           confirm states the mobile-data cost. No readback (R6): the
//           progress endpoint is the result. 409 "already running" just
//           shows the running test.
//   Stop  = tier 2 (inventory). Readback: progress reaches cancelled /
//           complete / error (polled, bounded — the runJob idea from the
//           CHILL page).
//
// Progress is read once on entry (so a running test or the last result
// shows when you come back) and every second while a test runs, also when
// the tab is hidden (long job).
import { useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { Play, Stop, ArrowClockwise } from "@phosphor-icons/react";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { useWriteOp } from "@/lib/api/writeOp";
import type { SpeedProgress, SpeedServers, SpeedStartBody } from "@/lib/api/schemas/tools";
import {
  Button,
  ConfirmInline,
  Freshness,
  Group,
  GroupTitle,
  Help,
  OpResult,
  Readout,
  ReadoutWall,
  Row,
  StatusBlock,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

const PROGRESS = "/api/speedtest/progress";
const SERVERS = "/api/speedtest/servers";
const DONE = new Set<SpeedProgress["phase"]>(["complete", "cancelled", "error"]);
const ACTIVE = new Set<SpeedProgress["phase"]>(["latency", "download", "upload"]);
const STOP_WAIT_MS = 15_000;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function phaseLabel(t: TFunction, p: SpeedProgress["phase"]): string {
  return {
    idle: t("speedtest.phaseIdle", "Idle"),
    latency: t("speedtest.phaseLatency", "Measuring latency…"),
    download: t("speedtest.phaseDownload", "Testing download…"),
    upload: t("speedtest.phaseUpload", "Testing upload…"),
    complete: t("speedtest.phaseComplete", "Complete"),
    cancelled: t("speedtest.phaseCancelled", "Cancelled"),
    error: t("speedtest.phaseError", "Error"),
  }[p];
}

const f1 = (v: number | null | undefined) => (v == null || !Number.isFinite(v) ? null : v.toFixed(1));

function mb(bytes: number): string {
  return (bytes / 1e6).toFixed(bytes >= 1e8 ? 0 : 1);
}

export default function SpeedTestPage() {
  const { t } = useTranslation();

  // Set between a successful start and the first finished phase: right after
  // start the agent reports "idle" until its worker thread begins.
  const [expectRun, setExpectRun] = useState(false);
  const expectRef = useRef(expectRun);
  expectRef.current = expectRun;

  const progress = useApi<SpeedProgress>(PROGRESS, {
    refreshInterval: (d) => (expectRef.current || (d && ACTIVE.has(d.phase)) ? 1000 : 0),
    refreshWhenHidden: true,
    onSuccess: (d) => {
      if (expectRef.current && DONE.has(d.phase)) setExpectRun(false);
    },
  });
  const p = progress.data;
  const phase = p?.phase;
  const running = expectRun || (phase != null && ACTIVE.has(phase));

  // Server list: the device fetches it from speedtest.net (a little cellular
  // data, cached 5 min on the device). Once on entry, as before.
  const servers = useApi<SpeedServers>(SERVERS, {
    revalidateOnFocus: false,
    revalidateOnReconnect: false,
    shouldRetryOnError: false,
  });
  const [serverId, setServerId] = useState<number | "auto">("auto");

  // ── writes ──
  const [confirmStart, setConfirmStart] = useState(false);
  const startInline = useConfirmInline(confirmStart);
  const startOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("speedtest.start", "Start"),
        run: async () => {
          const body: SpeedStartBody = serverId === "auto" ? {} : { server_id: serverId };
          try {
            await apiFetch(`/api/speedtest/start`, { method: "POST", body });
            setExpectRun(true);
          } finally {
            void progress.mutate();
          }
        },
      },
    ],
  });

  const [confirmStop, setConfirmStop] = useState(false);
  const stopInline = useConfirmInline(confirmStop);
  const stopOp = useWriteOp({
    tier: 2,
    steps: [{ label: t("speedtest.stop", "Stop"), run: () => apiFetch("/api/speedtest/stop", { method: "POST", body: {} }) }],
    verify: async () => {
      const until = Date.now() + STOP_WAIT_MS;
      for (;;) {
        const d = await apiFetch<SpeedProgress>(PROGRESS);
        void progress.mutate(d, { revalidate: false });
        if (!ACTIVE.has(d.phase) && (d.phase !== "idle" || !expectRef.current)) {
          setExpectRun(false);
          return true;
        }
        if (Date.now() > until) return false;
        await sleep(1000);
      }
    },
  });

  // ── status ──
  const pct = p ? Math.max(0, Math.min(100, Math.round(p.progress))) : 0;
  const live = p && ACTIVE.has(p.phase) ? f1(p.live_speed_mbps) : null;
  let tone: Tone = "neutral";
  let word: string;
  let reason: ReactNode = null;
  if (!p && progress.error) {
    tone = "bad";
    word = t("speedtest.unreadable", "Can't read the speed test");
    reason = String(progress.error.message ?? progress.error);
  } else if (!p) {
    word = t("speedtest.reading", "Reading…");
  } else if (running) {
    tone = progress.stale ? "stale" : "neutral";
    word = `${ACTIVE.has(p.phase) ? phaseLabel(t, p.phase) : t("speedtest.starting", "Starting…")} · ${pct}%`;
    reason = live != null ? t("speedtest.liveNow", "Live: {{v}} Mbps", { v: live }) : null;
  } else if (p.phase === "complete") {
    tone = "ok";
    word = t("speedtest.phaseComplete", "Complete");
    reason = p.server ? t("speedtest.lastRunOn", "Last test, on {{server}}", { server: p.server }) : null;
  } else if (p.phase === "error") {
    tone = "bad";
    word = t("speedtest.failed", "Speed test failed");
    reason = t("speedtest.failedReason", "{{e}} · Check that mobile data is on, then start again.", {
      e: p.error || t("speedtest.noReason", "The device gave no reason"),
    });
  } else if (p.phase === "cancelled") {
    word = t("speedtest.phaseCancelled", "Cancelled");
    reason = t("speedtest.cancelledReason", "The test was stopped before it finished.");
  } else {
    word = t("speedtest.noResultYet", "No result yet");
    reason = t("speedtest.idleReason", "Pick a server below and press “Start”.");
  }

  // Readings: a finished phase shows its result; the phase under way shows
  // its live rate (marked "live").
  const haveResult = p && p.phase !== "idle";
  const loadingVals = !p && !progress.error;
  const dlLive = p?.phase === "download";
  const ulLive = p?.phase === "upload";
  const dl = loadingVals ? undefined : dlLive ? live : haveResult ? f1(p?.download_mbps) : null;
  const ul = loadingVals ? undefined : ulLive ? live : haveResult ? f1(p?.upload_mbps) : null;
  const ping = loadingVals ? undefined : haveResult ? f1(p?.ping_ms) : null;
  const jitter = loadingVals ? undefined : haveResult ? f1(p?.jitter_ms) : null;
  const liveWord = t("speedtest.live", "Live");
  const usedBytes = p ? p.download_bytes + p.upload_bytes : 0;

  const actionStop = t("speedtest.stop", "Stop");

  return (
    <div className="max-w-[720px]">
      <h1 className="nd-title mb-4 mt-2">{t("speedtest.title", "Speed Test")}</h1>

      <div className="grid gap-6">
        <section aria-label={t("speedtest.statusLabel", "Speed test status")}>
          <StatusBlock
            tone={tone}
            state={word}
            reason={reason}
            meta={
              running ? (
                <div className="grid gap-2">
                  <div
                    role="progressbar"
                    aria-label={t("speedtest.progress", "Progress")}
                    aria-valuemin={0}
                    aria-valuemax={100}
                    aria-valuenow={pct}
                    className="h-2 w-full overflow-hidden rounded-full bg-nd-track"
                  >
                    <div
                      className="h-full rounded-full transition-[width] duration-500"
                      style={{ width: `${pct}%`, background: "var(--nd-t1)" }}
                    />
                  </div>
                  <Freshness stale={progress.stale} lastOkAt={progress.lastOkAt} what={t("speedtest.progress", "Progress")} />
                </div>
              ) : !p && progress.error ? (
                <Button variant="secondary" size="sm" onPress={() => void progress.mutate()}>
                  <ArrowClockwise size={18} weight="bold" aria-hidden />
                  {t("common.retry", "Retry")}
                </Button>
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
                <Button
                  onPress={() => setConfirmStart((o) => !o)}
                  isDisabled={startOp.busy || servers.isLoading || (!p && !progress.error)}
                  pending={startOp.busy}
                  aria-describedby="st-cost"
                  {...startInline.triggerProps}
                >
                  <Play size={18} weight="fill" aria-hidden />
                  {t("speedtest.start", "Start")}
                </Button>
              )
            }
          />
          <ConfirmInline
            id={startInline.id}
            open={confirmStart && !running}
            actionLabel={t("speedtest.start", "Start")}
            consequence={t("speedtest.costNote", "Uses mobile data: one test downloads and uploads for about 10 seconds each — tens to hundreds of MB on a fast connection.")}
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
            consequence={t("speedtest.stopConsequence", "The test ends now; results so far are kept. You can start it again at any time.")}
            onCancel={() => setConfirmStop(false)}
            onConfirm={() => {
              setConfirmStop(false);
              stopOp.start();
              stopOp.confirm();
            }}
          />
          <div className="mt-2 grid gap-1 px-1">
            {!running && (
              <p id="st-cost" className="nd-aux">
                {t("speedtest.costNote", "Uses mobile data: one test downloads and uploads for about 10 seconds each — tens to hundreds of MB on a fast connection.")}
              </p>
            )}
            <OpResult op={startOp} />
            <OpResult op={stopOp} />
          </div>
        </section>

        <ReadoutWall cols={3} label={t("speedtest.results", "Results")}>
          <Readout
            full
            hero
            label={t("speedtest.download", "Download")}
            value={dl}
            unit="Mbps"
            sub={dlLive ? liveWord : undefined}
            stale={running && progress.stale}
          />
          <Readout label={t("speedtest.upload", "Upload")} value={ul} unit="Mbps" sub={ulLive ? liveWord : undefined} stale={running && progress.stale} />
          <Readout
            label={<>{t("speedtest.ping", "Ping")} <Help label={t("speedtest.ping", "Ping")} text={t("speedtest.pingHelp", "Round-trip time to the test server. Lower is better.")} /></>}
            value={ping}
            unit="ms"
            stale={running && progress.stale}
          />
          <Readout
            label={<>{t("speedtest.jitter", "Jitter")} <Help label={t("speedtest.jitter", "Jitter")} text={t("speedtest.jitterHelp", "How much the ping varies. Lower is steadier; matters for calls and games.")} /></>}
            value={jitter}
            unit="ms"
            stale={running && progress.stale}
          />
        </ReadoutWall>

        <section aria-labelledby="st-server">
          <GroupTitle id="st-server">{t("speedtest.server", "Server")}</GroupTitle>
          <div className="nd-group grid gap-2 p-4 lg:p-5">
            <label htmlFor="st-server-select" className="sr-only">
              {t("speedtest.server", "Server")}
            </label>
            <select
              id="st-server-select"
              className="nd-field"
              value={serverId}
              onChange={(e) => setServerId(e.target.value === "auto" ? "auto" : Number(e.target.value))}
              disabled={running || servers.isLoading}
            >
              <option value="auto">{t("speedtest.autoBestServer", "Auto (best server)")}</option>
              {(servers.data ?? []).map((s) => (
                <option key={s.id} value={s.id}>
                  {s.sponsor} — {s.name}, {s.country}
                </option>
              ))}
            </select>
            {servers.isLoading && <p className="nd-aux" role="status">{t("speedtest.loadingServers", "Getting the server list…")}</p>}
            {servers.error && (
              <div role="alert" className="flex flex-wrap items-center gap-3">
                <span className="nd-body text-nd-badT">
                  {t("speedtest.serversErr", "Couldn't get the server list: {{e}}. “Auto” still works if the device can reach the internet.", {
                    e: String(servers.error.message ?? servers.error),
                  })}
                </span>
                <Button variant="secondary" size="sm" onPress={() => void servers.mutate()} pending={servers.isValidating}>
                  {t("common.retry", "Retry")}
                </Button>
              </div>
            )}
          </div>
        </section>

        {haveResult && (
          <Group title={t("speedtest.details", "Details")} stale={running && progress.stale}>
            <Row label={t("speedtest.serverUsed", "Server used")} value={p?.server || "—"} />
            <Row
              label={t("speedtest.dataUsed", "Mobile data used")}
              sub={t("speedtest.dataUsedSplit", "Download {{d}} MB · upload {{u}} MB", { d: mb(p?.download_bytes ?? 0), u: mb(p?.upload_bytes ?? 0) })}
              value={`${mb(usedBytes)} MB`}
            />
          </Group>
        )}
      </div>
    </div>
  );
}
