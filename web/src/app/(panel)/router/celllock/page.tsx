"use client";

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Input } from "@/components/admin/Button";

interface SignalData {
  network_type?: string;
  nr5g_pci?: number;
  nr5g_action_channel?: number;
  nr5g_action_band?: string;
  nr5g_cell_id?: number;
  lte_pci?: number;
  lte_earfcn?: number;
  lte_band?: string;
  lte_cell_id?: number;
  nr5g_rsrp?: number;
}

export default function CellLockPage() {
  const { t } = useTranslation();
  const { data: sig } = useApi<SignalData>("/api/network/signal", { refreshInterval: 5000 });

  const [nrPCI, setNrPCI] = useState("");
  const [nrEARFCN, setNrEARFCN] = useState("");
  const [nrBand, setNrBand] = useState("");
  const [ltePCI, setLtePCI] = useState("");
  const [lteEARFCN, setLteEARFCN] = useState("");

  const [nrNeighbors, setNrNeighbors] = useState<Record<string, string>[]>([]);
  const [lteNeighbors, setLteNeighbors] = useState<Record<string, string>[]>([]);
  const [scanning, setScanning] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [msgIsErr, setMsgIsErr] = useState(false);

  function showMsg(text: string, isErr: boolean) {
    setMsg(text);
    setMsgIsErr(isErr);
  }

  async function lockNR() {
    if (!nrPCI || !nrEARFCN) { showMsg(t("celllock.pciEarfcnRequired", "PCI and EARFCN are required"), true); return; }
    setBusy(true);
    try {
      const params: Record<string, string> = { pci: nrPCI, earfcn: nrEARFCN };
      if (nrBand) params.band = nrBand;
      await apiFetch("/api/cell/lock/nr", { method: "POST", body: params });
      showMsg(t("celllock.nrCellLocked", "NR cell locked"), false);
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function lockLTE() {
    if (!ltePCI || !lteEARFCN) { showMsg(t("celllock.pciEarfcnRequired", "PCI and EARFCN are required"), true); return; }
    setBusy(true);
    try {
      await apiFetch("/api/cell/lock/lte", { method: "POST", body: { pci: ltePCI, earfcn: lteEARFCN } });
      showMsg(t("celllock.lteCellLocked", "LTE cell locked"), false);
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function scanNeighbors() {
    setScanning(true);
    setNrNeighbors([]);
    setLteNeighbors([]);
    try {
      await apiFetch("/api/cell/neighbors/scan", { method: "POST" });
      await new Promise((r) => setTimeout(r, 3000));

      try {
        const nr = await apiFetch<Record<string, unknown>>("/api/cell/neighbors/nr");
        if (nr && typeof nr === "object") {
          const cells = extractCells(nr);
          setNrNeighbors(cells);
        }
      } catch { /* ignore */ }

      try {
        const lte = await apiFetch<Record<string, unknown>>("/api/cell/neighbors/lte");
        if (lte && typeof lte === "object") {
          const cells = extractCells(lte);
          setLteNeighbors(cells);
        }
      } catch { /* ignore */ }

      showMsg(t("celllock.scanComplete", "Scan complete"), false);
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setScanning(false);
    }
  }

  function extractCells(data: Record<string, unknown>): Record<string, string>[] {
    // Try common shapes: array at data.cells, data.list, or flat object
    const tryArr = data.cells ?? data.list;
    if (Array.isArray(tryArr)) {
      return tryArr.map((item) =>
        Object.fromEntries(Object.entries(item as Record<string, unknown>).map(([k, v]) => [k, String(v)]))
      );
    }
    // fallback: treat top-level as single row
    return [Object.fromEntries(Object.entries(data).map(([k, v]) => [k, String(v)]))];
  }

  async function unlockAll() {
    if (!confirm(t("celllock.confirmReset", "Reset all cell locks?"))) return;
    setBusy(true);
    try {
      await apiFetch("/api/cell/lock/reset", { method: "POST" });
      showMsg(t("celllock.cellLockReset", "Cell lock reset"), false);
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  function autofillNR(row: Record<string, string>) {
    setNrPCI(row.pci ?? row.nr_pci ?? "");
    setNrEARFCN(row.earfcn ?? row.nr_earfcn ?? "");
    setNrBand(row.band ?? "");
  }

  function autofillLTE(row: Record<string, string>) {
    setLtePCI(row.pci ?? row.lte_pci ?? "");
    setLteEARFCN(row.earfcn ?? row.lte_earfcn ?? "");
  }

  return (
    <>
      <PageHeader
        title={t("celllock.title", "Cell Lock")}
        description={t("celllock.desc", "Lock the modem to a specific NR or LTE cell.")}
        actions={
          <Button variant="danger" size="sm" loading={busy} onClick={unlockAll}>
            {t("celllock.unlockAll", "Unlock All")}
          </Button>
        }
      />

      <SectionCard title={t("celllock.currentCell", "Current Cell")} className="mb-4">
        <div className="grid grid-cols-2 gap-x-8 gap-y-1 text-sm sm:grid-cols-4">
          {((): [string, string | number | undefined][] => {
            const isNR = (sig?.network_type || "").toUpperCase().includes("SA") || (sig?.nr5g_rsrp ?? 0) !== 0;
            return [
              ["PCI", isNR ? sig?.nr5g_pci : sig?.lte_pci],
              ["EARFCN", isNR ? sig?.nr5g_action_channel : sig?.lte_earfcn],
              ["Band", isNR ? sig?.nr5g_action_band : sig?.lte_band],
              [t("celllock.cellId", "Cell ID"), isNR ? sig?.nr5g_cell_id : sig?.lte_cell_id],
            ];
          })().map(([k, v]) => (
            <div key={k}>
              <span className="text-text-dim">{k}: </span>
              <span className="font-mono">{v ?? "—"}</span>
            </div>
          ))}
        </div>
      </SectionCard>

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
        <SectionCard title={t("celllock.lockNrCell", "Lock NR Cell")}>
          <div className="space-y-3">
            <div>
              <label className="mb-1 block text-xs text-text-dim">PCI *</label>
              <Input value={nrPCI} onChange={(e) => setNrPCI(e.target.value)} placeholder={t("celllock.placeholderNrPci", "e.g. 123")} />
            </div>
            <div>
              <label className="mb-1 block text-xs text-text-dim">EARFCN *</label>
              <Input value={nrEARFCN} onChange={(e) => setNrEARFCN(e.target.value)} placeholder={t("celllock.placeholderNrEarfcn", "e.g. 627264")} />
            </div>
            <div>
              <label className="mb-1 block text-xs text-text-dim">{t("celllock.bandOptional", "Band (optional)")}</label>
              <Input value={nrBand} onChange={(e) => setNrBand(e.target.value)} placeholder={t("celllock.placeholderBand", "e.g. 78")} />
            </div>
            <Button size="sm" loading={busy} onClick={lockNR}>
              {t("celllock.lockNr", "Lock NR")}
            </Button>
          </div>
        </SectionCard>

        <SectionCard title={t("celllock.lockLteCell", "Lock LTE Cell")}>
          <div className="space-y-3">
            <div>
              <label className="mb-1 block text-xs text-text-dim">PCI *</label>
              <Input value={ltePCI} onChange={(e) => setLtePCI(e.target.value)} placeholder={t("celllock.placeholderLtePci", "e.g. 456")} />
            </div>
            <div>
              <label className="mb-1 block text-xs text-text-dim">EARFCN *</label>
              <Input value={lteEARFCN} onChange={(e) => setLteEARFCN(e.target.value)} placeholder={t("celllock.placeholderLteEarfcn", "e.g. 1300")} />
            </div>
            <Button size="sm" loading={busy} onClick={lockLTE}>
              {t("celllock.lockLte", "Lock LTE")}
            </Button>
          </div>
        </SectionCard>
      </div>

      <div className="mt-4">
        <div className="mb-3 flex items-center gap-3">
          <h3 className="font-display text-sm font-semibold">{t("celllock.neighborCells", "Neighbor Cells")}</h3>
          <Button variant="outline" size="sm" loading={scanning} onClick={scanNeighbors}>
            {scanning ? t("celllock.scanning", "Scanning…") : t("celllock.scanNeighbors", "Scan Neighbors")}
          </Button>
        </div>

        {nrNeighbors.length > 0 && (
          <SectionCard title={t("celllock.nrNeighbors", "NR Neighbors")} className="mb-4">
            <NeighborTable rows={nrNeighbors} onSelect={autofillNR} />
          </SectionCard>
        )}

        {lteNeighbors.length > 0 && (
          <SectionCard title={t("celllock.lteNeighbors", "LTE Neighbors")}>
            <NeighborTable rows={lteNeighbors} onSelect={autofillLTE} />
          </SectionCard>
        )}

        {!scanning && nrNeighbors.length === 0 && lteNeighbors.length === 0 && (
          <p className="text-sm text-text-dim">{t("celllock.scanHint", 'Click "Scan Neighbors" to discover nearby cells.')}</p>
        )}
      </div>
    </>
  );
}

function NeighborTable({
  rows,
  onSelect,
}: {
  rows: Record<string, string>[];
  onSelect: (row: Record<string, string>) => void;
}) {
  const { t } = useTranslation();
  if (rows.length === 0) return null;
  const keys = Object.keys(rows[0]);
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-xs">
        <thead>
          <tr className="border-b border-border text-left text-text-dim">
            {keys.map((k) => (
              <th key={k} className="pb-2 pr-3 font-medium">{k}</th>
            ))}
            <th className="pb-2 font-medium">{t("celllock.use", "Use")}</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr key={i} className="border-b border-border/50 last:border-0 hover:bg-bg-elevated/50">
              {keys.map((k) => (
                <td key={k} className="py-1.5 pr-3 font-mono">{row[k] ?? "—"}</td>
              ))}
              <td className="py-1.5">
                <button
                  className="text-accent underline-offset-2 hover:underline"
                  onClick={() => onSelect(row)}
                >
                  {t("celllock.select", "Select")}
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
