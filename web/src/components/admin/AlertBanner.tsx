"use client";

// Shown on every page while there are unread alerts. Renders nothing
// otherwise — the alerts page is where the quiet state lives.

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { mutate as globalMutate } from "swr";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ALERTS_PATH, AlertsData, kindLabel } from "@/lib/alerts";
import { Button } from "@/components/admin/Button";

export function AlertBanner() {
  const { t } = useTranslation();
  const pathname = usePathname() ?? "";
  const { data, mutate } = useApi<AlertsData>(ALERTS_PATH, { refreshInterval: 30000 });
  const [busy, setBusy] = useState(false);

  if (!data || data.unread === 0 || pathname.replace(/\/$/, "") === "/alerts") return null;

  const latest = data.events.find((e) => e.unread) ?? data.events[0];
  const lastSms = data.sms.recent[0];
  const smsNote = !data.sms.configured
    ? t("alerts.bannerSmsOff", "SMS alerts are not set up.")
    : lastSms?.result === "failed"
      ? t("alerts.bannerSmsFailed", "The last alert SMS failed to send.")
      : null;

  async function dismiss() {
    setBusy(true);
    try {
      await apiFetch("/api/alerts/read", { method: "POST", body: { seq: data!.events[0]?.seq ?? 0 } });
      await mutate();
      globalMutate("/api/public/status");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      role="status"
      aria-live="polite"
      className="mb-6 flex flex-col gap-3 rounded-md border border-warning/40 bg-warning/10 px-4 py-3 text-[13px] text-text sm:flex-row sm:items-center"
    >
      <div className="min-w-0 flex-1">
        <p className="font-medium">
          <span className="text-warning" aria-hidden>▲ </span>
          {t("alerts.bannerCount", "{{n}} new alert(s)", { n: data.unread })}
          {latest && <> · {kindLabel(t, latest.kind)}</>}
        </p>
        {(latest?.text || smsNote) && (
          <p className="mt-0.5 text-text-dim">
            {latest?.text}
            {latest?.text && smsNote ? " " : ""}
            {smsNote}
          </p>
        )}
      </div>
      <div className="flex shrink-0 items-center gap-2">
        <Button size="sm" variant="ghost" onClick={dismiss} loading={busy}>
          {t("alerts.bannerDismiss", "Got it")}
        </Button>
        <Link
          href="/alerts"
          className="inline-flex h-8 items-center rounded-lg bg-accent px-3 text-[12px] font-medium text-white hover:bg-accent-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/40"
        >
          {t("alerts.bannerView", "View")}
        </Link>
      </div>
    </div>
  );
}
