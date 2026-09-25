// Pure helpers for the home page, mirroring the touchscreen (touch-ui
// src/ui.c signal card, src/ui_logic.c ui_sig_state) so both screens say
// the same thing about the same data.
import type { NetworkSignal } from "@/lib/api/schemas/network";

export interface Carrier {
  kind: "nr" | "lte";
  /** Band number without prefix (n78 → 78); null for the serving row when unknown. */
  band: string;
  pci: number | null;
  arfcn: number | null;
  bw: number | null;
  rsrp: number | null;
  rsrq: number | null;
  sinr: number | null;
  /** RSRP at the -140 floor = configured but not scheduled. */
  active: boolean;
  serving: boolean;
}

/**
 * nrca / lteca: ';'-separated carriers, 11 ','-fields each
 * (idx,PCI,?,band,arfcn,bw,?,rsrp,rsrq,sinr,rssi). Empty without CA.
 * Records that don't have 11 numeric fields are skipped (legacy 5-field
 * LTE firmwares), exactly like touch-ui parse_ca().
 */
export function parseCa(s: string | undefined | null, kind: "nr" | "lte"): Carrier[] {
  if (!s) return [];
  const out: Carrier[] = [];
  for (const rec of s.split(";")) {
    const f = rec.split(",").map((x) => Number(x.trim()));
    if (f.length !== 11 || f.some((n) => !Number.isFinite(n))) continue;
    const [, pci, , band, arfcn, bw, , rsrp, rsrq, sinr] = f;
    out.push({
      kind,
      band: String(band),
      pci,
      arfcn,
      bw,
      rsrp,
      rsrq,
      sinr,
      active: rsrp > -140,
      serving: false,
    });
  }
  return out;
}

function num(v: unknown): number | null {
  if (v === undefined || v === null || v === "") return null;
  const n = typeof v === "number" ? v : parseFloat(String(v));
  return Number.isFinite(n) ? n : null;
}

/**
 * Serving cell first (its live signal is only in nr5g_*), then every nrca
 * and lteca entry as-is — the serving band may appear again as an inactive
 * -140 entry; that is what the modem reports, so it is not de-duplicated.
 */
export function carriers(sig: NetworkSignal | undefined): Carrier[] {
  if (!sig) return [];
  const out: Carrier[] = [];
  if (num(sig.nr5g_rsrp)) {
    out.push({
      kind: "nr",
      band: String(sig.nr5g_action_band ?? "").replace(/^n/i, ""),
      pci: num(sig.nr5g_pci),
      arfcn: num(sig.nr5g_action_channel),
      bw: num(sig.nr5g_bandwidth),
      rsrp: num(sig.nr5g_rsrp),
      rsrq: num(sig.nr5g_rsrq),
      sinr: num(sig.nr5g_snr),
      active: true,
      serving: true,
    });
  } else if (num(sig.lte_rsrp)) {
    out.push({
      kind: "lte",
      band: String(sig.wan_active_band ?? "").replace(/^b/i, ""),
      pci: num(sig.lte_pci),
      arfcn: num(sig.wan_active_channel),
      bw: null,
      rsrp: num(sig.lte_rsrp),
      rsrq: num(sig.lte_rsrq),
      sinr: num(sig.lte_snr),
      active: true,
      serving: true,
    });
  }
  out.push(...parseCa(sig.nrca, "nr"), ...parseCa(sig.lteca, "lte"));
  return out;
}

/**
 * Cell id of the serving cell: NR (nr5g_cell_id) when NR serves, LTE
 * (cell_id, then lte_cell_id) when LTE serves, else whichever is present.
 * SA on B27 has only nr5g_pci / nr5g_cell_id (no lte_pci / cell_id).
 */
export function servingCellId(sig: NetworkSignal | undefined, serving: Carrier | undefined): number | undefined {
  if (!sig) return undefined;
  const nr = num(sig.nr5g_cell_id) ?? undefined;
  const lte = num(sig.cell_id) ?? num(sig.lte_cell_id) ?? undefined;
  return serving?.kind === "lte" ? lte ?? nr : nr ?? lte;
}

/** Sum of every reported carrier's bandwidth, active or not (touch total_bw). */
export function totalBandwidth(cs: Carrier[]): number | null {
  if (cs.length === 0) return null;
  return cs.reduce((a, c) => a + (c.bw ?? 0), 0);
}

export function carrierCounts(cs: Carrier[]): { nr: number; lte: number } {
  return { nr: cs.filter((c) => c.kind === "nr").length, lte: cs.filter((c) => c.kind === "lte").length };
}

export type SigState = "loading" | "stale" | "nosim" | "none" | "weak" | "good";

/** touch-ui ui_sig_state: bars ≤ 0 → none; bars ≤ 2 or SINR < 0 → weak. */
export function sigState(args: {
  everValid: boolean;
  valid: boolean;
  simState?: string | null;
  bars: number | null;
  sinr: number | null;
}): SigState {
  if (!args.everValid) return "loading";
  if (!args.valid) return "stale";
  const st = args.simState;
  if (st && !st.includes("ready")) return "nosim";
  if (args.bars == null || args.bars <= 0) return "none";
  if (args.bars <= 2 || (args.sinr != null && args.sinr < 0)) return "weak";
  return "good";
}

/** bytes/second → Mbps number string with sensible precision. */
export function mbps(bytesPerSec: number | undefined | null): string | null {
  if (bytesPerSec == null || !Number.isFinite(bytesPerSec) || bytesPerSec < 0) return null;
  const v = (bytesPerSec * 8) / 1_000_000;
  if (v >= 100) return v.toFixed(0);
  if (v >= 10) return v.toFixed(1);
  return v.toFixed(2);
}

/** Bytes → "1.23 GB" style; null when unknown. */
export function bytes(n: number | string | null | undefined): string | null {
  const v = typeof n === "string" ? Number(n) : n;
  if (v == null || !Number.isFinite(v) || v < 0) return null;
  const units = ["B", "KB", "MB", "GB", "TB"];
  let x = v;
  let i = 0;
  while (x >= 1000 && i < units.length - 1) {
    x /= 1000;
    i++;
  }
  return `${i === 0 ? x.toFixed(0) : x.toFixed(x >= 100 ? 0 : x >= 10 ? 1 : 2)} ${units[i]}`;
}

/** zwrt_bsp.thermal get_cpu_temp → °C (MU5250: cpuss_temp, milli-°C when ≥ 1000). */
export function cpuTempC(t: Record<string, unknown> | undefined): number | null {
  const v = num(t?.cpuss_temp);
  if (v == null || v < 0) return null;
  return v >= 1000 ? Math.round(v / 1000) : v;
}

export function uptimeParts(secs: number | undefined): { d: number; h: number; m: number } | null {
  if (!secs || secs < 0) return null;
  return { d: Math.floor(secs / 86400), h: Math.floor((secs % 86400) / 3600), m: Math.floor((secs % 3600) / 60) };
}

/** /api/network/signal `net_select_mode` (B27: auto_select | manual_select) in words; other values as-is. */
export function selectionWord(mode: string | undefined, t: (k: string, d: string) => string): string | undefined {
  if (mode === "auto_select") return t("home.selAuto", "Automatic");
  if (mode === "manual_select") return t("home.selManual", "Manual");
  return mode || undefined;
}
