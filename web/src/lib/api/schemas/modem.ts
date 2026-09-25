// Response shapes for the modem (radio on/off, mobile data, carrier scan /
// manual registration, network mode) and cell (cell lock, band lock,
// neighbours, STC whitelist, signal-quality detection) endpoints of zte-agent
// (zte-agent/src/handlers.rs, modem_ext.rs, cell.rs).
//
// Almost every handler here is a ubus passthrough: the agent returns the
// firmware's JSON as-is inside {ok,data}. The firmware shapes are not in the
// repo, so page types + web/docs/controls-inventory.md are the evidence; fields
// the inventory marks 未确认 carry `// unconfirmed`. Only /api/modem/status and
// /api/modem/online build their JSON in Rust. A ubus call that prints nothing
// comes back as `data: null` (ubus.rs:26-28). Every ubus failure is
// 503 {ok:false,error:"ubus call <obj> <method> failed: <stderr>"}.
//
// Types only — no runtime code.

// ─── modem ───────────────────────────────────────────────────────────────────

/**
 * GET /api/modem/status — handlers.rs:129 `modem_status`.
 * Built in Rust from `uci get zte_nwinfo.sys_info.operate_mode`; uci failure = 503.
 * mobile-network page: airplane mode is "on" when operate_mode !== "ONLINE"
 * (and not undefined) — so `null` would read as airplane ON.
 */
export interface ModemStatus {
  /** "ONLINE" | "LPM" (low-power / airplane) | other firmware modes (e.g. "OFFLINE"). */
  operate_mode: string;
}

/**
 * POST /api/modem/online — handlers.rs:171 `modem_online` (also reached via
 * POST /api/modem/airplane {operate_mode:"ONLINE"}, modem_ext.rs:30-32).
 * Sends AT+CFUN=1 (8 s fixed wait). Success data = {status:"ok"}.
 * Failure without "OK" in the reply: 500 {ok:false,error:"AT+CFUN=1 failed",raw:"<AT reply>"}
 * — the extra `raw` field is outside the standard envelope.
 */
export interface ModemOnlineResult {
  status: "ok";
}

/**
 * GET /api/modem/data — modem_ext.rs:6 `modem_data_get`.
 * ubus passthrough: `zwrt_data get_wwaniface {"cid":1}`, shape from page
 * (router/mobile-network/page.tsx `ModemData`) + inventory.
 * The page does `!!enable` / `!!roam_enable`: if the firmware returns these as
 * strings, "0" would read as on.
 */
export interface ModemData {
  cid?: number; // unconfirmed
  /** 1 = auto connect (page default). */
  connect_mode?: number; // unconfirmed
  /** 1 = data roaming allowed. */
  roam_enable?: number; // unconfirmed
  /** 1 = mobile data on. */
  enable?: number; // unconfirmed
  /** "connected" | "disconnected" | "connecting" ... */
  connect_status?: string; // unconfirmed
  /** Declared by the page, never displayed. */
  roll_connect_status?: string; // unconfirmed
}

/** PUT /api/modem/data body (modem_ext.rs:13, ubus `zwrt_data set_wwaniface`, body forwarded as-is). */
export interface ModemDataSetBody {
  cid: number;
  connect_mode: number;
  roam_enable: number;
  enable: number;
  /** Page adds "disconnected" when turning data off. */
  connect_status?: string;
}

/** POST /api/modem/airplane body (modem_ext.rs:24; non-ONLINE → ubus `zte_nwinfo_api nwinfo_set_mode`). */
export interface ModemAirplaneBody {
  operate_mode: "LPM" | "ONLINE" | string;
}

/** PUT /api/modem/network-mode body (modem_ext.rs:39, ubus `nwinfo_set_netselect`). Readback is /api/network/signal `net_select`. */
export interface ModemNetworkModeBody {
  /** "auto_select" | "5G_only" | "5G_4G" | "4G_only" | "3G_only" | "2G_only" (page options). */
  net_select: string;
}

/**
 * GET /api/modem/scan/status — modem_ext.rs:57 `modem_scan_status`.
 * ubus passthrough: `zte_nwinfo_api nwinfo_m_netselect_status`, shape from page.
 * Page treats "done" | "complete" | "2" as finished; anything else = keep polling
 * (2 s × 30).
 */
export interface ModemScanStatus {
  status?: string; // unconfirmed
}

/** One row of the manual carrier scan (page `ScanOperator`). */
export interface ModemScanOperator {
  /** e.g. "46692". Sent back verbatim to /api/modem/register. */
  m_mcc_mnc?: string; // unconfirmed
  m_oper_name?: string; // unconfirmed
  /** Radio access tech, sent back verbatim; value set unknown (mock uses AT+COPS AcT codes "7" LTE / "12" NR). */
  m_rat?: string; // unconfirmed
  /** Mock uses AT+COPS stat codes: "1" available, "2" current, "3" forbidden. */
  m_status?: string; // unconfirmed
}

/**
 * GET /api/modem/scan/results — modem_ext.rs:64 `modem_scan_results`.
 * ubus passthrough: `zte_nwinfo_api nwinfo_m_netselect_contents`, shape from page.
 */
export interface ModemScanResults {
  operators?: ModemScanOperator[]; // unconfirmed
}

/** POST /api/modem/register body (modem_ext.rs:71, ubus `nwinfo_manual_register`). */
export interface ModemRegisterBody {
  m_mcc_mnc?: string;
  m_rat?: string;
}

/**
 * GET /api/modem/register/result — modem_ext.rs:82 `modem_register_result`.
 * ubus passthrough: `zte_nwinfo_api nwinfo_m_netselect_result`, shape from page.
 * Page: "success" | "1" = registered, "fail" | "0" = failed, anything else =
 * keep polling (2 s × 15).
 */
export interface ModemRegisterResult {
  result?: string; // unconfirmed
}

// ─── cell ────────────────────────────────────────────────────────────────────

/** POST /api/cell/lock/nr body (cell.rs:6, ubus `nwinfo_lock_nr_cell`). Page sends strings. */
export interface CellLockNrBody {
  pci: string;
  earfcn: string;
  band?: string;
}

/** POST /api/cell/lock/lte body (cell.rs:17, ubus `nwinfo_lock_lte_cell`). */
export interface CellLockLteBody {
  pci: string;
  earfcn: string;
}

/** POST /api/cell/band/nr body (cell.rs:56, ubus `nwinfo_set_nrbandlock`). bandlock page sends it twice: "nsa" then "sa". */
export interface CellBandNrBody {
  nr5g_type: "nsa" | "sa";
  /** Comma list without "n", e.g. "78,41". */
  nr5g_band: string;
}

/** POST /api/cell/band/lte body (cell.rs:67, ubus `nwinfo_set_gwl_bandlock`). */
export interface CellBandLteBody {
  is_lte_band: string;
  /** Comma list without "B", e.g. "1,3,7". */
  lte_band_mask: string;
  is_gw_band: string;
  gw_band_mask: string;
}

/**
 * One neighbour cell. The page renders every key as a column (String(v)), so
 * the field set is whatever the firmware returns; these are the mock's.
 */
export interface CellNeighbor {
  pci?: number; // unconfirmed
  earfcn?: number; // unconfirmed
  band?: string; // unconfirmed
  rsrp?: number | null; // unconfirmed
  rsrq?: number | null; // unconfirmed
  sinr?: number | null; // unconfirmed
}

/**
 * GET /api/cell/neighbors/nr — cell.rs:42 `cell_neighbors_nr`.
 * ubus passthrough: `zte_nwinfo_api nwinfo_get_nr5g_nbr_contents`, shape from page.
 * Page looks for `cells[]` then `list[]`; otherwise the whole object is one row.
 */
export interface CellNeighborsNr {
  cells?: CellNeighbor[]; // unconfirmed
  list?: CellNeighbor[]; // unconfirmed
}

/**
 * GET /api/cell/neighbors/lte — cell.rs:49 `cell_neighbors_lte`.
 * ubus passthrough: `zte_nwinfo_api nwinfo_get_lte_nbr_contents`, shape from page (same as NR).
 */
export interface CellNeighborsLte {
  cells?: CellNeighbor[]; // unconfirmed
  list?: CellNeighbor[]; // unconfirmed
}

/**
 * GET /api/cell/stc/params — cell.rs:85 `cell_stc_params_get` (PUT = cell.rs:92, same shape as body).
 * ubus passthrough: `zte_nwinfo_api nwinfo_get_stc_white_list_par`, shape from page.
 * B27 (recorded 2026-09-25): 503 "… nwinfo_get_stc_white_list_par … (Method
 * not found)" — the firmware has no STC read methods (status too); the page
 * shows "not available on this firmware".
 */
export interface CellStcParams {
  lte_collect_timer?: string; // unconfirmed
  nrsa_collect_timer?: string; // unconfirmed
  lte_whitelist_max?: string; // unconfirmed
  nrsa_whitelist_max?: string; // unconfirmed
}

/**
 * GET /api/cell/stc/status — cell.rs:103 `cell_stc_status`. B27: 503 Method not found.
 * ubus passthrough: `zte_nwinfo_api nwinfo_get_stc_white_list_status`, shape from page.
 * Page counts true / non-zero number / "1" / "true" / "enabled" as on.
 */
export interface CellStcStatus {
  enabled?: boolean | string | number; // unconfirmed
}

/**
 * GET /api/cell/signal-detect/progress — cell.rs:152 `cell_signal_detect_progress`.
 * ubus passthrough: `zte_nwinfo_api nwinfo_get_progress_and_quality`, shape from page.
 * Page: parseFloat(progress) >= 100 = done.
 * B27 (recorded 2026-09-25): both are strings, "" while idle.
 */
export interface CellSignalDetectProgress {
  /** String on B27 ("" = idle); the default mock sends a number. */
  progress?: number | string;
  /** Firmware quality word; "" while idle (value set unknown). */
  quality?: string;
  [key: string]: unknown;
}

/** One signal-quality detection record (page renders every key as a column). */
export interface CellSignalDetectRecord {
  [key: string]: string | number | null;
}

/**
 * GET /api/cell/signal-detect/results — cell.rs:145 `cell_signal_detect_results`.
 * ubus passthrough: `zte_nwinfo_api nwinfo_get_detect_quality_recorder`, shape from page.
 * Page looks for `cells[]`, `results[]`, `list[]`; otherwise the whole object is one row.
 * B27 answers `{}` when there are no results (recorded 2026-09-25).
 */
export interface CellSignalDetectResults {
  cells?: CellSignalDetectRecord[]; // unconfirmed
  results?: CellSignalDetectRecord[]; // unconfirmed
  list?: CellSignalDetectRecord[]; // unconfirmed
}

/** GET endpoints of this file, keyed by exact path (record.ts). */
export interface ModemGetMap {
  "/api/modem/status": ModemStatus;
  "/api/modem/data": ModemData;
  "/api/modem/scan/status": ModemScanStatus;
  "/api/modem/scan/results": ModemScanResults;
  "/api/modem/register/result": ModemRegisterResult;
  "/api/cell/neighbors/nr": CellNeighborsNr;
  "/api/cell/neighbors/lte": CellNeighborsLte;
  "/api/cell/stc/params": CellStcParams;
  "/api/cell/stc/status": CellStcStatus;
  "/api/cell/signal-detect/results": CellSignalDetectResults;
  "/api/cell/signal-detect/progress": CellSignalDetectProgress;
}
