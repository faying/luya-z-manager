"use client";
// Alerts (new design) — what failed while nobody was watching, and where the
// SMS about it goes. The events come from the device's process supervisor and
// Wi-Fi watchdog (u60-guard), which also sends the SMS, so both keep working
// when this admin backend is the thing that died. See docs/RELIABILITY.md.
//
// Writes (controls-inventory §/alerts, design §3.1):
// - Mark all as read: tier 1, POST /api/alerts/read {seq}; readback
//   /api/alerts `read` >= seq (the agent clamps the cursor; a newer event
//   arriving meanwhile must not read as a failure). /api/public/status is
//   revalidated too (its unread count feeds the shell).
// - SMS number: tier 2, PUT /api/alerts/sms {number} ("" = off); readback
//   `sms.number` / `sms.configured`, compared after the agent's own
//   normalisation (trim, drop spaces and dashes).
// - Abroad switch: tier 2, PUT /api/alerts/sms {abroad}; readback
//   `sms.abroad_allowed`.
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { mutate as globalMutate } from "swr";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { useWriteOp } from "@/lib/api/writeOp";
import { ALERTS_PATH, eventTime, kindLabel, smsResultLabel } from "@/lib/alerts";
import type { AlertsData } from "@/lib/api/schemas/system";
import {
  Button,
  ConfirmInline,
  Freshness,
  GroupTitle,
  OpResult,
  Row,
  StatusBlock,
  StatusMark,
  Switch,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

/** Same normalisation as alerts.rs alerts_sms_set. */
const normNumber = (s: string) => s.trim().replace(/[ -]/g, "");
/** alerts.rs valid_number: optional +, ≥ 3 digits, ≤ 20 characters. */
function numberProblem(n: string): boolean {
  if (n === "") return false;
  const digits = n.startsWith("+") ? n.slice(1) : n;
  return n.length > 20 || digits.length < 3 || !/^\d+$/.test(digits);
}

function smsTone(result: string): Tone {
  if (result === "sent") return "ok";
  if (result === "failed") return "bad";
  return "neutral";
}

function Skeleton({ rows = 3, width = "18ch" }: { rows?: number; width?: string }) {
  return (
    <div className="nd-group">
      {Array.from({ length: rows }, (_, i) => (
        <div key={i} className="nd-row">
          <span className="nd-skel" style={{ width }} />
        </div>
      ))}
    </div>
  );
}

export default function AlertsPage() {
  const { t } = useTranslation();
  const alerts = useApi<AlertsData>(ALERTS_PATH, { refreshInterval: 30000 });
  const data = alerts.data;
  const sms = data?.sms;

  // ── mark all as read (tier 1) ──
  const readSeqRef = useRef(0);
  const readOp = useWriteOp({
    tier: 1,
    steps: [
      {
        label: t("alerts.markRead", "Mark all as read"),
        run: () => apiFetch("/api/alerts/read", { method: "POST", body: { seq: readSeqRef.current } }),
      },
    ],
    verify: async () => {
      const d = await apiFetch<AlertsData>(ALERTS_PATH);
      await alerts.mutate(d, { revalidate: false });
      globalMutate("/api/public/status");
      return d.read >= readSeqRef.current;
    },
  });
  function markAllRead() {
    const newest = data?.events[0]?.seq;
    if (newest == null) return;
    readSeqRef.current = newest;
    readOp.start();
  }

  // ── SMS number (tier 2) ──
  const saved = sms?.number ?? "";
  // null = untouched: the field follows the saved number.
  const [edit, setNumber] = useState<string | null>(null);
  const number = edit ?? saved;
  const draft = normNumber(number);
  const badNumber = numberProblem(draft);
  const numberRef = useRef("");
  const [numberSent, setNumberSent] = useState("");
  const [askNumber, setAskNumber] = useState(false);
  const numberInline = useConfirmInline(askNumber);
  const numberOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("alerts.smsNumber", "Phone number"),
        run: () => apiFetch("/api/alerts/sms", { method: "PUT", body: { number: numberRef.current } }),
      },
    ],
    verify: async () => {
      const d = await apiFetch<AlertsData>(ALERTS_PATH);
      await alerts.mutate(d, { revalidate: false });
      const want = numberRef.current;
      return want === "" ? !d.sms.configured : d.sms.configured && d.sms.number === want;
    },
  });
  function confirmNumber() {
    numberRef.current = draft;
    setNumberSent(draft);
    numberOp.start();
    numberOp.confirm();
    setAskNumber(false);
  }

  // ── abroad switch (tier 2) ──
  const [askAbroad, setAskAbroad] = useState<boolean | null>(null);
  const abroadInline = useConfirmInline(askAbroad !== null);
  const abroadRef = useRef(false);
  const [abroadSent, setAbroadSent] = useState(false);
  const abroadOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("alerts.smsAbroad", "Also send while abroad (may cost roaming fees)"),
        run: () => apiFetch("/api/alerts/sms", { method: "PUT", body: { abroad: abroadRef.current } }),
      },
    ],
    verify: async () => {
      const d = await apiFetch<AlertsData>(ALERTS_PATH);
      await alerts.mutate(d, { revalidate: false });
      return d.sms.abroad_allowed === abroadRef.current;
    },
  });
  function confirmAbroad() {
    if (askAbroad === null) return;
    abroadRef.current = askAbroad;
    setAbroadSent(askAbroad);
    abroadOp.start();
    abroadOp.confirm();
    setAskAbroad(null);
  }

  const smsBusy = numberOp.busy || abroadOp.busy;
  const locked = !sms || alerts.stale || smsBusy;

  // ── status ──
  let tone: Tone = "neutral";
  let state: string = t("alerts.reading", "Reading alerts…");
  let reason: string | null = null;
  if (!data && alerts.error) {
    tone = "bad";
    state = t("alerts.unreadable", "Can't read alerts");
    reason = alerts.error.message;
  } else if (data) {
    if (data.unread > 0) {
      tone = "warn";
      state = t("alerts.bannerCount", "{{n}} new alert(s)", { n: data.unread });
      reason = data.events[0] ? kindLabel(t, data.events[0].kind) : null;
    } else if (data.events.length === 0) {
      tone = "ok";
      state = t("alerts.noneState", "No alerts");
      reason = t("alerts.noneReason", "The device is fine.");
    } else {
      tone = "ok";
      state = t("alerts.allRead", "No new alerts");
    }
    if (alerts.stale) tone = "stale";
  }

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("alerts.title", "Alerts")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">
        {t(
          "alerts.desc",
          "Crashes, restarts and Wi-Fi rescues on the device. Recorded by a watchdog that keeps running even when this admin backend is down.",
        )}
      </p>

      <div className="grid max-w-[720px] gap-6">
        <section>
          <StatusBlock
            tone={tone}
            state={state}
            reason={reason}
            meta={data && alerts.stale ? <Freshness stale lastOkAt={alerts.lastOkAt} what={t("alerts.listWord", "List")} /> : undefined}
            actions={
              !data && alerts.error ? (
                <Button variant="secondary" size="sm" onPress={() => alerts.mutate()}>
                  {t("common.retry", "Retry")}
                </Button>
              ) : data && data.unread > 0 ? (
                <Button variant="secondary" size="sm" pending={readOp.busy} isDisabled={readOp.busy} onPress={markAllRead}>
                  {t("alerts.markRead", "Mark all as read")}
                </Button>
              ) : undefined
            }
          />
          {readOp.phase !== "idle" && (
            <div className="mt-2 px-1">
              <OpResult op={readOp} />
            </div>
          )}
        </section>

        <section aria-labelledby="alerts-events">
          <GroupTitle id="alerts-events">{t("alerts.eventsTitle", "Events")}</GroupTitle>
          {data && alerts.error && (
            <p className="nd-aux mb-2 flex flex-wrap items-center gap-2 px-1" role="alert">
              <StatusMark tone="warn">{t("alerts.refreshFailed", "Couldn't refresh the list; showing the last one.")}</StatusMark>
              <Button variant="secondary" size="sm" onPress={() => alerts.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            </p>
          )}
          {!data ? (
            alerts.error ? null : <Skeleton />
          ) : data.events.length === 0 ? (
            <p className="nd-body px-1">
              <StatusMark tone="ok">
                {t(
                  "alerts.empty",
                  "Nothing has gone wrong. If a program crashes or the watchdog has to turn Wi-Fi back on, it shows up here.",
                )}
              </StatusMark>
            </p>
          ) : (
            <ul className={`nd-group${alerts.stale ? " nd-stale" : ""}`}>
              {data.events.map((e) => (
                <li key={e.seq} className="nd-row nd-row--two items-start">
                  <span className="nd-row__text">
                    <span className={e.unread ? "nd-row__label" : "font-normal text-nd-t1"}>
                      {e.unread && (
                        <span className="text-nd-warnT">
                          <span aria-hidden>▲ </span>
                          <span className="sr-only">{t("alerts.unread", "unread")}: </span>
                        </span>
                      )}
                      {kindLabel(t, e.kind)}
                    </span>
                    {e.text && <span className="nd-row__sub block break-words">{e.text}</span>}
                  </span>
                  <span className={`shrink-0 text-[14px] text-nd-t3${e.time != null ? " nd-mono" : ""}`}>{eventTime(t, e)}</span>
                </li>
              ))}
            </ul>
          )}
          {data && !data.clock_set && (
            <p className="nd-aux mt-2 px-1">
              {t("alerts.clockNotSet", "The device clock is not set yet, so times are shown relative to boot.")}
            </p>
          )}
        </section>

        <section aria-labelledby="alerts-sms">
          <GroupTitle id="alerts-sms">{t("alerts.smsTitle", "SMS alerts")}</GroupTitle>
          <p className="nd-body mb-3 px-1">
            {!sms ? (
              alerts.error ? null : <span className="nd-skel" style={{ width: "24ch" }} />
            ) : sms.configured ? (
              <StatusMark tone="ok">
                {t("alerts.smsOn", "On — sent from this device's SIM to {{n}}.", { n: sms.number })}
              </StatusMark>
            ) : (
              <StatusMark tone="warn">
                {t("alerts.smsOff", "Not set up. Alerts are only shown here and on the touch screen until you add a number.")}
              </StatusMark>
            )}
          </p>

          <div className={`nd-group${alerts.stale ? " nd-stale" : ""}`}>
            <form
              className="nd-row flex-col items-stretch gap-2"
              onSubmit={(ev) => {
                ev.preventDefault();
                if (locked || badNumber || draft === saved) return;
                setAskNumber(true);
              }}
            >
              <label htmlFor="alerts-sms-number" className="nd-row__label">
                {t("alerts.smsNumber", "Phone number")}
              </label>
              <div className="flex flex-wrap items-center gap-2">
                <input
                  id="alerts-sms-number"
                  className="nd-field nd-mono min-w-0 flex-1 sm:max-w-[280px]"
                  value={number}
                  onChange={(e) => setNumber(e.target.value)}
                  inputMode="tel"
                  autoComplete="tel"
                  placeholder="+8613xxxxxxxxx"
                  disabled={!sms || smsBusy}
                  aria-invalid={badNumber || undefined}
                  aria-describedby="alerts-sms-limits alerts-sms-number-err"
                />
                <Button
                  {...numberInline.triggerProps}
                  type="submit"
                  pending={numberOp.busy}
                  isDisabled={locked || badNumber || draft === saved}
                >
                  {t("alerts.smsSave", "Save")}
                </Button>
              </div>
              <span id="alerts-sms-number-err" className="nd-aux" role={badNumber ? "alert" : undefined}>
                {badNumber
                  ? t("alerts.smsNumberBad", "Digits only, an optional leading +, 3 to 20 characters.")
                  : null}
              </span>
              <span id="alerts-sms-limits" className="nd-row__sub">
                {t(
                  "alerts.smsLimits",
                  "At most one SMS per kind of alert per hour and five a day; anything over that is only recorded here. The message says what failed and when — nothing else. Leave empty to turn off.",
                )}
              </span>
            </form>
            <Row
              label={t("alerts.smsAbroad", "Also send while abroad (may cost roaming fees)")}
              sub={
                sms?.abroad_now && !sms.abroad_allowed
                  ? t("alerts.smsAbroadNow", "The device is in an abroad scenario, so SMS alerts are held back right now.")
                  : undefined
              }
              control={
                <span {...(askAbroad !== null ? abroadInline.triggerProps : {})}>
                  <Switch
                    label={t("alerts.smsAbroad", "Also send while abroad (may cost roaming fees)")}
                    isSelected={abroadOp.busy ? abroadSent : !!sms?.abroad_allowed}
                    isDisabled={locked}
                    onChange={(v) => setAskAbroad(v)}
                  />
                </span>
              }
            />
          </div>

          <ConfirmInline
            id={numberInline.id}
            open={askNumber}
            actionLabel={draft === "" ? t("alerts.turnSmsOff", "turn SMS alerts off") : t("alerts.saveNumberAction", "save the number")}
            consequence={
              draft === ""
                ? t("alerts.cNumberOff", "No more alert SMS. Alerts are still recorded here and on the touch screen.")
                : t("alerts.cNumberOn", "Alerts will be texted from this device's SIM to {{n}}.", { n: draft })
            }
            onCancel={() => setAskNumber(false)}
            onConfirm={confirmNumber}
          />
          <ConfirmInline
            id={abroadInline.id}
            open={askAbroad !== null}
            actionLabel={askAbroad ? t("alerts.abroadOnAction", "send while abroad") : t("alerts.abroadOffAction", "hold SMS while abroad")}
            consequence={
              askAbroad
                ? t("alerts.cAbroadOn", "Alert SMS will also go out while the device is abroad; each may cost a roaming fee.")
                : t("alerts.cAbroadOff", "While the device is abroad, alerts are only recorded here, not texted.")
            }
            onCancel={() => setAskAbroad(null)}
            onConfirm={confirmAbroad}
          />
          {numberOp.phase !== "idle" && numberOp.phase !== "confirming" && (
            <div className="mt-2 grid gap-1 px-1">
              <span className="nd-aux">
                {numberSent === "" ? t("alerts.smsCleared", "SMS alerts turned off") : t("alerts.smsSaved", "Number saved")}
              </span>
              <OpResult op={numberOp} />
            </div>
          )}
          {abroadOp.phase !== "idle" && abroadOp.phase !== "confirming" && (
            <div className="mt-2 grid gap-1 px-1">
              <span className="nd-aux">
                {abroadSent ? t("alerts.smsAbroadOn", "Will also send abroad") : t("alerts.smsAbroadOff", "Will not send abroad")}
              </span>
              <OpResult op={abroadOp} />
            </div>
          )}
          {alerts.stale && data && (
            <p className="nd-aux mt-2 px-1">
              <Freshness stale lastOkAt={alerts.lastOkAt} what={t("alerts.settingsWord", "Settings")} />
              {t("alerts.refreshToEdit", " — refresh before changing anything.")}
            </p>
          )}

          {sms && sms.recent.length > 0 && (
            <div className="mt-6">
              <GroupTitle>{t("alerts.smsRecentTitle", "Recent SMS · {{n}} sent in the last 24 h", { n: sms.sent_24h })}</GroupTitle>
              <ul className={`nd-group${alerts.stale ? " nd-stale" : ""}`}>
                {sms.recent.map((r) => (
                  <li key={`${r.seq}-${r.time}-${r.result}`} className="nd-row nd-row--two items-start">
                    <span className="nd-row__text">
                      <span className="block">{kindLabel(t, r.kind)}</span>
                      <span className="nd-row__sub block nd-mono">
                        {/* Before 2024-01-01 the clock was not set; this log has no uptime to fall back on. */}
                        {r.time >= 1704067200 ? eventTime(t, { time: r.time, uptime: 0 }) : "—"}
                      </span>
                    </span>
                    <span className="shrink-0 text-[14px]">
                      <StatusMark tone={smsTone(r.result)}>{smsResultLabel(t, r.result)}</StatusMark>
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </section>
      </div>
    </>
  );
}
