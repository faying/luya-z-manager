"use client";
// Shown on every page while there are unread alerts (was admin/AlertBanner).
// Same data, same two actions: "Got it" marks them read, "View" opens /alerts.
import Link from "next/link";
import { usePathname } from "next/navigation";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { mutate as globalMutate } from "swr";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ALERTS_PATH, type AlertsData, kindLabel } from "@/lib/alerts";
import { Button } from "../Button";

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

  // Compact ink strip (Cohere's announcement bar): one line on phones, the
  // detail text joins it from 640px. Not a colour block, so it never stacks
  // a second pastel panel above the page's status block.
  return (
    <div role="status" aria-live="polite" className="nd-announce">
      <p className="nd-announce__text">
        <span aria-hidden="true" className="nd-announce__sym">▲</span>
        <span className="font-semibold">
          {t("alerts.bannerCount", "{{n}} new alert(s)", { n: data.unread })}
          {latest && <> · {kindLabel(t, latest.kind)}</>}
        </span>
        {(latest?.text || smsNote) && (
          <span className="nd-announce__detail">
            {latest?.text}
            {latest?.text && smsNote ? " " : ""}
            {smsNote}
          </span>
        )}
      </p>
      <div className="flex shrink-0 items-center">
        <Button variant="ghost" size="sm" className="nd-announce__btn" onPress={dismiss} pending={busy}>
          {t("alerts.bannerDismiss", "Got it")}
        </Button>
        <Link href="/alerts" className="nd-btn nd-btn--sm nd-announce__btn nd-announce__view">
          {t("alerts.bannerView", "View")}
        </Link>
      </div>
    </div>
  );
}
