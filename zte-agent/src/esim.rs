//! eSIM (removable eUICC) manager — wraps lpac to manage profiles on an
//! eSTK.me / 5ber-style eUICC card in the physical SIM slot.
//!
//! lpac lives under `/data/esim/` (Alpine musl build with the `qmi_qrtr` APDU
//! driver compiled in, plus its shared-lib closure and a `statx` compat shim
//! for the device's older musl — see scripts/esim/). It talks straight to the
//! modem's QMI UIM service over QRTR, coexisting with the ZTE daemons.
//!
//! The platform quirk (verified on-device): `profile enable` switches the CARD
//! immediately, but the ZTE userspace only reads the SIM at daemon init — no
//! runtime QMI/ubus poke makes it re-read. The switch path resyncs it without a
//! full reboot: power-cycle the UIM session (modem re-reads), then restart just
//! `zte_topsw_mdm` (userspace re-reads; nwinfo/data re-dial off its signal).
//! This keeps the daemon sync barrier healthy and reattaches data in under a
//! minute. If identity doesn't converge or the barrier breaks, it falls back to
//! a reboot, which is always safe. APN auto-selects by the new profile's PLMN.
//! Whether the card switched at all is read back from its own profile list,
//! not taken from lpac's exit; a card that answers catBusy is reported as
//! needing a device restart (or reinsert), with no retries and no cooldown.
//! On success, superseded enable/disable notifications are removed and the
//! switch's own are sent once data is back (rules above `plan_notifications`).
//!
//! Slow / network-touching operations (download, delete, notification
//! processing, switch) run on a detached thread guarded by a single-slot job,
//! mirroring the ShellCrash profile manager; the UI polls `/api/esim/job`.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};

use crate::handlers::AppState;

const LPAC_BIN: &str = "/data/esim/lpac";
const LIB_DIR: &str = "/data/esim/lib";
const COMPAT_SHIM: &str = "/data/esim/lib/libmusl-compat.so";
const WAKE_LOCK: &str = "/sys/power/wake_lock";
const WAKE_UNLOCK: &str = "/sys/power/wake_unlock";
const WAKE_TAG: &str = "esim_op";

/// Quick card-local ops (chip info, profile list) — no network involved.
const LPAC_TIMEOUT_FAST: Duration = Duration::from_secs(30);
/// Ops that talk to SM-DP+ servers (download, notifications).
const LPAC_TIMEOUT_NET: Duration = Duration::from_secs(240);

const MAX_CODE_LEN: usize = 512;
const MAX_NICKNAME_LEN: usize = 64;

/// Reading the profile list back after `profile enable` (the card is the judge
/// of whether the switch happened, not lpac's exit): a few tries, because a
/// timed-out enable can leave the card answering slowly for a moment.
const READBACK_TRIES: usize = 3;
const READBACK_GAP: Duration = Duration::from_millis(1500);
/// Sending this switch's enable/disable notifications waits for the data call
/// to come back (checked every `NOTIF_DATA_POLL`, at most `NOTIF_DATA_WAIT`)
/// and then gives lpac `LPAC_TIMEOUT_NOTIF`: while it runs it holds the job
/// slot, and the user may want to switch straight back if the new profile has
/// no service — so it is kept short, and skipped when data is not up.
const NOTIF_DATA_POLL: Duration = Duration::from_secs(5);
const NOTIF_DATA_WAIT: Duration = Duration::from_secs(60);
const LPAC_TIMEOUT_NOTIF: Duration = Duration::from_secs(45);

/* -------------------------------------------------------------- *
 *  State
 * -------------------------------------------------------------- */

#[derive(Clone, Serialize)]
pub struct JobState {
    id: u64,
    kind: String,   // "switch" | "download" | "delete" | "notifications"
    status: String, // "idle" | "running" | "done" | "error"
    message: String,
    iccid: String,
    started_unix: u64,
    finished_unix: u64,
    /// Set when the finished job scheduled a device reboot (switch).
    rebooting: bool,
    /// Machine-readable cause of an error, so the UIs need not parse
    /// `message`: "card_busy" (the card answered catBusy and the profile did
    /// not change — only a device restart or reinserting the card clears it),
    /// "not_switched" (lpac or the card refused; the card still has the old
    /// profile), "" otherwise.
    reason: String,
}

pub struct EsimAdmin {
    job: Arc<Mutex<JobState>>,
    running: Arc<AtomicBool>,
}

impl EsimAdmin {
    pub fn new() -> Self {
        Self {
            job: Arc::new(Mutex::new(JobState {
                id: 0,
                kind: String::new(),
                status: "idle".into(),
                message: String::new(),
                iccid: String::new(),
                started_unix: 0,
                finished_unix: 0,
                rebooting: false,
                reason: String::new(),
            })),
            running: Arc::new(AtomicBool::new(false)),
        }
    }
}

/* -------------------------------------------------------------- *
 *  lpac invocation
 * -------------------------------------------------------------- */

fn installed() -> bool {
    Path::new(LPAC_BIN).exists() && Path::new(LIB_DIR).is_dir()
}

/// Run lpac and return the `data` field of its final `{"type":"lpa"}` line.
/// lpac emits one JSON object per stdout line: progress lines first, then the
/// lpa result line with code / message / data.
fn run_lpac(args: &[&str], timeout: Duration) -> Result<Value, String> {
    if !installed() {
        return Err("lpac not installed under /data/esim".into());
    }
    let child = Command::new(LPAC_BIN)
        .args(args)
        .env("LD_LIBRARY_PATH", LIB_DIR)
        .env("LD_PRELOAD", COMPAT_SHIM)
        .env("LPAC_APDU", "qmi_qrtr")
        .env("LPAC_HTTP", "curl")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn lpac failed: {e}"))?;

    // Watchdog: kill the child if it wedges (e.g. modem unresponsive). A killed
    // child makes wait_with_output return promptly with whatever was written.
    let pid = child.id();
    let done = Arc::new(AtomicBool::new(false));
    {
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let step = Duration::from_millis(250);
            let mut waited = Duration::ZERO;
            while waited < timeout {
                if done.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(step);
                waited += step;
            }
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
        });
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("lpac wait failed: {e}"));
    done.store(true, Ordering::Relaxed);
    let out = out?;

    parse_lpac_output(&String::from_utf8_lossy(&out.stdout), out.status.success())
        .map_err(|e| if e.is_empty() {
            format!("lpac exited with {} and no result (killed after timeout?)", out.status)
        } else {
            e
        })
}

/// lpac's stdout → the `data` of its final `{"type":"lpa"}` line, or the error
/// text. `Err("")` = no result line and lpac did not exit cleanly (the caller
/// words that, it knows the exit status).
fn parse_lpac_output(stdout: &str, exited_ok: bool) -> Result<Value, String> {
    for line in stdout.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v["type"].as_str() != Some("lpa") {
            continue;
        }
        let payload = &v["payload"];
        let code = payload["code"].as_i64().unwrap_or(-1);
        if code == 0 {
            return Ok(payload["data"].clone());
        }
        let msg = payload["message"].as_str().unwrap_or("unknown");
        let detail = payload["data"].as_str().unwrap_or("");
        return Err(if detail.is_empty() {
            format!("lpac: {msg} (code {code})")
        } else {
            format!("lpac: {msg}: {detail} (code {code})")
        });
    }
    if !exited_ok {
        return Err(String::new());
    }
    Err("lpac produced no result line".into())
}

fn hold_wake_lock() {
    let _ = std::fs::write(WAKE_LOCK, WAKE_TAG);
}

fn release_wake_lock() {
    let _ = std::fs::write(WAKE_UNLOCK, WAKE_TAG);
}

/* -------------------------------------------------------------- *
 *  ZTE stack resync (the no-reboot switch path)
 * -------------------------------------------------------------- */

/// QMI UIM power-cycle helper, deployed alongside lpac. Used to make the modem
/// re-read the card after a profile switch (see qmi_uim_probe.c `simreset`).
const QMI_PROBE: &str = "/data/esim/qmi_uim_probe";
/// The one daemon.conf daemon we restart on a switch. It caches the SIM
/// identity; restarting it makes the ZTE userspace re-read, and nwinfo/data
/// re-dial off its signal on their own — verified on-device to keep the daemon
/// sync barrier healthy. Restarting the whole set is unnecessary.
const MDM_INITD: &str = "/etc/init.d/zte_topsw_mdm";

fn run_cmd(cmd: &str) -> String {
    Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// (imsi, iccid) the ZTE modem stack currently believes is active — its cached
/// identity, which lags the card's enabled profile until the stack re-reads.
/// The ZTE `sim_iccid` carries a trailing BCD pad nibble ("…072F") and reads
/// empty for a beat right after a re-read, so callers lean on the IMSI.
fn ubus_sim_identity() -> (String, String) {
    let v = serde_json::from_str::<Value>(&run_cmd("ubus call zwrt_zte_mdm.api get_sim_info '{}'"))
        .unwrap_or(Value::Null);
    let imsi = v["sim_imsi"].as_str().unwrap_or("").to_string();
    let iccid = v["sim_iccid"].as_str().unwrap_or("").to_string();
    (imsi, iccid)
}

/// Has the ZTE stack converged onto the switched-in profile? Primary signal is
/// the IMSI changing away from the pre-switch value (IMSI always populates);
/// a normalized ICCID match is accepted too once `sim_iccid` fills in.
fn switch_converged(target_iccid: &str, old_imsi: &str) -> bool {
    if !ubus_sync_ok() {
        return false;
    }
    let (imsi, iccid) = ubus_sim_identity();
    // An empty pre-switch IMSI read would make any IMSI look "changed": then
    // only the ICCID counts.
    let imsi_changed = !imsi.is_empty() && !old_imsi.is_empty() && imsi != old_imsi;
    let iccid_matches = iccid_eq(&iccid, target_iccid);
    imsi_changed || iccid_matches
}

/// Is the ZTE daemon sync barrier healthy? A broken barrier after the mdm
/// restart means the stack is wedged and we must fall back to a reboot.
fn ubus_sync_ok() -> bool {
    serde_json::from_str::<Value>(&run_cmd("ubus call zwrt_topsw_daemon.sync get_sync_info '{}'"))
        .ok()
        .and_then(|v| v["noSyncModuleName"].as_str().map(|s| s == "sync success"))
        .unwrap_or(false)
}

/* -------------------------------------------------------------- *
 *  Read endpoints
 * -------------------------------------------------------------- */

/// GET /api/esim/status — chip identity + storage. `installed:false` means the
/// lpac bundle isn't deployed (UI shows setup instructions).
pub fn status(_state: &AppState) -> (u16, Value) {
    if !installed() {
        return (200, json!({"ok": true, "data": {"installed": false}}));
    }
    match run_lpac(&["chip", "info"], LPAC_TIMEOUT_FAST) {
        Ok(d) => {
            let info2 = &d["EUICCInfo2"];
            (200, json!({"ok": true, "data": {
                "installed": true,
                "eid": d["eidValue"],
                "default_smdp": d["EuiccConfiguredAddresses"]["defaultDpAddress"],
                "root_smds": d["EuiccConfiguredAddresses"]["rootDsAddress"],
                "sgp22_version": info2["profileVersion"],
                "firmware_version": info2["euiccFirmwareVer"],
                "free_nvm": info2["extCardResource"]["freeNonVolatileMemory"],
            }}))
        }
        Err(e) => (502, json!({"ok": false, "error": e})),
    }
}

/// GET /api/esim/profiles
pub fn profiles(state: &AppState) -> (u16, Value) {
    if !installed() {
        return (200, json!({"ok": true, "data": {"installed": false, "profiles": []}}));
    }
    // While a job runs the card may be mid-operation (or the device about to
    // reboot); skip the APDU round-trip and let the UI keep its last list.
    if state.esim.running.load(Ordering::Relaxed) {
        return (200, json!({"ok": true, "data": {"installed": true, "busy": true, "profiles": Value::Null}}));
    }
    match run_lpac(&["profile", "list"], LPAC_TIMEOUT_FAST) {
        Ok(d) => (200, json!({"ok": true, "data": {"installed": true, "busy": false, "profiles": d}})),
        Err(e) => (502, json!({"ok": false, "error": e})),
    }
}

/// GET /api/esim/notifications
pub fn notifications(state: &AppState) -> (u16, Value) {
    if !installed() {
        return (200, json!({"ok": true, "data": []}));
    }
    if state.esim.running.load(Ordering::Relaxed) {
        return (200, json!({"ok": true, "data": Value::Null}));
    }
    match run_lpac(&["notification", "list"], LPAC_TIMEOUT_FAST) {
        Ok(d) => (200, json!({"ok": true, "data": d})),
        Err(e) => (502, json!({"ok": false, "error": e})),
    }
}

/// An eSIM job (switch, download, …) is running: the ICCID may flip mid-way.
pub fn busy(state: &AppState) -> bool {
    state.esim.running.load(Ordering::Relaxed)
}

/// GET /api/esim/job
pub fn job(state: &AppState) -> (u16, Value) {
    let j = state.esim.job.lock().unwrap().clone();
    (200, json!({"ok": true, "data": j}))
}

/* -------------------------------------------------------------- *
 *  Mutations
 * -------------------------------------------------------------- */

fn valid_iccid(s: &str) -> bool {
    (18..=20).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

fn body_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v[key].as_str().map(str::trim).filter(|s| !s.is_empty())
}

/// POST /api/esim/nickname — { iccid, nickname } (empty nickname clears).
/// Synchronous: it's a single local APDU exchange.
pub fn nickname(state: &AppState, body: &[u8]) -> (u16, Value) {
    if state.esim.running.load(Ordering::Relaxed) {
        return (409, json!({"ok": false, "error": "operation in progress"}));
    }
    let v: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let iccid = match body_str(&v, "iccid") {
        Some(i) if valid_iccid(i) => i.to_string(),
        _ => return (400, json!({"ok": false, "error": "invalid iccid"})),
    };
    let nickname = v["nickname"].as_str().unwrap_or("").trim().to_string();
    if nickname.len() > MAX_NICKNAME_LEN {
        return (400, json!({"ok": false, "error": "nickname too long"}));
    }
    match run_lpac(&["profile", "nickname", &iccid, &nickname], LPAC_TIMEOUT_FAST) {
        Ok(_) => (200, json!({"ok": true})),
        Err(e) => (502, json!({"ok": false, "error": e})),
    }
}

/// lpac 2.3.0 maps enableResult 1–4 to names and everything else (5 = catBusy,
/// 127 = undefinedError) to "unknown"; newer builds may spell it out.
fn is_card_busy(err: &str) -> bool {
    err.contains("es10c_enable_profile") && (err.contains(": unknown") || err.contains("catBusy"))
}

/// ICCIDs compared the way the card and the ZTE stack spell them: lpac and
/// `sim_iccid` can carry the BCD pad nibble ("…072F").
fn iccid_eq(a: &str, b: &str) -> bool {
    let a = a.trim().trim_end_matches(['F', 'f']);
    !a.is_empty() && a == b.trim().trim_end_matches(['F', 'f'])
}

/// What the card's own profile list says about the switch target.
#[derive(Debug, Clone, Copy, PartialEq)]
enum CardSays {
    /// The target is the enabled profile.
    Enabled,
    /// The list was read and the target is not enabled (or not on the card).
    NotEnabled,
    /// The list could not be read.
    Unknown,
}

/// `lpac profile list` data → is `iccid` the enabled profile?
fn target_state(list: &Value, iccid: &str) -> CardSays {
    match list.as_array() {
        None => CardSays::Unknown,
        Some(a) => {
            let on = a.iter().any(|p| {
                iccid_eq(p["iccid"].as_str().unwrap_or(""), iccid)
                    && p["profileState"].as_str() == Some("enabled")
            });
            if on { CardSays::Enabled } else { CardSays::NotEnabled }
        }
    }
}

#[derive(Debug, PartialEq)]
enum EnableOutcome {
    /// The card is on the target: resync the ZTE stack (simreset + mdm restart,
    /// convergence wait, reboot fallback).
    Resync,
    /// The card did not switch. `busy` = it answered catBusy.
    Failed { busy: bool, msg: String },
}

/// Did the switch happen? The card decides, not lpac's exit: a killed
/// (timed-out) or erroring enable may still have switched the card, and "not
/// in disabled state" means the target was already on. Only when the list
/// can't be read does lpac's own answer stand.
fn decide_enable(lpac: &Result<(), String>, card: CardSays) -> EnableOutcome {
    match (card, lpac) {
        (CardSays::Enabled, _) => EnableOutcome::Resync,
        (CardSays::Unknown, Ok(())) => EnableOutcome::Resync,
        (CardSays::NotEnabled, Ok(())) => EnableOutcome::Failed {
            busy: false,
            msg: "lpac reported success but the card still has the old profile enabled".into(),
        },
        (_, Err(e)) if is_card_busy(e) => EnableOutcome::Failed {
            busy: true,
            // "card is busy" is matched by touch-ui builds before `reason`.
            msg: format!(
                "card is busy (catBusy) — restart the device (or take the card out and put it back), then switch again ({e})"
            ),
        },
        (_, Err(e)) => EnableOutcome::Failed { busy: false, msg: e.clone() },
    }
}

/// Read the profile list a few times until it answers.
fn read_profiles_retry() -> Option<Value> {
    for i in 0..READBACK_TRIES {
        if i > 0 {
            std::thread::sleep(READBACK_GAP);
        }
        if let Ok(v) = run_lpac(&["profile", "list"], LPAC_TIMEOUT_FAST) {
            if v.is_array() {
                return Some(v);
            }
        }
    }
    None
}

/* ---- pending notifications after a switch ----
 *
 * Every enable on the card queues two notifications for the SM-DP+ servers:
 * "enable" for the profile switched in and "disable" for the one switched out.
 * Until 10-04 the switch never touched them (only download/delete sent theirs),
 * so every back-and-forth piled up more on the card. Rules, conservative:
 *
 * - Superseded = an enable/disable notification for a profile that has a newer
 *   (higher seqNumber) enable/disable notification for the same ICCID. Its
 *   state change was overtaken by a later one; the server only cares about the
 *   latest, and sending it late would report an old state. These are REMOVED
 *   (card-local, no network) on every successful switch, on both the converged
 *   and the reboot path. lpac's `notification remove` loses them for good —
 *   that is the point, and the newer one for the same profile stays.
 * - This switch's own (seqNumber above the highest seen just before enabling,
 *   enable/disable only) are SENT with `process -r`, which removes each one
 *   only after the server accepted it (lpac stops at the first failure and
 *   keeps the rest). Only on the converged path, only once the data call is
 *   back, and with a short timeout; otherwise they stay pending for the eSIM
 *   page's "send notifications".
 * - The latest enable/disable of a profile this switch didn't touch, and every
 *   install/delete notification, are never removed or sent here.
 */

/// seqNumber of one `notification list` entry (lpac prints a number).
fn notif_seq(n: &Value) -> Option<i64> {
    n["seqNumber"].as_i64().or_else(|| n["seqNumber"].as_str().and_then(|s| s.parse().ok()))
}

fn notif_is_state_change(n: &Value) -> bool {
    matches!(n["profileManagementOperation"].as_str(), Some("enable") | Some("disable"))
}

#[derive(Debug, Default, PartialEq)]
struct NotifPlan {
    /// superseded enable/disable — remove without sending
    remove: Vec<i64>,
    /// this switch's enable/disable — send (and remove once delivered)
    send: Vec<i64>,
}

/// `notification list` data (+ the highest seqNumber seen before the enable,
/// None = unknown, then nothing is sent) → what to remove and what to send.
fn plan_notifications(list: &Value, seq_before: Option<i64>) -> NotifPlan {
    let items: Vec<(i64, String)> = list
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|n| notif_is_state_change(n))
                .filter_map(|n| {
                    let iccid = n["iccid"].as_str()?.trim().trim_end_matches(['F', 'f']).to_string();
                    if iccid.is_empty() {
                        return None;
                    }
                    Some((notif_seq(n)?, iccid))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut plan = NotifPlan::default();
    for (seq, iccid) in &items {
        let newest = items.iter().filter(|(_, i)| i == iccid).map(|(s, _)| *s).max().unwrap_or(*seq);
        if *seq < newest {
            plan.remove.push(*seq);
        } else if seq_before.is_some_and(|b| *seq > b) {
            plan.send.push(*seq);
        }
    }
    plan.remove.sort_unstable();
    plan.send.sort_unstable();
    plan
}

/// Highest seqNumber of any pending notification (None = list unreadable;
/// Some(-1) = none pending).
fn max_notif_seq(list: &Value) -> Option<i64> {
    list.as_array().map(|a| a.iter().filter_map(notif_seq).max().unwrap_or(-1))
}

/// Remove the superseded notifications now (card-local). Returns the ones to
/// send later. Best effort: any lpac failure just leaves them pending.
fn prune_notifications(seq_before: Option<i64>) -> Vec<i64> {
    let list = match run_lpac(&["notification", "list"], LPAC_TIMEOUT_FAST) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("esim: notification list after switch failed: {e}");
            return Vec::new();
        }
    };
    let plan = plan_notifications(&list, seq_before);
    if !plan.remove.is_empty() {
        let seqs: Vec<String> = plan.remove.iter().map(i64::to_string).collect();
        let mut args = vec!["notification", "remove"];
        args.extend(seqs.iter().map(String::as_str));
        match run_lpac(&args, LPAC_TIMEOUT_FAST) {
            Ok(_) => eprintln!("esim: removed superseded notifications {seqs:?}"),
            Err(e) => eprintln!("esim: removing superseded notifications {seqs:?} failed: {e}"),
        }
    }
    plan.send
}

/// Data call up? (zwrt_data cid 1, the same read netinfo uses.)
fn data_up() -> bool {
    crate::netinfo::data_connected(&crate::netinfo::read_wwan())
}

/// After a converged switch: once data is back, send this switch's
/// notifications as a short "notifications" job. Skipped (left pending) when
/// data doesn't come back in time or another eSIM job holds the slot.
fn send_switch_notifications(job: &Arc<Mutex<JobState>>, running: &Arc<AtomicBool>, seqs: Vec<i64>) {
    if seqs.is_empty() {
        return;
    }
    let mut waited = Duration::ZERO;
    loop {
        // The first sleep also lets the UIs (1–1.5 s polls) see the switch
        // job's own result before this job takes the slot.
        std::thread::sleep(NOTIF_DATA_POLL);
        waited += NOTIF_DATA_POLL;
        if data_up() {
            break;
        }
        if waited >= NOTIF_DATA_WAIT {
            eprintln!("esim: data not back after the switch; notifications {seqs:?} left pending");
            return;
        }
    }
    if running.compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed).is_err() {
        return;
    }
    {
        let mut j = job.lock().unwrap();
        let next = j.id.wrapping_add(1);
        *j = JobState {
            id: next,
            kind: "notifications".into(),
            status: "running".into(),
            message: String::new(),
            iccid: String::new(),
            started_unix: now_unix(),
            finished_unix: 0,
            rebooting: false,
            reason: String::new(),
        };
    }
    let strs: Vec<String> = seqs.iter().map(i64::to_string).collect();
    let mut args = vec!["notification", "process", "-r"];
    args.extend(strs.iter().map(String::as_str));
    let n = seqs.len();
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_lpac(&args, LPAC_TIMEOUT_NOTIF).map(|_| format!("sent {n} notification(s) for the switch"))
    }))
    .unwrap_or_else(|_| Err("internal panic".into()));
    if let Err(e) = &res {
        eprintln!("esim: sending switch notifications {strs:?} failed: {e}");
    }
    finish_job(job, running, res, false);
}

/// POST /api/esim/switch — { iccid }. Enables the profile on the card, then
/// moves the ZTE stack onto it (see module docs), rebooting only if that
/// doesn't converge. The UI warns about the outage before calling.
///
/// History: 9-17 every attempt started a 5–10 min cooldown; 9-26 only a real
/// catBusy did, after 3 retries a second apart. 10-04: no retries and no
/// cooldown. Retrying a busy card didn't help in the trials and the wait
/// guessed at a timeout nobody measured; what clears catBusy is a power cycle
/// of the card (device restart, or taking the card out and back), so the job
/// fails with `reason: "card_busy"` and both UIs say exactly that. Whether
/// the switch happened is read back from the card's profile list — see
/// `decide_enable` — and on success superseded enable/disable notifications
/// are removed and this switch's own are sent (see the notifications block).
pub fn switch(state: &AppState, body: &[u8]) -> (u16, Value) {
    let v: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let iccid = match body_str(&v, "iccid") {
        Some(i) if valid_iccid(i) => i.to_string(),
        _ => return (400, json!({"ok": false, "error": "invalid iccid"})),
    };
    let job_id = match try_start_job(state, "switch", &iccid) {
        Some(j) => j,
        None => return (409, json!({"ok": false, "error": "operation in progress"})),
    };
    let job = Arc::clone(&state.esim.job);
    let running = Arc::clone(&state.esim.running);
    let source = crate::datad_write::current();
    std::thread::spawn(move || {
        hold_wake_lock();
        // The switch (and the simreset / zte_topsw_mdm restart after it) does
        // not go through datad: tell it first, so a network-mode change still
        // confirming is not rolled back over the new card (D40). Best effort.
        crate::datad_write::interrupt_as(source, "esim");
        // Record the pre-switch identity so we can detect when the ZTE stack has
        // actually moved off it (the robust convergence signal).
        let (old_imsi, _) = ubus_sim_identity();
        // Highest pending notification before the enable: anything above it
        // afterwards is this switch's own.
        let seq_before = run_lpac(&["notification", "list"], LPAC_TIMEOUT_FAST)
            .ok()
            .and_then(|l| max_notif_seq(&l));

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_lpac(&["profile", "enable", &iccid], LPAC_TIMEOUT_FAST).map(|_| ())
        }))
        .unwrap_or_else(|_| Err("internal panic".into()));
        let card = read_profiles_retry().map_or(CardSays::Unknown, |l| target_state(&l, &iccid));
        if let Err(e) = &res {
            eprintln!("esim: profile enable {iccid}: {e}; card says {card:?}");
        }
        if let EnableOutcome::Failed { busy, msg } = decide_enable(&res, card) {
            release_wake_lock();
            let reason = if busy { "card_busy" } else { "not_switched" };
            finish_job_with(&job, &running, Err(msg), false, reason);
            return;
        }

        // Card-local and quick, so done on both paths below (the reboot one
        // can't come back to it).
        let to_send = prune_notifications(seq_before);

        // The card is now on the new profile, but the ZTE userspace still holds
        // the old identity — it only re-reads the SIM at daemon init, not on any
        // runtime QMI indication. The fast path avoids a full reboot: (1) power-
        // cycle the UIM session so the *modem* re-reads the new profile, then
        // (2) restart just zte_topsw_mdm so the *userspace* re-reads; nwinfo and
        // data re-dial off its signal on their own. Verified on-device to keep
        // the daemon sync barrier healthy and reattach data in well under a
        // minute. If identity doesn't converge or the barrier breaks, fall back
        // to the always-safe reboot.
        if Path::new(QMI_PROBE).exists() {
            let _ = run_cmd(&format!("{QMI_PROBE} simreset"));
        }
        let _ = run_cmd(&format!("{MDM_INITD} restart"));

        let mut converged = false;
        for _ in 0..15 {
            std::thread::sleep(Duration::from_millis(2000));
            if switch_converged(&iccid, &old_imsi) {
                converged = true;
                break;
            }
        }

        if converged {
            finish_job(&job, &running, Ok("switched — no reboot needed".into()), false);
            send_switch_notifications(&job, &running, to_send);
            release_wake_lock();
        } else {
            // Fast path didn't take; reboot recovers cleanly. Keep the wake lock
            // held so the device can't doze before it reboots.
            finish_job(
                &job,
                &running,
                Ok("switched — rebooting to finish".into()),
                true,
            );
            let _ = Command::new("sh").args(["-c", "sleep 2; reboot"]).spawn();
        }
    });
    (200, json!({"ok": true, "data": {"job_id": job_id, "status": "running"}}))
}

/// POST /api/esim/download — { code, confirmation_code? }. Downloads a new
/// profile via the activation code (LPA:1$…), then delivers the install
/// notification. Does NOT enable it — switching stays an explicit step.
pub fn download(state: &AppState, body: &[u8]) -> (u16, Value) {
    let v: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let code = match body_str(&v, "code") {
        Some(c) => c.to_string(),
        None => return (400, json!({"ok": false, "error": "missing activation code"})),
    };
    if code.len() > MAX_CODE_LEN || code.chars().any(|c| c.is_control()) {
        return (400, json!({"ok": false, "error": "invalid activation code"}));
    }
    let conf = v["confirmation_code"].as_str().unwrap_or("").trim().to_string();
    if conf.len() > MAX_CODE_LEN || conf.chars().any(|c| c.is_control()) {
        return (400, json!({"ok": false, "error": "invalid confirmation code"}));
    }
    let job_id = match try_start_job(state, "download", "") {
        Some(j) => j,
        None => return (409, json!({"ok": false, "error": "operation in progress"})),
    };
    let job = Arc::clone(&state.esim.job);
    let running = Arc::clone(&state.esim.running);
    std::thread::spawn(move || {
        hold_wake_lock();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut args = vec!["profile", "download", "-a", code.as_str()];
            if !conf.is_empty() {
                args.push("-c");
                args.push(conf.as_str());
            }
            run_lpac(&args, LPAC_TIMEOUT_NET)?;
            // Deliver the install notification (spec-required). Best effort:
            // the profile is already on the card even if this leg fails.
            // Superseded enable/disable ones go first, so `-a` doesn't send
            // stale state changes late.
            let _ = prune_notifications(None);
            let _ = run_lpac(&["notification", "process", "-a", "-r"], LPAC_TIMEOUT_NET);
            Ok("profile downloaded".to_string())
        }))
        .unwrap_or_else(|_| Err("internal panic".into()));
        release_wake_lock();
        finish_job(&job, &running, res, false);
    });
    (200, json!({"ok": true, "data": {"job_id": job_id, "status": "running"}}))
}

/// POST /api/esim/delete — { iccid }. Refuses to delete the enabled profile.
pub fn delete(state: &AppState, body: &[u8]) -> (u16, Value) {
    let v: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let iccid = match body_str(&v, "iccid") {
        Some(i) if valid_iccid(i) => i.to_string(),
        _ => return (400, json!({"ok": false, "error": "invalid iccid"})),
    };
    // Check profile state up-front (also serializes against concurrent jobs).
    if state.esim.running.load(Ordering::Relaxed) {
        return (409, json!({"ok": false, "error": "operation in progress"}));
    }
    match run_lpac(&["profile", "list"], LPAC_TIMEOUT_FAST) {
        Ok(list) => {
            let found = list.as_array().and_then(|a| {
                a.iter().find(|p| p["iccid"].as_str() == Some(iccid.as_str()))
            }).cloned();
            match found {
                None => return (404, json!({"ok": false, "error": "profile not found"})),
                Some(p) if p["profileState"].as_str() == Some("enabled") => {
                    return (400, json!({"ok": false, "error": "cannot delete the active profile — switch away first"}));
                }
                Some(_) => {}
            }
        }
        Err(e) => return (502, json!({"ok": false, "error": e})),
    }
    let job_id = match try_start_job(state, "delete", &iccid) {
        Some(j) => j,
        None => return (409, json!({"ok": false, "error": "operation in progress"})),
    };
    let job = Arc::clone(&state.esim.job);
    let running = Arc::clone(&state.esim.running);
    std::thread::spawn(move || {
        hold_wake_lock();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_lpac(&["profile", "delete", &iccid], LPAC_TIMEOUT_FAST)?;
            let _ = prune_notifications(None);
            let _ = run_lpac(&["notification", "process", "-a", "-r"], LPAC_TIMEOUT_NET);
            Ok("profile deleted".to_string())
        }))
        .unwrap_or_else(|_| Err("internal panic".into()));
        release_wake_lock();
        finish_job(&job, &running, res, false);
    });
    (200, json!({"ok": true, "data": {"job_id": job_id, "status": "running"}}))
}

/// POST /api/esim/notifications/process — deliver all pending notifications to
/// their SM-DP+ servers and remove them from the card.
pub fn notifications_process(state: &AppState, _body: &[u8]) -> (u16, Value) {
    let job_id = match try_start_job(state, "notifications", "") {
        Some(j) => j,
        None => return (409, json!({"ok": false, "error": "operation in progress"})),
    };
    let job = Arc::clone(&state.esim.job);
    let running = Arc::clone(&state.esim.running);
    std::thread::spawn(move || {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_lpac(&["notification", "process", "-a", "-r"], LPAC_TIMEOUT_NET)
                .map(|_| "notifications processed".to_string())
        }))
        .unwrap_or_else(|_| Err("internal panic".into()));
        finish_job(&job, &running, res, false);
    });
    (200, json!({"ok": true, "data": {"job_id": job_id, "status": "running"}}))
}

/* -------------------------------------------------------------- *
 *  Job lifecycle
 * -------------------------------------------------------------- */

fn try_start_job(state: &AppState, kind: &str, iccid: &str) -> Option<u64> {
    if state
        .esim
        .running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
        .is_err()
    {
        return None;
    }
    let mut j = state.esim.job.lock().unwrap();
    let next = j.id.wrapping_add(1);
    *j = JobState {
        id: next,
        kind: kind.to_string(),
        status: "running".into(),
        message: String::new(),
        iccid: iccid.to_string(),
        started_unix: now_unix(),
        finished_unix: 0,
        rebooting: false,
        reason: String::new(),
    };
    Some(next)
}

fn finish_job(
    job: &Arc<Mutex<JobState>>,
    running: &Arc<AtomicBool>,
    res: Result<String, String>,
    rebooting: bool,
) {
    finish_job_with(job, running, res, rebooting, "");
}

fn finish_job_with(
    job: &Arc<Mutex<JobState>>,
    running: &Arc<AtomicBool>,
    res: Result<String, String>,
    rebooting: bool,
    reason: &str,
) {
    {
        let mut j = job.lock().unwrap();
        match res {
            Ok(m) => {
                j.status = "done".into();
                j.message = m;
            }
            Err(e) => {
                j.status = "error".into();
                j.message = e;
            }
        }
        j.rebooting = rebooting;
        j.reason = reason.to_string();
        j.finished_unix = now_unix();
    }
    running.store(false, Ordering::Relaxed);
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One lpac 2.3.0 run as it prints it: progress lines, then the lpa line.
    fn lpa(code: i64, message: &str, data: Value) -> String {
        format!(
            "{}\n{}\n",
            json!({"type": "progress", "payload": {"code": 0, "message": "es10c_enable_profile", "data": null}}),
            json!({"type": "lpa", "payload": {"code": code, "message": message, "data": data}})
        )
    }

    fn enable_err(stdout: &str) -> Result<(), String> {
        parse_lpac_output(stdout, false).map(|_| ())
    }

    fn profile(iccid: &str, state: &str) -> Value {
        json!({"iccid": iccid, "isdpAid": "A0000005591010FFFFFFFF8900001000", "profileState": state,
               "profileNickname": null, "serviceProviderName": "X", "profileName": "X",
               "iconType": null, "icon": null, "profileClass": "operational"})
    }

    const A: &str = "89852000000000000011";
    const B: &str = "8985200000000000002";

    #[test]
    fn parses_lpac_lines() {
        assert_eq!(parse_lpac_output(&lpa(0, "success", json!([profile(A, "enabled")])), true).unwrap()[0]["iccid"], A);
        assert_eq!(
            enable_err(&lpa(-1, "es10c_enable_profile", json!("unknown"))).unwrap_err(),
            "lpac: es10c_enable_profile: unknown (code -1)"
        );
        // killed by the watchdog: no lpa line
        assert_eq!(parse_lpac_output("{\"type\":\"progress\"}\n", false), Err(String::new()));
        assert!(parse_lpac_output("", true).unwrap_err().contains("no result line"));
    }

    #[test]
    fn busy_only_for_enable_unknown_or_catbusy() {
        // lpac 2.3.0: enableResult 5 falls through to "unknown"
        assert!(is_card_busy(&enable_err(&lpa(-1, "es10c_enable_profile", json!("unknown"))).unwrap_err()));
        assert!(is_card_busy("lpac: es10c_enable_profile: catBusy (code -1)"));
        // the reasons lpac 2.3.0 does name (strings in the binary)
        for r in ["profile not in disabled state", "iccid or aid not found", "disallowed by policy"] {
            assert!(!is_card_busy(&enable_err(&lpa(-1, "es10c_enable_profile", json!(r))).unwrap_err()), "{r}");
        }
        assert!(!is_card_busy("lpac exited with exit status: 9 and no result (killed after timeout?)"));
        assert!(!is_card_busy("lpac: es10b_list_notification: unknown (code -1)"));
    }

    #[test]
    fn card_state_from_profile_list() {
        let list = json!([profile(A, "disabled"), profile(B, "enabled")]);
        assert_eq!(target_state(&list, B), CardSays::Enabled);
        assert_eq!(target_state(&list, A), CardSays::NotEnabled);
        assert_eq!(target_state(&list, "89852000000000000099"), CardSays::NotEnabled);
        // BCD pad nibble on either side
        assert_eq!(target_state(&json!([profile("8985200000000000002F", "enabled")]), B), CardSays::Enabled);
        assert_eq!(target_state(&Value::Null, B), CardSays::Unknown);
    }

    #[test]
    fn the_card_decides_whether_the_switch_happened() {
        let busy = enable_err(&lpa(-1, "es10c_enable_profile", json!("unknown")));
        let timeout: Result<(), String> = Err("lpac exited with signal: 9 and no result (killed after timeout?)".into());
        let already = enable_err(&lpa(-1, "es10c_enable_profile", json!("profile not in disabled state")));
        // an error, but the card is on the target: carry on with the resync
        assert_eq!(decide_enable(&timeout, CardSays::Enabled), EnableOutcome::Resync);
        assert_eq!(decide_enable(&busy, CardSays::Enabled), EnableOutcome::Resync);
        assert_eq!(decide_enable(&already, CardSays::Enabled), EnableOutcome::Resync);
        // lpac said ok but the card still has the old profile
        assert!(matches!(decide_enable(&Ok(()), CardSays::NotEnabled), EnableOutcome::Failed { busy: false, .. }));
        // catBusy and the card did not move: say restart, no retry
        match decide_enable(&busy, CardSays::NotEnabled) {
            EnableOutcome::Failed { busy: true, msg } => {
                assert!(msg.starts_with("card is busy"), "old touch-ui matches this: {msg}");
                assert!(msg.contains("restart"));
            }
            o => panic!("{o:?}"),
        }
        // the list can't be read: lpac's own answer stands
        assert_eq!(decide_enable(&Ok(()), CardSays::Unknown), EnableOutcome::Resync);
        assert!(matches!(decide_enable(&busy, CardSays::Unknown), EnableOutcome::Failed { busy: true, .. }));
        assert!(matches!(decide_enable(&timeout, CardSays::Unknown), EnableOutcome::Failed { busy: false, .. }));
    }

    fn notif(seq: i64, op: &str, iccid: &str) -> Value {
        json!({"seqNumber": seq, "profileManagementOperation": op,
               "notificationAddress": "rsp.example.com", "iccid": iccid})
    }

    #[test]
    fn notifications_superseded_are_removed_this_switch_sent() {
        const C: &str = "89860000000000000033";
        // A→B, B→A earlier (never sent), install of C, delete of a gone one;
        // then this switch A→B queued 9 (enable B) and 10 (disable A).
        let list = json!([
            notif(3, "install", C),
            notif(4, "enable", B), notif(5, "disable", A),
            notif(6, "enable", A), notif(7, "disable", B),
            notif(8, "delete", "89860000000000000044"),
            notif(9, "enable", B), notif(10, "disable", A),
        ]);
        let p = plan_notifications(&list, Some(8));
        assert_eq!(p.remove, vec![4, 5, 6, 7]);
        assert_eq!(p.send, vec![9, 10]);
        // seq before unknown: still prune, send nothing
        let p = plan_notifications(&list, None);
        assert_eq!(p.remove, vec![4, 5, 6, 7]);
        assert!(p.send.is_empty());
    }

    #[test]
    fn notifications_left_alone_when_not_superseded() {
        const C: &str = "89860000000000000033";
        // latest state change of a profile this switch didn't touch (C) stays;
        // install/delete never count; padded ICCIDs match.
        let list = json!([
            notif(1, "disable", C),
            notif(2, "install", B),
            notif(3, "enable", "8985200000000000002F"),
            notif(4, "enable", B),
            notif(5, "delete", A),
        ]);
        let p = plan_notifications(&list, Some(3));
        assert_eq!(p.remove, vec![3]);
        assert_eq!(p.send, vec![4]);
        assert_eq!(plan_notifications(&json!([]), Some(-1)), NotifPlan::default());
        assert_eq!(plan_notifications(&Value::Null, Some(0)), NotifPlan::default());
        assert_eq!(max_notif_seq(&list), Some(5));
        assert_eq!(max_notif_seq(&json!([])), Some(-1));
        assert_eq!(max_notif_seq(&Value::Null), None);
    }

    #[test]
    fn job_reports_reason() {
        let job = Arc::new(Mutex::new(EsimAdmin::new().job.lock().unwrap().clone()));
        let running = Arc::new(AtomicBool::new(true));
        finish_job_with(&job, &running, Err("card is busy".into()), false, "card_busy");
        let v = serde_json::to_value(job.lock().unwrap().clone()).unwrap();
        assert_eq!(v["reason"], "card_busy");
        assert_eq!(v["status"], "error");
        finish_job(&job, &running, Ok("x".into()), false);
        assert_eq!(job.lock().unwrap().reason, "");
    }
}
