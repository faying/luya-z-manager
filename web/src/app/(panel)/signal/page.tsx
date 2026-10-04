"use client";
// Signal (design doc §6, 15A): status (state · reason · next) → RSRP as the
// page's one big number + SINR, RSRQ, bars → 2-minute trend → serving-cell
// detail → "Band lock ›" and "Signal detect ›". ≥1024: two columns.
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import type { NetworkSignal } from "@/lib/api/schemas/network";
import type { CellExtra } from "@/lib/api/schemas/modem";
import { Group, Help, Readout, ReadoutWall, Row } from "@/components/nd";
import { SignalStatus } from "@/components/signal/SignalStatus";
import { TrendChart } from "@/components/signal/TrendChart";
import { carrierCounts, carriers, lteAnchor, plmn, selectionWord, servingCellId, servingRssi, sigState } from "@/lib/home";
import { rsrpWord, rsrqWord, sinrWord } from "@/lib/signalWords";
import { Broadcast, CellTower } from "@phosphor-icons/react";

const MAX_SAMPLES = 60; // 2 minutes at 2 s

export default function SignalPage() {
  const { t } = useTranslation();
  const [history, setHistory] = useState<number[]>([]);
  // Record one sample per successful poll (SWR onSuccess, not an effect).
  const sig = useApi<NetworkSignal>("/api/network/signal", {
    refreshInterval: 2000,
    onSuccess: (d) => {
      const v = carriers(d)[0]?.rsrp;
      if (v != null) setHistory((h) => [...h.slice(-(MAX_SAMPLES - 1)), v]);
    },
  });
  const s = sig.data;
  // QCI / AMBR from datad (audit C); 404 on older agents → the rows stay away
  const extra = useApi<CellExtra>("/api/cell/extra", { refreshInterval: 30000 });
  const x = extra.data;
  const cs = useMemo(() => carriers(s), [s]);
  const serving = cs[0];
  const bars = s?.signalbar != null && s.signalbar !== "" ? Number(s.signalbar) : null;
  const state = sigState({ everValid: sig.lastOkAt != null, valid: !sig.stale, bars, sinr: serving?.sinr ?? null });

  const rsrp = serving?.rsrp ?? null;

  const loading = state === "loading";
  const cellId = servingCellId(s, serving);
  const band = serving ? (serving.kind === "nr" ? `n${serving.band}` : `B${serving.band}`) : null;
  const rssi = servingRssi(s, serving?.kind);
  const anchor = lteAnchor(s);
  const net = plmn(s);
  const ambr = x && x.ambr_dl_mbps != null ? `↓ ${fmtMbps(x.ambr_dl_mbps)} ↑ ${x.ambr_ul_mbps != null ? fmtMbps(x.ambr_ul_mbps) : "—"} Mbps` : null;

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("nav.signal", "Signal")}</h1>
      <div className="grid gap-4 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)] lg:gap-6">
        <div className="grid content-start gap-4">
          <SignalStatus
            state={state}
            allDown={sig.stale}
            sig={s}
            serving={serving}
            counts={carrierCounts(cs)}
            bars={bars}
            lastOkAt={sig.lastOkAt}
            speedStale={false}
            onRetry={() => sig.mutate()}
          />
          <ReadoutWall cols={3} label={t("signal.readings", "Signal readings")}>
            <Readout
              full
              hero
              label={<>RSRP <Help label="RSRP" text={t("help.rsrp", "Received signal power. Closer to 0 is stronger; above −100 dBm is good.")} /></>}
              value={loading ? undefined : rsrp}
              unit="dBm"
              sub={rsrpWord(t, rsrp)}
              stale={sig.stale}
            />
            <Readout
              label={<>SINR <Help label="SINR" text={t("help.sinr", "Signal-to-noise — higher is cleaner. Above 13 dB is good.")} /></>}
              value={loading ? undefined : serving?.sinr ?? null}
              unit="dB"
              sub={sinrWord(t, serving?.sinr ?? null)}
              stale={sig.stale}
            />
            <Readout
              label={<>RSRQ <Help label="RSRQ" text={t("help.rsrq", "Signal quality. Closer to 0 is better; below −15 is poor.")} /></>}
              value={loading ? undefined : serving?.rsrq ?? null}
              unit="dB"
              sub={rsrqWord(t, serving?.rsrq ?? null)}
              stale={sig.stale}
            />
            <Readout
              label={<>{t("signal.bars", "Bars")} <Help label={t("signal.bars", "Bars")} text={t("help.bars", "The carrier's own 0–5 strength estimate.")} /></>}
              value={loading ? undefined : bars != null && bars >= 0 ? bars : null}
              unit="/5"
              sub={bars != null && bars >= 0 ? (bars >= 3 ? t("home.good", "Good") : t("home.weak", "Weak")) : undefined}
              stale={sig.stale}
            />
          </ReadoutWall>
          <section aria-labelledby="trend-title">
            <h2 id="trend-title" className="nd-group-title">{t("signal.trendTitle", "Signal trend")}</h2>
            <TrendChart samples={history} stale={sig.stale} />
          </section>
        </div>

        <div className="grid content-start gap-4">
          <Group title={t("signal.cellTitle", "Serving cell")} stale={sig.stale}>
            <Row label={<>Cell ID <Help label="Cell ID" text={t("help.cellId", "Unique ID of the tower sector serving you.")} /></>} value={cellId ?? "—"} mono />
            <Row label={<>PCI <Help label="PCI" text={t("help.pci", "Physical Cell ID — tells nearby towers apart.")} /></>} value={serving?.pci ?? "—"} mono />
            <Row label={<>ARFCN <Help label="ARFCN" text={t("help.earfcn", "The radio channel number your device is tuned to.")} /></>} value={serving?.arfcn ?? "—"} mono />
            <Row label={<>{t("signal.band", "Band")} <Help label={t("signal.band", "Band")} text={t("help.band", "The frequency band currently in use.")} /></>} value={band ?? "—"} mono />
            <Row label={<>{t("signal.bandwidth", "Bandwidth")} <Help label={t("signal.bandwidth", "Bandwidth")} text={t("help.bandwidth", "Channel width — wider generally means faster.")} /></>} value={serving?.bw != null ? `${serving.bw} MHz` : "—"} />
            <Row label={<>RSSI <Help label="RSSI" text={t("help.rssi", "Total power received on the channel, including noise and other users.")} /></>} value={rssi != null ? `${rssi} dBm` : "—"} />
            <Row label={<>PLMN <Help label="PLMN" text={t("help.plmn", "The network's country code and operator code (MCC-MNC).")} /></>} value={net ?? "—"} mono />
            {x && (
              <Row
                label={<>QCI · AMBR <Help label="QCI · AMBR" text={t("help.ambr", "QCI is the traffic class the carrier gives your data; AMBR is the speed cap the network set for this connection.")} /></>}
                value={x.qci != null || ambr ? [x.qci != null ? `QCI ${x.qci}` : null, ambr].filter(Boolean).join(" · ") : "—"}
              />
            )}
            <Row label={<>{t("home.netSelect", "Network selection")} <Help label={t("home.netSelect", "Network selection")} text={t("help.netSelect", "Whether the network is chosen automatically or manually.")} /></>} value={selectionWord(s?.net_select_mode, t) ?? "—"} />
          </Group>
          {anchor && (
            <Group title={t("signal.anchorTitle", "4G anchor (5G NSA)")} stale={sig.stale}>
              <Row label="PCI" value={anchor.pci ?? "—"} mono />
              <Row label="EARFCN" value={anchor.earfcn ?? "—"} mono />
              <Row label={t("signal.band", "Band")} value={anchor.band ? `B${anchor.band}` : "—"} mono />
              <Row
                label={t("signal.anchorSignal", "Signal")}
                value={[
                  anchor.rsrp != null ? `RSRP ${anchor.rsrp}` : null,
                  anchor.rsrq != null ? `RSRQ ${anchor.rsrq}` : null,
                  anchor.sinr != null ? `SINR ${anchor.sinr}` : null,
                  anchor.rssi != null ? `RSSI ${anchor.rssi}` : null,
                ]
                  .filter(Boolean)
                  .join(" · ") || "—"}
              />
            </Group>
          )}
          <Group>
            <Row icon={CellTower} label={t("signal.toBandlock", "Band lock")} href="/bandlock" />
            <Row icon={Broadcast} label={t("nav.signalDetect", "Signal Detect")} href="/router/signal-detect" />
          </Group>
        </div>
      </div>
    </>
  );
}

/** 150 → "150", 12.5 → "12.5". */
function fmtMbps(v: number): string {
  return v >= 100 ? v.toFixed(0) : String(Math.round(v * 10) / 10);
}
