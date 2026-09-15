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

    let stdout = String::from_utf8_lossy(&out.stdout);
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
    if !out.status.success() {
        return Err(format!(
            "lpac exited with {} and no result (killed after timeout?)",
            out.status
        ));
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
    let imsi_changed = !imsi.is_empty() && imsi != old_imsi;
    let iccid_matches = !iccid.is_empty() && iccid.trim_end_matches(['F', 'f']) == target_iccid;
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

/// POST /api/esim/switch — { iccid }. Enables the profile on the card, then
/// reboots the device (the only way to get the ZTE stack onto the new profile —
/// see module docs). The UI warns about the ~2 min outage before calling.
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
    std::thread::spawn(move || {
        hold_wake_lock();
        // Record the pre-switch identity so we can detect when the ZTE stack has
        // actually moved off it (the robust convergence signal).
        let (old_imsi, _) = ubus_sim_identity();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_lpac(&["profile", "enable", &iccid], LPAC_TIMEOUT_FAST).map(|_| ())
        }))
        .unwrap_or_else(|_| Err("internal panic".into()));
        if let Err(e) = res {
            release_wake_lock();
            finish_job(&job, &running, Err(e), false);
            return;
        }

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
            release_wake_lock();
            finish_job(&job, &running, Ok("switched — no reboot needed".into()), false);
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
    };
    Some(next)
}

fn finish_job(
    job: &Arc<Mutex<JobState>>,
    running: &Arc<AtomicBool>,
    res: Result<String, String>,
    rebooting: bool,
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
