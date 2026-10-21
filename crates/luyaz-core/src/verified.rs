//! Restore the last verified connection (`docs/designs/restore-verified-connection.md`): the
//! device side. The rules are `luyaz_api::execution::verified`.
//!
//! ```text
//!  user's deep check (POST /api/diagnose) ─▶ run done ─▶ no txn / settled? ─▶ O2 bound check
//!        ─▶ pass, same connection + card, country known ─▶ verified/<card>-<mcc>.json
//!  GET  /api/verified ─▶ this card × country's set, what differs, each step's rollback
//!  /control verified.restore (screen / web only) ─▶ one ordinary transaction per item, in order;
//!        stops at the first that doesn't confirm ─▶ one O2 check at the end, nothing else
//! ```
//! - Data: the deep check already sends ~14 DNS + 2 HTTP; this adds one O2 run (3 HTTPS heads,
//!   ~20 KB, ≤ 20 s), only after a deep check the user started (user decision 10-20).
//! - Never from the agent: `/control` takes `verified.restore` only with source screen / web.
//! - Each step passes `session_gate` like any `/control` write, and carries `expect_iccid`.
//! - The restore re-records nothing: its end check only says whether the network is back (a
//!   transaction just ended, so the record rule's settle time would refuse it anyway).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use luyaz_api::clock::BootTime;
use luyaz_api::execution::verified::{self as rules, Proof, Set, Step, Txn};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::ops::txn::{Phase, Who};
use crate::ops::{Engine, UbusDevice};

type Eng = Engine<UbusDevice>;

static INSTALLED: OnceLock<(Eng, Arc<crate::block::Hub>)> = OnceLock::new();
/// One check-and-record or one restore at a time.
static BUSY: AtomicBool = AtomicBool::new(false);
static PROGRESS: Mutex<Option<Value>> = Mutex::new(None);

/// A deep check is waited for at most this long (≤ 20 s by its own budget).
const DIAG_WAIT: Duration = Duration::from_secs(90);
/// One step's transaction: its deadline plus confirmation, like the decision loop's follow.
const STEP_MAX: Duration = Duration::from_secs(10 * 60);
const POLL: Duration = Duration::from_secs(2);

/// Startup (`server.rs`): the engine and the hub this module works with.
pub fn install(e: Eng, hub: Arc<crate::block::Hub>) {
    let _ = INSTALLED.set((e, hub));
}

struct Held;

impl Held {
    fn take() -> Option<Self> {
        (!BUSY.swap(true, Ordering::SeqCst)).then_some(Held)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

fn dir() -> PathBuf {
    let base = std::env::var("LUYAZ_DATA_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/data/luyaz/core".into());
    PathBuf::from(base).join("verified")
}

/// `<sha256(ICCID) first 16 hex>-<mcc>.json`: the card number never lands in a file name.
fn file(d: &std::path::Path, iccid: &str, mcc: i64) -> PathBuf {
    let h = Sha256::digest(iccid.as_bytes());
    let hex: String = h.iter().take(8).map(|b| format!("{b:02x}")).collect();
    d.join(format!("{hex}-{mcc}.json"))
}

fn load(iccid: &str, mcc: i64) -> Option<Set> {
    load_in(&dir(), iccid, mcc)
}

fn load_in(d: &std::path::Path, iccid: &str, mcc: i64) -> Option<Set> {
    serde_json::from_slice(&std::fs::read(file(d, iccid, mcc)).ok()?).ok()
}

fn save(iccid: &str, s: &Set) -> std::io::Result<()> {
    save_in(&dir(), iccid, s)
}

fn save_in(d: &std::path::Path, iccid: &str, s: &Set) -> std::io::Result<()> {
    let p = file(d, iccid, s.mcc);
    std::fs::create_dir_all(d)?;
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(s).expect("set serializes"))?;
    std::fs::rename(&tmp, &p)
}

fn mcc(hub: &crate::block::Hub) -> Option<i64> {
    hub.observations(&["signal"])
        .first()
        .and_then(|o| o.data["serving_mcc"].as_i64())
        .filter(|&m| m > 0)
}

fn txn(e: &Eng) -> Txn {
    let s = e.status(None);
    if s.is_null() {
        return Txn::None;
    }
    let fin = serde_json::from_value::<Phase>(s["phase"].clone()).is_ok_and(Phase::is_final);
    if !fin {
        return Txn::Active;
    }
    Txn::Final(s["age_ms"].as_u64().unwrap_or(0))
}

fn epochs(hub: &crate::block::Hub) -> luyaz_api::execution::Epochs {
    hub.firmware()
        .loop_epochs(
            BootTime::read().as_ref(),
            crate::api::propose::RADIO_EXEC_S,
        )
        .0
}

/// One O2 run with the epochs around it; None = no source address to bind to.
async fn o2(hub: Arc<crate::block::Hub>) -> Option<(luyaz_api::execution::Recheck, Epochs2)> {
    tokio::task::spawn_blocking(move || {
        let then = epochs(&hub);
        let (samples, v) = crate::o2::check(crate::ops::reconnect::IFACE)?;
        let now = epochs(&hub);
        let r = luyaz_api::execution::https::recheck(&samples, &v, &then, &now);
        Some((r, (then, now)))
    })
    .await
    .ok()
    .flatten()
}

type Epochs2 = (luyaz_api::execution::Epochs, luyaz_api::execution::Epochs);

/// After the user's deep check (`api/watch.rs`): wait for the run, then check and maybe record.
pub fn after_diagnose(diag: Arc<luyaz_netwatch::diagnose::Diagnosis>) {
    let Some((e, hub)) = INSTALLED.get() else {
        return;
    };
    let (e, hub) = (e.clone(), hub.clone());
    tokio::spawn(async move {
        let start = tokio::time::Instant::now();
        loop {
            tokio::time::sleep(POLL).await;
            let (_, v) = diag.get();
            if v["data"]["state"] == "done" {
                break;
            }
            if start.elapsed() > DIAG_WAIT || v["data"]["state"] == "idle" {
                return;
            }
        }
        let why = check_and_record(&e, hub).await;
        eprintln!("verified: after the deep check: {why}");
    });
}

/// "recorded" or why not. Cheap refusals first: no O2 while a transaction runs or settles.
async fn check_and_record(e: &Eng, hub: Arc<crate::block::Hub>) -> &'static str {
    let Some(_held) = Held::take() else {
        return "busy";
    };
    let t = txn(e);
    if matches!(t, Txn::Active) || matches!(t, Txn::Final(a) if a < rules::SETTLE_MS) {
        return "txn_not_settled";
    }
    let Some(m) = mcc(&hub) else {
        return "no_country";
    };
    let Ok(before) = crate::ops::current().await else {
        return "read_failed";
    };
    let Some((r, (then, now))) = o2(hub.clone()).await else {
        return "no_source";
    };
    let Ok(cur) = crate::ops::current().await else {
        return "read_failed";
    };
    let p = Proof {
        txn: txn(e),
        recheck: &r,
        then: &then,
        now: &now,
        iccid_then: Some(&before.iccid),
        iccid_now: Some(&cur.iccid),
        mcc: mcc(&hub).filter(|&x| x == m),
    };
    if let Some(why) = rules::refuse_record(&p) {
        return why;
    }
    let at = i64::try_from(luyaz_netwatch::now_unix()).unwrap_or(0);
    let conn = then.conn.clone().unwrap_or_default();
    let Some(set) = rules::next(load(&cur.iccid, m).as_ref(), at, m, &conn, &cur.items) else {
        return "nothing_read";
    };
    match save(&cur.iccid, &set) {
        Ok(()) => "recorded",
        Err(e) => {
            eprintln!("verified: save: {e}");
            "save_failed"
        }
    }
}

fn step_json(s: &Step) -> Value {
    use crate::ops::spec::RollbackCapability as R;
    let rollback = crate::ops::spec::find(s.action).map(|sp| match sp.rollback_capability {
        R::CapturedAndProven => "proven",
        R::Unsupported => "unsupported",
        R::Unknown => "unknown",
    });
    json!({"item": s.item, "action": s.action, "now": s.now, "to": s.to, "rollback": rollback})
}

/// `GET /api/verified`: this card × country's set and what a restore would do.
pub async fn view() -> Value {
    let Some((e, hub)) = INSTALLED.get() else {
        return json!({"available": false, "why": "unavailable"});
    };
    let progress = PROGRESS.lock().map(|p| p.clone()).unwrap_or(None);
    let Ok(cur) = crate::ops::current().await else {
        return json!({"available": false, "why": "read_failed", "progress": progress});
    };
    let m = mcc(hub);
    let set = m.and_then(|m| load(&cur.iccid, m));
    let steps = set.as_ref().map(|s| rules::diff(s, &cur.items)).unwrap_or_default();
    let why = if BUSY.load(Ordering::SeqCst) {
        Some("busy")
    } else {
        rules::refuse_restore(m, set.as_ref(), matches!(txn(e), Txn::Active), &steps)
    };
    json!({
        "available": why.is_none(),
        "why": why,
        "mcc": m,
        "at": set.as_ref().map(|s| s.at),
        "prev_at": set.as_ref().and_then(|s| s.prev.as_ref()).map(|p| p.at),
        "steps": steps.iter().map(step_json).collect::<Vec<_>>(),
        "progress": progress,
    })
}

/// `/control verified.restore`: Ok = started (progress in `view`), Err = why not.
/// A person started it here: not an agent's proposal a person approved (source mcp).
fn a_person(who: Who) -> bool {
    who.source.can_confirm() && who.approved_by.can_confirm()
}

pub async fn restore(who: Who) -> Result<Value, &'static str> {
    if !a_person(who) {
        return Err("not_a_person");
    }
    let (e, hub) = INSTALLED.get().ok_or("unavailable")?;
    let held = Held::take().ok_or("busy")?;
    let cur = crate::ops::current().await.map_err(|_| "read_failed")?;
    let m = mcc(hub);
    let set = m.and_then(|m| load(&cur.iccid, m));
    let steps = set.as_ref().map(|s| rules::diff(s, &cur.items)).unwrap_or_default();
    if let Some(why) = rules::refuse_restore(m, set.as_ref(), matches!(txn(e), Txn::Active), &steps) {
        return Err(why);
    }
    // the same session rule as every other write (a guard session holds the network)
    if steps.iter().any(|s| e.session_gate(s.action, None).is_err()) {
        return Err("session_active");
    }
    let p = json!({
        "steps": steps.iter().map(|s| json!({"item": s.item, "state": "waiting"})).collect::<Vec<_>>(),
        "end": null,
    });
    if let Ok(mut g) = PROGRESS.lock() {
        *g = Some(p.clone());
    }
    let (e, hub) = (e.clone(), hub.clone());
    tokio::spawn(async move {
        let end = run(&e, hub, who, &cur.iccid, &steps).await;
        eprintln!("verified: restore ended: {end}");
        set_progress(|p| p["end"] = json!(end));
        drop(held);
    });
    Ok(p)
}

fn set_progress(f: impl FnOnce(&mut Value)) {
    if let Ok(mut g) = PROGRESS.lock()
        && let Some(p) = g.as_mut()
    {
        f(p);
    }
}

fn mark(i: usize, state: &str, extra: Value) {
    set_progress(|p| {
        p["steps"][i]["state"] = json!(state);
        if let (Some(o), Some(x)) = (p["steps"][i].as_object_mut(), extra.as_object()) {
            o.extend(x.clone());
        }
    });
}

/// The steps in order; the first that doesn't confirm stops the rest (its own transaction has
/// already done whatever its rollback does). Then one O2 check, and nothing else.
async fn run(e: &Eng, hub: Arc<crate::block::Hub>, who: Who, iccid: &str, steps: &[Step]) -> &'static str {
    for (i, s) in steps.iter().enumerate() {
        mark(i, "running", json!({}));
        let Some(spec) = crate::ops::spec::find(s.action) else {
            mark(i, "failed", json!({"why": "no_such_action"}));
            return "stopped";
        };
        let to = if s.item == "apn" {
            let list = match crate::ops::current().await {
                Ok(c) if c.iccid == iccid => c.apn_list,
                Ok(_) => {
                    mark(i, "failed", json!({"why": "sim_changed"}));
                    return "stopped";
                }
                Err(_) => {
                    mark(i, "failed", json!({"why": "read_failed"}));
                    return "stopped";
                }
            };
            match rules::apn_target(&s.to, &list) {
                Some(v) => v,
                None => {
                    mark(i, "failed", json!({"why": "apn_profile_gone"}));
                    return "stopped";
                }
            }
        } else {
            s.to.clone()
        };
        let params = (spec.params)(&to);
        let target = match spec.target(&params) {
            Ok(t) => t,
            Err(_) => {
                mark(i, "failed", json!({"why": "bad_value"}));
                return "stopped";
            }
        };
        if e.session_gate(s.action, None).is_err() {
            mark(i, "failed", json!({"why": "session_active"}));
            return "stopped";
        }
        let req = crate::ops::Request {
            spec,
            target,
            params,
            source: who.source,
            approved_by: who.approved_by,
            op_id: None,
            undo: false,
            rollback: None,
            expect_iccid: Some(iccid.to_owned()),
            receipt: false,
        };
        let op_id = match e.submit(req).await {
            crate::ops::Submit::Applied { op, .. } => op["op_id"].as_str().unwrap_or("").to_owned(),
            crate::ops::Submit::Busy(_) => {
                mark(i, "failed", json!({"why": "txn_active"}));
                return "stopped";
            }
            crate::ops::Submit::Refused(r) | crate::ops::Submit::NoCapture(r) => {
                eprintln!("verified: {}: {r}", s.action);
                mark(i, "failed", json!({"why": "refused"}));
                return "stopped";
            }
            _ => {
                mark(i, "failed", json!({"why": "not_a_transaction"}));
                return "stopped";
            }
        };
        let phase = follow(e, &op_id).await;
        let ok = phase == Some(Phase::Confirmed);
        mark(i, if ok { "done" } else { "failed" }, json!({"op_id": op_id, "phase": phase}));
        if !ok {
            return "stopped";
        }
    }
    match o2(hub).await {
        Some((r, _)) if r.layers.iter().all(|(_, c)| *c == luyaz_api::execution::Check::Pass)
            && !r.layers.is_empty() =>
        {
            "restored"
        }
        _ => "restored_still_down",
    }
}

/// The step's final phase; None = not final within [`STEP_MAX`].
async fn follow(e: &Eng, op_id: &str) -> Option<Phase> {
    let start = tokio::time::Instant::now();
    while start.elapsed() < STEP_MAX {
        let s = e.status(Some(op_id));
        if let Ok(p) = serde_json::from_value::<Phase>(s["phase"].clone())
            && p.is_final()
        {
            return Some(p);
        }
        tokio::time::sleep(POLL).await;
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn only_a_person_on_the_screen_or_web() {
        use crate::ops::Source;
        assert!(a_person(Who::from(Source::Web)) && a_person(Who::from(Source::Screen)));
        assert!(!a_person(Who::from(Source::Mcp)));
        // an agent's proposal approved on the web is still the agent's restore
        assert!(!a_person(Who::new(Source::Mcp, Some(Source::Web)).unwrap()));
        let r = tokio::runtime::Builder::new_current_thread().build().unwrap();
        assert_eq!(r.block_on(restore(Who::from(Source::Mcp))), Err("not_a_person"));
    }

    #[test]
    fn the_file_name_hides_the_card_and_splits_by_country() {
        let d = std::path::Path::new("/x");
        let a = file(d, "89860012345678901234", 460);
        let b = file(d, "89860012345678901234", 454);
        let name = a.file_name().unwrap().to_str().unwrap();
        assert!(!name.contains("8986"));
        assert!(name.ends_with("-460.json") && name.len() == 16 + "-460.json".len());
        assert_ne!(a, b);
    }

    #[test]
    fn save_and_load_round_trip() {
        let d = std::env::temp_dir().join(format!("verified-{:016x}", rand::random::<u64>()));
        let items: BTreeMap<String, Value> = [("band.lte".to_owned(), json!("1,3"))].into();
        let s = rules::next(None, 5, 460, "c1", &items).unwrap();
        save_in(&d, "8986x", &s).unwrap();
        assert_eq!(load_in(&d, "8986x", 460), Some(s));
        assert_eq!(load_in(&d, "8986x", 454), None);
        let _ = std::fs::remove_dir_all(d);
    }
}
