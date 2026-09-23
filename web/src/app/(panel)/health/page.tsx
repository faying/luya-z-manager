"use client";

// Health — the device check (doctor.sh on the device, run by the agent every
// minute) and the crash logs kept by the process supervisor. The same check is
// `./install.sh doctor` in the install kit, so this page and SSH never disagree.

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { fmtDevice } from "@/lib/deviceClock";
import { PageHeader, ErrorBanner } from "@/components/admin/StatCard";
import { Button } from "@/components/admin/Button";

interface Check {
  level: "ok" | "warn" | "bad";
  id: string;
  label: string;
  detail: string;
}
interface CrashLog {
  program: string;
  file: string;
  time: number;
  size: number;
  status: string;
}
interface Health {
  checks: Check[];
  bad: number;
  warn: number;
  checked_at: number | null;
  error: string | null;
  crashlogs: CrashLog[];
}

const SYMBOL: Record<Check["level"], { s: string; cls: string }> = {
  ok: { s: "●", cls: "text-success" },
  warn: { s: "▲", cls: "text-warning" },
  bad: { s: "■", cls: "text-error" },
};

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section>
      <h3 className="text-xs font-semibold uppercase tracking-[0.04em] text-text-dim">{title}</h3>
      <hr className="mb-3 mt-2 border-[color:var(--admin-divider-light)]" />
      {children}
    </section>
  );
}

export default function HealthPage() {
  const { t } = useTranslation();
  const { data, error, mutate } = useApi<Health>("/api/health", { refreshInterval: 60000 });
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const [text, setText] = useState<string>("");

  async function recheck() {
    setBusy(true);
    try {
      await mutate(await apiFetch<Health>("/api/health?refresh=1"), { revalidate: false });
    } finally {
      setBusy(false);
    }
  }

  async function toggle(c: CrashLog) {
    const key = `${c.program}/${c.file}`;
    if (open === key) {
      setOpen(null);
      return;
    }
    setOpen(key);
    setText(t("health.loading", "Loading…"));
    try {
      const r = await apiFetch<{ text: string }>(
        `/api/health/crashlog?program=${encodeURIComponent(c.program)}&file=${encodeURIComponent(c.file)}`,
      );
      setText(r.text);
    } catch {
      setText(t("health.loadFailed", "Could not load this log."));
    }
  }

  const summary = !data
    ? ""
    : data.bad === 0 && data.warn === 0
      ? t("health.allGood", "Everything is fine.")
      : t("health.counts", "{{bad}} problem(s), {{warn}} to look at.", { bad: data.bad, warn: data.warn });

  return (
    <div className="mx-auto max-w-[720px]">
      <PageHeader
        title={t("health.title", "Health")}
        description={t(
          "health.desc",
          "A read-only check of the device, rerun every minute. The same check runs over SSH as ./install.sh doctor, even when this admin backend is down.",
        )}
        actions={
          <Button size="sm" variant="outline" onClick={recheck} loading={busy}>
            {t("health.recheck", "Check now")}
          </Button>
        }
      />
      {error && <ErrorBanner message={error instanceof Error ? error.message : String(error)} />}

      <div className="grid gap-10">
        <Section title={t("health.checksTitle", "Checks")}>
          {data?.error ? (
            <p className="text-sm text-text-dim">
              <span className="text-warning" aria-hidden>▲ </span>
              {t("health.noDoctor", "The check could not run: {{e}}", { e: data.error })}
            </p>
          ) : data && data.checked_at == null ? (
            <p className="text-sm text-text-dim">{t("health.notYet", "The first check runs about 20 seconds after the admin backend starts.")}</p>
          ) : (
            <>
              <p className="mb-2 text-sm">
                <span className={data && data.bad ? "text-error" : data && data.warn ? "text-warning" : "text-success"} aria-hidden>
                  {data && data.bad ? "■ " : data && data.warn ? "▲ " : "● "}
                </span>
                {summary}
                {data?.checked_at != null && (
                  <span className="ml-2 text-xs text-text-dim">
                    {t("health.checkedAt", "checked {{time}}", { time: fmtDevice(data.checked_at, "time") })}
                  </span>
                )}
              </p>
              <ul className="divide-y divide-[color:var(--admin-divider-light)]">
                {data?.checks.map((c) => (
                  <li key={c.id} className="flex gap-3 py-2 text-sm">
                    <span className={`w-4 shrink-0 ${SYMBOL[c.level].cls}`} aria-label={c.level}>
                      {SYMBOL[c.level].s}
                    </span>
                    <span className="w-36 shrink-0 font-medium">{c.label}</span>
                    <span className="min-w-0 flex-1 break-words text-text-dim">{c.detail}</span>
                  </li>
                ))}
              </ul>
            </>
          )}
        </Section>

        <Section title={t("health.crashTitle", "Crash logs")}>
          {data && data.crashlogs.length === 0 ? (
            <p className="text-sm text-text-dim">
              <span className="text-success" aria-hidden>● </span>
              {t("health.noCrashes", "No crashes recorded. When a supervised program dies, its last output is kept here (5 per program).")}
            </p>
          ) : (
            <ul className="divide-y divide-[color:var(--admin-divider-light)]">
              {data?.crashlogs.map((c) => {
                const key = `${c.program}/${c.file}`;
                return (
                  <li key={key} className="py-2 text-sm">
                    <button
                      type="button"
                      onClick={() => toggle(c)}
                      aria-expanded={open === key}
                      className="flex w-full min-h-[44px] items-center gap-3 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/40"
                    >
                      <span className="w-28 shrink-0 tabular-nums text-text-dim">{fmtDevice(c.time)}</span>
                      <span className="w-28 shrink-0 font-medium">{c.program}</span>
                      <span className="min-w-0 flex-1 text-text-dim">{c.status}</span>
                      <span className="text-xs text-accent">{open === key ? t("health.hide", "Hide") : t("health.show", "Show")}</span>
                    </button>
                    {open === key && (
                      <pre className="mt-2 max-h-80 overflow-auto rounded-md bg-bg p-3 font-mono text-[11px] leading-relaxed text-text">
                        {text}
                      </pre>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </Section>
      </div>
    </div>
  );
}
