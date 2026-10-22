//! The touch screen's writes (Phase 4 step 8d), ported from the agent's handlers as they are:
//! `netinfo.rs` (manual register and its guard, back to automatic, operator search, APN switch),
//! `scenario.rs` (pin), `chill.rs` (exit). Every device write goes to u60d's own `/control`
//! ([`super::control`]) as `screen`, a resumed guard as `guard`; the decisions are u60-api's.
//!
//! Only with u60d's writes on (step 9; with them off the routes answer 404 like anything not
//! here). The guard's
//! marker is u60d's own (`/data/u60d/netinfo.guard`): a restart of u60d must never pick up the
//! agent's.
//!
//! Not here yet: deep diagnosis' gate (D10) moves with diagnose (8e).

use axum::{body::Bytes, extract::State, response::Response};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use luyaz_api::control::{Reply, retry_after};
use luyaz_api::netinfo as ni;
use luyaz_policy::scenario as sc;

use super::control::{Session, Writer};
use super::{Compat, reply};

/// u60d's guard marker (the agent's is `/data/netinfo.guard`).
pub const GUARD_MARKER: &str = "/data/luyaz/core/netinfo.guard";
const PIN_FILE: &str = "/data/scenario/pin";
const RESTORE_FILE: &str = "/data/scenario/restore.json";

/// The agent's GUARD_POLL, SCAN_POLL, OP_BUSY_MAX, SCAN_TIMEOUT, session renew.
const GUARD_POLL: u64 = 3;
const SCAN_POLL: u64 = 3;
const SCAN_TIMEOUT: i64 = 180;
const OP_BUSY_MAX: u64 = 300;
const SESSION_RENEW: u64 = 20;

/// Time for the writes: the wall clock and real sleeps; tests run it `scale` times faster (every
/// wait and every elapsed time), so a 90 s guard takes 90 ms.
pub struct Clock {
    scale: u64,
    origin: Instant,
    origin_unix: i64,
}

impl Default for Clock {
    fn default() -> Self {
        Clock::scaled(1)
    }
}

impl Clock {
    pub fn scaled(scale: u64) -> Clock {
        Clock {
            scale: scale.max(1),
            origin: Instant::now(),
            origin_unix: super::home::now_unix() as i64,
        }
    }

    pub fn now(&self) -> i64 {
        if self.scale == 1 {
            return super::home::now_unix() as i64;
        }
        self.origin_unix + (self.origin.elapsed().as_secs_f64() * self.scale as f64) as i64
    }

    /// How many times faster than real time (1 on the device).
    pub fn scale(&self) -> u64 {
        self.scale
    }

    pub fn secs(&self, s: u64) -> Duration {
        Duration::from_secs(s) / self.scale as u32
    }

    pub async fn sleep(&self, s: u64) {
        tokio::time::sleep(self.secs(s)).await;
    }
}

fn http(r: (u16, Value)) -> Response {
    reply(r.0, r.1)
}

fn err(status: u16, zh: &str) -> Response {
    reply(
        status,
        json!({"ok": false, "error": zh, "error_en": ni::error_en(zh)}),
    )
}

impl Compat {
    async fn ubus(&self, obj: &'static str, method: &'static str, args: Value) -> Value {
        (self.src.ubus)(obj, method, args)
            .await
            .unwrap_or(Value::Null)
    }

    pub(super) async fn ubus_res(
        &self,
        obj: &'static str,
        method: &'static str,
        args: Value,
    ) -> Result<Value, String> {
        (self.src.ubus)(obj, method, args).await
    }

    async fn netinfo(&self) -> Value {
        self.ubus("zte_nwinfo_api", "nwinfo_get_netinfo", json!({}))
            .await
    }

    async fn wwan(&self) -> Value {
        self.ubus(
            "zwrt_data",
            "get_wwaniface",
            json!({"source_module": "zte_topsw_data", "cid": 1}),
        )
        .await
    }

    /// The bearer from `/proc` (under the test root); `None` when it cannot be read.
    pub(crate) fn bearer(&self) -> Option<bool> {
        crate::ops::reconnect::proc_bearer(&self.root)
    }

    /// The data call is really up ([`ni::data_up`]): B31's `connect_status` alone is not enough.
    async fn data_up(&self) -> bool {
        ni::data_up(&self.wwan().await, self.bearer())
    }

    pub(super) fn writer(&self, source: &'static str) -> Writer {
        Writer::new(self.src.control.clone(), source)
    }

    async fn session(&self, source: &'static str) -> Result<Session, Reply> {
        Session::open(
            self.src.control.clone(),
            source,
            self.clock.secs(SESSION_RENEW),
        )
        .await
    }

    fn write_marker(&self, m: &ni::Marker) -> std::io::Result<()> {
        let p = self.path(GUARD_MARKER);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        atomic_write(&p, ni::marker_text(m, self.clock.now()).as_bytes())
    }

    fn remove_marker(&self) {
        let _ = std::fs::remove_file(self.path(GUARD_MARKER));
    }

    /// Deep diagnosis' gate: take it before the ops lock, hold it until the claim is made.
    pub(super) fn gate(&self) -> std::sync::MutexGuard<'_, luyaz_api::diagnose::Gate> {
        self.gate.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn ops(&self) -> std::sync::MutexGuard<'_, ni::Ops> {
        self.net.ops.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub(crate) fn atomic_write(p: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("tmp-u60d");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, p)
}

// ── register, its guard, back to automatic ───────────────────────────────

/// POST /api/modem/register `{"m_mcc_mnc", "m_rat"}`: 202 at once, the call and the guard in a
/// task of their own (the agent's `register`).
pub async fn register(State(c): State<Compat>, body: Bytes) -> Response {
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let target = ni::js(&parsed, "m_mcc_mnc").to_string();
    let rat = match &parsed["m_rat"] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    if ni::plmn_from_str(&target).is_none() {
        return reply(
            400,
            json!({"ok": false, "error": "m_mcc_mnc must be 5–6 digits"}),
        );
    }
    let started = c.clock.now();
    // the search session first (D17), before any lock: refused or unreachable → nothing touched
    let sess = match c.session("screen").await {
        Ok(s) => s,
        Err(r) => return http(r.into_http_screen()),
    };
    // claimed and marked under the ops lock; any refusal closes the session after it
    let claimed: Result<Value, Response> = {
        // checked and claimed under deep diagnosis' gate (D10), as the agent
        let gate = c.gate();
        let mut o = c.ops();
        // Err(Ok(refusal)): a diagnosis runs; Err(Err(why)): another operation holds the modem
        let claim = match luyaz_api::diagnose::refusal_at(&gate, c.clock.now() as u64) {
            Some(r) => Err(Ok(r)),
            None => ni::claim(&mut o, ni::OpKind::Registering).map_err(Err),
        };
        match claim {
            Err(Ok(r)) => Err(http(r)),
            Err(Err(why)) => Err(reply(
                409,
                json!({"ok": false, "error": why, "error_en": ni::error_en(why), "guard": o.guard.json()}),
            )),
            Ok(()) => match c.write_marker(&ni::Marker::Register {
                target: target.clone(),
                started,
            }) {
                Err(e) => {
                    o.kind = ni::OpKind::Idle;
                    let msg = format!("写不了保护标记，没有注册：{e}");
                    Err(reply(
                        500,
                        json!({"ok": false, "error_en": ni::error_en(&msg), "error": msg}),
                    ))
                }
                Ok(()) => {
                    o.cancel = false;
                    o.guard = ni::Guard {
                        phase: "registering",
                        target: target.clone(),
                        rat: rat.clone(),
                        started_at: started,
                        ..ni::Guard::default()
                    };
                    Ok(o.guard.json())
                }
            },
        }
    };
    let snapshot = match claimed {
        Ok(s) => s,
        Err(r) => {
            sess.close().await;
            return r;
        }
    };
    let c2 = c.clone();
    tokio::spawn(async move {
        let w = sess.writer();
        let before = ni::register_result_str(
            &c2.ubus("zte_nwinfo_api", "nwinfo_m_netselect_result", json!({}))
                .await,
        );
        let p = json!({"mcc_mnc": target, "rat": rat});
        if let Err(e) = w.send("netselect.register", &p).await.into_result() {
            eprintln!("[compat] manual register call: {e} (guarding anyway)");
            c2.ops().guard.last_result = ni::clip(format!("调用出错：{e}"));
        }
        guard_run(&c2, &w, &target, started, &before).await;
        sess.close().await;
    });
    reply(202, json!({"ok": true, "guard": snapshot}))
}

/// Watch a manual register until the modem is on the target with data (keep it), or it clearly is
/// not going to be (back to automatic). A "back to automatic" request is honoured at every step.
async fn guard_run(c: &Compat, w: &Writer, target: &str, started: i64, before: &str) {
    loop {
        c.clock.sleep(GUARD_POLL).await;
        let cancel = std::mem::take(&mut c.ops().cancel);
        if cancel {
            return revert_until_auto(c, w, "手动恢复自动").await;
        }
        let result = ni::register_result_str(
            &c.ubus("zte_nwinfo_api", "nwinfo_m_netselect_result", json!({}))
                .await,
        );
        let serving = ni::serving_plmn(&c.netinfo().await);
        let connected = c.data_up().await;
        if !result.is_empty() {
            c.ops().guard.last_result = result.clone();
        }
        let obs = ni::Obs {
            result: &result,
            before,
            serving: &serving,
            connected,
        };
        match ni::decide(target, &obs, c.clock.now() - started) {
            ni::Verdict::Wait => {}
            ni::Verdict::Revert(why) => return revert_until_auto(c, w, why).await,
            ni::Verdict::Ok => {
                // keep it only if the data call holds
                if !ensure_data_up(c, w).await {
                    return revert_until_auto(c, w, "已注册但数据没通").await;
                }
                let cancelled = {
                    let mut o = c.ops();
                    let cancel = std::mem::take(&mut o.cancel);
                    if !cancel {
                        c.remove_marker();
                        o.guard.phase = "ok";
                        o.guard.finished_at = c.clock.now();
                        o.kind = ni::OpKind::Idle;
                        // its "current" marks are out of date now
                        o.scan = ni::Scan::default();
                    }
                    cancel
                };
                if cancelled {
                    return revert_until_auto(c, w, "手动恢复自动").await;
                }
                c.net.selected("manual", c.clock.now());
                return;
            }
        }
    }
}

async fn selection_now(c: &Compat) -> Option<&'static str> {
    ni::selection_from_netinfo(&c.netinfo().await)
}

/// `AT+COPS=0` (through `/control`) until the modem reports automatic selection, then the data call
/// back. Never gives up: every miss backs off 30 s … 5 min; the marker stays until it is confirmed.
async fn revert_until_auto(c: &Compat, w: &Writer, why: &'static str) {
    {
        let mut o = c.ops();
        o.kind = ni::OpKind::Reverting;
        o.cancel = false;
        o.guard.phase = "reverting";
        o.guard.set_reason(ni::Reason::why(why));
    }
    if let Err(e) = c.write_marker(&ni::Marker::Revert) {
        eprintln!("[compat] revert marker: {e}");
    }
    let mut backoff = 30u64;
    // already automatic: no command, which would only make it re-register
    let already = selection_now(c).await == Some("auto");
    let mut attempt = 0u32;
    let mut held = false;
    loop {
        if already {
            break;
        }
        let sent = w.send("netselect.auto", &json!({})).await.into_result();
        if let Some(pause) = retry_after(&sent) {
            // a network-mode change is being confirmed (D40): not an attempt, no backoff
            if !held {
                c.ops().guard.set_reason(ni::Reason::auto_held(why));
                held = true;
            }
            c.clock.sleep(pause.as_secs()).await;
            if selection_now(c).await == Some("auto") {
                break;
            }
            continue;
        }
        held = false;
        attempt += 1;
        let mut auto = false;
        for _ in 0..20 {
            if selection_now(c).await == Some("auto") {
                auto = true;
                break;
            }
            c.clock.sleep(3).await;
        }
        if auto {
            break;
        }
        let e = sent.err().unwrap_or_else(|| ni::AUTO_NOT_YET.into());
        eprintln!("[compat] back-to-automatic attempt {attempt} failed: {e}");
        c.ops()
            .guard
            .set_reason(ni::Reason::auto_retry(why, attempt, &e, backoff));
        c.clock.sleep(backoff).await;
        backoff = (backoff * 2).min(300);
    }
    let data_up = redial_until_up(c, w).await;
    c.net.selected("auto", c.clock.now());
    let mut o = c.ops();
    o.guard.phase = "reverted";
    o.guard.set_reason(if data_up {
        ni::Reason::why(why)
    } else {
        ni::Reason::auto_no_data(why)
    });
    o.guard.finished_at = c.clock.now();
    o.kind = ni::OpKind::Idle;
}

/// Automatic again; get the data call back: a few tries (0, 30, 120 s), bounded (mobile data may be
/// off on purpose).
async fn redial_until_up(c: &Compat, w: &Writer) -> bool {
    let _ = c.write_marker(&ni::Marker::Redial);
    let mut up = false;
    for (n, pause) in [0u64, 30, 120].into_iter().enumerate() {
        if pause > 0 {
            c.ops().guard.set_reason(ni::Reason::redial_retry(n, pause));
            c.clock.sleep(pause).await;
        }
        if ensure_data_up(c, w).await {
            up = true;
            break;
        }
    }
    c.remove_marker();
    up
}

/// After anything that can drop the data call: wait for it to come back (connected on two reads in
/// a row), dial it again if it does not (the agent's `ensure_data_up`).
async fn ensure_data_up(c: &Compat, w: &Writer) -> bool {
    let _one = c.net.dial.lock().await;
    let wait = |secs: u64| async move {
        let mut streak = 0;
        for _ in 0..secs / 2 {
            if c.data_up().await {
                streak += 1;
                if streak >= 2 {
                    return true;
                }
            } else {
                streak = 0;
            }
            c.clock.sleep(2).await;
        }
        false
    };
    if wait(20).await {
        return true;
    }
    eprintln!("[compat] data call still down, dialling it again");
    let mut sent = w.send("cellular.redial", &json!({})).await.into_result();
    let mut held = 0u64;
    while let Some(pause) = retry_after(&sent) {
        if held >= OP_BUSY_MAX {
            break;
        }
        c.clock.sleep(pause.as_secs()).await;
        held += pause.as_secs();
        if c.data_up().await {
            return true;
        }
        sent = w.send("cellular.redial", &json!({})).await.into_result();
    }
    if let Err(e) = sent {
        eprintln!("[compat] redial: {e}");
    }
    wait(30).await
}

/// A session for a revert, best effort: back to automatic and redial go ahead without one.
async fn revert_writer(c: &Compat, source: &'static str) -> (Writer, Option<Session>) {
    match c.session(source).await {
        Ok(s) => (s.writer(), Some(s)),
        Err(r) => {
            eprintln!("[compat] no session for back-to-automatic ({r:?}), going ahead");
            (c.writer(source), None)
        }
    }
}

async fn close(s: Option<Session>) {
    if let Some(s) = s {
        s.close().await;
    }
}

/// POST /api/modem/netselect/auto (the agent's `select_auto`).
pub async fn select_auto(State(c): State<Compat>) -> Response {
    let snapshot = {
        let mut o = c.ops();
        match o.kind {
            ni::OpKind::Reverting => {
                return reply(202, json!({"ok": true, "data": o.guard.json()}));
            }
            // a search does not change the selection mode
            ni::OpKind::Scanning => return err(409, "正在搜索网络，搜完再操作"),
            ni::OpKind::Registering | ni::OpKind::Idle => {}
        }
        // accepted means it survives a restart: the marker before the 202, or refused
        if let Err(e) = c.write_marker(&ni::Marker::Revert) {
            let msg = format!("写不了保护标记：{e}");
            return reply(
                500,
                json!({"ok": false, "error_en": ni::error_en(&msg), "error": msg}),
            );
        }
        if o.kind == ni::OpKind::Registering {
            // the guard reads this under the same lock at every step
            o.cancel = true;
            return reply(202, json!({"ok": true, "data": o.guard.json()}));
        }
        o.kind = ni::OpKind::Reverting;
        o.guard = ni::Guard {
            phase: "reverting",
            started_at: c.clock.now(),
            ..ni::Guard::default()
        };
        o.guard.set_reason(ni::Reason::why("手动恢复自动"));
        o.guard.json()
    };
    let c2 = c.clone();
    tokio::spawn(async move {
        let (w, s) = revert_writer(&c2, "screen").await;
        revert_until_auto(&c2, &w, "手动恢复自动").await;
        close(s).await;
    });
    reply(202, json!({"ok": true, "data": snapshot}))
}

/// At startup (writes on): finish whatever u60d's marker says was in flight, as `guard`. An
/// unreadable marker goes back to automatic, the safe direction.
pub async fn resume(c: Compat) {
    let text = match std::fs::read(c.path(GUARD_MARKER)) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            eprintln!("[compat] marker unreadable ({e}), going back to automatic");
            String::new()
        }
    };
    match ni::parse_marker(&text) {
        Some(ni::Marker::Register { target, started }) => {
            eprintln!("[compat] resuming the register guard for {target}");
            {
                let mut o = c.ops();
                o.kind = ni::OpKind::Registering;
                o.guard = ni::Guard {
                    phase: "registering",
                    target: target.clone(),
                    started_at: started,
                    ..ni::Guard::default()
                };
            }
            let (w, s) = revert_writer(&c, "guard").await;
            guard_run(&c, &w, &target, started, "").await;
            close(s).await;
        }
        Some(ni::Marker::Redial) => {
            eprintln!("[compat] resuming the redial after back-to-automatic");
            {
                let mut o = c.ops();
                o.kind = ni::OpKind::Reverting;
                o.guard = ni::Guard {
                    phase: "reverting",
                    started_at: c.clock.now(),
                    ..ni::Guard::default()
                };
                o.guard.set_reason(ni::Reason::why("重启前还没拨上"));
            }
            let (w, s) = revert_writer(&c, "guard").await;
            let up = redial_until_up(&c, &w).await;
            close(s).await;
            let mut o = c.ops();
            o.guard.phase = "reverted";
            if !up {
                o.guard.set_reason(ni::Reason::auto_no_data(""));
            }
            o.guard.finished_at = c.clock.now();
            o.kind = ni::OpKind::Idle;
        }
        _ => {
            eprintln!("[compat] resuming back-to-automatic");
            c.ops().kind = ni::OpKind::Reverting;
            let (w, s) = revert_writer(&c, "guard").await;
            revert_until_auto(&c, &w, "重启前没做完").await;
            close(s).await;
        }
    }
}

// ── operator search ────────────────────────────────────────────────────────

/// POST /api/netinfo/scan (the agent's `operator_scan`): data is down for the whole search.
pub async fn scan(State(c): State<Compat>) -> Response {
    {
        let gate = c.gate();
        let mut o = c.ops();
        if o.kind == ni::OpKind::Scanning {
            return reply(202, json!({"ok": true, "data": {"state": "scanning"}}));
        }
        if let Some(r) = luyaz_api::diagnose::refusal_at(&gate, c.clock.now() as u64) {
            return http(r);
        }
        if let Err(why) = ni::claim(&mut o, ni::OpKind::Scanning) {
            return err(409, why);
        }
        o.scan = ni::Scan {
            state: "scanning",
            started_at: c.clock.now(),
            ..ni::Scan::default()
        };
    }
    // the search session holds other connectivity writes off meanwhile (D17)
    let sess = match c.session("screen").await {
        Ok(s) => s,
        Err(r) => {
            let mut o = c.ops();
            o.kind = ni::OpKind::Idle;
            o.scan = ni::Scan::default();
            return http(r.into_http_screen());
        }
    };
    let c2 = c.clone();
    tokio::spawn(async move {
        let w = sess.writer();
        let outcome = run_scan(&c2, &w).await;
        ensure_data_up(&c2, &w).await;
        sess.set_result(if outcome.is_ok() {
            "scanned"
        } else {
            "scan_failed"
        });
        sess.close().await;
        let mut o = c2.ops();
        match outcome {
            Ok(ops) if !ops.is_empty() => {
                o.scan.state = "done";
                o.scan.operators = ops;
            }
            Ok(_) => {
                o.scan.state = "error";
                o.scan.error = "没搜到网络".into();
            }
            Err(e) => {
                o.scan.state = "error";
                o.scan.error = ni::clip(e);
            }
        }
        o.scan.finished_at = c2.clock.now();
        o.kind = ni::OpKind::Idle;
    });
    reply(202, json!({"ok": true, "data": {"state": "scanning"}}))
}

async fn run_scan(c: &Compat, w: &Writer) -> Result<Vec<Value>, String> {
    // a call that errors may still have started the search: watch the status either way
    let call = w.send("netselect.scan", &json!({})).await.into_result();
    let started = c.clock.now();
    let mut seen_search = false;
    loop {
        c.clock.sleep(SCAN_POLL).await;
        let st = c
            .ubus("zte_nwinfo_api", "nwinfo_m_netselect_status", json!({}))
            .await;
        let status = ni::loose_str(&st, &["status"]);
        c.ops().scan.last_status = status.clone();
        let searching = status.to_ascii_lowercase().contains("selecting");
        seen_search |= searching;
        match ni::scan_finished(&status) {
            Some(true) => break,
            Some(false) => return Err(format!("模组报告搜索失败（{status}）")),
            None => {}
        }
        let waited = c.clock.now() - started;
        if let Err(e) = &call
            && !seen_search
            && waited >= 15
        {
            return Err(e.clone());
        }
        // past the usual time, keep holding while it still says it is searching (hard stop at twice)
        if waited >= SCAN_TIMEOUT && (!searching || waited >= 2 * SCAN_TIMEOUT) {
            break;
        }
    }
    let v = c
        .ubus_res("zte_nwinfo_api", "nwinfo_m_netselect_contents", json!({}))
        .await?;
    let mut ops = Vec::new();
    ni::collect_operators(&v, &mut ops);
    ops.truncate(ni::SCAN_MAX_OPS);
    Ok(ops)
}

// ── APN ───────────────────────────────────────────────────────────────────

/// The card's ICCID: u60d's `sim` block when fresh, else one read.
pub(super) async fn current_iccid(c: &Compat) -> String {
    let raw = match c
        .src
        .hub
        .as_ref()
        .and_then(|h| h.view("sim"))
        .filter(|v| !v.stale)
    {
        Some(b) => b.data["iccid"].as_str().unwrap_or("").to_string(),
        None => c.ubus("zwrt_zte_mdm.api", "get_sim_info", json!({})).await["sim_iccid"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    };
    raw.trim().trim_end_matches(['F', 'f']).to_string()
}

/// The ICCID for a candidate pick, a few tries (it reads empty for a moment after a re-read).
async fn iccid_for_pick(c: &Compat) -> String {
    for i in 0..3 {
        let id = current_iccid(c).await;
        if !id.is_empty() {
            return id;
        }
        if i < 2 {
            c.clock.sleep(1).await;
        }
    }
    String::new()
}

/// Read-modify-write of the agent's pick store (`/data/apn-pick.json`), keeping what it does not
/// know. Step 9 gives it one owner (the watcher moves here too).
pub(super) fn pick_store(c: &Compat, f: impl FnOnce(&mut serde_json::Map<String, Value>) -> bool) {
    let p = c.path(ni::APN_PICK_STORE);
    let mut v: Value = std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let o = v.as_object_mut().expect("object");
    if f(o) {
        let _ = atomic_write(&p, v.to_string().as_bytes());
    }
}

/// POST /api/netinfo/apn `{"id"}`: checks, 202, then switches in a task (the agent's `apn_use`).
///
/// Second phase (u60-mcp.md D3, O3, O4): the switch is one u60d transaction (`apn.use`: enable and
/// mode together, one capture, one lock, rolled back by the engine when it does not come through),
/// written as the caller (`X-U60-Source`: the web's `web`, the agent's `mcp`; else the screen). A
/// candidate is copied into the manual list first (a plain write: not the APN in use). The pick per
/// card is written only once the trial passed; a rolled back switch leaves it as it was. The
/// agent's (`mcp`) first passes its gate (sensitive → refused with the page to send the owner to;
/// one APN change per half hour), its writes carrying the gate's ticket.
pub async fn apn_use(State(c): State<Compat>, req: axum::extract::Request) -> Response {
    let source = super::charge::source(&req);
    let Ok(body) = axum::body::to_bytes(req.into_body(), 64 * 1024).await else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let id = parsed["id"].as_str().unwrap_or("").to_string();
    if id.is_empty() {
        return reply(400, json!({"ok": false, "error": "missing 'id'"}));
    }
    let list_has = |m: &'static str| {
        let (c, id) = (c.clone(), id.clone());
        async move {
            c.ubus("zwrt_apn_object", m, json!({})).await["apnListArray"]
                .as_array()
                .is_some_and(|a| a.iter().any(|p| p["profileId"] == id.as_str()))
        }
    };
    let candidate = id != "auto" && id.starts_with("auto");
    if candidate && !list_has("get_auto_apn_list").await {
        return err(404, "没有这个候选 APN");
    }
    if !candidate && id != "auto" && !list_has("get_manu_apn_list").await {
        return err(404, "没有这个手动 APN");
    }
    // a candidate is remembered per card: without the card number, refuse
    let iccid = if candidate {
        iccid_for_pick(&c).await
    } else {
        String::new()
    };
    if candidate && iccid.is_empty() {
        return err(409, "读不到 SIM 卡号，没法只给这张卡记住");
    }
    let ticket = if source == "mcp" {
        match agent_ticket().await {
            Ok(t) => Some(t),
            Err(r) => return r,
        }
    } else {
        None
    };
    let gate = crate::ops::agent::gate();
    // a person may switch again during the trial (the engine supersedes it); the agent waits
    if (source == "mcp" && c.net.apn_trial.load(Ordering::SeqCst))
        || c.net.apn_switching.swap(true, Ordering::SeqCst)
    {
        if let Some(t) = &ticket {
            gate.release(t);
        }
        return err(409, "上一次 APN 切换还没做完");
    }
    c.net.apn_error("");
    c.net.apn_changed();
    let op_id = format!("apn-{:016x}", rand::random::<u64>());
    *c.net.apn_current.lock().unwrap_or_else(|e| e.into_inner()) = op_id.clone();
    let c2 = c.clone();
    let op = op_id.clone();
    tokio::spawn(async move {
        let mut w = c2.writer(source);
        if let Some(t) = &ticket {
            w.extra = Some(json!({"agent_ticket": t.id}));
        }
        let iccid = if iccid.is_empty() {
            current_iccid(&c2).await
        } else {
            iccid
        };
        // what to use, and for a candidate the copy it became (the pick to remember)
        let plan = if id == "auto" {
            Ok((json!({"auto": true}), None))
        } else if candidate {
            manual_for_candidate(&c2, &w, &id)
                .await
                .map(|(manual, apn)| (json!({"profile_id": manual}), Some((manual, apn))))
        } else {
            Ok((json!({"profile_id": id}), None))
        };
        let mut wu = w.clone();
        let mut extra = w.extra.clone().unwrap_or_else(|| json!({}));
        extra["op_id"] = json!(op);
        wu.extra = Some(extra);
        let sent = match plan {
            Ok((params, pick)) => match wu.send("apn.use", &params).await {
                // a transaction (op) to follow; none = it was already so
                Reply::Done { op: txn, .. } => Ok((txn.is_some(), pick)),
                other => other.into_result().map(|_| (false, pick)),
            },
            Err(e) => Err(e),
        };
        if let Err(e) = &sent {
            eprintln!("[compat] APN switch to {id}: {e}");
        }
        c2.net
            .apn_error(sent.as_ref().err().map(String::as_str).unwrap_or(""));
        let trial = sent.as_ref().is_ok_and(|(t, _)| *t);
        c2.net.apn_trial.store(trial, Ordering::SeqCst);
        c2.net.apn_switching.store(false, Ordering::SeqCst);
        c2.net.apn_changed();
        let current = |c: &Compat| *c.net.apn_current.lock().unwrap_or_else(|e| e.into_inner()) == op;
        if let Ok((_, pick)) = sent {
            // O4: the pick is the card's only once the switch came through
            let end = if trial {
                trial_end(&w, &op).await
            } else {
                "confirmed".to_string()
            };
            if !current(&c2) {
                // a later switch took over: its task owns the state and the pick
                eprintln!("[compat] APN switch to {id} ended {end}, superseded by a later one");
            } else if matches!(end.as_str(), "confirmed" | "unverified") {
                match &pick {
                    Some((manual, apn)) => remember(&c2, &iccid, manual, apn),
                    None => forget(&c2, &iccid),
                }
                clear_sticky(&c2);
            } else {
                eprintln!("[compat] APN switch to {id} ended {end}: the card's pick left as it was");
                c2.net.apn_error(match end.as_str() {
                    "rolled_back" => "新的 APN 没通，已退回原来的",
                    "cancelled" => "切换被别的改动打断",
                    _ => "切换没有生效",
                });
            }
        }
        if current(&c2) {
            c2.net.apn_trial.store(false, Ordering::SeqCst);
        }
        if let Some(t) = &ticket {
            crate::ops::agent::gate().done(t);
        }
    });
    reply(202, json!({"ok": true, "data": {"switching": true, "op_id": op_id}}))
}

/// The agent's gate for a switch (second phase D5, D6): not sensitive, the APN layer free. The
/// refusal is the reply.
async fn agent_ticket() -> Result<crate::ops::agent::Ticket, Response> {
    use crate::ops::agent::{self, Layer, Refusal};
    let now = agent::now_unix();
    let r = match agent::apn_sensitivity().await {
        Ok(iccid) => match agent::gate().reserve(Layer::Apn, Some(iccid), now) {
            Ok(t) => return Ok(t),
            Err(r) => r,
        },
        Err(why) => Refusal::Sensitive(why),
    };
    let (status, code, message, extra) = r.reply(now);
    let mut v = json!({"ok": false, "error": message, "code": code});
    if let (Some(o), Some(m)) = (v.as_object_mut(), extra.as_object()) {
        o.extend(m.clone());
    }
    Err(reply(status, v))
}

/// How the switch's transaction ended (`op.status` every 2 s, at most 4 minutes); "unknown" when
/// u60d stops answering.
async fn trial_end(w: &Writer, op_id: &str) -> String {
    for _ in 0..120 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if let Reply::Done { result, .. } = w.send("op.status", &json!({"op_id": op_id})).await {
            let phase = result["phase"].as_str().unwrap_or("");
            if !matches!(phase, "accepted" | "applying" | "verifying" | "rolling_back" | "") {
                return phase.to_string();
            }
        }
    }
    "unknown".into()
}

/// GET /api/agent/roam-cheap: the cards the owner marked "roaming is not expensive" (second phase
/// D6), the card in now and whether the agent changed each layer lately (Unix seconds).
pub async fn roam_cheap_get(State(c): State<Compat>) -> Response {
    let g = crate::ops::agent::gate();
    let cheap = g.cheap();
    let now = crate::ops::agent::norm_iccid(&current_iccid(&c).await);
    reply(
        200,
        json!({"ok": true, "data": {
            "cheap": cheap,
            "current_iccid": now,
            "current_cheap": !now.is_empty() && cheap.contains(&now),
            "agent_last": {
                "apn": g.last(crate::ops::agent::Layer::Apn),
                "chill": g.last(crate::ops::agent::Layer::Chill),
            },
        }}),
    )
}

/// PUT /api/agent/roam-cheap `{"iccid", "cheap": bool}`: the owner's setting for one card. Only a
/// person sets it: the agent's own calls (`X-U60-Source: mcp`) are refused.
pub async fn roam_cheap_put(req: axum::extract::Request) -> Response {
    if super::charge::source(&req) == "mcp" {
        return reply(403, json!({"ok": false, "error": "only the owner sets this"}));
    }
    let Ok(body) = axum::body::to_bytes(req.into_body(), 4096).await else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (Some(iccid), Some(cheap)) = (parsed["iccid"].as_str(), parsed["cheap"].as_bool()) else {
        return reply(400, json!({"ok": false, "error": "need iccid and cheap"}));
    };
    match crate::ops::agent::gate().set_cheap(iccid, cheap) {
        Ok(list) => reply(200, json!({"ok": true, "data": {"cheap": list}})),
        Err(e) => reply(400, json!({"ok": false, "error": e})),
    }
}

/// Select the profile, then the mode, so the re-dial uses it (the agent's order since 9-25).
pub(super) async fn dial_manual(w: &Writer, id: &str) -> Result<(), String> {
    w.send("apn.enable", &json!({"profile_id": id}))
        .await
        .into_result()?;
    w.send("apn.set_mode", &json!({"mode": 1}))
        .await
        .into_result()
        .map(|_| ())
}

pub(super) async fn dial_auto(w: &Writer) -> Result<(), String> {
    w.send("apn.set_mode", &json!({"mode": 0}))
        .await
        .into_result()
        .map(|_| ())
}

/// The manual profile for auto candidate `auto_id`: an identical one if it exists, else a copy
/// added now. Its profileId and APN name.
async fn manual_for_candidate(
    c: &Compat,
    w: &Writer,
    auto_id: &str,
) -> Result<(String, String), String> {
    let auto = c
        .ubus_res("zwrt_apn_object", "get_auto_apn_list", json!({}))
        .await?;
    let cand = auto["apnListArray"]
        .as_array()
        .and_then(|a| a.iter().find(|p| p["profileId"] == auto_id))
        .cloned()
        .ok_or_else(|| "没有这个候选 APN".to_string())?;
    let apn = cand["wanapn"].as_str().unwrap_or("").to_string();
    let manual = || c.ubus_res("zwrt_apn_object", "get_manu_apn_list", json!({}));
    if let Some(id) = ni::find_same(&manual().await?, &cand) {
        return Ok((id, apn));
    }
    w.send("apn.add", &ni::apn_params(&cand, false)?)
        .await
        .into_result()?;
    // add_manu_apn's reply does not reliably carry the new id: find it
    ni::find_same(&manual().await?, &cand)
        .map(|id| (id, apn))
        .ok_or_else(|| "复制成手动 APN 后没读到它".to_string())
}

fn remember(c: &Compat, iccid: &str, manual_id: &str, apn: &str) {
    if iccid.is_empty() {
        return;
    }
    pick_store(c, |o| {
        let picks = o.entry("picks").or_insert_with(|| json!({}));
        if !picks.is_object() {
            *picks = json!({});
        }
        picks[iccid] = json!({"manual_id": manual_id, "apn": apn});
        o.insert("last_iccid".into(), json!(iccid));
        true
    });
}

fn forget(c: &Compat, iccid: &str) {
    if iccid.is_empty() {
        return;
    }
    pick_store(c, |o| {
        o.get_mut("picks")
            .and_then(Value::as_object_mut)
            .is_some_and(|p| p.remove(iccid).is_some())
    });
}

fn clear_sticky(c: &Compat) {
    pick_store(c, |o| {
        if o.get("notice")
            .is_some_and(|n| n["sticky"].as_bool() == Some(true))
        {
            o.insert("notice".into(), Value::Null);
            return true;
        }
        false
    });
}

// ── scenario pin ───────────────────────────────────────────────────────────

/// POST /api/scenario/pin `{"id": "home"}` or `{"id": null}`: the pin file the engine reads on
/// every look. The screen reads only the status; the agent's body (the whole `/api/scenario`) is
/// the web's.
pub async fn scenario_pin(State(c): State<Compat>, body: Bytes) -> Response {
    // u60d runs the engine (9h): its own route, the agent's full reply (the web's too)
    if c.scenario.engine().is_some() {
        return super::scenario::pin(State(c), body).await;
    }
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
        return reply(400, json!({"ok": false, "error": "invalid JSON"}));
    };
    let pin = c.path(PIN_FILE);
    match parsed.get("id").and_then(Value::as_str) {
        Some(id) => {
            let cfg: sc::Config = std::fs::read_to_string(c.path(super::status::SCENARIO_CONFIG))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            if !cfg.scenarios.iter().any(|s| s.id == id) {
                return reply(
                    400,
                    json!({"ok": false, "error": format!("unknown scenario {id:?}")}),
                );
            }
            if let Some(d) = pin.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if let Err(e) = atomic_write(&pin, id.as_bytes()) {
                return reply(
                    500,
                    json!({"ok": false, "error": format!("write pin: {e}")}),
                );
            }
            reply(200, json!({"ok": true, "data": {"pin": id}}))
        }
        None => {
            let _ = std::fs::remove_file(&pin);
            reply(200, json!({"ok": true, "data": {"pin": null}}))
        }
    }
}

// ── CHILL exit ─────────────────────────────────────────────────────────────

/// PUT /api/services/chill/exit `{"state"}` (the agent's `chill::exit_set`): group first, then
/// mode, both straight to mihomo; then, abroad, the scenario engine's "back to proxy when home".
pub async fn chill_exit(State(c): State<Compat>, headers: axum::http::HeaderMap, body: Bytes) -> Response {
    use luyaz_api::chill as ch;
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let want = match ch::exit_wanted(&parsed) {
        Ok(w) => w.to_string(),
        Err(r) => return http(r),
    };
    let env = std::fs::read_to_string(c.path(ch::CHILL_ENV)).unwrap_or_default();
    let secret = ch::env_var(&env, "CHILL_SECRET");
    let client = match reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(cl) => cl,
        Err(_) => return reply(500, json!({"ok": false, "error": "internal"})),
    };
    let auth = |r: reqwest::RequestBuilder| {
        if secret.is_empty() {
            r
        } else {
            r.header("Authorization", format!("Bearer {secret}"))
        }
    };
    let group_url = format!(
        "{}/proxies/{}",
        c.src.mihomo,
        super::home::enc(ch::MAIN_GROUP)
    );
    let info: Option<Value> = match auth(client.get(&group_url)).send().await {
        Ok(r) if r.status().is_success() => r
            .bytes()
            .await
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok()),
        _ => None,
    };
    let Some(info) = info else {
        return reply(409, json!({"ok": false, "error": "CHILL is not running"}));
    };
    let now = info["now"].as_str().unwrap_or("").to_string();
    let options: Vec<&str> = info["all"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let remembered = std::fs::read_to_string(c.path(ch::LAST_MAIN)).ok();
    let backup = c
        .manual_first
        .live()
        .map(|m| m.backup())
        .or_else(|| {
            std::fs::read_to_string(c.path(ch::MANUAL_FIRST_STATE))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| v["backup"].as_str().map(str::to_string))
        })
        .unwrap_or_default();
    let plan = match ch::exit_plan(
        &want,
        &now,
        &options,
        remembered.as_deref().map(str::trim),
        &backup,
    ) {
        Ok(p) => p,
        Err(r) => return http(r),
    };
    if let Some(r) = &plan.remember {
        let _ = std::fs::write(c.path(ch::LAST_MAIN), r);
    }
    if let Some(m) = &plan.member {
        match auth(
            client
                .put(&group_url)
                .header("Content-Type", "application/json")
                .body(json!({"name": m}).to_string()),
        )
        .send()
        .await
        {
            Ok(r) if r.status().as_u16() < 400 => {}
            Ok(r) => {
                let code = r.status().as_u16();
                return reply(
                    502,
                    json!({"ok": false, "error": format!("mihomo rejected switch (HTTP {code})")}),
                );
            }
            Err(e) => {
                return reply(
                    502,
                    json!({"ok": false, "error": format!("switch request failed: {e}")}),
                );
            }
        }
    }
    let mode = plan.mode;
    match auth(
        client
            .patch(format!("{}/configs", c.src.mihomo))
            .header("Content-Type", "application/json")
            .body(json!({"mode": mode}).to_string()),
    )
    .send()
    .await
    {
        Ok(r) if r.status().as_u16() < 400 => {
            let _ = atomic_write(&c.path(ch::CHILL_MODE_FILE), format!("{mode}\n").as_bytes());
            // a scenario restore putting the exit back is not a change by hand (the listener is
            // loopback only, so the header comes from the agent or u60d's own engine)
            if !headers.contains_key(luyaz_api::headers::RESTORE_HEADER) {
                exit_changed_abroad(&c, c.clock.now());
            }
            reply(200, json!({"ok": true, "data": {"exit": want}}))
        }
        Ok(r) => {
            let code = r.status().as_u16();
            reply(
                502,
                json!({"ok": false, "error": format!("mihomo rejected mode (HTTP {code})")}),
            )
        }
        Err(e) => reply(
            502,
            json!({"ok": false, "error": format!("mode request failed: {e}")}),
        ),
    }
}

/// The agent's `chill_exit_changed`: abroad (engine on, current scenario an abroad one), an exit
/// changed by hand goes back to proxy when home. Same entry, same file.
fn exit_changed_abroad(c: &Compat, now: i64) {
    note_exit_abroad(&c.root, now)
}

/// [`exit_changed_abroad`] for a device root (`""` on the device): u60d's own `chill.exit`
/// transactions (the agent's, `ops::chill`) register the same restore entry.
pub(crate) fn note_exit_abroad(root: &str, now: i64) {
    use super::status::{SCENARIO_CONFIG, SCENARIO_DISABLED, SCENARIO_STATE};
    let path = |p: &str| std::path::PathBuf::from(format!("{root}{p}"));
    let read = |p: &str| std::fs::read_to_string(path(p)).ok();
    if path(SCENARIO_DISABLED).exists() {
        return;
    }
    let cfg: sc::Config = read(SCENARIO_CONFIG)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let st: sc::RunState = read(SCENARIO_STATE)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if !cfg
        .scenarios
        .iter()
        .any(|s| s.id == st.current && sc::is_abroad(s))
    {
        return;
    }
    let mut pending: Vec<sc::PendingRestore> = read(RESTORE_FILE)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if pending
        .iter()
        .any(|p| p.key == luyaz_api::chill::CHILL_EXIT_KEY)
    {
        return;
    }
    let Ok(entry) =
        serde_json::from_value::<sc::PendingRestore>(luyaz_api::chill::chill_exit_restore(now))
    else {
        return;
    };
    pending.push(entry);
    if let Ok(text) = serde_json::to_string_pretty(&pending) {
        let _ = atomic_write(&path(RESTORE_FILE), text.as_bytes());
    }
}
