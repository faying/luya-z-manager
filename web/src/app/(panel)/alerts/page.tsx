"use client";

// Alerts — what failed while nobody was watching, and where the SMS about it
// goes. The events come from the device's process supervisor and Wi-Fi
// watchdog (u60-guard), which also sends the SMS, so both keep working when
// this admin backend is the thing that died. See docs/RELIABILITY.md.

import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { mutate as globalMutate } from "swr";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { ALERTS_PATH, AlertsData, eventTime, kindLabel, smsResultLabel } from "@/lib/alerts";
import { PageHeader, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Input, Toggle } from "@/components/admin/Button";

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section>
      <h3 className="text-xs font-semibold uppercase tracking-[0.04em] text-text-dim">{title}</h3>
      <hr className="mb-3 mt-2 border-[color:var(--admin-divider-light)]" />
      {children}
    </section>
  );
}

export default function AlertsPage() {
  const { t } = useTranslation();
  const { data, error, mutate } = useApi<AlertsData>(ALERTS_PATH, { refreshInterval: 30000 });
  const [number, setNumber] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);

  const saved = data?.sms.number ?? "";
  useEffect(() => setNumber(saved), [saved]);

  async function markAllRead() {
    if (!data?.events[0]) return;
    setBusy(true);
    try {
      await apiFetch("/api/alerts/read", { method: "POST", body: { seq: data.events[0].seq } });
      await mutate();
      globalMutate("/api/public/status");
    } finally {
      setBusy(false);
    }
  }

  async function saveSms(body: { number?: string; abroad?: boolean }, okText: string) {
    setBusy(true);
    setMsg(null);
    try {
      await apiFetch("/api/alerts/sms", { method: "PUT", body });
      await mutate();
      setMsg({ text: okText, err: false });
    } catch (e) {
      setMsg({ text: e instanceof ApiError ? e.message : t("alerts.saveFailed", "Could not save"), err: true });
    } finally {
      setBusy(false);
    }
  }

  const sms = data?.sms;

  return (
    <div className="mx-auto max-w-[720px]">
      <PageHeader
        title={t("alerts.title", "Alerts")}
        description={t(
          "alerts.desc",
          "Crashes, restarts and Wi-Fi rescues on the device. Recorded by a watchdog that keeps running even when this admin backend is down.",
        )}
      />

      {error && <ErrorBanner message={error instanceof Error ? error.message : String(error)} />}
      {msg && (
        <div
          className={`mb-4 rounded-md border px-3 py-2 text-sm ${
            msg.err ? "border-error/40 bg-error/10 text-error" : "border-success/40 bg-success/10 text-success"
          }`}
        >
          {msg.text}
        </div>
      )}

      <div className="grid gap-10">
        <Section title={t("alerts.eventsTitle", "Events")}>
          {data && data.events.length === 0 ? (
            <p className="text-sm text-text-dim">
              <span className="text-success" aria-hidden>● </span>
              {t(
                "alerts.empty",
                "Nothing has gone wrong. If a program crashes or the watchdog has to turn Wi-Fi back on, it shows up here.",
              )}
            </p>
          ) : (
            <>
              <ul className="divide-y divide-[color:var(--admin-divider-light)]">
                {data?.events.map((e) => (
                  <li key={e.seq} className="flex gap-3 py-2.5 text-sm">
                    <span className="w-24 shrink-0 tabular-nums text-text-dim">{eventTime(t, e)}</span>
                    <div className="min-w-0 flex-1">
                      <p className={e.unread ? "font-medium" : ""}>
                        {e.unread && (
                          <span className="text-warning" aria-label={t("alerts.unread", "unread")}>
                            ▲{" "}
                          </span>
                        )}
                        {kindLabel(t, e.kind)}
                      </p>
                      {e.text && <p className="mt-0.5 break-words text-text-dim">{e.text}</p>}
                    </div>
                  </li>
                ))}
              </ul>
              {!!data?.unread && (
                <div className="mt-3">
                  <Button size="sm" variant="outline" onClick={markAllRead} loading={busy}>
                    {t("alerts.markRead", "Mark all as read")}
                  </Button>
                </div>
              )}
              {data && !data.clock_set && (
                <p className="mt-3 text-xs text-text-dim">
                  {t("alerts.clockNotSet", "The device clock is not set yet, so times are shown relative to boot.")}
                </p>
              )}
            </>
          )}
        </Section>

        <Section title={t("alerts.smsTitle", "SMS alerts")}>
          <p className="mb-3 text-sm">
            {sms?.configured ? (
              <>
                <span className="text-success" aria-hidden>● </span>
                {t("alerts.smsOn", "On — sent from this device's SIM to {{n}}.", { n: sms.number })}
              </>
            ) : (
              <>
                <span className="text-warning" aria-hidden>▲ </span>
                {t(
                  "alerts.smsOff",
                  "Not set up. Alerts are only shown here and on the touch screen until you add a number.",
                )}
              </>
            )}
          </p>

          <form
            className="flex flex-wrap items-end gap-2"
            onSubmit={(ev) => {
              ev.preventDefault();
              saveSms({ number }, number.trim() ? t("alerts.smsSaved", "Number saved") : t("alerts.smsCleared", "SMS alerts turned off"));
            }}
          >
            <label className="grid gap-1 text-xs text-text-dim">
              {t("alerts.smsNumber", "Phone number")}
              <Input
                value={number}
                onChange={(e) => setNumber(e.target.value)}
                inputMode="tel"
                autoComplete="tel"
                placeholder="+8613xxxxxxxxx"
                className="w-56"
              />
            </label>
            <Button type="submit" size="sm" loading={busy} disabled={number.trim() === saved}>
              {t("alerts.smsSave", "Save")}
            </Button>
          </form>
          <p className="mt-2 text-xs text-text-dim">
            {t(
              "alerts.smsLimits",
              "At most one SMS per kind of alert per hour and five a day; anything over that is only recorded here. The message says what failed and when — nothing else. Leave empty to turn off.",
            )}
          </p>

          <div className="mt-4">
            <Toggle
              checked={!!sms?.abroad_allowed}
              onChange={(v) =>
                saveSms(
                  { abroad: v },
                  v ? t("alerts.smsAbroadOn", "Will also send abroad") : t("alerts.smsAbroadOff", "Will not send abroad"),
                )
              }
              disabled={busy || !sms}
              label={t("alerts.smsAbroad", "Also send while abroad (may cost roaming fees)")}
            />
            {sms?.abroad_now && !sms.abroad_allowed && (
              <p className="mt-1 text-xs text-text-dim">
                {t("alerts.smsAbroadNow", "The device is in an abroad scenario, so SMS alerts are held back right now.")}
              </p>
            )}
          </div>

          {sms && sms.recent.length > 0 && (
            <div className="mt-6">
              <p className="text-xs text-text-dim">
                {t("alerts.smsRecentTitle", "Recent SMS · {{n}} sent in the last 24 h", { n: sms.sent_24h })}
              </p>
              <ul className="mt-1 divide-y divide-[color:var(--admin-divider-light)]">
                {sms.recent.map((r) => (
                  <li key={`${r.seq}-${r.time}-${r.result}`} className="flex gap-3 py-2 text-sm">
                    <span className="w-24 shrink-0 tabular-nums text-text-dim">
                      {eventTime(t, { time: r.time >= 1704067200 ? r.time : null, uptime: 0 })}
                    </span>
                    <span className="min-w-0 flex-1">{kindLabel(t, r.kind)}</span>
                    <span className={r.result === "sent" ? "text-success" : r.result === "failed" ? "text-error" : "text-text-dim"}>
                      {smsResultLabel(t, r.result)}
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </Section>
      </div>
    </div>
  );
}
