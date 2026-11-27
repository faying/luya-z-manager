//! Which eSIM cards /router/esim manages (dual-slot design §1, §6; R13/D15).
//!
//! The single-slot unit (no `cards` block, or `cards.source == "sim_info"`, and no `sim.dual`)
//! is one card addressed as before: nothing names the card on any request, today's element ids.
//! A dual-slot unit (`cards.source == "uim"`, or `sim.dual` while the core has not published its
//! first card table yet) gets one block per slot holding an eUICC or a card still being
//! identified; a slot with a plain SIM or no card is not shown. Requests name the card by its
//! slot (`?slot=` / `"slot"`): the core resolves the slot to the eUICC it identified there, so the
//! page never needs the full EID (the `cards` block only carries its tail).

use serde_json::Value;

use crate::hub::St;
use crate::i18n::Lang;

/// One slot the page shows on a dual-slot unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seat {
    pub n: u8,
    /// data | voice | standby | off
    pub role: String,
    /// eid | pending | unknown
    pub id: String,
    /// Another tail in the same slot = another card.
    pub eid_tail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Layout {
    /// The single-slot unit: one card, today's page.
    Single,
    /// A dual-slot unit: the slots shown (possibly none). `table` is false while the core has not
    /// published its card table yet (every card counts as being identified).
    Dual { seats: Vec<Seat>, table: bool },
}

impl Layout {
    /// The slot numbers the page is built for (0 = the single-slot unit's one card).
    pub fn ns(&self) -> Vec<u8> {
        match self {
            Layout::Single => vec![0],
            Layout::Dual { seats, .. } => seats.iter().map(|s| s.n).collect(),
        }
    }
    pub fn seat(&self, n: u8) -> Option<&Seat> {
        match self {
            Layout::Single => None,
            Layout::Dual { seats, .. } => seats.iter().find(|s| s.n == n),
        }
    }
}

/// The page's layout from the `cards` block (`sim.dual` marks a dual-slot unit before it).
pub fn parse(cards: Option<&Value>, sim_dual: bool) -> Layout {
    match cards {
        Some(c) if c["source"] == "uim" => {
            let seats = c["slots"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|s| s["present"] == true)
                .filter(|s| matches!(s["id"].as_str(), Some("eid" | "pending" | "unknown")))
                .filter_map(|s| {
                    Some(Seat {
                        n: u8::try_from(s["slot"].as_u64()?)
                            .ok()
                            .filter(|n| (1..=2).contains(n))?,
                        role: s["role"].as_str().unwrap_or("").to_owned(),
                        id: s["id"].as_str().unwrap_or("").to_owned(),
                        eid_tail: s["eid_tail"].as_str().unwrap_or("").to_owned(),
                    })
                })
                .collect();
            Layout::Dual { seats, table: true }
        }
        Some(c) if c["source"] == "sim_info" => Layout::Single,
        _ if sim_dual => Layout::Dual {
            seats: Vec::new(),
            table: false,
        },
        _ => Layout::Single,
    }
}

pub fn layout(st: &St) -> Layout {
    let sim_dual = st
        .blocks
        .get("sim")
        .is_some_and(|b| b.data["dual"].is_object());
    parse(st.blocks.get("cards").map(|b| &b.data), sim_dual)
}

/// The `cards` block as last seen (a change wakes the page's tabs).
pub fn mark(st: &St) -> Option<Value> {
    st.blocks.get("cards").map(|b| b.data.clone())
}

/// What a slot does now (`cards.slots[].role`).
pub fn role_word(role: &str, l: Lang) -> &'static str {
    match role {
        "data" => l.pick("In use for data", "上网中"),
        "voice" => l.pick("Calls only", "只打电话"),
        "standby" => l.pick("Standby", "待机"),
        _ => l.pick("Off", "未开启"),
    }
}

/// 「卡槽 N · 上网中」: the slot and what it does now.
pub fn heading(n: u8, role: &str, l: Lang) -> String {
    let what = role_word(role, l);
    match l {
        Lang::En => format!("Slot {n} · {what}"),
        Lang::Zh => format!("卡槽 {n} · {what}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn slot(n: u64, id: &str, role: &str, tail: &str) -> Value {
        json!({"slot": n, "present": id != "none", "app": "ready", "provisioned": true, "role": role,
               "id": id, "iccid_tail": "", "eid_tail": tail})
    }

    #[test]
    fn single_slot_units_keep_the_old_page() {
        assert_eq!(parse(None, false), Layout::Single);
        let c = json!({"source": "sim_info", "slots": [slot(1, "eid", "data", "1234")]});
        assert_eq!(parse(Some(&c), false), Layout::Single);
        assert_eq!(Layout::Single.ns(), vec![0]);
    }

    #[test]
    fn dual_slot_units_show_euicc_and_unidentified_slots_only() {
        let c = json!({"source": "uim", "slots": [slot(1, "plain", "data", ""), slot(2, "eid", "standby", "5678")]});
        let l = parse(Some(&c), true);
        assert_eq!(l.ns(), vec![2]);
        assert_eq!(l.seat(2).unwrap().role, "standby");
        let c = json!({"source": "uim", "slots": [slot(1, "eid", "data", "1234"), slot(2, "eid", "standby", "5678")]});
        assert_eq!(parse(Some(&c), true).ns(), vec![1, 2]);
        let c = json!({"source": "uim", "slots": [slot(1, "pending", "data", ""), slot(2, "none", "off", "")]});
        assert_eq!(parse(Some(&c), true).ns(), vec![1]);
        // no table yet on a dual-slot unit: nothing to address, the page says so
        assert_eq!(
            parse(None, true),
            Layout::Dual {
                seats: vec![],
                table: false
            }
        );
    }

    #[test]
    fn headings_name_the_slot_and_role() {
        assert_eq!(heading(1, "data", Lang::Zh), "卡槽 1 · 上网中");
        assert_eq!(heading(2, "standby", Lang::Zh), "卡槽 2 · 待机");
        assert_eq!(heading(2, "off", Lang::Zh), "卡槽 2 · 未开启");
        assert_eq!(heading(2, "voice", Lang::En), "Slot 2 · Calls only");
    }
}
