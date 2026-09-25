// Modem (radio, mobile data, carrier scan / manual registration, network mode)
// and cell (cell lock, band lock, neighbours, STC, signal detection)
// endpoints — handlers.rs, modem_ext.rs, cell.rs.
//
// Nearly all of these are ubus passthroughs to zte_nwinfo_api / zwrt_data, so
// the payloads follow the page types (see schemas/modem.ts for what is
// unconfirmed). Long-running firmware jobs (scan, register, neighbour scan,
// signal detection) are modelled with a start timestamp; status is computed
// from elapsed time at read time (no timers).

import type { Ctx, Reply, Route } from "../lib.ts";
import { ok, fail, clone, bodyField, mergeKnown, methodNotFound } from "../lib.ts";
import { shared } from "../shared.ts";
import type {
  ModemStatus,
  ModemOnlineResult,
  ModemData,
  ModemScanStatus,
  ModemScanOperator,
  ModemScanResults,
  ModemRegisterResult,
  CellNeighbor,
  CellNeighborsNr,
  CellNeighborsLte,
  CellStcParams,
  CellStcStatus,
  CellSignalDetectProgress,
  CellSignalDetectRecord,
  CellSignalDetectResults,
} from "../../../src/lib/api/schemas/modem.ts";

// ─── helpers ─────────────────────────────────────────────────────────────────

/** Same message format as ubus.rs:19-22; the handlers map it to 503. */
function ubusFail(obj: string, method: string, why: string): Reply {
  return fail(`ubus call ${obj} ${method} failed: ${why}`, 503);
}

/** Handlers that serde-parse the body answer 400 "invalid JSON" on an empty / non-JSON body. */
function needJson(ctx: Ctx): Reply | null {
  return ctx.body === undefined ? fail("invalid JSON", 400) : null;
}

/** A passthrough write whose ubus method printed nothing → `data: null` (ubus.rs:26-28). */
const UBUS_EMPTY = null;

function str(v: unknown): string | undefined {
  if (typeof v === "string") return v;
  if (typeof v === "number") return String(v);
  return undefined;
}

function num(v: unknown): number | undefined {
  if (typeof v === "number") return v;
  if (typeof v === "string" && v.trim() !== "" && !isNaN(Number(v))) return Number(v);
  return undefined;
}

function radioUp(ctx: Ctx): boolean {
  return !shared.airplane && !ctx.has("nosignal");
}

// ─── modem state ─────────────────────────────────────────────────────────────

/** Remembers the exact non-ONLINE mode last set (normally "LPM"). */
let offlineMode = "LPM";

const wwan = {
  cid: 1,
  connect_mode: 1,
  /** Roaming must be on: home SIM (China Mobile) roaming onto Chunghwa. */
  roam_enable: 1,
} satisfies Omit<ModemData, "enable" | "connect_status">;

function modemData(ctx: Ctx): ModemData {
  const connected = shared.mobileData && wwan.roam_enable === 1 && radioUp(ctx);
  return {
    ...wwan,
    enable: shared.mobileData ? 1 : 0,
    connect_status: connected ? "connected" : "disconnected",
    roll_connect_status: connected ? "connected" : "disconnected",
  };
}

/** Private: last network-mode write. Readback lives in /api/network/signal net_select (other area). */
let netSelect = "WL_AND_5G";

/** What the agent accepts (modem_ext.rs NET_SELECT_VALUES = the firmware's list). */
const NET_SELECT_VALUES = [
  "WL_AND_5G", "TCHGWL_5G", "Only_5G", "LTE_AND_5G", "4G_AND_5G", "WL_AND_NSA", "Only_LTE",
  "WCDMA_AND_LTE", "GSM_AND_LTE", "TDSCDMA_AND_LTE", "Only_WCDMA", "Only_GSM_WCDMA", "Only_TDSCDMA", "Only_GSM",
];

// Manual carrier scan / registration.
const SCAN_MS = 6000;
const REGISTER_MS = 4000;
let scanStartedAt: number | null = null;
let registerJob: { at: number; mccMnc: string; rat: string } | null = null;
/** Private: PLMN we are registered on (no shared field for the operator). */
let registeredPlmn = "46692";

const TW_OPERATORS: ModemScanOperator[] = [
  { m_oper_name: "Chunghwa Telecom", m_mcc_mnc: "46692", m_rat: "12", m_status: "2" },
  { m_oper_name: "Chunghwa Telecom", m_mcc_mnc: "46692", m_rat: "7", m_status: "1" },
  { m_oper_name: "Taiwan Mobile", m_mcc_mnc: "46697", m_rat: "12", m_status: "1" },
  { m_oper_name: "Taiwan Mobile", m_mcc_mnc: "46697", m_rat: "7", m_status: "1" },
  { m_oper_name: "FarEasTone", m_mcc_mnc: "46601", m_rat: "12", m_status: "1" },
  { m_oper_name: "FarEasTone", m_mcc_mnc: "46601", m_rat: "7", m_status: "1" },
  { m_oper_name: "T Star", m_mcc_mnc: "46689", m_rat: "7", m_status: "1" },
];

/** Private: the agent's guarded register / back-to-auto job (netinfo.rs Guard). */
let guardJob: { kind: "register" | "auto"; at: number; mccMnc: string; rat: string; success: boolean } | null = null;
const AUTO_MS = 2000;

/** netinfo.rs `Guard::json` as GET /api/modem/register/guard and /api/netinfo `guard` return it. */
function guardNow(now: number) {
  const idle = { phase: "idle", target: "", rat: "", reason: "", started_at: 0, finished_at: 0, last_result: "" };
  if (!guardJob) return idle;
  const j = guardJob;
  const started_at = Math.floor(j.at / 1000);
  if (j.kind === "auto") {
    if (now - j.at < AUTO_MS) return { ...idle, phase: "reverting", reason: "手动恢复自动", started_at };
    registeredPlmn = "46692";
    return { ...idle, phase: "reverted", reason: "手动恢复自动", started_at, finished_at: started_at + AUTO_MS / 1000 };
  }
  const base = { ...idle, target: j.mccMnc, rat: j.rat, started_at };
  if (now - j.at < REGISTER_MS) return { ...base, phase: "registering" };
  const finished_at = started_at + REGISTER_MS / 1000;
  if (j.success) {
    registeredPlmn = j.mccMnc;
    return { ...base, phase: "ok", last_result: "1", finished_at };
  }
  return { ...base, phase: "reverted", reason: "注册不上（LIMITED_SERVICE_SA），已回到自动选网", last_result: "0", finished_at };
}

/** netinfo.rs `Scan::json`: rat 11 = NR, 7 = LTE (ubus m_rat as the device reports it). */
function netinfoScanNow(ctx: Ctx) {
  const idle = { state: "idle", started_at: 0, finished_at: 0, error: null, last_status: "", operators: [] as { status: string; name: string; plmn: string; rat: string; country: string | null }[] };
  if (scanStartedAt === null) return idle;
  const started_at = Math.floor(scanStartedAt / 1000);
  if (ctx.now - scanStartedAt < SCAN_MS) return { ...idle, state: "scanning", started_at, last_status: "manual_selecting" };
  const operators = ctx.has("nosignal")
    ? []
    : scanOperators().map((o) => ({ status: o.m_status ?? "1", name: o.m_oper_name ?? "", plmn: o.m_mcc_mnc ?? "", rat: o.m_rat === "12" ? "11" : o.m_rat ?? "", country: "中国台湾" }));
  return { ...idle, state: "done", started_at, finished_at: started_at + SCAN_MS / 1000, last_status: "manual_selected", operators };
}

/** netinfo.rs operator_scan: 202, one modem job at a time. */
function startScan(ctx: Ctx): Reply {
  if (shared.airplane) return ubusFail(NW, "nwinfo_manual_scan", "Operation not permitted");
  if (guardJob && guardNow(ctx.now).phase.endsWith("ing")) return fail("正在选网，完成后再搜", 409);
  if (scanStartedAt === null || ctx.now - scanStartedAt >= SCAN_MS) scanStartedAt = ctx.now;
  return { status: 202, data: { state: "scanning" } };
}

function scanOperators(): ModemScanOperator[] {
  // The first (highest-RAT) entry of the registered PLMN is "current".
  const cur = TW_OPERATORS.findIndex((o) => o.m_mcc_mnc === registeredPlmn);
  return TW_OPERATORS.map((o, i) => ({ ...o, m_status: i === cur ? "2" : "1" }));
}

// ─── cell state ──────────────────────────────────────────────────────────────

/** Private: last cell-lock writes (no GET exists; indirect readback is /api/network/signal PCI). */
const cellLock: {
  nr: { pci: string; earfcn: string; band?: string } | null;
  lte: { pci: string; earfcn: string } | null;
} = { nr: null, lte: null };

/** Private: both NR band-lock writes (bandlock page sends nsa then sa). No GET exists; shared.bandLock.nr mirrors the latest. */
const nrBandLock: { nsa: string | null; sa: string | null } = { nsa: null, sa: null };

const NEIGHBOR_MS = 2500;
export const NBR_UNSUPPORTED = "原厂扫描会断网且拿不到数据，已停用";
let neighborScanAt: number | null = null;

const NR_NEIGHBORS: CellNeighbor[] = [
  { pci: 212, earfcn: 630000, band: "n78", rsrp: -97, rsrq: -11, sinr: 16 },
  { pci: 213, earfcn: 630000, band: "n78", rsrp: -103, rsrq: -13, sinr: 9 },
  { pci: 87, earfcn: 630000, band: "n78", rsrp: -108, rsrq: -14, sinr: 4 },
  { pci: 406, earfcn: 156510, band: "n1", rsrp: -101, rsrq: -12, sinr: 11 },
];

const LTE_NEIGHBORS: CellNeighbor[] = [
  { pci: 301, earfcn: 1400, band: "B3", rsrp: -92, rsrq: -10, sinr: 17 },
  { pci: 302, earfcn: 1400, band: "B3", rsrp: -99, rsrq: -12, sinr: 10 },
  { pci: 145, earfcn: 3050, band: "B7", rsrp: -104, rsrq: -13, sinr: 6 },
  { pci: 145, earfcn: 300, band: "B1", rsrp: -100, rsrq: -12, sinr: 8 },
  { pci: 58, earfcn: 3625, band: "B8", rsrp: -95, rsrq: -11, sinr: 12 },
];

function neighbors(ctx: Ctx, list: CellNeighbor[]): CellNeighbor[] {
  if (neighborScanAt === null || ctx.now - neighborScanAt < NEIGHBOR_MS || !radioUp(ctx)) return [];
  if (!ctx.has("weak")) return clone(list);
  return list.slice(0, 2).map((c) => ({
    ...c,
    rsrp: (c.rsrp ?? -100) - 18,
    rsrq: (c.rsrq ?? -11) - 6,
    sinr: (c.sinr ?? 10) - 14,
  }));
}

const stcParams: CellStcParams = {
  lte_collect_timer: "60",
  nrsa_collect_timer: "60",
  lte_whitelist_max: "8",
  nrsa_whitelist_max: "8",
} satisfies CellStcParams;

const stcStatus: CellStcStatus = { enabled: "0" } satisfies CellStcStatus;

// Signal-quality detection.
const DETECT_MS = 10000;
const detect: { startedAt: number | null; stoppedAt: number | null } = { startedAt: null, stoppedAt: null };

function detectProgress(now: number): number {
  if (detect.startedAt === null) return 0;
  const end = detect.stoppedAt ?? now;
  return Math.min(100, Math.floor(((end - detect.startedAt) / DETECT_MS) * 100));
}

function detectRecords(ctx: Ctx): CellSignalDetectRecord[] {
  if (detect.startedAt === null) return [];
  const p = detectProgress(ctx.now);
  const n = Math.floor(p / 10);
  const weak = ctx.has("weak");
  const none = ctx.has("nosignal");
  const baseTs = Math.floor(detect.startedAt / 1000);
  const rows: CellSignalDetectRecord[] = [];
  for (let i = 0; i < n; i++) {
    const wobble = [0, -2, 1, -3, 2, -1, 0, -4, 1, -2][i] ?? 0;
    rows.push({
      time: baseTs + i,
      network_type: none ? null : "SA",
      band: none ? null : "n78",
      pci: none ? null : 212,
      rsrp: none ? null : (weak ? -115 : -95) + wobble,
      rsrq: none ? null : (weak ? -17 : -11) + Math.round(wobble / 2),
      sinr: none ? null : (weak ? -2 : 18) + wobble,
    });
  }
  return rows;
}

// ─── routes ──────────────────────────────────────────────────────────────────

const NW = "zte_nwinfo_api";

export const routes: Route[] = [
  // modem
  {
    method: "GET",
    path: "/api/modem/status",
    handler: () => ok({ operate_mode: shared.airplane ? offlineMode : "ONLINE" } satisfies ModemStatus),
  },
  {
    method: "POST",
    path: "/api/modem/online",
    handler: () => {
      shared.airplane = false;
      return { data: { status: "ok" } satisfies ModemOnlineResult, delayMs: 1500 };
    },
  },
  {
    method: "GET",
    path: "/api/modem/data",
    handler: (ctx) => ok(modemData(ctx)),
  },
  {
    method: "PUT",
    path: "/api/modem/data",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      if (ctx.has("fakesuccess")) return ok(UBUS_EMPTY); // firmware says ok, nothing changes
      const enable = num(bodyField(ctx.body, "enable"));
      const roam = num(bodyField(ctx.body, "roam_enable"));
      const mode = num(bodyField(ctx.body, "connect_mode"));
      if (enable !== undefined) shared.mobileData = enable !== 0;
      if (roam !== undefined) wwan.roam_enable = roam !== 0 ? 1 : 0;
      if (mode !== undefined) wwan.connect_mode = mode;
      return ok(UBUS_EMPTY);
    },
  },
  {
    method: "POST",
    path: "/api/modem/airplane",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      const mode = str(bodyField(ctx.body, "operate_mode"));
      if (mode === "ONLINE") {
        // modem_ext.rs:30-32 delegates to modem_online (AT+CFUN=1).
        shared.airplane = false;
        return { data: { status: "ok" } satisfies ModemOnlineResult, delayMs: 1500 };
      }
      if (!mode) return ubusFail(NW, "nwinfo_set_mode", "Invalid argument");
      if (ctx.has("fakesuccess")) return ok(UBUS_EMPTY);
      offlineMode = mode;
      shared.airplane = true;
      return ok(UBUS_EMPTY);
    },
  },
  {
    method: "PUT",
    path: "/api/modem/network-mode",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      const v = str(bodyField(ctx.body, "net_select"));
      if (!v || !NET_SELECT_VALUES.includes(v)) return fail(`net_select must be one of ${NET_SELECT_VALUES.join(", ")}`, 400);
      if (!ctx.has("fakesuccess")) netSelect = v;
      return ok(UBUS_EMPTY);
    },
  },
  {
    method: "POST",
    path: "/api/modem/scan",
    handler: (ctx) => startScan(ctx),
  },
  {
    method: "POST",
    path: "/api/netinfo/scan",
    handler: (ctx) => startScan(ctx),
  },
  {
    method: "GET",
    path: "/api/modem/scan/status",
    handler: (ctx) => {
      let status = "idle";
      if (scanStartedAt !== null) status = ctx.now - scanStartedAt < SCAN_MS ? "scanning" : "done";
      return ok({ status } satisfies ModemScanStatus);
    },
  },
  {
    method: "GET",
    path: "/api/modem/scan/results",
    handler: (ctx) => {
      const done = scanStartedAt !== null && ctx.now - scanStartedAt >= SCAN_MS;
      const operators = done && !ctx.has("nosignal") ? scanOperators() : [];
      return ok({ operators } satisfies ModemScanResults);
    },
  },
  {
    method: "POST",
    path: "/api/modem/register",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      if (shared.airplane) return ubusFail(NW, "nwinfo_manual_register", "Operation not permitted");
      const mccMnc = str(bodyField(ctx.body, "m_mcc_mnc")) ?? "";
      const rat = str(bodyField(ctx.body, "m_rat")) ?? "";
      if (!/^\d{5,6}$/.test(mccMnc)) return fail("m_mcc_mnc must be 5–6 digits", 400);
      if (scanStartedAt !== null && ctx.now - scanStartedAt < SCAN_MS) return fail("正在搜索网络，搜完再操作", 409);
      registerJob = { at: ctx.now, mccMnc, rat };
      const known = TW_OPERATORS.some((o) => o.m_mcc_mnc === mccMnc && (o.m_rat === rat || (rat === "11" && o.m_rat === "12")));
      guardJob = { kind: "register", at: ctx.now, mccMnc, rat, success: known && !ctx.has("nosignal") && !ctx.has("fakesuccess") };
      return { status: 202, data: guardNow(ctx.now) };
    },
  },
  {
    method: "GET",
    path: "/api/modem/register/guard",
    handler: (ctx) => ok(guardNow(ctx.now)),
  },
  {
    method: "POST",
    path: "/api/modem/netselect/auto",
    handler: (ctx) => {
      if (scanStartedAt !== null && ctx.now - scanStartedAt < SCAN_MS) return fail("正在搜索网络，搜完再操作", 409);
      guardJob = { kind: "auto", at: ctx.now, mccMnc: "", rat: "", success: true };
      return { status: 202, data: guardNow(ctx.now) };
    },
  },
  {
    method: "GET",
    path: "/api/modem/register/result",
    handler: (ctx) => {
      // Pending must not be "0"/"1"/"success"/"fail" (the page treats those as final).
      if (!registerJob || ctx.now - registerJob.at < REGISTER_MS) return ok({ result: "" } satisfies ModemRegisterResult);
      const job = registerJob;
      const known = TW_OPERATORS.some((o) => o.m_mcc_mnc === job.mccMnc && o.m_rat === job.rat);
      const success = known && !ctx.has("nosignal") && !ctx.has("fakesuccess");
      if (success) registeredPlmn = job.mccMnc;
      return ok({ result: success ? "success" : "fail" } satisfies ModemRegisterResult);
    },
  },

  // cell lock
  {
    method: "POST",
    path: "/api/cell/lock/nr",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      const pci = str(bodyField(ctx.body, "pci"));
      const earfcn = str(bodyField(ctx.body, "earfcn"));
      if (!pci || !earfcn) return ubusFail(NW, "nwinfo_lock_nr_cell", "Invalid argument");
      if (!ctx.has("fakesuccess")) cellLock.nr = { pci, earfcn, band: str(bodyField(ctx.body, "band")) };
      return ok(UBUS_EMPTY);
    },
  },
  {
    method: "POST",
    path: "/api/cell/lock/lte",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      const pci = str(bodyField(ctx.body, "pci"));
      const earfcn = str(bodyField(ctx.body, "earfcn"));
      if (!pci || !earfcn) return ubusFail(NW, "nwinfo_lock_lte_cell", "Invalid argument");
      if (!ctx.has("fakesuccess")) cellLock.lte = { pci, earfcn };
      return ok(UBUS_EMPTY);
    },
  },
  {
    method: "POST",
    path: "/api/cell/lock/reset",
    handler: (ctx) => {
      // nwinfo_reset_band_cell_setting: by its name it resets band AND cell
      // settings (unconfirmed) — the mock clears both.
      if (ctx.has("fakesuccess")) return ok(UBUS_EMPTY);
      cellLock.nr = null;
      cellLock.lte = null;
      nrBandLock.nsa = null;
      nrBandLock.sa = null;
      shared.bandLock.nr = null;
      shared.bandLock.lte = null;
      return ok(UBUS_EMPTY);
    },
  },

  // neighbours
  {
    method: "POST",
    path: "/api/cell/neighbors/scan",
    handler: (ctx) => {
      // netinfo.rs neighbors_scan: 410 on the real device — the stock scan
      // drops mobile data and returns no cells. Scenario "nbrscan" keeps the
      // old simulated scan for the cell-lock page's own tests.
      if (!ctx.has("nbrscan")) return fail(NBR_UNSUPPORTED, 410);
      if (shared.airplane) return ubusFail(NW, "nwinfo_scan_nbr", "Operation not permitted");
      neighborScanAt = ctx.now;
      return ok(UBUS_EMPTY);
    },
  },
  {
    method: "GET",
    path: "/api/cell/neighbors/nr",
    handler: (ctx) => ok({ cells: neighbors(ctx, NR_NEIGHBORS) } satisfies CellNeighborsNr),
  },
  {
    method: "GET",
    path: "/api/cell/neighbors/lte",
    handler: (ctx) => ok({ cells: neighbors(ctx, LTE_NEIGHBORS) } satisfies CellNeighborsLte),
  },

  // band lock
  {
    method: "POST",
    path: "/api/cell/band/nr",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      const type = str(bodyField(ctx.body, "nr5g_type"));
      const bands = str(bodyField(ctx.body, "nr5g_band"));
      if ((type !== "nsa" && type !== "sa") || !bands) return ubusFail(NW, "nwinfo_set_nrbandlock", "Invalid argument");
      if (ctx.has("fakesuccess")) return ok(UBUS_EMPTY);
      nrBandLock[type] = bands;
      shared.bandLock.nr = bands;
      return ok(UBUS_EMPTY);
    },
  },
  {
    method: "POST",
    path: "/api/cell/band/lte",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      const isLte = str(bodyField(ctx.body, "is_lte_band"));
      const mask = str(bodyField(ctx.body, "lte_band_mask")) ?? "";
      if (isLte === "1" && !mask) return ubusFail(NW, "nwinfo_set_gwl_bandlock", "Invalid argument");
      if (ctx.has("fakesuccess")) return ok(UBUS_EMPTY);
      shared.bandLock.lte = isLte === "1" ? mask : null;
      return ok(UBUS_EMPTY);
    },
  },
  {
    method: "POST",
    path: "/api/cell/band/reset",
    handler: (ctx) => {
      if (ctx.has("fakesuccess")) return ok(UBUS_EMPTY);
      nrBandLock.nsa = null;
      nrBandLock.sa = null;
      shared.bandLock.nr = null;
      shared.bandLock.lte = null;
      return ok(UBUS_EMPTY);
    },
  },

  // STC (whitelist cell lock). B27 answers 503 Method not found for both
  // reads (recorded; `firmware-b27`); by default the mock models a firmware
  // that has them, so the page's readback is testable.
  {
    method: "GET",
    path: "/api/cell/stc/params",
    handler: (ctx) => (ctx.has("firmware-b27") ? methodNotFound(NW, "nwinfo_get_stc_white_list_par") : ok(clone(stcParams))),
  },
  {
    method: "PUT",
    path: "/api/cell/stc/params",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      for (const k of ["lte_collect_timer", "nrsa_collect_timer", "lte_whitelist_max", "nrsa_whitelist_max"]) {
        const v = bodyField(ctx.body, k);
        if (v !== undefined && (str(v) === undefined || !/^\d+$/.test(str(v) ?? ""))) {
          return ubusFail(NW, "nwinfo_set_stc_white_list_par", "Invalid argument");
        }
      }
      if (!ctx.has("fakesuccess")) {
        const clean: Record<string, string> = {};
        for (const [k, v] of Object.entries(ctx.body as Record<string, unknown>)) {
          const s = str(v);
          if (s !== undefined) clean[k] = s;
        }
        mergeKnown(stcParams, clean);
      }
      return ok(UBUS_EMPTY);
    },
  },
  {
    method: "GET",
    path: "/api/cell/stc/status",
    handler: (ctx) => (ctx.has("firmware-b27") ? methodNotFound(NW, "nwinfo_get_stc_white_list_status") : ok(clone(stcStatus))),
  },
  {
    method: "POST",
    path: "/api/cell/stc/enable",
    handler: (ctx) => {
      if (!ctx.has("fakesuccess")) stcStatus.enabled = "1";
      return ok(UBUS_EMPTY);
    },
  },
  {
    method: "POST",
    path: "/api/cell/stc/disable",
    handler: (ctx) => {
      if (!ctx.has("fakesuccess")) stcStatus.enabled = "0";
      return ok(UBUS_EMPTY);
    },
  },
  {
    method: "POST",
    path: "/api/cell/stc/reset",
    // Whitelist has no read endpoint; enabled flag is left as is.
    handler: () => ok(UBUS_EMPTY),
  },

  // signal-quality detection
  {
    method: "POST",
    path: "/api/cell/signal-detect/start",
    handler: (ctx) => {
      if (shared.airplane) return ubusFail(NW, "nwinfo_start_detect_signal_quality", "Operation not permitted");
      detect.startedAt = ctx.now;
      detect.stoppedAt = null;
      return ok(UBUS_EMPTY);
    },
  },
  {
    method: "POST",
    path: "/api/cell/signal-detect/stop",
    handler: (ctx) => {
      if (detect.startedAt !== null && detect.stoppedAt === null) detect.stoppedAt = ctx.now;
      return ok(UBUS_EMPTY);
    },
  },
  {
    method: "GET",
    path: "/api/cell/signal-detect/progress",
    handler: (ctx) => {
      if (!ctx.has("firmware-b27")) return ok({ progress: detectProgress(ctx.now) } satisfies CellSignalDetectProgress);
      // B27 (recorded idle): strings, "" when nothing has run. Running values are a guess.
      const idle = detect.startedAt === null;
      return ok({ progress: idle ? "" : String(detectProgress(ctx.now)), quality: "" } satisfies CellSignalDetectProgress);
    },
  },
  {
    method: "GET",
    path: "/api/cell/signal-detect/results",
    handler: (ctx) => {
      const rows = detectRecords(ctx);
      // B27 answers `{}` when there are no results (recorded).
      if (ctx.has("firmware-b27") && rows.length === 0) return ok({} satisfies CellSignalDetectResults);
      return ok({ results: rows } satisfies CellSignalDetectResults);
    },
  },
];

/** For other fixtures/tests: private state not exposed through any GET here. */
export const modemPrivate = {
  get netSelect() {
    return netSelect;
  },
  get registeredPlmn() {
    return registeredPlmn;
  },
  cellLock,
  nrBandLock,
  guardNow,
  netinfoScanNow,
};
