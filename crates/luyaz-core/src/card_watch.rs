//! What the card table's changes mean (docs/designs/dual-slot-removable.md §3 D2, §4 D3; T7).
//!
//! - **Another slot's card changed (D2).** Each slot's last settled reading (no card, or an
//!   identified card with its ICCID) is kept here and in `cards.json`. When a slot that does
//!   not carry data reads another one, `sim-standby-changed` goes to the inbox (never texted):
//!   a card in, out, another card, or the same eUICC on another profile. A data slot's change is
//!   the SIM identity tracking's (`sim-changed`, texted as before). A slot still waiting for its
//!   identity is not read (R11). The first reading after a start is compared with the file, so a
//!   swap while off is told too; with no file (or an older one) the readings are only recorded:
//!   seeding is not a change. Our own switch on a slot within 15 min says so.
//! - **The phone setting follows its card (D3).** `voice-card` next to `voice-slot` holds the
//!   card's id (`eid:` / `plain:`, so a profile switch on an eUICC keeps it). Each round on a
//!   dual-slot unit: no `voice-card` yet → the card in the voice slot now; that card in the
//!   other slot → the voice slot moves there (as if the owner set it; inbox); in neither slot →
//!   nothing moves, the inbox says so once a boot. Not while a call is up, an eSIM job runs, or a
//!   present card is still unidentified.

use crate::cards::{Row, iccid_eq, tail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

const FILE: &str = "/data/luyaz/core/cards.json";
const VOICE_CARD: &str = "/data/luyaz/core/voice-card";
/// Our own switch on a slot names itself this long.
const OWN_FOR: u64 = 900;

/// A slot's settled reading.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub slot: u8,
    pub present: bool,
    /// `eid:…` / `plain:…`; empty with no card
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub iccid: String,
}

#[derive(Serialize, Deserialize)]
struct File {
    v: u8,
    slots: Vec<Seen>,
}

/// A row's reading once it means something: no card, or a card identified with its ICCID.
pub fn settled(r: &Row) -> Option<Seen> {
    if !r.present {
        return Some(Seen {
            slot: r.slot,
            ..Seen::default()
        });
    }
    let id = r.card_id()?;
    (!r.iccid.is_empty()).then(|| Seen {
        slot: r.slot,
        present: true,
        id,
        iccid: r.iccid.clone(),
    })
}

/// The event's text, ASCII (the alert queue keeps ASCII only; luyaz-alerts' detail.rs says it
/// in Chinese); `None` = the same card.
pub fn change(old: &Seen, new: &Seen, own: bool) -> Option<String> {
    let what = match (old.present, new.present) {
        (false, false) => return None,
        (true, false) => "card out".to_string(),
        (false, true) => format!("card in (...{})", tail(&new.iccid)),
        // the same profile is the same card, whatever it was identified as
        (true, true) if iccid_eq(&old.iccid, &new.iccid) => return None,
        (true, true) if old.id == new.id => format!("profile now ...{}", tail(&new.iccid)),
        (true, true) => format!("another card (...{})", tail(&new.iccid)),
    };
    let by = if own {
        " (switched on the eSIM page)"
    } else {
        ""
    };
    Some(format!("slot {}: {what}{by}", new.slot))
}

/// The slots' last readings; `None` until the first round (then loaded from the file).
#[derive(Debug, Default)]
pub struct Watch {
    known: Option<BTreeMap<u8, Seen>>,
    /// slots read empty once where a card was: a second empty reading confirms it (a modem
    /// restart can read a card absent for a beat)
    out: std::collections::BTreeSet<u8>,
}

impl Watch {
    /// One round over the rows (roles already set): the events (slot, text), and whether the
    /// readings changed (the file is written then).
    pub fn step(
        &mut self,
        rows: &[Row],
        load: impl FnOnce() -> Option<Vec<Seen>>,
        own: impl Fn(u8) -> bool,
    ) -> (Vec<(u8, String)>, bool) {
        let known = self.known.get_or_insert_with(|| {
            load()
                .unwrap_or_default()
                .into_iter()
                .map(|s| (s.slot, s))
                .collect()
        });
        let (mut events, mut moved) = (Vec::new(), false);
        for r in rows {
            let Some(now) = settled(r) else { continue };
            let was_in = known.get(&r.slot).is_some_and(|o| o.present);
            if !now.present && was_in && self.out.insert(r.slot) {
                continue;
            }
            self.out.remove(&r.slot);
            match known.get(&r.slot) {
                Some(old) if *old == now => continue,
                Some(old) => {
                    if r.role != "data"
                        && let Some(t) = change(old, &now, own(r.slot))
                    {
                        events.push((r.slot, t));
                    }
                }
                None => {} // seeding
            }
            known.insert(r.slot, now);
            moved = true;
        }
        (events, moved)
    }

    pub fn text(&self) -> Option<String> {
        let slots: Vec<Seen> = self.known.as_ref()?.values().cloned().collect();
        if slots.is_empty() {
            return None;
        }
        serde_json::to_string(&File { v: 2, slots })
            .ok()
            .map(|s| s + "\n")
    }
}

/// `cards.json` as this version writes it; an older or unreadable one is no baseline.
pub fn parse(text: &str) -> Option<Vec<Seen>> {
    serde_json::from_str::<File>(text)
        .ok()
        .filter(|f| f.v == 2)
        .map(|f| f.slots)
}

/// What the phone setting does this round (D3).
#[derive(Debug, PartialEq, Eq)]
pub enum Voice {
    Nothing,
    /// no `voice-card` yet: the card in the voice slot now
    Seed(String),
    /// the card is in this other slot
    Move(u8),
    /// in neither slot; the voice slot stays
    Missing(u8),
}

/// Not into a slot the firmware has not provisioned (`off`): calls would still run on the
/// other one (`voice_sub`'s fallback) and the setting would point at a slot that takes none;
/// the card is not missing either, so nothing moves.
pub fn voice(rows: &[Row], voice_slot: u8, saved: Option<&str>) -> Voice {
    if rows.iter().any(|r| r.present && r.card_id().is_none()) {
        return Voice::Nothing; // a card still being identified
    }
    let Some(saved) = saved.map(str::trim).filter(|s| !s.is_empty()) else {
        return rows
            .iter()
            .find(|r| r.slot == voice_slot)
            .and_then(Row::card_id)
            .map_or(Voice::Nothing, Voice::Seed);
    };
    match rows.iter().find(|r| r.card_id().as_deref() == Some(saved)) {
        Some(r) if r.role == "off" => Voice::Nothing,
        Some(r) if r.slot != voice_slot => Voice::Move(r.slot),
        Some(_) => Voice::Nothing,
        None => Voice::Missing(voice_slot),
    }
}

// ---- running ----

static WATCH: Mutex<Watch> = Mutex::new(Watch {
    known: None,
    out: std::collections::BTreeSet::new(),
});
/// `voice-slot` and `voice-card` change together: the owner's setting and a round's follow.
static VOICE_LOCK: Mutex<()> = Mutex::new(());
/// The missing voice card seen last round and how many rounds in a row (said at 2, once).
static MISSING: Mutex<(String, u8)> = Mutex::new((String::new(), 0));
static OWN: Mutex<BTreeMap<u8, u64>> = Mutex::new(BTreeMap::new());
type Busy = Box<dyn Fn() -> bool + Send + Sync>;
static ESIM_BUSY: OnceLock<Busy> = OnceLock::new();

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// An eSIM job is running (the ICCID may flip mid-way): wired by the API at start.
pub fn set_esim_busy(f: Busy) {
    let _ = ESIM_BUSY.set(f);
}

/// An eSIM switch of ours is starting on `slot`.
pub fn own_switch(slot: u8, uptime: u64) {
    lock(&OWN).insert(slot, uptime);
}

/// After the table settled (roles set). `dual`: the card status poller's unit.
pub fn round(rows: &[Row], dual: bool, voice_slot: u8, uptime: u64) {
    let (events, text) = {
        let mut w = lock(&WATCH);
        let own = lock(&OWN).clone();
        let (ev, moved) = w.step(
            rows,
            || {
                let t = std::fs::read_to_string(crate::state::host_path(FILE)).ok()?;
                parse(&t)
            },
            |slot| {
                own.get(&slot)
                    .is_some_and(|t| uptime.saturating_sub(*t) <= OWN_FOR)
            },
        );
        (ev, if moved { w.text() } else { None })
    };
    for (slot, t) in events {
        crate::sim_alerts::note(&note_key(5, slot, uptime), "sim-standby-changed", &t);
    }
    if let Some(t) = text {
        let p = crate::state::host_path(FILE);
        if let Err(e) = luyaz_persist::write(
            std::path::Path::new(&p),
            t.as_bytes(),
            luyaz_persist::Tier::Cheap,
        ) {
            eprintln!("[cards] {FILE}: {e}");
        }
    }
    if dual {
        follow_voice(rows, voice_slot);
    }
}

fn follow_voice(rows: &[Row], voice_slot: u8) {
    if crate::at::in_call() || ESIM_BUSY.get().is_some_and(|b| b()) {
        return;
    }
    let _g = lock(&VOICE_LOCK);
    // the owner may have set the slot since this round read it: the next round goes by it
    if voice_slot != crate::ops::dual_sim::voice_slot() {
        return;
    }
    let path = crate::state::host_path(VOICE_CARD);
    let saved = std::fs::read_to_string(&path).ok();
    let v = voice(rows, voice_slot, saved.as_deref());
    let key = saved.as_deref().unwrap_or("").trim().to_string();
    let say_missing = {
        let mut m = lock(&MISSING);
        match v {
            Voice::Missing(_) if m.0 == key => m.1 = m.1.saturating_add(1),
            Voice::Missing(_) => *m = (key.clone(), 1),
            _ => *m = (String::new(), 0),
        }
        m.1 == 2
    };
    match v {
        Voice::Nothing => {}
        Voice::Seed(id) => write_voice_card(&path, &id),
        Voice::Move(slot) => match crate::ops::dual_sim::set_voice_slot(slot) {
            Ok(()) => {
                eprintln!("[cards] voice card moved: voice slot {voice_slot} -> {slot}");
                crate::sim_alerts::note(
                    &note_key(6, slot, crate::cards::uptime_now()),
                    "voice-card-followed",
                    &format!("voice card moved to slot {slot}"),
                );
            }
            Err(e) => eprintln!("[cards] voice slot {slot}: {e}"),
        },
        // two rounds in a row (a modem restart can read the card absent for a beat), once a boot
        Voice::Missing(slot) if say_missing => {
            let h = key
                .bytes()
                .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b.into()));
            crate::sim_alerts::note(
                &format!("7{:09}", h % 1_000_000_000),
                "voice-card-missing",
                &format!("voice card not in either slot; slot {slot} takes calls"),
            );
        }
        Voice::Missing(_) => {}
    }
}

/// An alert id's part after the boot id: digits only, at most 12 (`valid_event_id`); `code`
/// keeps them apart from `sim_alerts`' epoch ids (`epoch * 10 + kind`).
pub fn note_key(code: u8, slot: u8, uptime: u64) -> String {
    format!("{code}{slot}{}", uptime % 10_000_000_000)
}

fn write_voice_card(path: &str, id: &str) {
    if let Err(e) = luyaz_persist::write(
        std::path::Path::new(path),
        format!("{id}\n").as_bytes(),
        luyaz_persist::Tier::Cheap,
    ) {
        eprintln!("[cards] {VOICE_CARD}: {e}");
    }
}

/// The owner sets the voice slot (D3): the card in it now is the one to follow (none
/// identified yet: the old one is forgotten, a later round takes whatever is there).
pub fn set_voice(slot: u8) -> Result<(), String> {
    let _g = lock(&VOICE_LOCK);
    crate::ops::dual_sim::set_voice_slot(slot)?;
    let path = crate::state::host_path(VOICE_CARD);
    let id = crate::cards::current()
        .rows
        .iter()
        .find(|r| r.slot == slot)
        .and_then(Row::card_id);
    match id {
        Some(id) => write_voice_card(&path, &id),
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "card_watch_tests.rs"]
mod tests;
