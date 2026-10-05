//! Pick one of the carrier's auto-APN candidates, per SIM (2026-10-03, D39).
//!
//! The firmware's auto APN matches on MCC-MNC only and tries the candidates in
//! database order, so a card can sit on an IoT APN (`ctiot`) while the right
//! one (`ctnet`) is the next row. The stock web page only shows the candidate;
//! choosing one means a manual APN (`set_apn_mode 1` + `enable_manu_apn_id`).
//! Same here: picking a candidate copies it into the manual list (or reuses an
//! identical manual profile) and switches to it.
//!
//! A manual APN would follow the device to the next card, so the pick is
//! remembered under the SIM's ICCID. The owner authorised one automatic APN
//! write for this (2026-10-03, D39), and only this:
//! - another card goes in while our copy is the active manual APN → back to
//!   auto, or straight to that card's own pick if it has one;
//! - the card that made the pick comes back while the device is on auto (or on
//!   another card's copy) → its pick again.
//!
//! A manual APN the owner made themselves is never touched. Choosing auto or
//! one of their own manual APNs forgets this card's pick. The decision is
//! [`decide`], a pure function; the thread around it only does I/O.
//!
//! The card comes from datad's `sim` block (no ubus in the steady state: the
//! 9-25 rule is that background reads belong to datad). Only an older datad
//! or fallback mode makes the watcher read `get_sim_info` itself, once a
//! minute. The APN mode and manual list are read only once a card change is
//! confirmed. A change is committed only after its action worked; a failed
//! one is retried, and after 3 failures it stays as a notice until the next
//! APN change that works.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

const STORE: &str = "/data/apn-pick.json";
/// Re-look this soon after a first sighting or a failed switch.
const QUICK: Duration = Duration::from_secs(10);
const MAX_FAILS: u8 = 3;

/// Fields that make two APN profiles the same (`password` included: the copy
/// must dial exactly what the candidate does).
const SAME: [&str; 6] = ["wanapn", "pdpType", "roamingPdpType", "pppAuthMode", "username", "password"];

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Pick {
    /// The manual profile we dial (our copy, or an identical one that existed).
    pub manual_id: String,
    pub apn: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Store {
    /// Full ICCID (pad nibble trimmed) → its pick.
    #[serde(default)]
    pub picks: BTreeMap<String, Pick>,
    /// The card seen last; a change is what the watcher acts on.
    #[serde(default)]
    pub last_iccid: String,
    /// Last automatic change, for the UIs: {"at": unix, "text", "text_en",
    /// "sticky"}. Sticky (a switch that kept failing) stays until cleared.
    #[serde(default)]
    pub notice: Value,
}

static LOCK: Mutex<()> = Mutex::new(());

fn load() -> Store {
    fs::read_to_string(STORE).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save(s: &Store) {
    if let Ok(text) = serde_json::to_string(s) {
        let _ = crate::fsutil::atomic_write(STORE, text.as_bytes());
    }
}

/// ICCID as the card has it: the ZTE stack appends a BCD pad nibble ("…F").
pub fn norm_iccid(s: &str) -> String {
    s.trim().trim_end_matches(['F', 'f']).to_string()
}

/// Last four digits, for logs and the UIs (full ICCIDs stay in the store).
pub fn tail4(iccid: &str) -> &str {
    &iccid[iccid.len().saturating_sub(4)..]
}

/// IoT / M2M APNs: they bring up a bearer but not the normal internet, which
/// is exactly why the firmware keeps picking them. Shown, tagged, not hidden.
pub fn is_iot(apn: &str) -> bool {
    let a = apn.to_ascii_lowercase();
    a.contains("iot") || a.starts_with("m2m")
}

fn same_profile(a: &Value, b: &Value) -> bool {
    SAME.iter().all(|k| {
        let (x, y) = (&a[*k], &b[*k]);
        // Numbers and numeric strings both appear in the vendor lists.
        x == y || (x.is_null() && y.as_str() == Some("")) || (y.is_null() && x.as_str() == Some("")) || x.to_string().trim_matches('"') == y.to_string().trim_matches('"')
    })
}

/// The manual profile that dials the same as `cand`, if there is one.
pub fn find_same(manual: &Value, cand: &Value) -> Option<String> {
    manual["apnListArray"].as_array()?.iter().find(|p| same_profile(p, cand)).and_then(|p| p["profileId"].as_str()).map(str::to_string)
}

#[derive(Debug, PartialEq, Clone)]
pub enum Action {
    None,
    /// Go to automatic APN.
    Auto,
    /// Dial this manual profile.
    Use(String),
}

/// Two reads in a row of the same new ICCID before acting: the stack reads
/// empty, then the old card, for a moment around an eSIM switch.
#[derive(Debug, Default)]
pub struct Watch {
    pending: String,
    seen: u8,
    /// Failed switches for `pending`.
    pub fails: u8,
}

impl Watch {
    /// Looking at a new card, or retrying: look again soon.
    pub fn eager(&self) -> bool {
        self.seen > 0
    }
}

/// What to do now. `card` is set once a new card is confirmed: commit it to
/// `last_iccid` when `action` is None or after the action worked.
#[derive(Debug, PartialEq)]
pub struct Decision {
    pub action: Action,
    pub card: Option<String>,
}

const NOTHING: Decision = Decision { action: Action::None, card: None };

/// One look. `iccid` normalised ("" = unreadable); `busy` = an APN switch, a
/// network search/register or an eSIM job running. `apn` reads (manual mode,
/// selected manual id) — called only for a confirmed change, `None` = could
/// not read (try again next time).
pub fn decide(store: &Store, w: &mut Watch, iccid: &str, busy: bool, apn: impl FnOnce() -> Option<(bool, Option<String>)>) -> Decision {
    if busy || iccid.is_empty() || iccid == store.last_iccid {
        *w = Watch::default();
        return NOTHING;
    }
    if w.pending == iccid {
        w.seen = w.seen.saturating_add(1);
    } else {
        *w = Watch { pending: iccid.to_string(), seen: 1, fails: 0 };
    }
    if w.seen < 2 {
        return NOTHING;
    }
    let card = Some(iccid.to_string());
    if store.last_iccid.is_empty() {
        // Nothing to compare with (first run): just learn the card.
        return Decision { action: Action::None, card };
    }
    let Some((manual_mode, selected)) = apn() else { return NOTHING };
    let ours = |id: &str| store.picks.values().any(|p| p.manual_id == id);
    let on_our_copy = manual_mode && selected.as_deref().is_some_and(ours);
    let action = match store.picks.get(iccid) {
        Some(p) if manual_mode && selected.as_deref() == Some(p.manual_id.as_str()) => Action::None,
        Some(p) if !manual_mode || on_our_copy => Action::Use(p.manual_id.clone()),
        Some(_) => Action::None, // on the owner's own manual APN: leave it
        None if on_our_copy => Action::Auto,
        None => Action::None,
    };
    Decision { action, card }
}

fn call(m: &str, a: &str) -> Result<Value, String> {
    ubus::read("zwrt_apn_object", m, Some(a))
}

/// The card's ICCID: datad's `sim` block when subscribed, else one ubus read.
pub fn current_iccid() -> String {
    if let Some(i) = crate::datad_feed::global().and_then(|f| f.view().sim_iccid()) {
        return i;
    }
    ubus_iccid()
}

fn ubus_iccid() -> String {
    ubus::read("zwrt_zte_mdm.api", "get_sim_info", Some("{}"))
        .ok()
        .and_then(|v| v["sim_iccid"].as_str().map(norm_iccid))
        .unwrap_or_default()
}

/// For a user's candidate pick: the ICCID, trying a few times (it reads empty
/// for a moment after a re-read). Empty = refuse the pick.
pub fn iccid_for_pick() -> String {
    for i in 0..3 {
        let c = current_iccid();
        if !c.is_empty() {
            return c;
        }
        if i < 2 {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    String::new()
}

/// Same order as the agent's manual switch since 9-25: select the profile,
/// then the mode, so the re-dial uses it. (The stock page sends the mode
/// first; both have worked on this firmware.)
///
/// Through datad as this thread's source (the watcher runs as `auto`, a pick
/// on the screen or web as `web`).
pub fn dial_manual(id: &str) -> Result<(), String> {
    use crate::datad_write::send;
    send("apn.enable", &json!({"profile_id": id}))
        .into_result()
        .and_then(|_| send("apn.set_mode", &json!({"mode": 1})).into_result())
        .map(|_| ())
}

pub fn dial_auto() -> Result<(), String> {
    crate::datad_write::send("apn.set_mode", &json!({"mode": 0})).into_result().map(|_| ())
}

/// The manual profile for auto candidate `auto_id`: an identical one if it
/// exists, else a copy added now. Returns its profileId and APN name.
pub fn manual_for_candidate(auto_id: &str) -> Result<(String, String), String> {
    let auto = call("get_auto_apn_list", "{}")?;
    let cand = auto["apnListArray"]
        .as_array()
        .and_then(|a| a.iter().find(|p| p["profileId"] == auto_id))
        .cloned()
        .ok_or_else(|| "没有这个候选 APN".to_string())?;
    let apn = cand["wanapn"].as_str().unwrap_or("").to_string();
    if let Some(id) = find_same(&call("get_manu_apn_list", "{}")?, &cand) {
        return Ok((id, apn));
    }
    crate::datad_write::send("apn.add", &crate::router::apn_params(&cand, false)?).into_result()?;
    // add_manu_apn's reply does not reliably carry the new id: find it.
    find_same(&call("get_manu_apn_list", "{}")?, &cand)
        .map(|id| (id, apn))
        .ok_or_else(|| "复制成手动 APN 后没读到它".to_string())
}

/// Remember `manual_id` as this card's pick (after the switch worked).
pub fn remember(iccid: &str, manual_id: &str, apn: &str) {
    if iccid.is_empty() {
        return;
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load();
    s.picks.insert(iccid.to_string(), Pick { manual_id: manual_id.to_string(), apn: apn.to_string() });
    s.last_iccid = iccid.to_string();
    save(&s);
}

/// An APN change worked: a sticky failure notice is no longer true.
pub fn clear_sticky() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load();
    if s.notice["sticky"].as_bool() == Some(true) {
        s.notice = Value::Null;
        save(&s);
    }
}

/// The owner chose auto or one of their own manual APNs for this card.
pub fn forget(iccid: &str) {
    if iccid.is_empty() {
        return;
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load();
    if s.picks.remove(iccid).is_some() {
        save(&s);
    }
}

/// For the APN view: this card's picked manual id, and the last notice if it
/// is under 10 minutes old (null otherwise, so the UIs don't keep an old one).
pub fn view(iccid: &str) -> (Option<String>, Value) {
    let s = load();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    (s.picks.get(iccid).map(|p| p.manual_id.clone()), fresh_notice(&s.notice, now))
}

fn fresh_notice(n: &Value, now: u64) -> Value {
    if n["sticky"].as_bool() == Some(true) {
        return n.clone();
    }
    match n["at"].as_u64() {
        Some(at) if now >= at && now - at < 600 => n.clone(),
        _ => Value::Null,
    }
}

fn apn_state() -> Option<(bool, Option<String>)> {
    let mode = call("get_apn_mode", "{}").ok()?;
    let manual_mode = match &mode["apn_mode"] {
        Value::String(s) => s == "1",
        v => v.as_i64() == Some(1),
    };
    let list = call("get_manu_apn_list", "{}").ok()?;
    let selected = list["apnListArray"]
        .as_array()
        .and_then(|a| a.iter().find(|p| crate::netinfo::truthy(&p["isEnable"])))
        .and_then(|p| p["profileId"].as_str().map(str::to_string));
    Some((manual_mode, selected))
}

fn busy(state: &AppState) -> bool {
    crate::netinfo::apn_switching() || crate::netinfo::modem_busy().is_some() || crate::esim::busy(state)
}

fn set_notice(text: String, text_en: String, sticky: bool) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load();
    s.notice = json!({"at": now, "text": text, "text_en": text_en, "sticky": sticky});
    save(&s);
}

fn commit(card: &str) {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load();
    if s.last_iccid != card {
        s.last_iccid = card.to_string();
        save(&s);
    }
}

/// Watch for card changes (never in sidecar mode: it writes the APN).
pub fn start(state: Arc<AppState>) {
    std::thread::spawn(move || {
        let mut w = Watch::default();
        let mut version = 0;
        loop {
            // Wakes on datad's sim block changing; at most a minute between
            // looks (QUICK while confirming or retrying).
            let wait = if w.eager() { QUICK } else { crate::datad_feed::MAX_WAIT };
            let view = crate::datad_feed::wait(version, wait);
            version = view.version;
            let iccid = view.sim_iccid().unwrap_or_else(ubus_iccid);
            let store = load();
            let d = decide(&store, &mut w, &iccid, busy(&state), apn_state);
            let Some(card) = d.card else { continue };
            if d.action == Action::None {
                commit(&card);
                continue;
            }
            if !crate::netinfo::apn_switching_claim() {
                continue; // a switch started meanwhile: Watch still says act, next look retries
            }
            let res = crate::datad_write::with_source(crate::datad_write::Source::Auto, || match &d.action {
                Action::Auto => dial_auto(),
                Action::Use(id) => dial_manual(id),
                Action::None => unreachable!(),
            });
            if let Some(pause) = crate::datad_write::retry_after(&res) {
                // A network-mode change is confirming (D40): datad holds
                // automatic writes until it ends. Not a failed try; the Watch
                // still says act, so the next look does it again.
                crate::netinfo::apn_switching_release(None);
                eprintln!("[apn_pick] SIM …{} → {:?}: held off while a network-mode change is confirming, again in {}s", tail4(&card), d.action, pause.as_secs());
                std::thread::sleep(pause);
                continue;
            }
            crate::netinfo::apn_switching_release(res.as_ref().err().cloned());
            eprintln!("[apn_pick] SIM …{} → {:?}: {:?}", tail4(&card), d.action, res);
            let apn = store.picks.get(&card).map(|p| p.apn.clone()).unwrap_or_default();
            match res {
                Ok(()) => {
                    commit(&card);
                    w = Watch::default();
                    let (t, te) = match d.action {
                        Action::Auto => ("换了卡，APN 已切回自动".to_string(), "SIM changed; APN back to automatic".to_string()),
                        _ => (format!("换回这张卡，APN 用回 {apn}"), format!("This SIM is back; APN {apn} again")),
                    };
                    set_notice(t, te, false);
                }
                Err(e) => {
                    w.fails += 1;
                    if w.fails >= MAX_FAILS {
                        // Give up: the notice stays until an APN change works.
                        commit(&card);
                        w = Watch::default();
                        set_notice(
                            format!("换卡后切 APN 试了 {MAX_FAILS} 次没成功：{e}"),
                            format!("Couldn't switch APN after the SIM change ({MAX_FAILS} tries): {e}"),
                            true,
                        );
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fake ICCIDs: these repos sync to a public mirror.
    const A: &str = "8900000000000000001";
    const B: &str = "8900000000000000002";

    type Apn = Option<(bool, Option<String>)>;

    fn apn(manual: bool, sel: Option<&str>) -> Apn {
        Some((manual, sel.map(str::to_string)))
    }

    /// Two looks at `iccid`; the first never acts. Commits like the watcher
    /// does when the action is None.
    fn twice(s: &mut Store, w: &mut Watch, iccid: &str, a: Apn) -> Action {
        assert_eq!(decide(s, w, iccid, false, || a.clone()), NOTHING, "first read never acts");
        let d = decide(s, w, iccid, false, || a);
        if let (Action::None, Some(c)) = (&d.action, &d.card) {
            s.last_iccid = c.clone();
        }
        d.action
    }

    fn store_with_pick() -> Store {
        let mut s = Store { last_iccid: A.into(), ..Default::default() };
        s.picks.insert(A.into(), Pick { manual_id: "manu4".into(), apn: "ctnet".into() });
        s
    }

    /// datad answered op_busy (D40): the watcher leaves the Watch as it is,
    /// so the next look acts again, and it never counts toward giving up.
    #[test]
    fn op_busy_is_retried_and_not_counted() {
        let mut s = store_with_pick();
        s.last_iccid = B.into();
        let mut w = Watch::default();
        assert_eq!(twice(&mut s, &mut w, A, apn(false, None)), Action::Use("manu4".into()));
        let busy = crate::datad_write::Reply::OpBusy { message: "confirming".into(), op: None }.into_result().map(|_| ());
        assert!(crate::datad_write::retry_after(&busy).is_some());
        for _ in 0..(MAX_FAILS + 2) {
            let d = decide(&s, &mut w, A, false, || apn(false, None));
            assert_eq!(d.action, Action::Use("manu4".into()));
            assert!(w.eager(), "keeps looking soon");
        }
        assert_eq!(w.fails, 0);
    }

    #[test]
    fn iccid_pad_and_tail() {
        assert_eq!(norm_iccid("8900000000000000072F"), "8900000000000000072");
        assert_eq!(tail4("8900000000000000001"), "0001");
        assert_eq!(tail4("12"), "12");
    }

    #[test]
    fn iot_names() {
        for a in ["ctiot", "CMIOT5G", "m2m.example", "iot.1nce.net"] {
            assert!(is_iot(a), "{a}");
        }
        for a in ["ctnet", "plus.4g", "uad5gn.au-net.ne.jp", "cmnet"] {
            assert!(!is_iot(a), "{a}");
        }
    }

    #[test]
    fn same_profile_matches_numbers_and_strings() {
        // Shapes recorded on the owner's device 2026-10-03 (passwords blanked;
        // the manual list returns real passwords, checked by length the same day).
        let cand = json!({"profilename":"China Telecom 4G","wanapn":"ctnet","username":"","password":"","pdpType":3,"pppAuthMode":0,"profileId":"auto109600","roamingPdpType":2});
        let manual = json!({"apnListArray":[
            {"profilename":"Japan","wanapn":"plus.4g","username":"","password":"","pdpType":3,"pppAuthMode":0,"profileId":"manu2","roamingPdpType":0},
            {"profilename":"CTNET","wanapn":"ctnet","username":"","password":"","pdpType":"3","pppAuthMode":0,"profileId":"manu7","roamingPdpType":2}]});
        assert_eq!(find_same(&manual, &cand).as_deref(), Some("manu7"));
        let other_pdp = json!({"apnListArray":[{"wanapn":"ctnet","username":"","password":"","pdpType":1,"pppAuthMode":0,"profileId":"manu8","roamingPdpType":2}]});
        assert_eq!(find_same(&other_pdp, &cand), None);
        assert_eq!(find_same(&Value::Null, &cand), None);
    }

    #[test]
    fn same_card_does_nothing_and_reads_no_apn() {
        let s = store_with_pick();
        let mut w = Watch::default();
        for _ in 0..3 {
            assert_eq!(decide(&s, &mut w, A, false, || panic!("no APN read without a card change")), NOTHING);
        }
    }

    #[test]
    fn new_card_while_on_our_copy_goes_auto() {
        let mut s = store_with_pick();
        let mut w = Watch::default();
        assert_eq!(twice(&mut s, &mut w, B, apn(true, Some("manu4"))), Action::Auto);
        assert_eq!(s.last_iccid, A, "not committed until the switch worked");
    }

    #[test]
    fn picking_card_back_uses_its_pick() {
        let mut s = store_with_pick();
        s.last_iccid = B.into();
        let mut w = Watch::default();
        assert_eq!(twice(&mut s, &mut w, A, apn(false, None)), Action::Use("manu4".into()));
        let mut w = Watch::default();
        assert_eq!(twice(&mut s, &mut w, A, apn(true, Some("manu4"))), Action::None, "already dialling it");
        assert_eq!(s.last_iccid, A);
    }

    #[test]
    fn going_from_one_pick_to_another_is_one_switch() {
        let mut s = store_with_pick();
        s.picks.insert(B.into(), Pick { manual_id: "manu5".into(), apn: "plus.4g".into() });
        let mut w = Watch::default();
        assert_eq!(twice(&mut s, &mut w, B, apn(true, Some("manu4"))), Action::Use("manu5".into()));
    }

    #[test]
    fn owners_own_manual_apn_is_never_touched() {
        let mut s = store_with_pick();
        let mut w = Watch::default();
        assert_eq!(twice(&mut s, &mut w, B, apn(true, Some("manu2"))), Action::None);
        let mut w = Watch::default();
        assert_eq!(twice(&mut s, &mut w, A, apn(true, Some("manu2"))), Action::None);
    }

    #[test]
    fn new_card_on_auto_with_no_pick_does_nothing() {
        let mut s = store_with_pick();
        let mut w = Watch::default();
        assert_eq!(twice(&mut s, &mut w, B, apn(false, Some("manu4"))), Action::None);
        assert_eq!(s.last_iccid, B, "learned");
    }

    #[test]
    fn empty_flicker_and_busy_never_act() {
        let s = store_with_pick();
        let mut w = Watch::default();
        let a = || apn(true, Some("manu4"));
        assert_eq!(decide(&s, &mut w, B, false, a), NOTHING);
        assert_eq!(decide(&s, &mut w, "", false, a), NOTHING);
        assert_eq!(decide(&s, &mut w, B, false, a), NOTHING, "the empty read restarted the count");
        assert_eq!(decide(&s, &mut w, B, true, a), NOTHING);
        assert_eq!(decide(&s, &mut w, B, true, a), NOTHING);
    }

    #[test]
    fn unreadable_apn_state_retries() {
        let s = store_with_pick();
        let mut w = Watch::default();
        assert_eq!(decide(&s, &mut w, B, false, || None), NOTHING);
        assert_eq!(decide(&s, &mut w, B, false, || None), NOTHING, "can't read the APN: no card commit");
        let d = decide(&s, &mut w, B, false, || apn(true, Some("manu4")));
        assert_eq!(d, Decision { action: Action::Auto, card: Some(B.into()) });
    }

    #[test]
    fn failed_claim_or_switch_is_retried_on_the_next_look() {
        let s = store_with_pick();
        let mut w = Watch::default();
        let a = || apn(true, Some("manu4"));
        assert_eq!(decide(&s, &mut w, B, false, a), NOTHING);
        assert_eq!(decide(&s, &mut w, B, false, a).action, Action::Auto);
        // The watcher could not claim / the switch failed: nothing committed,
        // the very next look acts again (no new two-read wait).
        assert!(w.eager());
        w.fails += 1;
        assert_eq!(decide(&s, &mut w, B, false, a).action, Action::Auto);
        assert_eq!(w.fails, 1, "fail count kept for the same card");
    }

    #[test]
    fn first_run_only_learns_the_card() {
        let mut s = Store::default();
        let mut w = Watch::default();
        assert_eq!(twice(&mut s, &mut w, A, apn(true, Some("manu4"))), Action::None);
        assert_eq!(s.last_iccid, A);
    }

    #[test]
    fn notice_only_while_fresh_unless_sticky() {
        let n = json!({"at": 1000, "text": "x"});
        assert_eq!(fresh_notice(&n, 1300), n);
        assert!(fresh_notice(&n, 1600).is_null());
        assert!(fresh_notice(&n, 999).is_null(), "clock went back");
        assert!(fresh_notice(&Value::Null, 1000).is_null());
        let sticky = json!({"at": 1000, "text": "failed", "sticky": true});
        assert_eq!(fresh_notice(&sticky, 999_999), sticky);
    }

    #[test]
    fn store_round_trips() {
        let s = store_with_pick();
        let back: Store = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let old: Store = serde_json::from_str("{}").unwrap();
        assert!(old.picks.is_empty());
    }
}
