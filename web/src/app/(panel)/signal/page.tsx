"use client";

import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { PageHeader, SectionCard, ErrorBanner, Status, MetaRow } from "@/components/admin/StatCard";
import { Help } from "@/components/admin/Help";
import { cn } from "@/lib/utils";

interface SignalData {
  network_type?: string;
  network_provider?: string;
  network_provider_fullname?: string;
  nr5g_rsrp?: number;
  nr5g_rsrq?: number;
  nr5g_snr?: string;
  nr5g_cell_id?: number;
  nr5g_action_channel?: number;
  nr5g_pci?: number;
  nr5g_action_band?: string;
  nr5g_bandwidth?: string;
  lte_rsrp?: number;
  lte_rsrq?: number;
  lte_snr?: string;
  lte_cell_id?: number;
  lte_earfcn?: number;
  lte_pci?: number;
  lte_band?: string;
  signalbar?: string;
  net_select_mode?: string;
}

const MAX_SAMPLES = 60;

export default function SignalPage() {
  const { t } = useTranslation();
  const [rsrpHistory, setRsrpHistory] = useState<number[]>([]);
  const { data: sig, error, mutate } = useApi<SignalData>("/api/network/signal", { refreshInterval: 2000 });

  const isNR = (sig?.network_type || "").toUpperCase().includes("SA") || (sig?.nr5g_rsrp ?? 0) !== 0;
  const rsrp = isNR ? sig?.nr5g_rsrp : sig?.lte_rsrp;
  const rsrq = isNR ? sig?.nr5g_rsrq : sig?.lte_rsrq;
  const sinr = isNR ? sig?.nr5g_snr : sig?.lte_snr;
  const cellId = isNR ? sig?.nr5g_cell_id : sig?.lte_cell_id;
  const earfcn = isNR ? sig?.nr5g_action_channel : sig?.lte_earfcn;
  const pci = isNR ? sig?.nr5g_pci : sig?.lte_pci;
  const band = isNR ? sig?.nr5g_action_band : sig?.lte_band;
  const link = rsrpTone(rsrp);

  useEffect(() => {
    if (rsrp != null && rsrp !== 0) {
      setRsrpHistory((prev) => [...prev.slice(-(MAX_SAMPLES - 1)), rsrp]);
    }
  }, [rsrp]);

  return (
    <>
      <PageHeader title={t("signal.title", "Signal Monitor")} description={t("signal.desc", "Live cellular signal metrics, updated every 2 s.")} />

      {error && (
        <div className="mb-4">
          <ErrorBanner message={String(error.message ?? error)} onRetry={() => mutate()} />
        </div>
      )}

      {/* Hero — leads with plain language; raw figures preserved for the owner */}
      <div className="admin-card p-5 sm:p-6">
        <div className="flex flex-col gap-5 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-4">
            <SignalBars rsrp={rsrp} tone={link.tone} />
            <div className="min-w-0">
              <div className="flex items-center gap-3">
                <Status tone={link.tone}>{t(link.labelKey, link.label)}</Status>
                <Freshness healthy={!error} hasData={!!sig} />
              </div>
              <h3 className="mt-1.5 font-display text-xl font-semibold leading-tight tracking-tight text-text">
                {t(link.plainKey, link.plain)}
              </h3>
              <MetaRow
                className="mt-1.5"
                items={[
                  rsrp != null && rsrp !== 0 ? <span className="tabular-nums">{rsrp} dBm</span> : null,
                  sig?.network_provider_fullname || sig?.network_provider,
                  sig?.network_type,
                  band && `Band ${band}`,
                ]}
              />
            </div>
          </div>
          <div className="grid grid-cols-3 gap-5 border-t border-border/60 pt-4 sm:border-t-0 sm:pt-0 sm:text-right">
            <Metric label="SINR" value={sinr || "—"} unit="dB" help={t("help.sinr", "Signal-to-noise — higher is cleaner. Above 13 dB is good.")} />
            <Metric label="RSRQ" value={rsrq != null && rsrq !== 0 ? rsrq : "—"} unit="dB" help={t("help.rsrq", "Signal quality. Closer to 0 is better; below −15 is poor.")} />
            <Metric label="Bars" value={sig?.signalbar ?? "—"} unit="/5" help={t("help.bars", "The carrier's own 0–5 strength estimate.")} />
          </div>
        </div>
      </div>

      {/* RSRP trend */}
      <SectionCard title={t("signal.trendTitle", "Signal trend")} description={t("signal.trendDesc", "Received signal strength over the last ~2 minutes.")} className="mt-4">
        <Sparkline samples={rsrpHistory} />
      </SectionCard>

      {/* Serving cell */}
      <SectionCard title={t("signal.cellTitle", "Serving cell")} description={t("signal.cellDesc", "Technical details of the tower you're connected to.")} className="mt-4">
        <div className="grid grid-cols-2 gap-x-8 gap-y-1 sm:grid-cols-3">
          <Row k="Cell ID" v={cellId} help={t("help.cellId", "Unique ID of the tower sector serving you.")} />
          <Row k="PCI" v={pci} help={t("help.pci", "Physical Cell ID — tells nearby towers apart.")} />
          <Row k="EARFCN" v={earfcn} help={t("help.earfcn", "The radio channel number your device is tuned to.")} />
          <Row k="Band" v={band} help={t("help.band", "The frequency band currently in use.")} />
          <Row k="Bandwidth" v={sig?.nr5g_bandwidth ? `${sig.nr5g_bandwidth} MHz` : undefined} help={t("help.bandwidth", "Channel width — wider generally means faster.")} />
          <Row k="Net Select" v={sig?.net_select_mode} help={t("help.netSelect", "Whether the network is chosen automatically or manually.")} />
        </div>
      </SectionCard>
    </>
  );
}

function Sparkline({ samples }: { samples: number[] }) {
  const { t } = useTranslation();
  const w = 480;
  const h = 96;
  const pad = 6;
  if (samples.length < 2) {
    return (
      <div className="flex h-24 items-center justify-center text-sm text-text-dim">{t("signal.collecting", "Collecting data…")}</div>
    );
  }
  const min = Math.min(...samples);
  const max = Math.max(...samples);
  const range = max - min || 1;
  const xy = samples.map((v, i) => {
    const x = pad + (i / (samples.length - 1)) * (w - pad * 2);
    const y = pad + (1 - (v - min) / range) * (h - pad * 2);
    return [x, y] as const;
  });
  const line = xy.map(([x, y]) => `${x.toFixed(1)},${y.toFixed(1)}`).join(" ");
  const area = `${pad},${h - pad} ${line} ${w - pad},${h - pad}`;
  const [lx, ly] = xy[xy.length - 1];
  return (
    <svg width={w} height={h} viewBox={`0 0 ${w} ${h}`} className="w-full text-accent" preserveAspectRatio="none">
      <defs>
        <linearGradient id="rsrpFill" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="currentColor" stopOpacity="0.18" />
          <stop offset="100%" stopColor="currentColor" stopOpacity="0" />
        </linearGradient>
      </defs>
      <polygon points={area} fill="url(#rsrpFill)" />
      <polyline points={line} fill="none" stroke="currentColor" strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
      <circle cx={lx} cy={ly} r={3.5} className="fill-[var(--admin-glow)]" />
      <circle cx={lx} cy={ly} r={6} className="fill-[var(--admin-glow)]" opacity={0.25} />
      <text x={pad} y={h - 8} fontSize={10} className="fill-text-dim">{min.toFixed(0)}</text>
      <text x={pad} y={14} fontSize={10} className="fill-text-dim">{max.toFixed(0)} dBm</text>
    </svg>
  );
}

type ToneKey = "neutral" | "success" | "warning" | "danger" | "accent";

function SignalBars({ rsrp, tone }: { rsrp?: number; tone: ToneKey }) {
  const lvl =
    rsrp == null || rsrp === 0
      ? 0
      : rsrp >= -85 ? 5 : rsrp >= -95 ? 4 : rsrp >= -105 ? 3 : rsrp >= -115 ? 2 : 1;
  const fill = {
    success: "bg-success",
    warning: "bg-warning",
    danger: "bg-error",
    neutral: "bg-text-dim",
    accent: "bg-accent",
  }[tone];
  return (
    <div className="flex h-12 items-end gap-1" aria-label={`Signal ${lvl} of 5`}>
      {[1, 2, 3, 4, 5].map((i) => (
        <span
          key={i}
          className={cn("w-2 rounded-[3px] transition-colors", i <= lvl ? fill : "bg-border")}
          style={{ height: `${i * 16 + 16}%` }}
        />
      ))}
    </div>
  );
}

/** Live/Reconnecting indicator — surfaces silent polling failure. */
function Freshness({ healthy, hasData }: { healthy: boolean; hasData: boolean }) {
  const { t } = useTranslation();
  if (!hasData) return null;
  return (
    <span className="inline-flex items-center gap-1.5 text-[10px] font-medium uppercase tracking-[0.1em] text-text-dim">
      <span
        className={cn(
          "inline-block h-1.5 w-1.5 rounded-full",
          healthy ? "animate-pulse bg-success" : "bg-warning"
        )}
      />
      {healthy ? t("dashboard.live", "Live") : t("dashboard.reconnecting", "Reconnecting")}
    </span>
  );
}

function Metric({ label, value, unit, help }: { label: string; value: string | number; unit?: string; help?: string }) {
  return (
    <div>
      <div className="text-[10px] font-medium uppercase tracking-[0.12em] text-text-dim">
        {label}
        {help && <Help text={help} />}
      </div>
      <div className="mt-1 font-display text-lg font-semibold tabular-nums text-text">
        {value}
        {unit && <span className="ml-0.5 text-xs font-medium text-text-dim">{unit}</span>}
      </div>
    </div>
  );
}

function Row({ k, v, help }: { k: string; v: string | number | null | undefined; help?: string }) {
  return (
    <div className="flex items-center justify-between border-b border-border/50 py-2 text-[13px] sm:border-0">
      <span className="flex items-center text-text-dim">
        {k}
        {help && <Help text={help} />}
      </span>
      <span data-numeric className="font-mono text-[12.5px] text-text">
        {v == null || v === "" ? <span className="text-text-dim">—</span> : String(v)}
      </span>
    </div>
  );
}

function rsrpTone(rsrp?: number): { tone: ToneKey; labelKey: string; label: string; plainKey: string; plain: string } {
  if (rsrp == null || rsrp === 0)
    return { tone: "neutral", labelKey: "dashboard.linkNoLink", label: "No link", plainKey: "dashboard.plainNone", plain: "No signal — check the SIM or coverage" };
  if (rsrp >= -85)
    return { tone: "success", labelKey: "dashboard.linkExcellent", label: "Excellent", plainKey: "dashboard.plainExcellent", plain: "Strong, stable connection" };
  if (rsrp >= -100)
    return { tone: "success", labelKey: "dashboard.linkGood", label: "Good", plainKey: "dashboard.plainGood", plain: "Solid connection" };
  if (rsrp >= -110)
    return { tone: "warning", labelKey: "dashboard.linkFair", label: "Fair", plainKey: "dashboard.plainFair", plain: "Usable, but the signal is weak" };
  return { tone: "danger", labelKey: "dashboard.linkPoor", label: "Poor", plainKey: "dashboard.plainPoor", plain: "Weak signal — try repositioning the router" };
}
