"use client";
// Health (new design) — the device check (doctor.sh on the device, run by the
// agent every minute) and the crash logs kept by the process supervisor. The
// same check is `./install.sh doctor` in the install kit, so this page and SSH
// never disagree.
//
// Reads: plain GET /api/health (the agent's cached result) every 60 s, as
// before. "Check now" is GET /api/health?refresh=1, which runs doctor.sh on
// the device synchronously (up to ~20 s): tier 2 (controls-inventory
// §/health), only ever on a press — never on load, polling or a readback.
// Its readback is plain /api/health with a newer `checked_at`.
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { useWriteOp } from "@/lib/api/writeOp";
import { fmtDevice } from "@/lib/deviceClock";
import type { Health, HealthCheck, HealthCrashLog, HealthCrashLogText } from "@/lib/api/schemas/system";
import {
  Button,
  ConfirmInline,
  ConsoleBand,
  Freshness,
  GroupTitle,
  OpResult,
  StatusBlock,
  StatusMark,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

const LEVEL_TONE: Record<HealthCheck["level"], Tone> = { ok: "ok", warn: "warn", bad: "bad" };

type LogText = { state: "loading" } | { state: "error" } | { state: "ok"; text: string };

export default function HealthPage() {
  const { t } = useTranslation();
  const health = useApi<Health>("/api/health", { refreshInterval: 60000 });
  const data = health.data;

  // ── "Check now" (tier 2) ──
  const [asking, setAsking] = useState(false);
  const inline = useConfirmInline(asking);
  const beforeRef = useRef<number | null>(null);
  const op = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("health.recheck", "Check now"),
        run: async () => {
          // doctor.sh can take up to 20 s; the default GET limit is 8 s.
          const r = await apiFetch<Health>("/api/health?refresh=1", { timeoutMs: 30000 });
          await health.mutate(r, { revalidate: false });
          return r;
        },
      },
    ],
    // Plain /api/health only: a readback must never start another run.
    verify: async () => {
      const d = await apiFetch<Health>("/api/health");
      await health.mutate(d, { revalidate: false });
      const before = beforeRef.current;
      // checked_at moves on every run, even one that fails (health.rs refresh).
      return d.checked_at != null && (before == null || d.checked_at > before);
    },
  });

  function runCheck() {
    beforeRef.current = data?.checked_at ?? null;
    op.start();
    op.confirm();
    setAsking(false);
  }

  // ── crash logs: text kept per row, so a slow reply can't land in another row ──
  const [open, setOpen] = useState<string | null>(null);
  const [texts, setTexts] = useState<Record<string, LogText>>({});
  async function toggle(c: HealthCrashLog) {
    const key = `${c.program}/${c.file}`;
    if (open === key) {
      setOpen(null);
      return;
    }
    setOpen(key);
    if (texts[key]?.state === "ok") return;
    setTexts((m) => ({ ...m, [key]: { state: "loading" } }));
    try {
      const r = await apiFetch<HealthCrashLogText>(
        `/api/health/crashlog?program=${encodeURIComponent(c.program)}&file=${encodeURIComponent(c.file)}`,
      );
      setTexts((m) => ({ ...m, [key]: { state: "ok", text: r.text } }));
    } catch {
      setTexts((m) => ({ ...m, [key]: { state: "error" } }));
    }
  }

  // ── status ──
  let tone: Tone = "neutral";
  let state: string = t("health.reading", "Reading the device check…");
  let reason: string | null = null;
  if (!data && health.error) {
    tone = "bad";
    state = t("health.unreadable", "Can't read the device check");
    reason = health.error.message;
  } else if (data) {
    if (data.error) {
      tone = "warn";
      state = t("health.couldNotRun", "The check could not run");
      reason = t("health.noDoctor", "The check could not run: {{e}}", { e: data.error });
    } else if (data.checked_at == null) {
      tone = "neutral";
      state = t("health.notYetState", "Not checked yet");
      reason = t("health.notYet", "The first check runs about 20 seconds after the admin backend starts.");
    } else if (data.bad > 0) {
      tone = "bad";
      state = t("health.counts", "{{bad}} problem(s), {{warn}} to look at.", { bad: data.bad, warn: data.warn });
    } else if (data.warn > 0) {
      tone = "warn";
      state = t("health.counts", "{{bad}} problem(s), {{warn}} to look at.", { bad: data.bad, warn: data.warn });
    } else {
      tone = "ok";
      state = t("health.allGood", "Everything is fine.");
    }
    if (health.stale) tone = "stale";
  }

  const checkedAt =
    data?.checked_at != null ? t("health.checkedAt", "checked {{time}}", { time: fmtDevice(data.checked_at, "time") }) : null;
  const done = op.phase === "applied" || op.phase === "accepted";

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("health.title", "Health")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">
        {t(
          "health.desc",
          "A read-only check of the device, rerun every minute. The same check runs over SSH as ./install.sh doctor, even when this admin backend is down.",
        )}
      </p>

      <div className="grid max-w-[720px] gap-6">
        <section>
          <StatusBlock
            tone={tone}
            state={state}
            reason={reason}
            meta={
              <>
                {checkedAt && <span className="tabular-nums">{checkedAt}</span>}
                {data && health.stale && (
                  <>
                    {checkedAt && " · "}
                    <Freshness stale lastOkAt={health.lastOkAt} />
                  </>
                )}
              </>
            }
            actions={
              !data && health.error ? (
                <Button variant="secondary" size="sm" onPress={() => health.mutate()}>
                  {t("common.retry", "Retry")}
                </Button>
              ) : (
                <Button
                  {...inline.triggerProps}
                  variant="secondary"
                  size="sm"
                  pending={op.busy}
                  isDisabled={op.busy}
                  onPress={() => setAsking((a) => !a)}
                >
                  {t("health.recheck", "Check now")}
                </Button>
              )
            }
          />
          <ConfirmInline
            id={inline.id}
            open={asking}
            actionLabel={t("health.runCheck", "run the check")}
            consequence={t(
              "health.runConsequence",
              "Runs the health check script (doctor.sh) on the device now. It takes up to about 20 seconds.",
            )}
            onCancel={() => setAsking(false)}
            onConfirm={runCheck}
          />
          {op.phase !== "idle" && op.phase !== "confirming" && (
            <div className="mt-2 px-1">
              {done ? (
                <p role="status">
                  <StatusMark tone="ok">
                    {data?.checked_at != null
                      ? t("health.checkDoneAt", "Check finished at {{time}}", { time: fmtDevice(data.checked_at, "time") })
                      : t("health.checkDone", "Check finished")}
                  </StatusMark>
                </p>
              ) : (
                <OpResult op={op} />
              )}
            </div>
          )}
        </section>

        <section aria-labelledby="health-checks">
          <GroupTitle id="health-checks">{t("health.checksTitle", "Checks")}</GroupTitle>
          <div className={`nd-group${health.stale ? " nd-stale" : ""}`}>
            {!data ? (
              [0, 1, 2].map((i) => (
                <div key={i} className="nd-row">
                  <span className="nd-skel" style={{ width: "16ch" }} />
                </div>
              ))
            ) : data.checks.length === 0 ? (
              <div className="nd-row">
                <span className="nd-aux">
                  {data.error || data.checked_at == null
                    ? t("health.noChecksYet", "No results yet. Press \"Check now\" to run the check.")
                    : t("health.noChecks", "The check returned no items.")}
                </span>
              </div>
            ) : (
              data.checks.map((c) => (
                <div key={c.id} className="nd-row nd-row--two items-start">
                  <span className="nd-row__text">
                    <span className="nd-row__label">{c.label}</span>
                    <span className="nd-row__sub block break-words">{c.detail}</span>
                  </span>
                  <span className="shrink-0 text-[14px]">
                    <StatusMark tone={LEVEL_TONE[c.level]}>{levelWord(t, c.level)}</StatusMark>
                  </span>
                </div>
              ))
            )}
          </div>
        </section>

        <section aria-labelledby="health-crashes">
          <GroupTitle id="health-crashes">{t("health.crashTitle", "Crash logs")}</GroupTitle>
          {!data ? (
            <div className="nd-group">
              {[0, 1, 2].map((i) => (
                <div key={i} className="nd-row">
                  <span className="nd-skel" style={{ width: "20ch" }} />
                </div>
              ))}
            </div>
          ) : data.crashlogs.length === 0 ? (
            <p className="nd-body px-1">
              <StatusMark tone="ok">
                {t(
                  "health.noCrashes",
                  "No crashes recorded. When a supervised program dies, its last output is kept here (5 per program).",
                )}
              </StatusMark>
            </p>
          ) : (
            <div className={`nd-group [&>*+*]:border-t [&>*+*]:border-[color:var(--nd-sep)]${health.stale ? " nd-stale" : ""}`}>
              {data.crashlogs.map((c) => {
                const key = `${c.program}/${c.file}`;
                const isOpen = open === key;
                const txt = texts[key];
                const when = fmtDevice(c.time);
                const panelId = `crash-${key.replace(/[^a-zA-Z0-9_-]/g, "_")}`;
                return (
                  <div key={key}>
                    <button
                      type="button"
                      className="nd-row nd-row--two"
                      onClick={() => toggle(c)}
                      aria-expanded={isOpen}
                      aria-controls={panelId}
                      aria-label={
                        isOpen
                          ? t("health.hideLogOf", "Hide the crash log of {{program}} at {{time}}", { program: c.program, time: when })
                          : t("health.showLogOf", "Show the crash log of {{program}} at {{time}}", { program: c.program, time: when })
                      }
                    >
                      <span className="nd-row__text">
                        <span className="nd-row__label">{c.program}</span>
                        <span className="nd-row__sub block">
                          <span className="nd-mono">{when}</span>
                          {c.status && <> · {c.status}</>}
                        </span>
                      </span>
                      <span className="nd-aux font-medium text-nd-t2">
                        {isOpen ? t("health.hide", "Hide") : t("health.show", "Show")}
                      </span>
                    </button>
                    {isOpen && (
                      <div id={panelId} className="px-3 pb-3">
                        <ConsoleBand label={t("health.logOf", "Crash log of {{program}}", { program: c.program })}>
                          {!txt || txt.state === "loading" ? (
                            <p className="nd-console__muted" role="status">{t("health.loading", "Loading…")}</p>
                          ) : txt.state === "error" ? (
                            <p role="alert">{t("health.loadFailed", "Could not load this log.")}</p>
                          ) : (
                            <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words text-[12px] leading-relaxed">
                              {txt.text}
                            </pre>
                          )}
                        </ConsoleBand>
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </section>
      </div>
    </>
  );
}

function levelWord(t: (k: string, d: string) => string, level: HealthCheck["level"]): string {
  switch (level) {
    case "ok":
      return t("health.levelOk", "OK");
    case "warn":
      return t("health.levelWarn", "Needs a look");
    default:
      return t("health.levelBad", "Problem");
  }
}
