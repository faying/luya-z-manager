use super::*;
use serde_json::json;

const CARD: &str = "89860100000000001111";

fn hex(s: &str) -> Vec<u8> {
    let s = s.trim();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
/// u60g 11-16 (identifiers replaced): slot 1 a plain card in use, slot 2 an eUICC not provisioned
fn u60g() -> Vec<u8> {
    hex(include_str!("../tests/fixtures/u60g-uim-card-status.hex"))
}
/// the main device 11-16 (identifiers replaced): slot 1 in use, slot 2 empty
fn main_device() -> Vec<u8> {
    hex(include_str!("../tests/fixtures/main-uim-card-status.hex"))
}

#[test]
fn iccid_norm_and_eq() {
    assert_eq!(
        iccid_norm(" 8986010000000000111F \n"),
        "8986010000000000111"
    );
    assert_eq!(iccid_norm("8986010000000000111f"), "8986010000000000111");
    assert_eq!(iccid_norm("89852a"), "89852A");
    assert!(iccid_eq(CARD, &format!("{CARD}F")));
    assert!(!iccid_eq("", ""));
    assert!(!iccid_eq("F", "f"));
    assert!(!iccid_eq(CARD, "89860100000000002222"));
    assert_eq!(tail(&format!("{CARD}F")), "1111");
    assert_eq!(tail("12"), "12");
}

/// R5: the copies moved here (sim_identity, api/ops, ops/agent: trim + drop the F; one more that
/// also upper-cased, whose output is already stored on devices) give the same text for an ICCID
/// of digits, so stored keys keep matching.
#[test]
fn the_moved_copies_read_digits_as_before() {
    let old = |s: &str| s.trim().trim_end_matches(['F', 'f']).to_string();
    let old_line = |s: &str| s.trim().trim_end_matches(['F', 'f']).to_ascii_uppercase();
    for s in [
        CARD,
        " 89860100000000001111 ",
        "8986010000000000111F",
        "8986010000000000111ff",
        "",
        "  ",
        "898523000000000022220",
    ] {
        assert_eq!(iccid_norm(s), old(s), "{s:?}");
        assert_eq!(iccid_norm(s), old_line(s), "{s:?}");
    }
    assert_eq!(crate::ops::agent::norm_iccid(" 8986F"), "8986");
    assert_eq!(
        crate::sim_identity::Identity::new("8986F ", "460").iccid,
        "8986"
    );
}

#[test]
fn the_dual_slot_card_status_reads_both_slots() {
    let s = decode_card_status(&u60g()).unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(
        s[0],
        UimSlot {
            present: true,
            app: "ready",
            iccid: CARD.into()
        }
    );
    // not provisioned: the card is there, its USIM only detected; TLV 0x17 still has its ICCID
    assert_eq!(
        s[1],
        UimSlot {
            present: true,
            app: "detected",
            iccid: "89852300000000002222".into()
        }
    );
}

#[test]
fn the_single_slot_card_status_has_an_empty_second_slot() {
    let s = decode_card_status(&main_device()).unwrap();
    assert_eq!(
        s[0],
        UimSlot {
            present: true,
            app: "ready",
            iccid: "89860100000000003333".into()
        }
    );
    assert_eq!(
        s[1],
        UimSlot {
            present: false,
            app: "none",
            iccid: String::new()
        }
    );
}

#[test]
fn a_cut_reply_is_not_read_as_cards() {
    let b = u60g();
    assert_eq!(decode_card_status(&[]), None);
    // the card status TLV cut inside slot 1's app: no slots rather than a wrong one
    let mut cut = b.clone();
    let at = (0..cut.len())
        .find(|&i| cut[i] == 0x10 && cut.get(i + 1) == Some(&0x51))
        .unwrap();
    cut[at + 1] = 20;
    assert_eq!(decode_card_status(&cut[..at + 3 + 20]), None);
    // no TLV 0x17: slots without ICCIDs
    let no_ids = b.clone();
    let i17 = (0..no_ids.len())
        .find(|&i| no_ids[i] == 0x17 && no_ids.get(i + 1) == Some(&0x48))
        .unwrap();
    let mut v = no_ids[..i17].to_vec();
    v.extend_from_slice(&no_ids[i17 + 3 + 0x48..]);
    let s = decode_card_status(&v).unwrap();
    assert!(s.iter().all(|x| x.iccid.is_empty()) && s[0].present);
}

#[test]
fn the_generation_moves_only_for_another_card() {
    let mut t = Table::default();
    t.observe(2, false, "none", "", 10);
    assert_eq!(t.rows[0].generation, 0);
    t.observe(2, true, "detected", CARD, 20);
    let r = t.rows[0].clone();
    assert_eq!(
        (r.generation, r.pending_since, r.iccid.as_str()),
        (1, Some(20), CARD)
    );
    t.rows[0].eid = Some("E1".into());
    // the same card, and an empty or failed read: nothing changes
    t.observe(2, true, "detected", &format!("{CARD}F"), 30);
    t.observe(2, true, "detected", "", 40);
    assert_eq!(
        (
            t.rows[0].generation,
            t.rows[0].iccid.as_str(),
            t.rows[0].eid.as_deref()
        ),
        (1, CARD, Some("E1"))
    );
    // another ICCID (another card, or another profile on the eUICC): read the EID again
    t.observe(2, true, "detected", "89852300000000002222", 50);
    assert_eq!(
        (
            t.rows[0].generation,
            t.rows[0].eid.as_deref(),
            t.rows[0].pending_since
        ),
        (2, None, Some(50))
    );
    // pulled out and put back: another generation even with the same ICCID
    t.observe(2, false, "none", "", 60);
    assert!(!t.rows[0].present && t.rows[0].iccid.is_empty());
    t.observe(2, true, "detected", "89852300000000002222", 70);
    assert_eq!(t.rows[0].generation, 3);
}

#[test]
fn the_card_id_is_eid_plain_or_pending() {
    let mut t = Table::default();
    t.observe(1, true, "ready", CARD, 0);
    t.observe(2, true, "detected", "89852300000000002222", 0);
    t.resolve(None);
    assert!(t.rows.iter().all(|r| r.card_id().is_none()));
    // no reader wired: pending however long, never "unknown"
    assert_eq!(t.rows[0].id_state(false, 10_000), "pending");
    let src: EidSource = Box::new(|slot, generation| match (slot, generation) {
        (1, 1) => Eid::NotEuicc,
        (2, 1) => Eid::Found("89049000000000000000000000001234".into()),
        _ => Eid::Later,
    });
    t.resolve(Some(&src));
    assert_eq!(
        t.rows[0].card_id().as_deref(),
        Some("plain:89860100000000001111")
    );
    assert_eq!(
        t.rows[1].card_id().as_deref(),
        Some("eid:89049000000000000000000000001234")
    );
    assert_eq!(
        (t.rows[0].id_state(true, 0), t.rows[1].id_state(true, 0)),
        ("plain", "eid")
    );
    // a profile switch on the eUICC: pending again until the reader answers for the new generation
    t.observe(2, true, "detected", "89852300000000005555", 100);
    t.resolve(Some(&src));
    assert_eq!(t.rows[1].card_id(), None);
    assert_eq!(t.rows[1].id_state(true, 100 + PENDING_MAX - 1), "pending");
    assert_eq!(t.rows[1].id_state(true, 100 + PENDING_MAX), "unknown");
    t.observe(1, false, "none", "", 0);
    assert_eq!(
        (t.rows[0].card_id(), t.rows[0].id_state(true, 0)),
        (None, "none")
    );
}

#[test]
fn an_answer_for_a_card_that_changed_meanwhile_is_dropped() {
    let mut t = Table::default();
    t.observe(2, true, "detected", CARD, 0);
    let asked = t.unknown();
    assert_eq!(asked, vec![(2, 1)]);
    // while the reader was busy (outside the lock) the card became another one
    t.observe(2, true, "detected", "89852300000000002222", 5);
    t.answer(2, 1, Eid::Found("E-OLD".into()));
    assert_eq!(t.rows[0].eid, None);
    t.answer(2, 2, Eid::Found("e-new ".into()));
    assert_eq!(t.rows[0].eid.as_deref(), Some("E-NEW"));
    assert!(t.unknown().is_empty());
}

#[test]
fn roles_follow_provisioning_data_and_voice() {
    let mut t = Table::default();
    t.observe(1, true, "ready", CARD, 0);
    t.observe(2, true, "detected", "89852300000000002222", 0);
    let single_mode =
        json!({"dual_sim": 1, "current_slot": 1, "sim1_provision": 1, "sim2_provision": 0});
    t.roles(&single_mode, 1);
    assert_eq!(
        (t.rows[0].role.as_str(), t.rows[1].role.as_str()),
        ("data", "off")
    );
    assert!(!t.rows[1].provisioned);
    let both = json!({"dual_sim": 1, "current_slot": 1, "sim1_provision": 1, "sim2_provision": 1});
    t.roles(&both, 2);
    assert_eq!(
        (t.rows[0].role.as_str(), t.rows[1].role.as_str()),
        ("data", "voice")
    );
    t.roles(&both, 1);
    assert_eq!(t.rows[1].role, "standby");
    let on2 = json!({"dual_sim": 1, "current_slot": 2, "sim1_provision": 1, "sim2_provision": 1});
    t.roles(&on2, 1);
    assert_eq!(
        (t.rows[0].role.as_str(), t.rows[1].role.as_str()),
        ("voice", "data")
    );
}

/// R6/D8 ③ (T6 handed over): the single-slot unit never starts the card status poller, and its
/// one row comes from the sim block it already reads.
#[test]
fn the_main_device_does_not_poll_and_gets_one_row() {
    let sim: Value =
        serde_json::from_str(include_str!("../tests/fixtures/main-v2-sim.golden.json")).unwrap();
    assert!(!should_poll(&sim));
    let (present, app, iccid) = single_reading(&sim).unwrap();
    assert_eq!((present, app), (true, "ready"));
    let mut t = Table::default();
    t.observe(1, present, app, &iccid, 0);
    t.keep(1);
    t.roles(&sim, 1);
    assert_eq!(t.rows.len(), 1);
    assert_eq!(
        (t.rows[0].role.as_str(), t.rows[0].provisioned),
        ("data", true)
    );
    assert!(iccid_eq(&t.rows[0].iccid, "89860100000000001111"));
    assert!(should_poll(&json!({"dual_sim": 1})));
    for state in ["sim absent", "no sim"] {
        assert!(
            !single_reading(&json!({"state": state})).unwrap().0,
            "{state:?}"
        );
    }
    assert_eq!(
        single_reading(&json!({"state": "sim pin"})).unwrap().1,
        "locked"
    );
    // an empty state (a beat around a re-read) concludes nothing: not "no card"
    assert_eq!(single_reading(&json!({"state": ""})), None);
    assert_eq!(single_reading(&json!({"state": " ", "iccid": ""})), None);
}

#[test]
fn the_block_carries_tails_only() {
    let mut t = Table::default();
    t.observe(1, true, "ready", CARD, 0);
    t.observe(2, false, "none", "", 0);
    t.rows[0].eid = Some("89049000000000000000000000004321".into());
    t.roles(
        &json!({"dual_sim": 1, "current_slot": 1, "sim1_provision": 1, "sim2_provision": 0}),
        1,
    );
    let b = crate::contract::value(&block(&t, "uim", false, 0));
    let text = b.to_string();
    assert!(!text.contains(CARD) && !text.contains("8904900"));
    assert_eq!(b["source"], "uim");
    assert_eq!(b["slots"][0]["iccid_tail"], "1111");
    assert_eq!(b["slots"][0]["eid_tail"], "4321");
    assert_eq!(b["slots"][0]["id"], "eid");
    assert_eq!(
        b["slots"][1],
        json!({"slot": 2, "present": false, "app": "none", "provisioned": false, "role": "off", "id": "none", "iccid_tail": "", "eid_tail": ""})
    );
}

/// T5 (R9/D11): the firmware's reading stays the source (it moves first, the table lags);
/// only slot 1's fields standing in for slot 2's card are overridden.
#[test]
fn the_identity_is_the_data_slots_card() {
    const OTHER: &str = "89852300000000002222";
    let mut t = Table::default();
    t.observe(1, true, "ready", CARD, 0);
    t.observe(2, true, "ready", OTHER, 0);
    let on = |slot: i64| json!({"dual_sim": 1, "current_slot": slot});
    let fw = |iccid: &str, imsi: &str| Identity::new(iccid, imsi);
    // agreeing, or the ICCID empty for a beat: the firmware's, no extra read
    assert_eq!(
        t.data_identity(&on(2), fw(&format!("{OTHER}F"), "45407"), None),
        (fw(OTHER, "45407"), false)
    );
    assert_eq!(
        t.data_identity(&on(2), fw("", "45407"), None),
        (fw("", "45407"), false)
    );
    // data on slot 2, `sim2_iccid` empty: the fields are slot 1's card, not a change; the
    // card already tracked stays, one never seen is no identity yet
    let known = fw(OTHER, "45407");
    assert_eq!(
        t.data_identity(&on(2), fw(CARD, "46001"), Some(&known)),
        (known.clone(), false)
    );
    assert_eq!(
        t.data_identity(&on(2), fw(CARD, "46001"), None),
        (Identity::default(), false)
    );
    // data on slot 1 reading slot 1's card is just that
    assert_eq!(
        t.data_identity(&on(1), fw(CARD, "46001"), Some(&known)),
        (fw(CARD, "46001"), false)
    );
    // a card the table has not seen yet: the firmware's, and the table reads now
    assert_eq!(
        t.data_identity(&on(2), fw("89860300000000003333", "46011"), Some(&known)),
        (fw("89860300000000003333", "46011"), true)
    );
    t.observe(2, false, "", "", 1);
    assert_eq!(
        t.data_identity(&on(2), fw(OTHER, "45407"), None),
        (fw(OTHER, "45407"), true)
    );
    // no data slot reported: the firmware's
    assert_eq!(
        t.data_identity(&json!({"dual_sim": 1}), fw(CARD, "46001"), None),
        (fw(CARD, "46001"), false)
    );
    // single-slot unit: the firmware's reading, untouched (R6/D8)
    assert_eq!(
        data_identity(
            &json!({"dual_sim": 0, "current_slot": 1}),
            fw(CARD, "46001"),
            None
        ),
        fw(CARD, "46001")
    );
}

struct Track {
    s: crate::sim_identity::SimIdentity,
    t: Table,
    t0: std::time::Instant,
}

impl Track {
    fn new() -> Self {
        Track {
            s: Default::default(),
            t: Table::default(),
            t0: std::time::Instant::now(),
        }
    }
    fn at(&self, n: u64) -> std::time::Instant {
        self.t0 + std::time::Duration::from_secs(n)
    }
    /// one snapshot round: the firmware's data-card fields with data on `slot`
    fn see(
        &mut self,
        slot: i64,
        iccid: &str,
        imsi: &str,
        bearer: bool,
        n: u64,
    ) -> (crate::sim_identity::Phase, u64) {
        let sim = json!({"dual_sim": 1, "current_slot": slot});
        let (id, _) = self
            .t
            .data_identity(&sim, Identity::new(iccid, imsi), self.s.known());
        self.s.observe(id, bearer, self.at(n));
        (self.s.phase(), self.s.epoch())
    }
}

/// T5 verify, in the order a device produces (the firmware moves first, the table lags):
/// the data card moving to the other slot is no card change, a missing `sim2_iccid` is no card
/// change, another card in the data slot is one at once.
#[test]
fn moving_slots_is_no_sim_change() {
    use crate::sim_identity::Phase;
    const PLAIN: &str = "89852300000000002222";
    let mut k = Track::new();
    k.t.observe(1, true, "ready", CARD, 0);
    assert_eq!(k.see(1, CARD, "46001", true, 0), (Phase::Online, 0));
    // the owner moves the card to slot 2: the firmware has it there before the table does
    assert_eq!(k.see(2, CARD, "46001", true, 1), (Phase::Online, 0));
    k.t.observe(1, false, "", "", 2);
    k.t.observe(2, true, "ready", CARD, 2);
    assert_eq!(k.see(2, CARD, "46001", true, 2), (Phase::Online, 0));
    // a plain card goes in slot 1; `sim2_iccid` reads empty, so the fields are slot 1's
    k.t.observe(1, true, "ready", PLAIN, 3);
    for n in 3..6 {
        assert_eq!(k.see(2, PLAIN, "45407", true, n), (Phase::Online, 0));
    }
    // another card in the data slot: a change in the same round, before the table reads it
    assert_eq!(
        k.see(2, "89860300000000003333", "46011", false, 10),
        (Phase::Changed, 1)
    );
    k.t.observe(2, true, "ready", "89860300000000003333", 70);
    assert_eq!(
        k.see(2, "89860300000000003333", "46011", true, 70),
        (Phase::Online, 1)
    );
}

/// T5 verify: an eSIM switch on the data slot's eUICC, the table catching up after the job.
#[test]
fn a_data_slot_switch_is_one_epoch() {
    use crate::sim_identity::Phase;
    const NEW: &str = "89852300000000005555";
    let mut k = Track::new();
    k.t.observe(1, true, "ready", "89852300000000002222", 0);
    k.t.observe(2, true, "ready", CARD, 0);
    assert_eq!(k.see(2, CARD, "46001", true, 0), (Phase::Online, 0));
    k.s.intent(k.at(1));
    assert_eq!(k.see(2, "", "", false, 2), (Phase::Leaving, 1));
    assert_eq!(k.see(2, NEW, "45407", false, 5), (Phase::Changed, 1));
    k.s.job_ended(k.at(6));
    assert_eq!(k.see(2, NEW, "45407", true, 7), (Phase::Online, 1));
    k.t.observe(2, true, "ready", NEW, 60);
    assert_eq!(k.see(2, NEW, "45407", true, 60), (Phase::Online, 1));
}
