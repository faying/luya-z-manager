//! The card table (docs/designs/dual-slot-removable.md §1, T3): one row per physical slot, so
//! code stops reading `sim_*` fields or assuming slot 1.
//!
//! - **Where a row comes from.** On a dual-slot unit (`support_dual_sim` = 1) a poller reads QMI
//!   UIM `GET_CARD_STATUS` (read only): TLV 0x10 says per slot whether a card is in and its USIM
//!   state (a slot the firmware has not provisioned still shows its card as "detected"), TLV 0x17
//!   carries each slot's ICCID (u60g and the main device 11-16, read only: slot 1 matched
//!   `sim_iccid`, the unprovisioned slot 2 read 20 digits; not yet checked against lpac's enabled
//!   profile, nor whether it follows a profile switch without a card reset: T4 checks that after
//!   its first standby-slot switch). Every 60 s, 5 min with the screen off; [`kick`] asks for a read now (after
//!   a profile operation). The single-slot unit gets one row from what the snapshot already read
//!   and starts no poller (R6/D8: no new QMI traffic, power baseline unchanged); an empty SIM state
//!   there is "not known now", not "no card". A dual-slot unit publishes no block until its first
//!   good read.
//!   Not done (O-6, needs a hot-plug with the owner there): the UIM status indication and the
//!   R4 re-subscribe; polling only until then.
//! - **Which card.** `card_id` is `eid:<EID>`, `plain:<ICCID>` (only once the ISD-R said the card
//!   is not an eUICC) or pending (R11/D13). The EID comes from luyaz-esim, the one owner of the
//!   logical channels (R2/D4), through [`set_eid_source`]; it is called without the table lock
//!   held, from the poller and (single-slot unit) the snapshot round, so it must answer from a
//!   cache and return `Later` while it reads. Until one is set every card stays
//!   pending and is never called "unrecognised". A slot's generation goes up when a card appears
//!   (absent → present) or its ICCID turns into another one; the EID is then read again (R12/D14).
//!   An empty or failed ICCID read is not another card.
//! - **ICCIDs** are compared with [`iccid_norm`] / [`iccid_eq`] everywhere in core (R5/D7).
//! - **Out**: the `/v2` block `cards` (tails only); `sim` is left exactly as it was (R13/D15).
//!   `cards.json` (each slot's last settled reading, written only when it changes) and what a
//!   change means are `card_watch`'s (T7).

use crate::sim_identity::Identity;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once, OnceLock};

/// An ICCID as core compares it: trimmed, the BCD filler F dropped from the end, upper case.
/// Text stored by earlier versions with the same rule keeps matching.
pub fn iccid_norm(s: &str) -> String {
    s.trim().trim_end_matches(['F', 'f']).to_ascii_uppercase()
}

/// The same ICCID; an empty one equals nothing (not even another empty one).
#[allow(dead_code)] // T5/T7 compare cards with it
pub fn iccid_eq(a: &str, b: &str) -> bool {
    let (a, b) = (iccid_norm(a), iccid_norm(b));
    !a.is_empty() && a == b
}

/// The last four characters (what leaves the device about a card).
pub fn tail(s: &str) -> String {
    let s = iccid_norm(s);
    s.get(s.len().saturating_sub(4)..).unwrap_or("").to_string()
}

// ---- QMI UIM GET_CARD_STATUS ----

pub const SVC_UIM: u32 = 0x0B;
pub const MSG_UIM_GET_CARD_STATUS: u16 = 0x002F;

/// One slot of a card status reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UimSlot {
    pub present: bool,
    /// the USIM's state: ready, detected, locked, perso, illegal, unknown; none = no app
    pub app: &'static str,
    /// empty when the reply does not carry it
    pub iccid: String,
}

fn app_word(state: u8) -> &'static str {
    match state {
        7 => "ready",
        1 => "detected",
        2 | 3 | 5 => "locked",
        4 => "perso",
        6 => "illegal",
        _ => "unknown",
    }
}

/// Swapped-nibble BCD; a nibble above 9 is filler.
fn bcd(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for &p in b {
        for n in [p & 0x0f, p >> 4] {
            if n < 10 {
                s.push(char::from(b'0' + n));
            }
        }
    }
    s
}

/// TLV 0x17: a count, then per slot ICCID length + BCD, ATR length + ATR, one more byte.
fn iccids(v: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let Some(&n) = v.first() else { return out };
    let mut i = 1;
    for _ in 0..n {
        let Some(&il) = v.get(i) else { break };
        let Some(id) = v.get(i + 1..i + 1 + il as usize) else {
            break;
        };
        i += 1 + il as usize;
        let Some(&al) = v.get(i) else { break };
        i += 1 + al as usize + 1;
        out.push(bcd(id));
    }
    out
}

/// The slots of a `GET_CARD_STATUS` reply body (the TLVs after the QMI header); `None` when the
/// card status TLV is missing or cut short.
pub fn decode_card_status(body: &[u8]) -> Option<Vec<UimSlot>> {
    let st = crate::qmi_nas::tlv_find(body, 0x10)?;
    let n = *st.get(8)?;
    let ids = crate::qmi_nas::tlv_find(body, 0x17)
        .map(iccids)
        .unwrap_or_default();
    let mut pos = 9;
    let mut out = Vec::new();
    for s in 0..n as usize {
        let card = st.get(pos..pos + 6)?;
        pos += 6;
        let mut app = "none";
        for k in 0..card[5] {
            let a = st.get(pos..pos + 7)?;
            let span = 7 + a[6] as usize + 7;
            st.get(pos..pos + span)?;
            // the USIM (type 2), else the first app
            if a[0] == 2 || k == 0 && app == "none" {
                app = app_word(a[1]);
            }
            pos += span;
        }
        out.push(UimSlot {
            present: card[0] == 1,
            app,
            iccid: ids.get(s).cloned().unwrap_or_default(),
        });
    }
    Some(out)
}

// ---- identity ----

/// What luyaz-esim says about a slot's card (`Esim::eid_of_slot`, T4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Eid {
    Found(String),
    /// the ISD-R answered SimFileNotFound: a plain card
    NotEuicc,
    /// busy or failed: ask again next round
    Later,
}

/// (slot, generation) → the card's EID answer.
pub type EidSource = Box<dyn Fn(u8, u64) -> Eid + Send + Sync>;
static EID_SOURCE: OnceLock<EidSource> = OnceLock::new();

/// luyaz-esim's EID reader (`Esim::eid_of_slot`, wired in `api.rs`). Until it is set every
/// card is pending.
pub fn set_eid_source(f: EidSource) {
    let _ = EID_SOURCE.set(f);
}

/// A card pending this long (with an EID reader wired) is shown as unrecognised: a plain card
/// needs two reads a minute apart, and with the screen off the table polls every 5 min.
pub const PENDING_MAX: u64 = 1200;

/// One physical slot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub slot: u8,
    pub present: bool,
    pub app: String,
    pub provisioned: bool,
    /// normalized; the last one read while the card is in
    pub iccid: String,
    pub eid: Option<String>,
    pub plain: bool,
    /// goes up when a card appears or its ICCID becomes another one
    pub generation: u64,
    /// uptime the current card started waiting for its identity
    pub pending_since: Option<u64>,
    /// data | voice | standby | off
    pub role: String,
}

impl Row {
    /// `eid:<EID>` / `plain:<ICCID>`; `None` = no card or not known yet.
    pub fn card_id(&self) -> Option<String> {
        if !self.present {
            return None;
        }
        match (&self.eid, self.plain) {
            (Some(e), _) => Some(format!("eid:{e}")),
            (None, true) if !self.iccid.is_empty() => Some(format!("plain:{}", self.iccid)),
            _ => None,
        }
    }
    /// eid | plain | pending | unknown (pending too long with a reader wired) | none (no card)
    pub fn id_state(&self, wired: bool, now: u64) -> &'static str {
        if !self.present {
            return "none";
        }
        match (&self.eid, self.plain) {
            (Some(_), _) => "eid",
            (None, true) => "plain",
            _ if wired
                && self
                    .pending_since
                    .is_some_and(|t| now.saturating_sub(t) >= PENDING_MAX) =>
            {
                "unknown"
            }
            _ => "pending",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    pub rows: Vec<Row>,
}

impl Table {
    fn row(&mut self, slot: u8) -> &mut Row {
        if let Some(i) = self.rows.iter().position(|r| r.slot == slot) {
            return &mut self.rows[i];
        }
        self.rows.push(Row {
            slot,
            role: "off".into(),
            ..Row::default()
        });
        self.rows.sort_by_key(|r| r.slot);
        let i = self.rows.iter().position(|r| r.slot == slot).unwrap_or(0);
        &mut self.rows[i]
    }

    /// Keep only slots 1..=n (the row count follows `support_dual_sim`).
    pub fn keep(&mut self, n: u8) {
        self.rows.retain(|r| r.slot >= 1 && r.slot <= n);
    }

    /// One reading of a slot. `now`: uptime seconds.
    pub fn observe(&mut self, slot: u8, present: bool, app: &str, iccid: &str, now: u64) {
        let r = self.row(slot);
        let iccid = iccid_norm(iccid);
        r.app = app.to_string();
        if !present {
            if r.present {
                *r = Row {
                    slot,
                    provisioned: r.provisioned,
                    role: r.role.clone(),
                    app: app.to_string(),
                    generation: r.generation,
                    ..Row::default()
                };
            }
            return;
        }
        let another = !iccid.is_empty() && !r.iccid.is_empty() && iccid != r.iccid;
        if !r.present || another {
            r.generation += 1;
            r.eid = None;
            r.plain = false;
            r.pending_since = Some(now);
        }
        r.present = true;
        if !iccid.is_empty() {
            r.iccid = iccid;
        }
    }

    /// Ask the EID reader about every present card still pending.
    #[cfg(test)]
    pub fn resolve(&mut self, src: Option<&EidSource>) {
        let Some(src) = src else { return };
        for (slot, generation) in self.unknown() {
            self.answer(slot, generation, src(slot, generation));
        }
    }

    /// (slot, generation) of every present card whose identity is not known yet.
    pub fn unknown(&self) -> Vec<(u8, u64)> {
        self.rows
            .iter()
            .filter(|r| r.present && r.eid.is_none() && !r.plain)
            .map(|r| (r.slot, r.generation))
            .collect()
    }

    /// The EID reader's answer for (slot, generation); dropped when the card changed meanwhile.
    pub fn answer(&mut self, slot: u8, generation: u64, a: Eid) {
        let Some(r) = self
            .rows
            .iter_mut()
            .find(|r| r.slot == slot && r.generation == generation && r.present)
        else {
            return;
        };
        match a {
            Eid::Found(e) => r.eid = Some(e.trim().to_ascii_uppercase()),
            Eid::NotEuicc => r.plain = true,
            Eid::Later => {}
        }
    }

    /// Provisioning and roles from the projected `sim` block: off when the firmware has not
    /// provisioned the slot, data = the slot data is on, voice = the voice slot when it is
    /// another one (dual only), else standby.
    pub fn roles(&mut self, sim: &Value, voice_slot: u8) {
        let dual = should_poll(sim);
        let data = sim["current_slot"]
            .as_i64()
            .filter(|s| matches!(s, 1 | 2))
            .unwrap_or(1) as u8;
        for r in &mut self.rows {
            r.provisioned = !dual || sim[format!("sim{}_provision", r.slot)].as_i64() == Some(1);
            r.role = match () {
                _ if !r.provisioned => "off",
                _ if r.slot == data => "data",
                _ if dual && r.slot == voice_slot => "voice",
                _ => "standby",
            }
            .into();
        }
    }

    /// The data card's identity for `sim_identity` on a dual-slot unit (T5, R9/D11).
    /// The firmware's reading (`fw`, the projected `sim` block's data-card fields) stays the
    /// source: it changes within a round, this table up to 5 min later. One case is overridden:
    /// the firmware's `sim_*` fields are slot 1's unless `sim2_iccid` is set, so with data on
    /// slot 2 and `sim2_iccid` empty for a beat `fw` is slot 1's card. The table tells that
    /// apart (`fw` = slot 1's card, slot 2 holds another one): the card already tracked stays
    /// if it is slot 2's, else no identity is read (an empty reading, as before the fields
    /// come back). The bool asks the table for a read now: `fw` and the table disagree.
    pub fn data_identity(
        &self,
        sim: &Value,
        fw: Identity,
        known: Option<&Identity>,
    ) -> (Identity, bool) {
        let row = |slot: u8| self.rows.iter().find(|r| r.slot == slot && r.present);
        let data = sim["current_slot"].as_i64().filter(|s| matches!(s, 1 | 2));
        if fw.iccid.is_empty() {
            return (fw, false);
        }
        if data == Some(2)
            && let (Some(one), Some(two)) = (row(1), row(2))
            && one.iccid == fw.iccid
            && !two.iccid.is_empty()
            && two.iccid != fw.iccid
        {
            let kept = known.filter(|k| k.iccid == two.iccid).cloned();
            return (kept.unwrap_or_default(), false);
        }
        let stale = data.is_some_and(|d| {
            row(d as u8).is_none_or(|r| !r.iccid.is_empty() && r.iccid != fw.iccid)
        });
        (fw, stale)
    }
}

/// The identity the tracking reads (see [`Table::data_identity`]): the firmware's reading as
/// it is on a single-slot unit; on a dual-slot one with slot 1's fields standing in for slot
/// 2's card overridden by the card table.
pub fn data_identity(sim: &Value, fw: Identity, known: Option<&Identity>) -> Identity {
    if !should_poll(sim) {
        return fw;
    }
    let (id, stale) = lock(&TABLE).data_identity(sim, fw.clone(), known);
    if stale {
        // one extra read per disagreement (the table may never follow, e.g. R12)
        let mut last = lock(&DISAGREE);
        if *last != fw.iccid {
            last.clone_from(&fw.iccid);
            kick();
        }
    }
    id
}

/// Only a dual-slot unit polls the card status (T6 ③: the single-slot unit never starts it).
pub fn should_poll(sim: &Value) -> bool {
    sim["dual_sim"].as_i64() == Some(1)
}

/// The single-slot unit's one reading, from the `sim` block the snapshot already has.
/// `None`: the state reads empty (seen for a beat around a re-read), so nothing is concluded.
pub fn single_reading(sim: &Value) -> Option<(bool, &'static str, String)> {
    let raw = sim["state"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if raw.is_empty() {
        return None;
    }
    let present = !(raw.contains("absent") || raw.contains("no sim"));
    let app = match () {
        _ if !present => "none",
        _ if raw == "sim ready" => "ready",
        _ if raw.contains("pin") || raw.contains("puk") => "locked",
        _ => "detected",
    };
    Some((
        present,
        app,
        sim["iccid"].as_str().unwrap_or("").to_string(),
    ))
}

/// The `/v2` `cards` block.
pub fn block(t: &Table, source: &str, wired: bool, now: u64) -> crate::contract::Cards {
    crate::contract::Cards {
        source: source.into(),
        slots: t
            .rows
            .iter()
            .map(|r| crate::contract::CardSlot {
                slot: i64::from(r.slot),
                present: r.present,
                app: r.app.clone(),
                provisioned: r.provisioned,
                role: r.role.clone(),
                id: r.id_state(wired, now).into(),
                iccid_tail: if r.present {
                    tail(&r.iccid)
                } else {
                    String::new()
                },
                eid_tail: r.eid.as_deref().map(tail).unwrap_or_default(),
            })
            .collect(),
    }
}

// ---- the running table ----

const EVERY: u64 = 60;
const EVERY_DARK: u64 = 300;
const BACKLIGHT: &str = "/sys/class/leds/led:lcd/brightness";

static TABLE: Mutex<Table> = Mutex::new(Table { rows: Vec::new() });
/// The last projected `sim` block (roles, provisioning).
static SIM: Mutex<Option<Value>> = Mutex::new(None);
/// What the table was read from: uim (dual) | sim_info (single)
static SOURCE: Mutex<&str> = Mutex::new("");
static KICK: AtomicBool = AtomicBool::new(false);
/// The last read followed an indication: the next one comes `AFTER_EVENT` later.
static EVENT: AtomicBool = AtomicBool::new(false);
/// The firmware ICCID a read was last asked for because the table disagreed with it.
static DISAGREE: Mutex<String> = Mutex::new(String::new());
static POLLER: Once = Once::new();

/// Seconds since boot (0 when unreadable).
pub fn uptime_now() -> u64 {
    uptime()
}

fn uptime() -> u64 {
    std::fs::read_to_string(crate::state::host_path("/proc/uptime"))
        .ok()
        .and_then(|s| s.split('.').next()?.parse().ok())
        .unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Roles, identities and `cards.json` after a reading.
/// Called with no lock held: the EID reader is asked outside the table lock.
fn settle() {
    if let Some(src) = EID_SOURCE.get() {
        let ask = lock(&TABLE).unknown();
        let answers: Vec<_> = ask.into_iter().map(|(s, g)| (s, g, src(s, g))).collect();
        let mut t = lock(&TABLE);
        for (s, g, a) in answers {
            t.answer(s, g, a);
        }
    }
    let mut t = lock(&TABLE);
    let Some(sim) = lock(&SIM).clone() else {
        return; // roles need the sim block: the watch waits for it
    };
    let voice = crate::ops::dual_sim::voice_slot();
    t.roles(&sim, voice);
    let rows = t.rows.clone();
    drop(t);
    crate::card_watch::round(&rows, should_poll(&sim), voice, uptime());
}

/// Each snapshot round, with the projected `sim` block: the single-slot unit's row is read
/// here; a dual-slot unit starts the poller once.
pub fn offer(sim: Option<&Value>) {
    let Some(sim) = sim else { return };
    *lock(&SIM) = Some(sim.clone());
    if should_poll(sim) {
        POLLER.call_once(|| {
            *lock(&SOURCE) = "uim";
            if let Err(e) = std::thread::Builder::new()
                .name("cards".into())
                .spawn(poller)
            {
                eprintln!("[cards] poller: {e}");
            }
        });
        lock(&TABLE).roles(sim, crate::ops::dual_sim::voice_slot());
        return;
    }
    *lock(&SOURCE) = "sim_info";
    {
        let mut t = lock(&TABLE);
        if let Some((present, app, iccid)) = single_reading(sim) {
            t.observe(1, present, app, &iccid, uptime());
        } else if t.rows.is_empty() {
            return; // nothing known yet: no row rather than a wrong one
        }
        t.keep(1);
    }
    settle();
}

/// Read the cards now (after a profile operation).
pub fn kick() {
    KICK.store(true, Ordering::Relaxed);
}

/// The table now (the eSIM engine's card slots; T5 reads it too).
pub fn current() -> Table {
    lock(&TABLE).clone()
}

/// The `/v2` `cards` block; `None` before the first round.
pub fn report() -> Option<Value> {
    let src = *lock(&SOURCE);
    if src.is_empty() {
        return None;
    }
    let t = lock(&TABLE);
    if t.rows.is_empty() {
        return None; // dual-slot unit before its first good read
    }
    Some(crate::contract::value(&block(
        &t,
        src,
        EID_SOURCE.get().is_some(),
        uptime(),
    )))
}

/// One poll: `GET_CARD_STATUS` into the table (a failed read changes nothing).
fn poll_once() {
    let Some(body) = crate::qmi_nas::read_uim_status() else {
        return;
    };
    let Some(slots) = decode_card_status(&body) else {
        return;
    };
    let now = uptime();
    {
        let mut t = lock(&TABLE);
        for (i, s) in slots.iter().take(2).enumerate() {
            t.observe(i as u8 + 1, s.present, s.app, &s.iccid, now);
        }
        t.keep(2);
    }
    settle();
}

/// After an indication: read again this soon, so a pulled card (two absent reads, `card_watch`)
/// is told in seconds and a card that is still settling is read once it is ready.
const AFTER_EVENT: u64 = 10;
/// Re-register this often (a modem restart drops the registration silently).
const EVENT_SPAN: u64 = 600;

/// The UIM card status indications (dual-slot O-6, measured on u60g 11-17: a slot's power cycle
/// came as absent → detected → ready within 2 s): each one kicks the poller. Polling stays the
/// fallback; a refused registration is tried again a minute later.
fn events() {
    loop {
        let ok = crate::qmi_nas::watch_uim(EVENT_SPAN, |_| {
            EVENT.store(true, Ordering::Relaxed);
            KICK.store(true, Ordering::Relaxed);
            true
        });
        if ok.is_none() {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }
}

fn poller() {
    #[cfg(target_os = "linux")]
    if let Err(e) = std::thread::Builder::new().name("cards-ev".into()).spawn(events) {
        eprintln!("[cards] events: {e}");
    }
    loop {
        let evented = EVENT.swap(false, Ordering::Relaxed);
        poll_once();
        let dark = std::fs::read_to_string(crate::state::host_path(BACKLIGHT))
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .is_some_and(|b| b <= 0);
        let wait = if evented { AFTER_EVENT } else if dark { EVERY_DARK } else { EVERY };
        for _ in 0..wait {
            if KICK.swap(false, Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
#[path = "cards_tests.rs"]
mod tests;
