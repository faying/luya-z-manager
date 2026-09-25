// Response shapes for the eSIM (removable eUICC) endpoints of zte-agent
// (zte-agent/src/esim.rs). The agent wraps lpac (/data/esim/lpac, QMI over
// QRTR); list payloads are lpac's own JSON (`payload.data` of its final
// {"type":"lpa"} line) passed through, the rest the agent builds itself.
//
// Types only — no runtime code.

/**
 * GET /api/esim/status — esim.rs:265 `status`.
 * `{installed:false}` alone when the lpac bundle isn't deployed. Otherwise
 * fields are picked out of `lpac chip info`; any the chip doesn't report come
 * back as JSON null (serde `Value` indexing of a missing key). lpac error → 502.
 */
export interface EsimStatus {
  installed: boolean;
  /** 32-digit EID (lpac eidValue). */
  eid?: string | null;
  /** EuiccConfiguredAddresses.defaultDpAddress — often null/"" on consumer cards. Page doesn't show it. */
  default_smdp?: string | null;
  /** EuiccConfiguredAddresses.rootDsAddress, e.g. "lpa.ds.gsma.com". */
  root_smds?: string | null;
  /** EUICCInfo2.profileVersion, e.g. "2.2.2". */
  sgp22_version?: string | null;
  /** EUICCInfo2.euiccFirmwareVer. */
  firmware_version?: string | null;
  /** EUICCInfo2.extCardResource.freeNonVolatileMemory, bytes. */
  free_nvm?: number | null;
}

/** One entry of `lpac profile list` (lpac JSON, passed through). */
export interface EsimProfile {
  iccid: string;
  isdpAid: string;
  profileState: "enabled" | "disabled";
  profileNickname: string | null;
  serviceProviderName: string;
  profileName: string;
  /** lpac emits these; the page doesn't declare them. */
  iconType?: string | null;
  icon?: string | null;
  profileClass: "test" | "provisioning" | "operational" | string;
}

/**
 * GET /api/esim/profiles — esim.rs:287 `profiles`.
 * - not installed: `{installed:false, profiles:[]}` (no `busy`)
 * - job running: `{installed:true, busy:true, profiles:null}` (no APDU sent)
 * - otherwise `{installed:true, busy:false, profiles:[…]}`
 */
export interface EsimProfiles {
  installed: boolean;
  busy?: boolean;
  profiles: EsimProfile[] | null;
}

/** One entry of `lpac notification list` (lpac JSON, passed through). */
export interface EsimNotification {
  seqNumber: number;
  /** "install" | "enable" | "disable" | "delete". */
  profileManagementOperation: string;
  /** SM-DP+ host the notification is delivered to. */
  notificationAddress: string;
  iccid: string;
}

/**
 * GET /api/esim/notifications — esim.rs:303 `notifications`.
 * `[]` when not installed; `null` while a job is running (page: `Notification[] | null`).
 */
export type EsimNotifications = EsimNotification[] | null;

/**
 * GET /api/esim/job — esim.rs:317 `job`; struct esim.rs:65 `JobState`.
 * A single slot: the last job (or the idle placeholder, id 0, kind "").
 * page expects no `started_unix` / `finished_unix` (JobResp omits them).
 */
export interface EsimJob {
  id: number;
  kind: "" | "switch" | "download" | "delete" | "notifications";
  status: "idle" | "running" | "done" | "error";
  /** Result text: "switched — no reboot needed", "switched — rebooting to finish",
   *  "profile downloaded", "profile deleted", "notifications processed", or the lpac error. */
  message: string;
  /** Target ICCID for switch/delete; "" otherwise. */
  iccid: string;
  /** Unix seconds, device clock (local wall time labelled UTC). 0 when unset. */
  started_unix: number;
  finished_unix: number;
  /** A finished switch that didn't converge and is about to reboot the device. */
  rebooting: boolean;
}

/** `data` of POST /api/esim/switch, /download, /delete, /notifications/process. */
export interface EsimJobStarted {
  job_id: number;
  status: "running";
}

/** POST /api/esim/switch and /delete body. ICCID: 18–20 ASCII digits. */
export interface EsimIccidRequest {
  iccid: string;
}

/** POST /api/esim/download body (code ≤ 512 bytes, no control chars). */
export interface EsimDownloadRequest {
  code: string;
  confirmation_code?: string;
}

/** POST /api/esim/nickname body — "" clears; ≤ 64 bytes. Reply is `{ok:true}` with no data. */
export interface EsimNicknameRequest {
  iccid: string;
  nickname: string;
}

/** GET endpoints of this area. All four GETs except /job talk to the eUICC. */
export interface EsimGetMap {
  "/api/esim/status": EsimStatus;
  "/api/esim/profiles": EsimProfiles;
  "/api/esim/notifications": EsimNotifications;
  "/api/esim/job": EsimJob;
}
