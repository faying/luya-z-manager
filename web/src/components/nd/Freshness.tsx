"use client";
import { useTranslation } from "react-i18next";
import { formatDeviceTime } from "./time";

/**
 * "数字停在 hh:mm" for stale data, shown in device-local time. When the
 * device clock offset has never been seen, fall back to "N 分钟前".
 * Renders nothing while the data is fresh.
 */
export function Freshness({ stale, lastOkAt, what }: { stale: boolean; lastOkAt: number | null; what?: string }) {
  const { t } = useTranslation();
  if (!stale) return null;
  if (lastOkAt == null) return <span className="nd-fresh">{t("nd.noDataYet", "No data yet")}</span>;
  const when = formatDeviceTime(lastOkAt);
  const text = when.kind === "clock"
    ? t("nd.stoppedAt", "{{what}} stopped at {{time}}", { what: what ?? t("nd.numbers", "Numbers"), time: when.text })
    : t("nd.stoppedAgo", "{{what}} stopped {{min}} min ago", { what: what ?? t("nd.numbers", "Numbers"), min: when.minutes });
  return <span className="nd-fresh">{text}</span>;
}
