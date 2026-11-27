use super::*;

const EUICC: &str = "eid:89049000000000000000000000001234";
const P1: &str = "89852300000000002222";
const P2: &str = "89852300000000005555";
const PLAIN: &str = "89860100000000001111";

fn row(slot: u8, card: Option<(&str, &str)>, role: &str) -> Row {
    let mut r = Row {
        slot,
        role: role.into(),
        ..Row::default()
    };
    if let Some((id, iccid)) = card {
        r.present = true;
        r.iccid = iccid.into();
        match id.strip_prefix("eid:") {
            Some(e) => r.eid = Some(e.into()),
            None => r.plain = true,
        }
    }
    r
}

fn plain() -> Option<(&'static str, &'static str)> {
    Some(("plain", PLAIN))
}

fn step(w: &mut Watch, rows: &[Row]) -> Vec<String> {
    w.step(rows, || None, |_| false)
        .0
        .into_iter()
        .map(|(_, t)| t)
        .collect()
}

#[test]
fn the_first_round_only_seeds() {
    let mut w = Watch::default();
    let rows = [row(1, plain(), "data"), row(2, Some((EUICC, P1)), "off")];
    let (ev, moved) = w.step(&rows, || None, |_| false);
    assert!(ev.is_empty());
    assert!(moved, "the file is written");
    assert_eq!(w.step(&rows, || None, |_| false), (vec![], false));
}

#[test]
fn a_standby_slot_change_is_told_and_the_data_slot_is_not() {
    let mut w = Watch::default();
    step(
        &mut w,
        &[row(1, plain(), "data"), row(2, Some((EUICC, P1)), "off")],
    );
    // the same eUICC on another profile
    assert_eq!(
        step(
            &mut w,
            &[row(1, plain(), "data"), row(2, Some((EUICC, P2)), "off")]
        ),
        ["slot 2: profile now ...5555"]
    );
    // one empty reading is not "out" yet (a modem restart can read a card absent for a beat)
    let out = [row(1, plain(), "data"), row(2, None, "off")];
    assert_eq!(w.step(&out, || None, |_| false), (vec![], false));
    assert_eq!(step(&mut w, &out), ["slot 2: card out"]);
    assert_eq!(
        step(
            &mut w,
            &[
                row(1, plain(), "data"),
                row(2, Some(("plain", P1)), "standby")
            ]
        ),
        ["slot 2: card in (...2222)"]
    );
    assert_eq!(
        step(
            &mut w,
            &[
                row(1, plain(), "data"),
                row(2, Some((EUICC, P2)), "standby")
            ]
        ),
        ["slot 2: another card (...5555)"]
    );
    // the data slot's change is sim_identity's, not this event
    assert!(
        step(
            &mut w,
            &[row(1, None, "data"), row(2, Some((EUICC, P2)), "standby")]
        )
        .is_empty()
    );
}

#[test]
fn a_card_back_after_one_empty_read_was_never_out() {
    let mut w = Watch::default();
    let both = [row(1, plain(), "data"), row(2, Some((EUICC, P1)), "off")];
    step(&mut w, &both);
    assert!(step(&mut w, &[row(1, plain(), "data"), row(2, None, "off")]).is_empty());
    assert_eq!(w.step(&both, || None, |_| false), (vec![], false));
    // and the next single empty read waits again
    assert!(step(&mut w, &[row(1, plain(), "data"), row(2, None, "off")]).is_empty());
}

#[test]
fn the_alert_ids_are_valid() {
    let k = note_key(5, 2, 1_234_567);
    assert!(
        luyaz_api::alerts::valid_event_id(&format!("0123abcd-{k}")),
        "{k}"
    );
    let k = note_key(6, 1, u64::MAX);
    assert!(
        luyaz_api::alerts::valid_event_id(&format!("0123abcd-{k}")),
        "{k}"
    );
    let card = Seen {
        slot: 2,
        present: true,
        id: EUICC.into(),
        iccid: P1.into(),
    };
    assert!(change(&Seen::default(), &card, true).unwrap().is_ascii());
}

#[test]
fn a_pending_card_is_not_read() {
    let mut w = Watch::default();
    step(
        &mut w,
        &[row(1, plain(), "data"), row(2, Some((EUICC, P1)), "off")],
    );
    let mut pending = row(2, Some((EUICC, P2)), "off");
    pending.eid = None;
    assert_eq!(
        w.step(&[row(1, plain(), "data"), pending], || None, |_| false),
        (vec![], false)
    );
}

#[test]
fn a_card_judged_again_with_the_same_profile_is_the_same_card() {
    let mut w = Watch::default();
    step(&mut w, &[row(2, Some(("plain", P1)), "off")]);
    let (ev, moved) = w.step(
        &[row(2, Some((EUICC, &format!("{P1}F"))), "off")],
        || None,
        |_| false,
    );
    assert!(ev.is_empty());
    assert!(moved, "the new id is kept");
}

#[test]
fn our_own_switch_says_so() {
    let mut w = Watch::default();
    step(&mut w, &[row(2, Some((EUICC, P1)), "off")]);
    let (ev, _) = w.step(&[row(2, Some((EUICC, P2)), "off")], || None, |s| s == 2);
    assert_eq!(
        ev,
        [(
            2,
            "slot 2: profile now ...5555 (switched on the eSIM page)".to_string()
        )]
    );
}

#[test]
fn a_swap_while_off_is_told_from_the_file() {
    let mut w = Watch::default();
    step(
        &mut w,
        &[row(1, plain(), "data"), row(2, Some((EUICC, P1)), "off")],
    );
    let file = w.text().unwrap();
    assert!(file.starts_with("{\"v\":2,"), "{file}");
    // next boot: another profile in slot 2 than the file says
    let mut next = Watch::default();
    let (ev, _) = next.step(
        &[row(1, plain(), "data"), row(2, Some((EUICC, P2)), "off")],
        || parse(&file),
        |_| false,
    );
    assert_eq!(ev, [(2, "slot 2: profile now ...5555".to_string())]);
    // a file from the version before (v1) is no baseline: seeding
    let v1 = r#"{"cards":[{"id":"eid:E2","slot":2}],"v":1}"#;
    assert_eq!(parse(v1), None);
    assert_eq!(parse("junk"), None);
}

#[test]
fn the_voice_setting_follows_its_card() {
    let a = row(1, plain(), "data");
    let b = row(2, Some((EUICC, P1)), "voice");
    let plain_id = format!("plain:{PLAIN}");
    // no voice-card yet: the voice slot's card
    assert_eq!(
        voice(&[a.clone(), b.clone()], 2, None),
        Voice::Seed(EUICC.into())
    );
    assert_eq!(
        voice(&[a.clone(), b.clone()], 2, Some("\n")),
        Voice::Seed(EUICC.into())
    );
    // it stays where it is
    assert_eq!(
        voice(&[a.clone(), b.clone()], 2, Some(EUICC)),
        Voice::Nothing
    );
    // the owner swapped the cards: the setting goes with the eUICC to slot 1
    let swapped = [row(1, Some((EUICC, P2)), "data"), row(2, plain(), "voice")];
    assert_eq!(
        voice(&swapped, 2, Some(&format!("{EUICC}\n"))),
        Voice::Move(1)
    );
    // not into a slot the firmware has not provisioned (calls would not follow)
    let to_off = [row(1, plain(), "data"), row(2, Some((EUICC, P1)), "off")];
    assert_eq!(voice(&to_off, 1, Some(EUICC)), Voice::Nothing);
    // gone from both slots: the slot stays, said once
    assert_eq!(
        voice(&[a.clone(), row(2, None, "voice")], 2, Some(EUICC)),
        Voice::Missing(2)
    );
    // a card still being identified: nothing this round
    let mut pending = row(2, Some((EUICC, P1)), "voice");
    pending.eid = None;
    assert_eq!(
        voice(&[a.clone(), pending], 2, Some(&plain_id)),
        Voice::Nothing
    );
    // an empty voice slot with nothing saved: nothing to seed
    assert_eq!(voice(&[a, row(2, None, "voice")], 2, None), Voice::Nothing);
}
