"use client";
// Band lock (sample path 3/3). Every write here is tier 3: a dialog that
// says what happens, how long the uplink drops and how to undo it, with the
// Tailscale warning when remote. Writes go through useWriteOp:
//
//   NR lock   = step NSA → step SA (R10; each tracked, resubmit sends only
//               the unfinished one; the scope segment picks which apply —
//               both by default, which is what the page always did)
//   LTE lock  = one step
//   reset     = one step
//
// Readback (E4 T9c): GET /api/cell/band/lock gives the lock sets the firmware
// keeps (uci zte_nwinfo.band_lock.*) and the defaults a reset goes back to;
// after a write the page re-reads it (5 × 2 s) and compares as sets. When the
// firmware has none of those options the write reads "accepted" as before.
// Whether the firmware updates them at once is checked on the device (T12).
// Reset goes through datad band.reset = nwinfo_reset_band_cell_setting, which
// also clears cell locks (D22); the texts say so. datad stuck: writes off (DD8).
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { useWriteOp, type WriteStep } from "@/lib/api/writeOp";
import type { NetworkSignal } from "@/lib/api/schemas/network";
import { Button, Chips, ConfirmDialog, Group, OpResult, Row, Segmented } from "@/components/nd";
import { carriers } from "@/lib/home";
import { useOps } from "@/lib/hooks/useOps";

type BandLock = { nr_sa: string | null; nr_nsa: string | null; lte: string | null; default: { nr_sa: string | null; nr_nsa: string | null; lte: string | null } };
const LOCK = "/api/cell/band/lock";
const sameBands = (a: string | null | undefined, b: string | null | undefined) => {
  const set = (x: string | null | undefined) => [...new Set((x ?? "").split(",").map((s) => s.trim()).filter(Boolean).map(Number))].sort((p, q) => p - q).join(",");
  return set(a) === set(b);
};
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
/** Re-read the lock until `ok` holds: 5 tries, 2 s apart. */
async function readBack(ok: (l: BandLock) => boolean, seen: (l: BandLock) => void): Promise<boolean> {
  for (let i = 0; i < 5; i++) {
    if (i > 0) await sleep(2000);
    const l = await apiFetch<BandLock>(LOCK);
    seen(l);
    if (ok(l)) return true;
  }
  return false;
}

const NR_BANDS = ["n1", "n3", "n5", "n7", "n8", "n28", "n38", "n40", "n41", "n66", "n71", "n77", "n78", "n79"];
const LTE_BANDS = ["B1", "B2", "B3", "B4", "B5", "B7", "B8", "B12", "B17", "B20", "B28", "B38", "B40", "B41"];

type NrScope = "both" | "sa" | "nsa";
type Pending = null | "nr" | "lte" | "reset";

// Network writes drop the uplink for a moment; wait for the agent before
// reporting (it normally stays reachable on the LAN; over Tailscale it
// comes back once the modem re-attaches).
const NET_WAIT = { expectedSec: 30 };

export default function BandLockPage() {
  const { t } = useTranslation();
  const sig = useApi<NetworkSignal>("/api/network/signal", { refreshInterval: 5000 });
  const lock = useApi<BandLock>(LOCK, { refreshInterval: 30000 });
  const ops = useOps();
  // what the lock read back as, for the mismatch line
  const [lastLock, setLastLock] = useState<BandLock | null>(null);
  const canRead = !!lock.data && (lock.data.nr_sa !== null || lock.data.nr_nsa !== null || lock.data.lte !== null);
  const seen = (l: BandLock) => {
    setLastLock(l);
    void lock.mutate(l, { revalidate: false });
  };
  const cs = useMemo(() => carriers(sig.data), [sig.data]);
  const inUse = cs.filter((c) => c.active).map((c) => (c.kind === "nr" ? `n${c.band}` : `B${c.band}`));

  const [nr, setNr] = useState<Set<string>>(new Set());
  const [lte, setLte] = useState<Set<string>>(new Set());
  const [scope, setScope] = useState<NrScope>("both");
  const [dialog, setDialog] = useState<Pending>(null);

  const nrStr = [...nr].sort((a, b) => NR_BANDS.indexOf(a) - NR_BANDS.indexOf(b)).map((b) => b.slice(1)).join(",");
  const lteStr = [...lte].sort((a, b) => LTE_BANDS.indexOf(a) - LTE_BANDS.indexOf(b)).map((b) => b.slice(1)).join(",");

  const nrSteps: WriteStep[] = (["nsa", "sa"] as const)
    .filter((m) => scope === "both" || scope === m)
    .map((m) => ({
      label: m.toUpperCase(),
      run: () => apiFetch("/api/cell/band/nr", { method: "POST", body: { nr5g_type: m, nr5g_band: nrStr } }),
    }));

  const recovery = t("bandlock.recovery", "Press “Reset / unlock all” on this page. If the page can't be reached, open band lock on the device's touchscreen and restore the default.");

  const nrOp = useWriteOp({
    tier: 3,
    steps: nrSteps,
    verify: canRead
      ? () =>
          readBack(
            (l) => (scope !== "nsa" ? l.nr_sa === null || sameBands(l.nr_sa, nrStr) : true) && (scope !== "sa" ? l.nr_nsa === null || sameBands(l.nr_nsa, nrStr) : true),
            seen,
          )
      : undefined,
    waitDevice: { ...NET_WAIT, recovery },
  });
  const lteOp = useWriteOp({
    tier: 3,
    steps: [
      {
        label: "LTE",
        run: () =>
          apiFetch("/api/cell/band/lte", {
            method: "POST",
            body: { lte_band: lteStr },
          }),
      },
    ],
    verify: canRead && lock.data?.lte !== null ? () => readBack((l) => sameBands(l.lte, lteStr), seen) : undefined,
    waitDevice: { ...NET_WAIT, recovery },
  });
  const resetOp = useWriteOp({
    tier: 3,
    steps: [{ label: t("bandlock.reset", "Reset"), run: () => apiFetch("/api/cell/band/reset", { method: "POST" }) }],
    // back to the defaults, for each set the firmware has defaults for
    verify:
      canRead && lock.data && (lock.data.default.nr_sa || lock.data.default.nr_nsa || lock.data.default.lte)
        ? () =>
            readBack(
              (l) => (["nr_sa", "nr_nsa", "lte"] as const).every((k) => !l.default[k] || l[k] === null || sameBands(l[k], l.default[k])),
              seen,
            )
        : undefined,
    waitDevice: { ...NET_WAIT },
  });

  const busy = nrOp.busy || lteOp.busy || resetOp.busy || ops.stuck;
  const lockText = (l: BandLock | null | undefined) => {
    if (!l) return "—";
    const all = (["nr_sa", "nr_nsa", "lte"] as const).every((k) => !l[k] || !l.default[k] || sameBands(l[k], l.default[k]));
    if (all) return t("bandlock.lockedAll", "Not locked (all bands)");
    const nrs = (x: string | null) => (x ? x.split(",").map((b) => `n${b.trim()}`).join(" ") : "—");
    const ltes = (x: string | null) => (x ? x.split(",").map((b) => `B${b.trim()}`).join(" ") : "—");
    return `SA ${nrs(l.nr_sa)} · NSA ${nrs(l.nr_nsa)} · LTE ${ltes(l.lte)}`;
  };
  const mismatch = (o: { phase: string; errorKind?: string | null }) => o.phase === "failed" && o.errorKind === "mismatch";

  function confirm(which: Exclude<Pending, null>) {
    setDialog(null);
    const op = which === "nr" ? nrOp : which === "lte" ? lteOp : resetOp;
    op.start();
    op.confirm();
    if (which === "reset") {
      setNr(new Set());
      setLte(new Set());
    }
  }

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("bandlock.title", "Band Lock")}</h1>

      <div className="grid gap-4 lg:grid-cols-2 lg:gap-6">
        <div className="grid content-start gap-4">
          <Group title={t("bandlock.now", "In use now")} stale={sig.stale}>
            <Row
              label={t("bandlock.bandsInUse", "Bands in use")}
              value={sig.data ? (inUse.length ? inUse.join(" · ") : t("home.noCarriers", "No active carriers")) : "—"}
              mono
              href="/signal"
            />
            {canRead && <Row label={t("bandlock.locked", "Locked bands")} value={lockText(lock.data)} mono />}
          </Group>

          <section aria-labelledby="nr-title">
            <h2 id="nr-title" className="nd-group-title">{t("bandlock.nrCardTitle", "NR (5G) bands")}</h2>
            <div className="nd-group grid gap-4 p-4 lg:p-5">
              <Chips label={t("bandlock.nrCardTitle", "NR (5G) bands")} options={NR_BANDS} value={nr} onChange={setNr} isDisabled={busy} />
              <div className="flex flex-wrap items-center gap-3">
                <span className="nd-aux">{t("bandlock.applyTo", "Apply to")}</span>
                <Segmented<NrScope>
                  label={t("bandlock.applyTo", "Apply to")}
                  value={scope}
                  onChange={setScope}
                  isDisabled={busy}
                  options={[
                    { id: "both", label: t("bandlock.scopeBoth", "SA and NSA") },
                    { id: "sa", label: "SA" },
                    { id: "nsa", label: "NSA" },
                  ]}
                />
              </div>
              <div className="flex flex-wrap items-center gap-3">
                <Button onPress={() => setDialog("nr")} isDisabled={nr.size === 0 || busy}>
                  {t("bandlock.applyNrLock", "Apply NR lock")}
                </Button>
                <span className="nd-aux">
                  {nr.size > 0 ? t("bandlock.nSelected", "{{n}} selected", { n: nr.size }) : t("bandlock.noneSelected", "None selected")}
                </span>
              </div>
              <OpResult op={nrOp} />
              {mismatch(nrOp) && <p className="nd-aux">{t("bandlock.stillReports", "The device still reports {{now}}. It may still be switching; check again in a minute, and apply once more if it hasn't changed.", { now: lockText(lastLock) })}</p>}
            </div>
          </section>
        </div>

        <div className="grid content-start gap-4">
          <section aria-labelledby="lte-title">
            <h2 id="lte-title" className="nd-group-title">{t("bandlock.lteCardTitle", "LTE bands")}</h2>
            <div className="nd-group grid gap-4 p-4 lg:p-5">
              <Chips label={t("bandlock.lteCardTitle", "LTE bands")} options={LTE_BANDS} value={lte} onChange={setLte} isDisabled={busy} />
              <div className="flex flex-wrap items-center gap-3">
                <Button onPress={() => setDialog("lte")} isDisabled={lte.size === 0 || busy}>
                  {t("bandlock.applyLteLock", "Apply LTE lock")}
                </Button>
                <span className="nd-aux">
                  {lte.size > 0 ? t("bandlock.nSelected", "{{n}} selected", { n: lte.size }) : t("bandlock.noneSelected", "None selected")}
                </span>
              </div>
              <OpResult op={lteOp} />
              {mismatch(lteOp) && <p className="nd-aux">{t("bandlock.stillReports", "The device still reports {{now}}. It may still be switching; check again in a minute, and apply once more if it hasn't changed.", { now: lockText(lastLock) })}</p>}
            </div>
          </section>

          <section aria-labelledby="reset-title">
            <h2 id="reset-title" className="nd-group-title">{t("bandlock.resetTitle", "Automatic bands")}</h2>
            <div className="nd-group grid gap-3 p-4 lg:p-5">
              <p className="nd-body text-nd-t2">{t("bandlock.resetDesc", "Remove every NR and LTE band lock (cell locks go too) and let the modem pick again.")}</p>
              <div>
                <Button variant="secondary" onPress={() => setDialog("reset")} isDisabled={busy}>
                  {t("bandlock.resetUnlockAll", "Reset / unlock all")}
                </Button>
              </div>
              <OpResult op={resetOp} />
              {mismatch(resetOp) && <p className="nd-aux">{t("bandlock.stillReports", "The device still reports {{now}}. It may still be switching; check again in a minute, and apply once more if it hasn't changed.", { now: lockText(lastLock) })}</p>}
            </div>
          </section>
        </div>
      </div>

      <ConfirmDialog
        open={dialog === "nr"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("bandlock.confirmNrTitle", "Lock NR to {{bands}}?", { bands: [...nr].join(" ") })}
        what={
          scope === "both"
            ? t("bandlock.confirmNrWhat", "5G only uses these bands, for both SA and NSA. Other NR bands stop being used.")
            : t("bandlock.confirmNrWhatOne", "5G {{mode}} only uses these bands. Other NR bands stop being used.", { mode: scope.toUpperCase() })
        }
        downtime={t("bandlock.downtime", "The mobile connection drops for about 30 seconds while the modem re-attaches.")}
        recovery={recovery}
        actionLabel={t("bandlock.confirmNrAction", "Lock NR bands")}
        cutsUplink
        onConfirm={() => confirm("nr")}
      />
      <ConfirmDialog
        open={dialog === "lte"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("bandlock.confirmLteTitle", "Lock LTE to {{bands}}?", { bands: [...lte].join(" ") })}
        what={t("bandlock.confirmLteWhat", "4G only uses these bands. Other LTE bands stop being used.")}
        downtime={t("bandlock.downtime", "The mobile connection drops for about 30 seconds while the modem re-attaches.")}
        recovery={recovery}
        actionLabel={t("bandlock.confirmLteAction", "Lock LTE bands")}
        cutsUplink
        onConfirm={() => confirm("lte")}
      />
      <ConfirmDialog
        open={dialog === "reset"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("bandlock.confirmResetTitle", "Reset all band locks?")}
        what={t("bandlock.confirmReset", "Bands and cell locks reset together: every band lock and every cell lock is removed, and the device picks automatically again.")}
        downtime={t("bandlock.downtime", "The mobile connection drops for about 30 seconds while the modem re-attaches.")}
        actionLabel={t("bandlock.resetUnlockAll", "Reset / unlock all")}
        cutsUplink
        onConfirm={() => confirm("reset")}
      />
    </>
  );
}
