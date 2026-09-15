"use client";

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Toggle } from "@/components/admin/Button";

interface QosConfig {
  qos_switch?: string;
}

export default function QosPage() {
  const { t } = useTranslation();
  const { data, error, mutate } = useApi<QosConfig>("/api/router/qos");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);

  // /api/router/qos currently returns 503 — guard for missing data
  const qosOn = data?.qos_switch === "1";
  const unavailable = !data && !error;
  const serviceDown = error != null;

  async function toggleQos(enabled: boolean) {
    setBusy(true);
    setMsg(null);
    try {
      await apiFetch("/api/router/qos", { method: "PUT", body: { qos_switch: enabled ? "1" : "0" } });
      setMsg({ text: enabled ? t("qos.msgEnabled", "QoS enabled") : t("qos.msgDisabled", "QoS disabled"), err: false });
      mutate();
    } catch (e) {
      setMsg({ text: e instanceof ApiError ? e.message : String(e), err: true });
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <PageHeader title={t("qos.title", "QoS")} description={t("qos.desc", "Quality of Service traffic prioritization.")} />

      {serviceDown && (
        <div className="mb-4 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
          {t("qos.serviceDown", "QoS service is currently unavailable (the router may not support this feature).")}
        </div>
      )}
      {msg && (
        <div className={`mb-4 rounded-md px-3 py-2 text-sm ${msg.err ? "border border-error/40 bg-error/10 text-error" : "border border-success/40 bg-success/10 text-success"}`}>
          {msg.text}
        </div>
      )}

      <SectionCard title={t("qos.switchTitle", "QoS Switch")}>
        <div className="flex items-center justify-between">
          <div>
            <p className="text-sm font-medium">{t("qos.qualityOfService", "Quality of Service")}</p>
            <p className="mt-0.5 text-xs text-text-dim">{t("qos.switchHint", "QoS prioritizes interactive traffic over bulk transfers.")}</p>
          </div>
          <Toggle checked={qosOn} onChange={toggleQos} disabled={busy || serviceDown || unavailable} />
        </div>
      </SectionCard>
    </>
  );
}
