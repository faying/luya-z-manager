"use client";
// QCI / bearers ("look" page, read-only). Status first (how many PDP
// contexts are up), then the bearer table, the explanatory note and the
// raw AT+CGCONTRDP output (collapsed, console band).
//
// GET /api/network/qos is not cheap: every call sends AT+CGCONTRDP plus one
// AT+CGEQOSRDP=<cid> per context on the modem's AT port (qos.rs:18-70) and
// takes about 12 s. So: one request at a time (SWR waits for the previous
// reply), 15 s between polls — never faster than the old page's 5 s — and a
// 30 s timeout so a slow reply isn't counted as a failure.
import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import type { NetworkQos, QosContext } from "@/lib/api/schemas/network";
import { Button, ConsoleBand, Freshness, GroupTitle, StatusBlock, StatusMark, type Tone } from "@/components/nd";

const QOS = "/api/network/qos";

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

function gbr(c: QosContext): string {
  return c.dl_gbr_kbps != null && c.ul_gbr_kbps != null ? `${c.dl_gbr_kbps} · ${c.ul_gbr_kbps} kbps` : "—";
}

export default function QCIPage() {
  const { t } = useTranslation();
  const qos = useApi<NetworkQos>(QOS, {
    refreshInterval: 15000,
    timeoutMs: 30000,
    revalidateOnFocus: false,
  });
  const data = qos.data;
  const [showRaw, setShowRaw] = useState(false);
  const labels = qciLabels(t);

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("qci.reading", "Reading bearers…");
  let reason: ReactNode = t("qci.readingReason", "The modem is asked over its AT port; this takes about 12 seconds.");
  if (!data && qos.error) {
    tone = "bad";
    state = t("qci.unreadable", "Can't read the bearers");
    reason = qos.error.message;
  } else if (data && data.contexts.length === 0) {
    tone = "warn";
    state = t("qci.noContexts", "No PDP contexts active.");
    reason = t("qci.noContextsNext", "There is no mobile data connection right now. Check mobile data and signal.");
  } else if (data) {
    tone = "ok";
    state = t("qci.nActive", "{{n}} bearers active", { n: data.contexts.length });
    reason = null;
  }
  if (data && qos.stale) tone = "stale";

  const measured = t("qci.measured", "measured");
  const fromModem = t("qci.fromModem", "from modem");

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("qci.title", "QCI / Bearers")}</h1>

      <div className="grid max-w-[960px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={data && qos.stale ? <Freshness stale lastOkAt={qos.lastOkAt} /> : undefined}
          actions={
            qos.error ? (
              <Button variant="secondary" size="sm" onPress={() => qos.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        <section aria-labelledby="qci-bearers">
          <GroupTitle id="qci-bearers">{t("qci.activeBearers", "Active bearers")}</GroupTitle>
          <p className="nd-aux -mt-1 mb-3 px-1">{t("qci.desc", "PDP contexts and (where exposed) QoS Class Identifier.")}</p>
          {data && qos.error && (
            <p role="alert" className="nd-aux mb-2 px-1 text-nd-badT">
              {t("qci.refreshFailed", "Last refresh failed: {{e}}. Showing the previous reading.", { e: qos.error.message ?? "" })}
            </p>
          )}
          <div className={`nd-group${qos.stale ? " nd-stale" : ""}`}>
            {!data ? (
              <div className="grid gap-3 p-4 lg:p-5" aria-hidden={!qos.error}>
                {qos.error ? (
                  <p className="nd-body text-nd-t2">{t("qci.noDataYet", "Nothing to show yet · press “Retry” above.")}</p>
                ) : (
                  [0, 1].map((i) => <span key={i} className="nd-skel block h-5 w-full" />)
                )}
              </div>
            ) : data.contexts.length === 0 ? (
              <p className="nd-body p-4 text-nd-t2 lg:p-5">{t("qci.noContexts", "No PDP contexts active.")}</p>
            ) : (
              <div className="overflow-x-auto p-2 lg:p-3" tabIndex={0} role="region" aria-labelledby="qci-bearers">
                <table className="w-full text-[14px] leading-5">
                  <thead>
                    <tr className="text-left text-nd-t2">
                      <th scope="col" className="px-2 py-2 font-semibold">CID</th>
                      <th scope="col" className="px-2 py-2 font-semibold">{t("qci.colBearer", "Bearer")}</th>
                      <th scope="col" className="px-2 py-2 font-semibold">APN</th>
                      <th scope="col" className="px-2 py-2 font-semibold">QCI / 5QI</th>
                      <th scope="col" className="px-2 py-2 font-semibold">{t("qci.colSource", "Source")}</th>
                      <th scope="col" className="px-2 py-2 font-semibold">DL · UL GBR</th>
                    </tr>
                  </thead>
                  <tbody>
                    {data.contexts.map((c) => {
                      const isMeasured = c.qci != null;
                      const isDevice = !isMeasured && c.qci_device != null;
                      const q = c.qci ?? c.qci_device ?? c.qci_inferred;
                      return (
                        <tr key={c.cid} className="border-t border-nd-sep align-top">
                          <td className="nd-mono px-2 py-2 tabular-nums">{c.cid}</td>
                          <td className="nd-mono px-2 py-2 tabular-nums">{c.bearer_id}</td>
                          <td className="nd-mono px-2 py-2">{c.apn || "—"}</td>
                          <td className="px-2 py-2">
                            <span className="nd-mono text-[16px] font-medium tabular-nums">{q ?? "—"}</span>
                            <span className="ml-2 text-nd-t2">{(q != null && labels[q]) || t("qci.unknown", "unknown")}</span>
                          </td>
                          <td className="whitespace-nowrap px-2 py-2">
                            {isMeasured ? (
                              <StatusMark tone="ok">{measured}</StatusMark>
                            ) : isDevice ? (
                              <StatusMark tone="ok">{fromModem}</StatusMark>
                            ) : (
                              <StatusMark tone="warn">{t("qci.inferredFromApn", "inferred from APN")}</StatusMark>
                            )}
                          </td>
                          <td className="nd-mono whitespace-nowrap px-2 py-2 tabular-nums">{gbr(c)}</td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}
          </div>
          {data && (
            <p className="nd-aux mt-3 px-1">
              {data.note} <em>{fromModem}</em> {t("qci.noteFromModem", "matches the value on the device’s About screen;")}{" "}
              <em>{measured}</em> {t("qci.noteMeasured", "is a dedicated bearer’s live 5QI (e.g. 5QI=1 during a VoNR call).")}
            </p>
          )}
        </section>

        <section aria-labelledby="qci-raw">
          <GroupTitle id="qci-raw">AT+CGCONTRDP</GroupTitle>
          <Button
            variant="secondary"
            size="sm"
            onPress={() => setShowRaw((v) => !v)}
            aria-expanded={showRaw}
            aria-controls="qci-raw-out"
          >
            {showRaw ? t("qci.hideRaw", "Hide raw AT+CGCONTRDP") : t("qci.showRaw", "Show raw AT+CGCONTRDP")}
          </Button>
          {showRaw && (
            <div id="qci-raw-out" className="mt-3">
              <ConsoleBand label={t("qci.rawLabel", "Raw AT+CGCONTRDP output")}>
                {data ? (
                  <pre tabIndex={0} className="nd-mono overflow-x-auto whitespace-pre text-[13px] leading-5">{data.raw_cgcontrdp || "—"}</pre>
                ) : (
                  <p className="nd-console__muted">{t("qci.rawNotYet", "No output yet.")}</p>
                )}
              </ConsoleBand>
            </div>
          )}
        </section>
      </div>
    </>
  );
}
