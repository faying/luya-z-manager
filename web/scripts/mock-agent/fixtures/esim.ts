// eSIM on a removable eUICC — mirrors zte-agent/src/esim.rs (lpac wrapper).
//
// Model: `card` is what lpac would read off the eUICC (chip info, profile
// list, pending notifications). Slow ops (switch / download / delete /
// notifications process) go through the agent's single-slot job: the POST
// returns {job_id, status:"running"} at once, the job finishes after a fixed
// delay and only then is the card changed. Completion is evaluated lazily on
// every request (no timers). While a job runs, profiles/notifications return
// the agent's busy shapes and nickname/delete return 409.
//
// Switch cooldown: 429 within 300 s of the last switch *attempt* (success or
// failure), exactly like esim.rs:380-390. The mock starts with the last
// attempt two days ago.
//
// Scenarios: "nosignal" (or airplane / mobile data off) makes the
// network-touching jobs (download, notifications process) end in error.
// "fakesuccess" makes a switch end as done + rebooting:true ("switched —
// rebooting to finish") while the card never actually changed — the
// unverified-reboot path the inventory flags (esim.rs:440-449).
//
// Persona: an eSTK.me-style card in the SIM slot; the enabled profile is a
// Taiwan travel eSIM (roams on Chunghwa); a China home-number profile and a
// Japan travel profile are disabled. Two notifications from the last switch
// (two days ago) are still pending.

import type { Route, Ctx, Reply } from "../lib.ts";
import { ok, fail, bodyField, clone } from "../lib.ts";
import { shared } from "../shared.ts";
import type {
  EsimStatus,
  EsimProfile,
  EsimProfiles,
  EsimNotification,
  EsimNotifications,
  EsimJob,
  EsimJobStarted,
} from "../../../src/lib/api/schemas/esim.ts";

const DEVICE_UTC_OFFSET_S = 8 * 3600; // device clock = Taiwan wall time labelled UTC
const SWITCH_COOLDOWN_SECS = 300; // esim.rs:58
const MAX_CODE_LEN = 512; // esim.rs:49
const MAX_NICKNAME_LEN = 64; // esim.rs:50, bytes

/** How long each job kind takes in the mock (real: switch ≤ ~35 s, download up to minutes). */
const JOB_MS: Record<EsimJob["kind"], number> = {
  "": 0,
  switch: 9000,
  download: 12000,
  delete: 5000,
  notifications: 4000,
};

// ── Card state ──────────────────────────────────────────────────────────────

const chip: EsimStatus = {
  installed: true,
  eid: "89044045117727480000001984520371",
  default_smdp: null,
  root_smds: "lpa.ds.gsma.com",
  sgp22_version: "2.2.2",
  firmware_version: "3.1.0",
  free_nvm: 325632,
} satisfies EsimStatus;

// Made-up ICCIDs: country prefix + zeros (tools/privacy-scan.sh SCAN_FAKE_ID).
const ICCID_TW = "8988600000000000001";
const ICCID_CN = "8986000000000000002";
const ICCID_JP = "8981000000000000003";

const profiles: EsimProfile[] = [
  {
    iccid: ICCID_TW,
    isdpAid: "A0000005591010FFFFFFFF8900001100",
    profileState: "enabled",
    profileNickname: "台湾旅行",
    serviceProviderName: "Nova Travel",
    profileName: "Taiwan 20GB 10 Days",
    iconType: "none",
    icon: null,
    profileClass: "operational",
  },
  {
    iccid: ICCID_CN,
    isdpAid: "A0000005591010FFFFFFFF8900001000",
    profileState: "disabled",
    profileNickname: "国内主号",
    serviceProviderName: "China Mobile",
    profileName: "China Mobile eSIM",
    iconType: "none",
    icon: null,
    profileClass: "operational",
  },
  {
    iccid: ICCID_JP,
    isdpAid: "A0000005591010FFFFFFFF8900001200",
    profileState: "disabled",
    profileNickname: null,
    serviceProviderName: "Pacific Roam",
    profileName: "Japan 5GB 7 Days",
    iconType: "none",
    icon: null,
    profileClass: "operational",
  },
] satisfies EsimProfile[];

const SMDP: Record<string, string> = {
  [ICCID_TW]: "rsp.novatravel.example",
  [ICCID_CN]: "smdp.cmcc-esim.example",
  [ICCID_JP]: "smdp.pacificroam.example",
};

let nextSeq = 14;
const notifications: EsimNotification[] = [
  { seqNumber: 12, profileManagementOperation: "disable", notificationAddress: SMDP[ICCID_CN], iccid: ICCID_CN },
  { seqNumber: 13, profileManagementOperation: "enable", notificationAddress: SMDP[ICCID_TW], iccid: ICCID_TW },
] satisfies EsimNotification[];

// ── Job slot ────────────────────────────────────────────────────────────────

const job: EsimJob = {
  id: 0,
  kind: "",
  status: "idle",
  message: "",
  iccid: "",
  started_unix: 0,
  finished_unix: 0,
  rebooting: false,
} satisfies EsimJob;

interface Pending {
  dueMs: number;
  run: () => { ok: boolean; message: string; rebooting?: boolean };
}
let pending: Pending | null = null;

/** Real unix seconds of the last switch attempt (drives the cooldown). */
let lastSwitchAttempt = Math.floor(Date.now() / 1000) - 2 * 86400;

const devSecs = (ms: number) => Math.floor(ms / 1000) + DEVICE_UTC_OFFSET_S;

/** Finish the running job if its time has come (esim.rs finish_job). */
function tick(ctx: Ctx): void {
  if (!pending || ctx.now < pending.dueMs) return;
  const p = pending;
  pending = null;
  const r = p.run();
  job.status = r.ok ? "done" : "error";
  job.message = r.message;
  job.rebooting = r.rebooting ?? false;
  job.finished_unix = devSecs(p.dueMs);
}

const running = () => pending !== null;

function startJob(ctx: Ctx, kind: EsimJob["kind"], iccid: string, run: Pending["run"]): number | null {
  if (running()) return null;
  job.id += 1;
  job.kind = kind;
  job.status = "running";
  job.message = "";
  job.iccid = iccid;
  job.started_unix = devSecs(ctx.now);
  job.finished_unix = 0;
  job.rebooting = false;
  pending = { dueMs: ctx.now + JOB_MS[kind], run };
  return job.id;
}

function started(id: number): Reply {
  const data: EsimJobStarted = { job_id: id, status: "running" };
  return ok(data);
}

const BUSY = "operation in progress";

function netUp(ctx: Ctx): boolean {
  return !shared.airplane && shared.mobileData && !ctx.has("nosignal");
}

function isObj(b: unknown): b is Record<string, unknown> {
  return !!b && typeof b === "object" && !Array.isArray(b);
}

/** esim.rs:330 body_str — trimmed, non-empty string. */
function bodyStr(b: unknown, k: string): string | null {
  const v = bodyField(b, k);
  if (typeof v !== "string") return null;
  const t = v.trim();
  return t ? t : null;
}

/** esim.rs:326 valid_iccid — 18..=20 ASCII digits. */
const validIccid = (s: string) => /^\d{18,20}$/.test(s);

// eslint-disable-next-line no-control-regex
const hasControl = (s: string) => /[\u0000-\u001f\u007f-\u009f]/.test(s);

function pushNotification(op: string, iccid: string): void {
  notifications.push({
    seqNumber: nextSeq++,
    profileManagementOperation: op,
    notificationAddress: SMDP[iccid] ?? "smdp.unknown.example",
    iccid,
  });
}

// ── Read handlers ───────────────────────────────────────────────────────────

/** GET /api/esim/status — esim.rs:265 (lpac chip info). */
function status(ctx: Ctx): Reply {
  tick(ctx);
  return { data: clone(chip), delayMs: 600 };
}

/** GET /api/esim/profiles — esim.rs:287 (lpac profile list). */
function profilesGet(ctx: Ctx): Reply {
  tick(ctx);
  if (running()) {
    const busy: EsimProfiles = { installed: true, busy: true, profiles: null };
    return ok(busy);
  }
  const data: EsimProfiles = { installed: true, busy: false, profiles: clone(profiles) };
  return { data, delayMs: 500 };
}

/** GET /api/esim/notifications — esim.rs:303 (lpac notification list). */
function notificationsGet(ctx: Ctx): Reply {
  tick(ctx);
  if (running()) {
    const none: EsimNotifications = null;
    return ok(none);
  }
  const data: EsimNotifications = clone(notifications);
  return { data, delayMs: 400 };
}

/** GET /api/esim/job — esim.rs:317 (in-memory). */
function jobGet(ctx: Ctx): Reply {
  tick(ctx);
  return ok(clone(job));
}

// ── Write handlers ──────────────────────────────────────────────────────────

/** POST /api/esim/nickname — esim.rs:336. Synchronous; `{ok:true}` with no data. */
function nickname(ctx: Ctx): Reply {
  tick(ctx);
  if (running()) return fail(BUSY, 409);
  if (!isObj(ctx.body)) return fail("invalid JSON", 400);
  const iccid = bodyStr(ctx.body, "iccid");
  if (!iccid || !validIccid(iccid)) return fail("invalid iccid", 400);
  const raw = ctx.body.nickname;
  const nick = typeof raw === "string" ? raw.trim() : "";
  if (Buffer.byteLength(nick, "utf8") > MAX_NICKNAME_LEN) return fail("nickname too long", 400);
  const p = profiles.find((x) => x.iccid === iccid);
  if (!p) return fail("lpac: es10c_set_nickname: iccidOrAidNotFound (code -1)", 502);
  p.profileNickname = nick === "" ? null : nick;
  return { delayMs: 400 };
}

/** POST /api/esim/switch — esim.rs:371. */
function switchProfile(ctx: Ctx): Reply {
  tick(ctx);
  if (!isObj(ctx.body)) return fail("invalid JSON", 400);
  const iccid = bodyStr(ctx.body, "iccid");
  if (!iccid || !validIccid(iccid)) return fail("invalid iccid", 400);
  const now = Math.floor(ctx.now / 1000);
  if (lastSwitchAttempt !== 0 && now - lastSwitchAttempt < SWITCH_COOLDOWN_SECS) {
    const wait = SWITCH_COOLDOWN_SECS - (now - lastSwitchAttempt);
    return fail(`card needs a cooldown after the last switch attempt — wait ${wait}s and retry`, 429);
  }
  const fake = ctx.has("fakesuccess");
  const id = startJob(ctx, "switch", iccid, () => {
    const target = profiles.find((p) => p.iccid === iccid);
    if (!target) return { ok: false, message: "lpac: es10c_enable_profile: iccidOrAidNotFound (code -1)" };
    if (target.profileState === "enabled") {
      return { ok: false, message: "lpac: es10c_enable_profile: profileNotInDisabledState (code -1)" };
    }
    // Fast path didn't converge → agent reports done and reboots. Under
    // fakesuccess the card is left as it was.
    if (fake) return { ok: true, message: "switched — rebooting to finish", rebooting: true };
    const old = profiles.find((p) => p.profileState === "enabled");
    if (old) {
      old.profileState = "disabled";
      pushNotification("disable", old.iccid);
    }
    target.profileState = "enabled";
    pushNotification("enable", target.iccid);
    return { ok: true, message: "switched — no reboot needed" };
  });
  if (id === null) return fail(BUSY, 409);
  lastSwitchAttempt = now;
  return started(id);
}

let downloads = 0;

/** POST /api/esim/download — esim.rs:458. Downloads (never enables), then processes notifications. */
function download(ctx: Ctx): Reply {
  tick(ctx);
  if (!isObj(ctx.body)) return fail("invalid JSON", 400);
  const code = bodyStr(ctx.body, "code");
  if (!code) return fail("missing activation code", 400);
  if (code.length > MAX_CODE_LEN || hasControl(code)) return fail("invalid activation code", 400);
  const confRaw = ctx.body.confirmation_code;
  const conf = typeof confRaw === "string" ? confRaw.trim() : "";
  if (conf.length > MAX_CODE_LEN || hasControl(conf)) return fail("invalid confirmation code", 400);
  const online = netUp(ctx);
  const id = startJob(ctx, "download", "", () => {
    // lpac activation code: LPA:1$<smdp>$<matchingId>[$...]
    const m = /^LPA:1\$([^$]+)\$([^$]*)/.exec(code);
    if (!m) return { ok: false, message: "lpac: es10b_get_euicc_challenge_and_info: invalid activation code (code -1)" };
    if (!online) return { ok: false, message: `lpac: es9p_initiate_authentication: curl: Could not resolve host: ${m[1]} (code -1)` };
    const size = 38912;
    if ((chip.free_nvm ?? 0) < size) return { ok: false, message: "lpac: es10b_load_bound_profile_package: insufficientMemory (code -1)" };
    downloads += 1;
    const iccid = `8985235092301${String(1200 + downloads).padStart(6, "0")}${downloads % 10}`;
    SMDP[iccid] = m[1];
    profiles.push({
      iccid,
      isdpAid: `A0000005591010FFFFFFFF89000013${String(downloads).padStart(2, "0")}`,
      profileState: "disabled",
      profileNickname: null,
      serviceProviderName: "Nova Travel",
      profileName: "Asia 10GB 15 Days",
      iconType: "none",
      icon: null,
      profileClass: "operational",
    });
    chip.free_nvm = (chip.free_nvm ?? 0) - size;
    // `notification process -a -r` delivers and removes every pending notification.
    notifications.length = 0;
    return { ok: true, message: "profile downloaded" };
  });
  if (id === null) return fail(BUSY, 409);
  return started(id);
}

/** POST /api/esim/delete — esim.rs:502. Checks the card first; refuses the enabled profile. */
function deleteProfile(ctx: Ctx): Reply {
  tick(ctx);
  if (!isObj(ctx.body)) return fail("invalid JSON", 400);
  const iccid = bodyStr(ctx.body, "iccid");
  if (!iccid || !validIccid(iccid)) return fail("invalid iccid", 400);
  if (running()) return fail(BUSY, 409);
  const p = profiles.find((x) => x.iccid === iccid);
  if (!p) return fail("profile not found", 404);
  if (p.profileState === "enabled") return fail("cannot delete the active profile — switch away first", 400);
  const online = netUp(ctx);
  const id = startJob(ctx, "delete", iccid, () => {
    const i = profiles.findIndex((x) => x.iccid === iccid);
    if (i < 0) return { ok: false, message: "lpac: es10c_delete_profile: iccidOrAidNotFound (code -1)" };
    profiles.splice(i, 1);
    chip.free_nvm = (chip.free_nvm ?? 0) + 38912;
    // Best-effort notification delivery: offline, the delete notification stays pending.
    if (online) notifications.length = 0;
    else pushNotification("delete", iccid);
    return { ok: true, message: "profile deleted" };
  });
  if (id === null) return fail(BUSY, 409);
  return started(id);
}

/** POST /api/esim/notifications/process — esim.rs:552. Body ignored. */
function notificationsProcess(ctx: Ctx): Reply {
  tick(ctx);
  const online = netUp(ctx);
  const id = startJob(ctx, "notifications", "", () => {
    if (notifications.length > 0 && !online) {
      const addr = notifications[0].notificationAddress;
      return { ok: false, message: `lpac: es9p_handle_notification: curl: Could not resolve host: ${addr} (code -1)` };
    }
    notifications.length = 0;
    return { ok: true, message: "notifications processed" };
  });
  if (id === null) return fail(BUSY, 409);
  return started(id);
}

// ── Routes ──────────────────────────────────────────────────────────────────

export const routes: Route[] = [
  { method: "GET", path: "/api/esim/status", handler: status },
  { method: "GET", path: "/api/esim/profiles", handler: profilesGet },
  { method: "GET", path: "/api/esim/notifications", handler: notificationsGet },
  { method: "GET", path: "/api/esim/job", handler: jobGet },
  { method: "POST", path: "/api/esim/switch", handler: switchProfile },
  { method: "POST", path: "/api/esim/download", handler: download },
  { method: "POST", path: "/api/esim/nickname", handler: nickname },
  { method: "POST", path: "/api/esim/delete", handler: deleteProfile },
  { method: "POST", path: "/api/esim/notifications/process", handler: notificationsProcess },
];
