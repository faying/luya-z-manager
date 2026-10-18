use super::*;
use luyaz_api::diagnose::Level;
use luyaz_api::finding::{Confidence, Evidence, KIND_NO_INTERNET, LayerId};

const T: u64 = CLOCK_SET + 1_000_000;

fn finding(iccid: &str, layer: LayerId, conf: Confidence, detail: &str) -> Verdict {
    Verdict::Open(Finding {
        kind: KIND_NO_INTERNET,
        iccid: iccid.into(),
        layer,
        confidence: conf,
        symptom: "已连上，但外网不通".into(),
        symptom_en: "Connected, but the internet is unreachable".into(),
        evidence: vec![Evidence::new(layer, Level::Bad, detail, detail, T)],
        at: T,
    })
}

fn open(iccid: &str) -> Verdict {
    finding(
        iccid,
        LayerId::Internet,
        Confidence::Sure,
        "generate_204 失败",
    )
}

const UP: Verdict = Verdict::None { why: "all_ok" };

fn kinds(c: &[Change]) -> Vec<&'static str> {
    c.iter().map(|c| c.event().0).collect()
}

#[test]
fn opens_once_then_updates_only_when_it_moves() {
    let mut s = Store::default();
    assert_eq!(
        kinds(&s.observe(&open("8986001"), "8986001", T)),
        ["finding.open"]
    );
    // same answer: counted, no event
    assert!(s.observe(&open("8986001"), "8986001", T + 600).is_empty());
    let likely = finding("8986001", LayerId::Internet, Confidence::Likely, "x");
    assert_eq!(
        kinds(&s.observe(&likely, "8986001", T + 1200)),
        ["finding.update"]
    );
    assert_eq!(s.open.len(), 1);
    assert_eq!(
        (s.open[0].first, s.open[0].last, s.open[0].count),
        (T, T + 1200, 3)
    );
}

#[test]
fn resolves_after_two_up_checks_in_a_row() {
    let mut s = Store::default();
    s.observe(&open("1"), "1", T);
    assert!(s.observe(&UP, "1", T + 10).is_empty());
    // found again: the run of ups starts over
    s.observe(&open("1"), "1", T + 20);
    assert!(s.observe(&UP, "1", T + 30).is_empty());
    // can't tell does not count either way
    assert!(
        s.observe(&Verdict::Unverified { why: "not_probed" }, "1", T + 35)
            .is_empty()
    );
    let c = s.observe(&Verdict::None { why: "up" }, "1", T + 40);
    assert!(matches!(&c[..], [Change::Close(_, Close::Resolved)]));
    assert!(s.open.is_empty());
}

#[test]
fn expires_when_the_premise_goes_or_the_card_changes() {
    for why in ["expected", "not_registered"] {
        let mut s = Store::default();
        s.observe(&open("1"), "1", T);
        let c = s.observe(&Verdict::None { why }, "1", T + 10);
        assert!(
            matches!(&c[..], [Change::Close(_, Close::Expired)]),
            "{why}"
        );
    }
    let mut s = Store::default();
    s.observe(&open("1"), "1", T);
    let c = s.observe(&Verdict::Unverified { why: "premises" }, "2", T + 10);
    assert!(matches!(&c[..], [Change::Close(e, Close::Expired)] if e.iccid == "1"));
}

#[test]
fn dismiss_suppresses_the_key_for_a_day() {
    let mut s = Store::default();
    s.observe(&open("1"), "1", T);
    assert!(s.dismiss("other", "1", T).is_empty());
    let c = s.dismiss(KIND_NO_INTERNET, "1", T + 5);
    assert!(matches!(&c[..], [Change::Close(_, Close::Dismissed)]));
    assert!(s.observe(&open("1"), "1", T + 3600).is_empty());
    assert!(s.open.is_empty());
    // another card is another key
    assert_eq!(
        kinds(&s.observe(&open("2"), "2", T + 3600)),
        ["finding.open"]
    );
    // the day is over
    let mut s2 = s.clone();
    s2.open.clear();
    assert_eq!(
        kinds(&s2.observe(&open("1"), "1", T + 5 + SUPPRESS_SECS)),
        ["finding.open"]
    );
    assert!(s2.suppressed.is_empty());
}

#[test]
fn suppression_holds_while_the_clock_is_unset() {
    let mut s = Store::default();
    s.observe(&open("1"), "1", T);
    s.dismiss(KIND_NO_INTERNET, "1", T);
    // after a boot, before SNTP: epoch-ish time
    let early = 1_000;
    assert!(s.observe(&open("1"), "1", early).is_empty());
    assert_eq!(s.suppressed.len(), 1);
}

#[test]
fn round_trip_keeps_open_and_suppressed() {
    let mut s = Store::default();
    s.observe(&open("1"), "1", T);
    s.dismiss(KIND_NO_INTERNET, "1", T);
    s.observe(&open("2"), "2", T + 10);
    let text = serde_json::to_string(&s.to_value()).unwrap();
    let back = Store::from_value(&serde_json::from_str(&text).unwrap());
    assert_eq!(back, s);
    assert_eq!((back.open.len(), back.suppressed.len()), (1, 1));
    assert!(back.is_suppressed(KIND_NO_INTERNET, "1", T + 20));
    // junk or an older shape: empty, not a panic
    assert_eq!(Store::from_value(&json!({"open": 3})), Store::default());
    assert_eq!(Store::from_value(&Value::Null), Store::default());
}

#[test]
fn a_long_outage_does_not_grow() {
    let mut s = Store::default();
    let long = "很长的证据".repeat(400);
    let v = finding("1", LayerId::Internet, Confidence::Sure, &long);
    let first = s.observe(&v, "1", T);
    assert_eq!(kinds(&first), ["finding.open"]);
    for i in 1..=10_000u64 {
        s.observe(&v, "1", T + i * 600);
    }
    assert_eq!(s.open.len(), 1);
    assert_eq!(s.open[0].count, 10_001);
    assert!(size(&s.open[0].finding) <= ENTRY_MAX);
    assert!(size(&s.to_value()) <= MAX_ENTRIES * (ENTRY_MAX + 200));
}

#[test]
fn at_most_eight_entries_oldest_open_goes_first() {
    let mut s = Store::default();
    // 3 suppressions (each from a dismissed finding on card "s")
    for k in 0..3 {
        let kind: &'static str = ["a", "b", "c"][k];
        s.open.push(Entry {
            kind: kind.into(),
            iccid: "s".into(),
            first: T,
            last: T,
            count: 1,
            ups: 0,
            finding: Value::Null,
        });
        s.dismiss(kind, "s", T);
    }
    // 5 open on card "s" under other kinds
    for k in 0..5u64 {
        s.open.push(Entry {
            kind: format!("k{k}"),
            iccid: "s".into(),
            first: T + k,
            last: T + k,
            count: 1,
            ups: 0,
            finding: Value::Null,
        });
    }
    assert_eq!(s.open.len() + s.suppressed.len(), MAX_ENTRIES);
    let c = s.observe(&open("s"), "s", T + 100);
    assert_eq!(kinds(&c), ["finding.close", "finding.open"]);
    assert!(matches!(&c[0], Change::Close(e, Close::Evicted) if e.kind == "k0"));
    assert_eq!(s.open.len() + s.suppressed.len(), MAX_ENTRIES);
    assert_eq!(s.suppressed.len(), 3);
}

#[test]
fn events_carry_the_iccid_tail_only() {
    let mut s = Store::default();
    let c = s.observe(&open("89860012345678901234"), "89860012345678901234", T);
    let (kind, v) = c[0].event();
    assert_eq!(kind, "finding.open");
    assert_eq!(v["iccid_tail"], "1234");
    assert_eq!(v["layer"], "internet");
    assert_eq!(v["confidence"], "sure");
    assert!(!v.to_string().contains("8986001234"));
}

#[test]
fn no_card_reading_changes_nothing() {
    let mut s = Store::default();
    s.observe(&open("A"), "A", T);
    assert!(
        s.observe(&Verdict::Unverified { why: "premises" }, "", T + 10)
            .is_empty()
    );
    assert!(s.observe(&open(""), "", T + 20).is_empty());
    assert_eq!(s.open.len(), 1);
    assert_eq!(s.open[0].iccid, "A");
}
