"use client";

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button } from "@/components/admin/Button";

interface SignalData {
  nr5g_band?: string;
  lte_band?: string;
  nr5g_type?: string;
}

const NR_BANDS = ["n1", "n3", "n5", "n7", "n8", "n28", "n38", "n40", "n41", "n66", "n71", "n77", "n78", "n79"];
const LTE_BANDS = ["B1", "B2", "B3", "B4", "B5", "B7", "B8", "B12", "B17", "B20", "B28", "B38", "B40", "B41"];

export default function BandLockPage() {
  const { t } = useTranslation();
  const { data: sig } = useApi<SignalData>("/api/network/signal");

  const [nrSelected, setNrSelected] = useState<Set<string>>(new Set());
  const [lteSelected, setLteSelected] = useState<Set<string>>(new Set());
  const [nrMode, setNrMode] = useState<"nsa" | "sa">("nsa");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [msgIsErr, setMsgIsErr] = useState(false);

  function showMsg(text: string, isErr: boolean) {
    setMsg(text);
    setMsgIsErr(isErr);
  }

  function toggleNR(band: string) {
    setNrSelected((prev) => {
      const next = new Set(prev);
      if (next.has(band)) next.delete(band); else next.add(band);
      return next;
    });
  }

  function toggleLTE(band: string) {
    setLteSelected((prev) => {
      const next = new Set(prev);
      if (next.has(band)) next.delete(band); else next.add(band);
      return next;
    });
  }

  async function applyNR() {
    if (nrSelected.size === 0) { showMsg(t("bandlock.selectNrBand", "Select at least one NR band"), true); return; }
    setBusy(true);
    const bandStr = Array.from(nrSelected).map((b) => b.replace("n", "")).join(",");
    try {
      await apiFetch("/api/cell/band/nr", { method: "POST", body: { nr5g_type: "nsa", nr5g_band: bandStr } });
      await apiFetch("/api/cell/band/nr", { method: "POST", body: { nr5g_type: "sa", nr5g_band: bandStr } });
      showMsg(t("bandlock.nrLocked", "NR bands locked: {{bands}}", { bands: bandStr }), false);
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function applyLTE() {
    if (lteSelected.size === 0) { showMsg(t("bandlock.selectLteBand", "Select at least one LTE band"), true); return; }
    setBusy(true);
    const bandStr = Array.from(lteSelected).map((b) => b.replace("B", "")).join(",");
    try {
      await apiFetch("/api/cell/band/lte", {
        method: "POST",
        body: { is_lte_band: "1", lte_band_mask: bandStr, is_gw_band: "0", gw_band_mask: "" },
      });
      showMsg(t("bandlock.lteLocked", "LTE bands locked: {{bands}}", { bands: bandStr }), false);
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function resetAll() {
    if (!confirm(t("bandlock.confirmReset", "Reset all band locks? The device will switch to automatic band selection."))) return;
    setBusy(true);
    try {
      await apiFetch("/api/cell/band/reset", { method: "POST" });
      setNrSelected(new Set());
      setLteSelected(new Set());
      showMsg(t("bandlock.allUnlocked", "All bands unlocked"), false);
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <PageHeader
        title={t("bandlock.title", "Band Lock")}
        description={t("bandlock.desc", "Lock the modem to specific NR or LTE bands.")}
        actions={
          <Button variant="danger" size="sm" loading={busy} onClick={resetAll}>
            {t("bandlock.resetUnlockAll", "Reset / Unlock All")}
          </Button>
        }
      />

      {sig?.nr5g_band && (
        <p className="mb-3 text-sm text-text-dim">
          {t("bandlock.currentNrBand", "Current NR band:")} <span className="font-mono text-text">{sig.nr5g_band}</span>
          {sig.lte_band && (
            <> &nbsp;|&nbsp; {t("bandlock.lteBandLabel", "LTE band:")} <span className="font-mono text-text">{sig.lte_band}</span></>
          )}
        </p>
      )}

      {msg && (
        <div className="mb-3">
          {msgIsErr ? (
            <ErrorBanner message={msg} />
          ) : (
            <div className="rounded-md border border-success/40 bg-success/10 px-3 py-2 text-sm text-success">{msg}</div>
          )}
        </div>
      )}

      <div className="grid gap-4 md:grid-cols-2">
        <SectionCard title={t("bandlock.nrCardTitle", "NR (5G) Bands")}>
          <div className="mb-3 flex gap-3">
            <label className="flex items-center gap-1.5 text-sm">
              <input
                type="radio"
                name="nr-mode"
                value="nsa"
                checked={nrMode === "nsa"}
                onChange={() => setNrMode("nsa")}
                className="accent-accent"
              />
              NSA
            </label>
            <label className="flex items-center gap-1.5 text-sm">
              <input
                type="radio"
                name="nr-mode"
                value="sa"
                checked={nrMode === "sa"}
                onChange={() => setNrMode("sa")}
                className="accent-accent"
              />
              SA
            </label>
          </div>
          <div className="mb-4 flex flex-wrap gap-2">
            {NR_BANDS.map((band) => (
              <label
                key={band}
                className="flex cursor-pointer items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs transition hover:border-accent"
              >
                <input
                  type="checkbox"
                  className="accent-accent"
                  checked={nrSelected.has(band)}
                  onChange={() => toggleNR(band)}
                />
                {band}
              </label>
            ))}
          </div>
          <div className="flex items-center gap-2">
            <Button size="sm" loading={busy} onClick={applyNR} disabled={nrSelected.size === 0}>
              {t("bandlock.applyNrLock", "Apply NR Lock")}
            </Button>
            <span className="text-xs text-text-dim">
              {nrSelected.size > 0 ? t("bandlock.nSelected", "{{n}} selected", { n: nrSelected.size }) : t("bandlock.noneSelected", "None selected")}
            </span>
          </div>
        </SectionCard>

        <SectionCard title={t("bandlock.lteCardTitle", "LTE Bands")}>
          <div className="mb-4 flex flex-wrap gap-2">
            {LTE_BANDS.map((band) => (
              <label
                key={band}
                className="flex cursor-pointer items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs transition hover:border-accent"
              >
                <input
                  type="checkbox"
                  className="accent-accent"
                  checked={lteSelected.has(band)}
                  onChange={() => toggleLTE(band)}
                />
                {band}
              </label>
            ))}
          </div>
          <div className="flex items-center gap-2">
            <Button size="sm" loading={busy} onClick={applyLTE} disabled={lteSelected.size === 0}>
              {t("bandlock.applyLteLock", "Apply LTE Lock")}
            </Button>
            <span className="text-xs text-text-dim">
              {lteSelected.size > 0 ? t("bandlock.nSelected", "{{n}} selected", { n: lteSelected.size }) : t("bandlock.noneSelected", "None selected")}
            </span>
          </div>
        </SectionCard>
      </div>
    </>
  );
}
