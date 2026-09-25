"use client";
// Cell lock. Status first (the cell the modem is camped on now — the agent
// has no GET for the lock itself), then the NR / LTE lock forms and the
// reset, then the neighbour scan that fills the forms. ≥1024: two columns.
//
// Writes (controls-inventory §/router/celllock, design §3.1):
//   lock NR, lock LTE, unlock all  tier 3, cutsUplink, waitDevice 30 s.
//     No readback exists, so success reads "accepted by the device" (R6);
//     afterwards the page compares the camped cell with what was locked.
//   neighbour scan                  tier 2; steps ① POST scan ② read the
//     NR + LTE lists (first after 3 s as before, then every 3 s up to 30 s
//     while both are still empty — the job keeps running in a hidden tab).
//     Read errors are reported now (the old page swallowed them).
// Neighbour lists are ubus passthroughs of unconfirmed shape: rows come from
// cells[] / list[], else the object is one row; every key is a column.
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { CellTower, LockSimple, MagnifyingGlass } from "@phosphor-icons/react";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { useWriteOp } from "@/lib/api/writeOp";
import type { NetInfo, NetworkSignal } from "@/lib/api/schemas/network";
import type { CellLockLteBody, CellLockNrBody, CellNeighborsLte, CellNeighborsNr } from "@/lib/api/schemas/modem";
import { carriers } from "@/lib/home";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  Group,
  GroupTitle,
  OpResult,
  Row,
  StatusMark,
  useConfirmInline,
} from "@/components/nd";

type Rows = Record<string, string>[];
type Pending = null | "nr" | "lte" | "reset";

const SCAN_FIRST_MS = 3000;
const SCAN_EVERY_MS = 3000;
const SCAN_TRIES = 10;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function cellText(v: unknown): string {
  if (v == null || v === "") return "—";
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

/** cells[] / list[], else the object as one row; null / {} = no rows. */
function extractCells(data: CellNeighborsNr | CellNeighborsLte | null | undefined): Rows {
  if (!data || typeof data !== "object") return [];
  const arr = data.cells ?? data.list;
  if (Array.isArray(arr)) {
    return arr
      .filter((x): x is Record<string, unknown> => !!x && typeof x === "object")
      .map((item) => Object.fromEntries(Object.entries(item).map(([k, v]) => [k, cellText(v)])));
  }
  const entries = Object.entries(data);
  if (entries.length === 0) return [];
  return [Object.fromEntries(entries.map(([k, v]) => [k, cellText(v)]))];
}

const pick = (row: Record<string, string>, ...keys: string[]) => {
  for (const k of keys) {
    const v = row[k];
    if (v !== undefined && v !== "—") return v;
  }
  return "";
};

export default function CellLockPage() {
  const { t } = useTranslation();
  const sig = useApi<NetworkSignal>("/api/network/signal", { refreshInterval: 5000 });
  const s = sig.data;
  const serving = useMemo(() => carriers(s)[0], [s]);
  const isNr = serving?.kind === "nr";
  // Schema fix: LTE cell id is `cell_id` (the page read `lte_cell_id`),
  // LTE EARFCN is `wan_active_channel` and LTE band `wan_active_band`
  // (via carriers()); `lte_earfcn` / `lte_band` are not the serving values.
  const cellId = serving ? (isNr ? s?.nr5g_cell_id : s?.cell_id ?? s?.lte_cell_id) : undefined;
  const band = serving?.band ? `${isNr ? "n" : "B"}${serving.band}` : null;

  const [nrPCI, setNrPCI] = useState("");
  const [nrEARFCN, setNrEARFCN] = useState("");
  const [nrBand, setNrBand] = useState("");
  const [ltePCI, setLtePCI] = useState("");
  const [lteEARFCN, setLteEARFCN] = useState("");
  const [dialog, setDialog] = useState<Pending>(null);

  // What was sent (the op reads these when it runs).
  const nrRef = useRef<CellLockNrBody>({ pci: "", earfcn: "" });
  const lteRef = useRef<CellLockLteBody>({ pci: "", earfcn: "" });
  const [locked, setLocked] = useState<{ kind: "nr" | "lte"; pci: string } | null>(null);

  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const recovery = t(
    "celllock.recovery",
    "Press “Unlock all” on this page. If the page can't be reached, open cell lock on the device's touchscreen and restore the default, or reboot the device."
  );
  const wait = { expectedSec: 30, recovery };

  const nrOp = useWriteOp({
    tier: 3,
    steps: [{ label: t("celllock.lockNr", "Lock NR"), run: () => apiFetch("/api/cell/lock/nr", { method: "POST", body: nrRef.current }) }],
    waitDevice: wait,
  });
  const lteOp = useWriteOp({
    tier: 3,
    steps: [{ label: t("celllock.lockLte", "Lock LTE"), run: () => apiFetch("/api/cell/lock/lte", { method: "POST", body: lteRef.current }) }],
    waitDevice: wait,
  });
  const resetOp = useWriteOp({
    tier: 3,
    steps: [{ label: t("celllock.unlockAll", "Unlock All"), run: () => apiFetch("/api/cell/lock/reset", { method: "POST" }) }],
    waitDevice: { expectedSec: 30 },
  });

  // ── neighbour scan ──
  const [nrRows, setNrRows] = useState<Rows | null>(null);
  const [lteRows, setLteRows] = useState<Rows | null>(null);
  const [scanAsk, setScanAsk] = useState(false);
  const scanInline = useConfirmInline(scanAsk);
  // The agent refuses the stock neighbour scan (it cuts mobile data and finds
  // nothing); the reason comes with /api/netinfo so the button can say so.
  const ni = useApi<NetInfo>("/api/netinfo?lite=1", { refreshInterval: 60000 });
  const nbrOff = ni.data?.neighbors?.state === "unsupported";
  const scanOp = useWriteOp({
    tier: 2,
    steps: [
      { label: t("celllock.stepScan", "Start scan"), run: () => apiFetch("/api/cell/neighbors/scan", { method: "POST" }) },
      {
        label: t("celllock.stepRead", "Read neighbours"),
        run: async () => {
          await sleep(SCAN_FIRST_MS);
          for (let i = 0; ; i++) {
            if (!alive.current) throw new Error("page closed");
            const [nr, lte] = await Promise.allSettled([
              apiFetch<CellNeighborsNr | null>("/api/cell/neighbors/nr"),
              apiFetch<CellNeighborsLte | null>("/api/cell/neighbors/lte"),
            ]);
            const nrR = nr.status === "fulfilled" ? extractCells(nr.value) : null;
            const lteR = lte.status === "fulfilled" ? extractCells(lte.value) : null;
            if (nrR) setNrRows(nrR);
            if (lteR) setLteRows(lteR);
            const found = (nrR?.length ?? 0) + (lteR?.length ?? 0) > 0;
            const failed = [nr, lte].find((r): r is PromiseRejectedResult => r.status === "rejected");
            if ((found && !failed) || i >= SCAN_TRIES - 1) {
              if (failed) throw failed.reason;
              return;
            }
            await sleep(SCAN_EVERY_MS);
          }
        },
      },
    ],
  });

  const busy = nrOp.busy || lteOp.busy || resetOp.busy;

  function confirm(which: Exclude<Pending, null>) {
    setDialog(null);
    if (which === "nr") {
      const body: CellLockNrBody = { pci: nrPCI.trim(), earfcn: nrEARFCN.trim() };
      if (nrBand.trim()) body.band = nrBand.trim();
      nrRef.current = body;
      setLocked({ kind: "nr", pci: body.pci });
      nrOp.start();
      nrOp.confirm();
    } else if (which === "lte") {
      lteRef.current = { pci: ltePCI.trim(), earfcn: lteEARFCN.trim() };
      setLocked({ kind: "lte", pci: lteRef.current.pci });
      lteOp.start();
      lteOp.confirm();
    } else {
      setLocked(null);
      resetOp.start();
      resetOp.confirm();
    }
  }

  function fillNr(row: Record<string, string>) {
    setNrPCI(pick(row, "pci", "nr_pci"));
    setNrEARFCN(pick(row, "earfcn", "nr_earfcn", "arfcn"));
    setNrBand(pick(row, "band"));
  }
  function fillLte(row: Record<string, string>) {
    setLtePCI(pick(row, "pci", "lte_pci"));
    setLteEARFCN(pick(row, "earfcn", "lte_earfcn"));
  }

  /** After an accepted lock: is the modem on the locked cell yet? */
  function matchLine(kind: "nr" | "lte") {
    if (!locked || locked.kind !== kind || !serving) return null;
    const cur = serving.pci != null ? String(serving.pci) : null;
    if (cur === null) return null;
    return (
      <p className="nd-aux">
        {cur === locked.pci ? (
          <StatusMark tone="ok">{t("celllock.onLockedCell", "The modem is now on PCI {{pci}}", { pci: cur })}</StatusMark>
        ) : (
          <StatusMark tone="neutral">
            {t("celllock.notYetOnCell", "The modem is still on PCI {{cur}} (locked {{pci}})", { cur, pci: locked.pci })}
          </StatusMark>
        )}
      </p>
    );
  }

  const nrReady = nrPCI.trim() !== "" && nrEARFCN.trim() !== "";
  const lteReady = ltePCI.trim() !== "" && lteEARFCN.trim() !== "";
  const scanned = nrRows !== null || lteRows !== null;
  const downtime = t("celllock.downtime", "The mobile connection drops for about 30 seconds while the modem re-attaches. If the cell isn't reachable, it stays off until the lock is removed.");

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("celllock.title", "Cell Lock")}</h1>

      <div className="grid gap-4 lg:grid-cols-2 lg:gap-6">
        <div className="grid content-start gap-4">
          {/* ── current cell ── */}
          <section>
            <Group title={t("celllock.currentCell", "Current Cell")} stale={sig.stale}>
              <Row icon={CellTower} label={t("celllock.rat", "Network")} value={s ? (serving ? (isNr ? "NR" : "LTE") : t("celllock.noCell", "No serving cell")) : "—"} />
              <Row label="PCI" value={serving?.pci ?? "—"} mono />
              <Row label="EARFCN" value={serving?.arfcn ?? "—"} mono />
              <Row label={t("celllock.band", "Band")} value={band ?? "—"} mono />
              <Row label={t("celllock.cellId", "Cell ID")} value={cellId ?? "—"} mono />
            </Group>
            <div className="mt-1 px-1">
              {sig.stale && <Freshness stale lastOkAt={sig.lastOkAt} />}
              {!s && sig.error && (
                <p role="alert" className="nd-aux flex flex-wrap items-center gap-3 text-nd-badT">
                  {t("celllock.signalErr", "Can't read the current cell: {{e}}", { e: sig.error.message ?? "" })}
                  <Button variant="secondary" size="sm" onPress={() => sig.mutate()}>
                    {t("common.retry", "Retry")}
                  </Button>
                </p>
              )}
              <p className="nd-aux mt-1">
                {t("celllock.currentNote", "This is the cell the modem is using now, not the lock setting — the device can't report its lock.")}
              </p>
            </div>
          </section>

          {/* ── NR ── */}
          <section aria-labelledby="cl-nr">
            <GroupTitle id="cl-nr">{t("celllock.lockNrCell", "Lock NR Cell")}</GroupTitle>
            <div className="nd-group grid gap-3 p-4 lg:p-5">
              <Field label={t("celllock.nrPci", "NR PCI")} value={nrPCI} onChange={setNrPCI} placeholder={t("celllock.placeholderNrPci", "e.g. 123")} disabled={busy} />
              <Field label={t("celllock.nrEarfcn", "NR EARFCN")} value={nrEARFCN} onChange={setNrEARFCN} placeholder={t("celllock.placeholderNrEarfcn", "e.g. 627264")} disabled={busy} />
              <Field label={t("celllock.nrBand", "NR band (optional)")} value={nrBand} onChange={setNrBand} placeholder={t("celllock.placeholderBand", "e.g. 78")} disabled={busy} />
              <div className="flex flex-wrap items-center gap-3">
                <Button onPress={() => setDialog("nr")} isDisabled={!nrReady || busy} pending={nrOp.busy}>
                  <LockSimple size={20} weight="bold" aria-hidden />
                  {t("celllock.lockNr", "Lock NR")}
                </Button>
                {!nrReady && <span className="nd-aux">{t("celllock.pciEarfcnRequired", "PCI and EARFCN are required")}</span>}
              </div>
              <OpResult op={nrOp} />
              {(nrOp.phase === "accepted" || nrOp.phase === "applied") && matchLine("nr")}
            </div>
          </section>

          {/* ── LTE ── */}
          <section aria-labelledby="cl-lte">
            <GroupTitle id="cl-lte">{t("celllock.lockLteCell", "Lock LTE Cell")}</GroupTitle>
            <div className="nd-group grid gap-3 p-4 lg:p-5">
              <Field label={t("celllock.ltePci", "LTE PCI")} value={ltePCI} onChange={setLtePCI} placeholder={t("celllock.placeholderLtePci", "e.g. 456")} disabled={busy} />
              <Field label={t("celllock.lteEarfcn", "LTE EARFCN")} value={lteEARFCN} onChange={setLteEARFCN} placeholder={t("celllock.placeholderLteEarfcn", "e.g. 1300")} disabled={busy} />
              <div className="flex flex-wrap items-center gap-3">
                <Button onPress={() => setDialog("lte")} isDisabled={!lteReady || busy} pending={lteOp.busy}>
                  <LockSimple size={20} weight="bold" aria-hidden />
                  {t("celllock.lockLte", "Lock LTE")}
                </Button>
                {!lteReady && <span className="nd-aux">{t("celllock.pciEarfcnRequired", "PCI and EARFCN are required")}</span>}
              </div>
              <OpResult op={lteOp} />
              {(lteOp.phase === "accepted" || lteOp.phase === "applied") && matchLine("lte")}
            </div>
          </section>

          {/* ── reset ── */}
          <section aria-labelledby="cl-reset">
            <GroupTitle id="cl-reset">{t("celllock.resetTitle", "Automatic cell selection")}</GroupTitle>
            <div className="nd-group grid gap-3 p-4 lg:p-5">
              <p className="nd-body text-nd-t2">{t("celllock.resetDesc", "Remove every NR and LTE cell lock and let the modem choose cells again.")}</p>
              <div>
                <Button variant="secondary" onPress={() => setDialog("reset")} isDisabled={busy} pending={resetOp.busy}>
                  {t("celllock.unlockAll", "Unlock All")}
                </Button>
              </div>
              <OpResult op={resetOp} />
            </div>
          </section>
        </div>

        {/* ── neighbours ── */}
        <div className="grid content-start gap-4">
          <section aria-labelledby="cl-nbr">
            <GroupTitle id="cl-nbr">{t("celllock.neighborCells", "Neighbor Cells")}</GroupTitle>
            <div className="flex flex-wrap items-center gap-3">
              <span {...scanInline.triggerProps}>
                <Button variant="secondary" onPress={() => setScanAsk((o) => !o)} isDisabled={scanOp.busy || nbrOff} pending={scanOp.busy}>
                  <MagnifyingGlass size={20} weight="bold" aria-hidden />
                  {scanOp.busy ? t("celllock.scanning", "Scanning…") : t("celllock.scanNeighbors", "Scan Neighbors")}
                </Button>
              </span>
            </div>
            <ConfirmInline
              id={scanInline.id}
              open={scanAsk}
              actionLabel={t("celllock.scanNeighbors", "Scan Neighbors")}
              consequence={t("celllock.scanConsequence", "The modem measures nearby cells. Mobile data may drop while it scans.")}
              onCancel={() => setScanAsk(false)}
              onConfirm={() => {
                setScanAsk(false);
                setNrRows(null);
                setLteRows(null);
                scanOp.start();
                scanOp.confirm();
              }}
            />
            <div className="mt-2 grid gap-1 px-1">
              {scanOp.busy ? (
                <p className="nd-aux" role="status">
                  {t("celllock.scanWait", "Scanning — this can take up to 30 seconds.")}
                </p>
              ) : scanOp.phase === "accepted" ? (
                <p className="nd-aux" role="status">
                  {t("celllock.scanComplete", "Scan complete")}
                </p>
              ) : (
                <OpResult op={scanOp} />
              )}
            </div>

            {nbrOff ? (
              <p className="nd-body mt-3 px-1 text-nd-t2">
                {t("celllock.scanOff", "Neighbour scan is off: the stock scan drops mobile data for minutes and returns no cells on this firmware.")}
              </p>
            ) : (
              !scanned &&
              !scanOp.busy && (
                <p className="nd-body mt-3 px-1 text-nd-t2">{t("celllock.scanHint", "Click \"Scan Neighbors\" to discover nearby cells.")}</p>
              )
            )}
            {scanned && !scanOp.busy && (nrRows?.length ?? 0) === 0 && (lteRows?.length ?? 0) === 0 && (
              <p className="nd-body mt-3 px-1 text-nd-t2">
                {t("celllock.noNeighbors", "No neighbour cells reported · move the device or scan again.")}
              </p>
            )}
            {nrRows && nrRows.length > 0 && (
              <NeighborTable title={t("celllock.nrNeighbors", "NR Neighbors")} rows={nrRows} onSelect={fillNr} />
            )}
            {lteRows && lteRows.length > 0 && (
              <NeighborTable title={t("celllock.lteNeighbors", "LTE Neighbors")} rows={lteRows} onSelect={fillLte} />
            )}
          </section>
        </div>
      </div>

      <ConfirmDialog
        open={dialog === "nr"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("celllock.confirmNrTitle", "Lock NR to PCI {{pci}}?", { pci: nrPCI.trim() })}
        what={t("celllock.confirmNrWhat", "5G only uses the cell PCI {{pci}} on EARFCN {{earfcn}}{{band}}. Other NR cells stop being used.", {
          pci: nrPCI.trim(),
          earfcn: nrEARFCN.trim(),
          band: nrBand.trim() ? ` (n${nrBand.trim().replace(/^n/i, "")})` : "",
        })}
        downtime={downtime}
        recovery={recovery}
        actionLabel={t("celllock.lockNr", "Lock NR")}
        cutsUplink
        onConfirm={() => confirm("nr")}
      />
      <ConfirmDialog
        open={dialog === "lte"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("celllock.confirmLteTitle", "Lock LTE to PCI {{pci}}?", { pci: ltePCI.trim() })}
        what={t("celllock.confirmLteWhat", "4G only uses the cell PCI {{pci}} on EARFCN {{earfcn}}. Other LTE cells stop being used.", {
          pci: ltePCI.trim(),
          earfcn: lteEARFCN.trim(),
        })}
        downtime={downtime}
        recovery={recovery}
        actionLabel={t("celllock.lockLte", "Lock LTE")}
        cutsUplink
        onConfirm={() => confirm("lte")}
      />
      <ConfirmDialog
        open={dialog === "reset"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("celllock.confirmReset", "Reset all cell locks?")}
        what={t("celllock.confirmResetWhat", "Every NR and LTE cell lock is removed. Judging by the firmware call's name it may also reset band locks (not confirmed).")}
        downtime={t("bandlock.downtime", "The mobile connection drops for about 30 seconds while the modem re-attaches.")}
        actionLabel={t("celllock.unlockAll", "Unlock All")}
        cutsUplink
        onConfirm={() => confirm("reset")}
      />
    </>
  );
}

function Field({
  label,
  value,
  onChange,
  placeholder,
  disabled,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder: string;
  disabled?: boolean;
}) {
  return (
    <label className="grid gap-1">
      <span className="nd-aux font-semibold text-nd-t2">{label}</span>
      <input
        className="nd-field nd-mono"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        inputMode="numeric"
        autoComplete="off"
        spellCheck={false}
        disabled={disabled}
      />
    </label>
  );
}

function NeighborTable({
  title,
  rows,
  onSelect,
}: {
  title: string;
  rows: Rows;
  onSelect: (row: Record<string, string>) => void;
}) {
  const { t } = useTranslation();
  const keys = Array.from(new Set(rows.flatMap((r) => Object.keys(r))));
  return (
    <div className="mt-4">
      <h3 className="nd-group-title">{title}</h3>
      <div className="nd-group">
        <div className="overflow-x-auto p-2 lg:p-3">
          <table className="w-full text-[14px] leading-5">
            <thead>
              <tr className="text-left text-nd-t2">
                {keys.map((k) => (
                  <th key={k} scope="col" className="px-2 py-2 font-semibold">
                    {k}
                  </th>
                ))}
                <th scope="col" className="px-2 py-2 font-semibold">
                  {t("celllock.use", "Use")}
                </th>
              </tr>
            </thead>
            <tbody>
              {rows.map((row, i) => (
                <tr key={i} className="border-t border-nd-sep">
                  {keys.map((k) => (
                    <td key={k} className="nd-mono whitespace-nowrap px-2 py-2 tabular-nums">
                      {row[k] ?? "—"}
                    </td>
                  ))}
                  <td className="px-2 py-1">
                    <Button
                      variant="ghost"
                      onPress={() => onSelect(row)}
                      aria-label={t("celllock.useRow", "Fill the form with PCI {{pci}}", { pci: row.pci ?? row.nr_pci ?? row.lte_pci ?? String(i + 1) })}
                    >
                      {t("celllock.select", "Select")}
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}
