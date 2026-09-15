"use client";

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { PageHeader, SectionCard } from "@/components/admin/StatCard";
import { Button } from "@/components/admin/Button";

interface Context {
  cid: number;
  bearer_id: number;
  apn: string;
  qci: number | null;
  qci_device: number | null;
  qci_inferred: number;
  dl_gbr_kbps: number | null;
  ul_gbr_kbps: number | null;
  dl_mbr_kbps: number | null;
  ul_mbr_kbps: number | null;
}
interface QoSData {
  contexts: Context[];
  raw_cgcontrdp: string;
  note: string;
}

type TFn = (key: string, defaultValue: string) => string;

function qciLabels(t: TFn): Record<number, string> {
  return {
    1: t("qci.label1", "VoNR / VoLTE conversational voice"),
    2: t("qci.label2", "Conversational video"),
    3: t("qci.label3", "Real-time gaming, V2X"),
    4: t("qci.label4", "Non-conversational video buffered"),
    5: t("qci.label5", "IMS signaling"),
    6: t("qci.label6", "TCP-based (web, email, FTP)"),
    7: t("qci.label7", "Voice, video, interactive gaming"),
    8: t("qci.label8", "TCP-based (web, email, FTP)"),
    9: t("qci.label9", "TCP-based default (best-effort internet)"),
    65: t("qci.label65", "Mission-critical user plane push-to-talk"),
    66: t("qci.label66", "Non-mission-critical push-to-talk"),
    69: t("qci.label69", "Mission-critical signaling"),
    70: t("qci.label70", "Mission-critical data"),
    79: t("qci.label79", "V2X messages"),
    80: t("qci.label80", "Low-latency eMBB applications"),
  };
}

export default function QCIPage() {
  const { t } = useTranslation();
  const { data, error, isLoading } = useApi<QoSData>("/api/network/qos", {
    refreshInterval: 5000,
  });
  const [showRaw, setShowRaw] = useState(false);
  const QCI_LABELS = qciLabels(t);

  return (
    <>
      <PageHeader
        title={t("qci.title", "QCI / Bearers")}
        description={t("qci.desc", "PDP contexts and (where exposed) QoS Class Identifier.")}
      />

      {error && (
        <div className="mb-4 rounded-md border border-error/40 bg-error/10 px-3 py-2 text-sm text-error">
          {(error as Error).message}
        </div>
      )}

      <SectionCard title={t("qci.activeBearers", "Active bearers")}>
        {isLoading && !data ? (
          <div className="text-sm text-text-dim">{t("qci.loading", "Loading…")}</div>
        ) : !data || data.contexts.length === 0 ? (
          <div className="text-sm text-text-dim">{t("qci.noContexts", "No PDP contexts active.")}</div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border text-left text-xs uppercase tracking-wider text-text-dim">
                  <th className="py-2 pr-4">CID</th>
                  <th className="py-2 pr-4">{t("qci.colBearer", "Bearer")}</th>
                  <th className="py-2 pr-4">APN</th>
                  <th className="py-2 pr-4">QCI / 5QI</th>
                  <th className="py-2 pr-4">{t("qci.colSource", "Source")}</th>
                  <th className="py-2 pr-4">DL · UL GBR</th>
                </tr>
              </thead>
              <tbody>
                {data.contexts.map((ctx) => {
                  const measured = ctx.qci != null;
                  const fromModem = !measured && ctx.qci_device != null;
                  const qci = ctx.qci ?? ctx.qci_device ?? ctx.qci_inferred;
                  return (
                    <tr key={ctx.cid} className="border-b border-border/60 last:border-0">
                      <td className="py-2 pr-4 font-mono">{ctx.cid}</td>
                      <td className="py-2 pr-4 font-mono">{ctx.bearer_id}</td>
                      <td className="py-2 pr-4 font-mono">{ctx.apn || "—"}</td>
                      <td className="py-2 pr-4">
                        <span className="font-mono text-base font-semibold">{qci}</span>
                        <span className="ml-2 text-xs text-text-dim">
                          {QCI_LABELS[qci] ?? t("qci.unknown", "unknown")}
                        </span>
                      </td>
                      <td className="py-2 pr-4">
                        {measured ? (
                          <span className="rounded bg-success/15 px-2 py-0.5 text-xs text-success">
                            {t("qci.measured", "measured")}
                          </span>
                        ) : fromModem ? (
                          <span className="rounded bg-success/15 px-2 py-0.5 text-xs text-success">
                            {t("qci.fromModem", "from modem")}
                          </span>
                        ) : (
                          <span className="rounded bg-warning/15 px-2 py-0.5 text-xs text-warning">
                            {t("qci.inferredFromApn", "inferred from APN")}
                          </span>
                        )}
                      </td>
                      <td className="py-2 pr-4 font-mono text-xs">
                        {ctx.dl_gbr_kbps != null && ctx.ul_gbr_kbps != null
                          ? `${ctx.dl_gbr_kbps} · ${ctx.ul_gbr_kbps} kbps`
                          : "—"}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </SectionCard>

      {data && (
        <div className="mt-3 text-xs text-text-dim">
          {data.note} <em>{t("qci.fromModem", "from modem")}</em>{" "}
          {t("qci.noteFromModem", "matches the value on the device’s About screen;")}{" "}
          <em>{t("qci.measured", "measured")}</em>{" "}
          {t("qci.noteMeasured", "is a dedicated bearer’s live 5QI (e.g. 5QI=1 during a VoNR call).")}
        </div>
      )}

      <div className="mt-4">
        <Button variant="outline" size="sm" onClick={() => setShowRaw((v) => !v)}>
          {showRaw
            ? t("qci.hideRaw", "Hide raw AT+CGCONTRDP")
            : t("qci.showRaw", "Show raw AT+CGCONTRDP")}
        </Button>
        {showRaw && data && (
          <pre className="mt-2 overflow-x-auto rounded-md border border-border bg-bg-elevated p-3 font-mono text-xs">
            {data.raw_cgcontrdp}
          </pre>
        )}
      </div>
    </>
  );
}
