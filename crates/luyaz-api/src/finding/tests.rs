use super::*;

const NOW: u64 = 1_000;

fn home() -> Premises {
    Premises {
        sim_present: Some(true),
        airplane: Some(false),
        data_on: Some(true),
        roaming: Some(false),
        roam_allowed: Some(false),
    }
}

fn up() -> DataIn {
    DataIn {
        connect_status: Some("ipv4_ipv6_connected".into()),
        fail_count: Some(0),
        has_ip: Some(true),
        pdp_active: Some(true),
        ceer: None,
    }
}

fn check(layers: Vec<Evidence>) -> Check {
    Check {
        premises: home(),
        passive_down: true,
        can_probe: true,
        iccid: "8986000000000000000".into(),
        layers,
        now: NOW,
    }
}

fn layers(data: DataIn, internet: Option<Probe>, dns: Option<Probe>) -> Vec<Evidence> {
    vec![
        judge_signal(Reg::Home, NOW),
        judge_data(&data, NOW),
        judge_internet(internet, NOW),
        judge_dns(dns, NOW),
    ]
}

fn open(v: Verdict) -> Finding {
    match v {
        Verdict::Open(f) => f,
        other => panic!("not open: {other:?}"),
    }
}

const OK: Option<Probe> = Some(Probe::Ok { ms: 40 });
const FAIL: Option<Probe> = Some(Probe::Failed);

#[test]
fn each_layer_first_bad_is_sure() {
    let down = DataIn {
        connect_status: Some("disconnected".into()),
        fail_count: Some(3),
        ..up()
    };
    let f = open(judge(&check(layers(down, None, None))));
    assert_eq!((f.layer, f.confidence), (LayerId::Data, Confidence::Sure));
    assert!(f.evidence[1].detail.contains("失败 3 次"));

    let f = open(judge(&check(layers(up(), FAIL, FAIL))));
    assert_eq!(
        (f.layer, f.confidence),
        (LayerId::Internet, Confidence::Sure)
    );

    let f = open(judge(&check(layers(up(), OK, FAIL))));
    assert_eq!((f.layer, f.confidence), (LayerId::Dns, Confidence::Sure));
    assert_eq!(f.kind, KIND_NO_INTERNET);
    assert_eq!(f.evidence.len(), 4);
}

#[test]
fn internet_ok_dns_bad_blames_dns() {
    // the internet probe does not need DNS, so a DNS fault stays a DNS fault
    let f = open(judge(&check(layers(up(), OK, FAIL))));
    assert_eq!(f.layer, LayerId::Dns);
    assert_eq!(f.symptom_en, "Internet reachable, but DNS is not resolving");
}

#[test]
fn na_before_the_bad_layer_is_likely() {
    let f = open(judge(&check(layers(up(), None, FAIL))));
    assert_eq!((f.layer, f.confidence), (LayerId::Dns, Confidence::Likely));
}

#[test]
fn stale_bad_reading_is_likely() {
    let mut l = layers(up(), OK, FAIL);
    l[3].at = NOW - FRESH_SECS - 1;
    assert_eq!(open(judge(&check(l))).confidence, Confidence::Likely);
    let mut l = layers(up(), OK, FAIL);
    l[3].at = NOW - FRESH_SECS;
    assert_eq!(open(judge(&check(l))).confidence, Confidence::Sure);
}

#[test]
fn expected_outages_do_not_open() {
    let cases = [
        (
            Premises {
                sim_present: Some(false),
                ..home()
            },
            Expected::NoSim,
        ),
        (
            Premises {
                airplane: Some(true),
                ..home()
            },
            Expected::Airplane,
        ),
        (
            Premises {
                data_on: Some(false),
                ..home()
            },
            Expected::DataOff,
        ),
        (
            Premises {
                roaming: Some(true),
                roam_allowed: Some(false),
                ..home()
            },
            Expected::RoamingOff,
        ),
    ];
    for (p, want) in cases {
        assert_eq!(expected_outage(&p), Ok(Some(want)));
        let mut c = check(layers(up(), FAIL, FAIL));
        c.premises = p;
        assert_eq!(judge(&c), Verdict::None { why: "expected" }, "{want:?}");
    }
    // roaming with roaming allowed is not expected
    let p = Premises {
        roaming: Some(true),
        roam_allowed: Some(true),
        ..home()
    };
    assert_eq!(expected_outage(&p), Ok(None));
}

#[test]
fn unreadable_premise_is_unverified_not_open() {
    let mut c = check(layers(up(), FAIL, FAIL));
    c.premises.data_on = None;
    assert_eq!(judge(&c), Verdict::Unverified { why: "premises" });
    // roam_allowed only matters while roaming
    let p = Premises {
        roam_allowed: None,
        ..home()
    };
    assert_eq!(expected_outage(&p), Ok(None));
}

#[test]
fn no_probe_is_unverified() {
    let mut c = check(layers(up(), None, None));
    c.can_probe = false;
    assert_eq!(judge(&c), Verdict::Unverified { why: "not_probed" });
}

#[test]
fn all_ok_or_not_down_does_not_open() {
    assert_eq!(
        judge(&check(layers(up(), OK, OK))),
        Verdict::None { why: "all_ok" }
    );
    let mut c = check(layers(up(), FAIL, FAIL));
    c.passive_down = false;
    assert_eq!(judge(&c), Verdict::None { why: "up" });
    // nothing bad but something na: not proven
    assert_eq!(
        judge(&check(layers(up(), OK, None))),
        Verdict::Unverified {
            why: "inconclusive"
        }
    );
}

#[test]
fn registration_states() {
    assert_eq!(Reg::from_stat(3), Reg::Denied);
    assert_eq!(Reg::from_stat(2), Reg::Searching);
    assert_eq!(Reg::from_stat(1), Reg::Home);
    assert_eq!(Reg::from_stat(5), Reg::Roaming);
    assert_eq!(Reg::from_stat(0), Reg::NotRegistered);
    assert_eq!(Reg::from_stat(4), Reg::Unknown);
    assert!(Reg::Roaming.registered() && !Reg::Denied.registered());
    for (reg, level) in [
        (Reg::Denied, Level::Bad),
        (Reg::Searching, Level::Bad),
        (Reg::Home, Level::Ok),
        (Reg::Roaming, Level::Ok),
        (Reg::Unknown, Level::Na),
    ] {
        assert_eq!(judge_signal(reg, NOW).level, level, "{reg:?}");
    }
    // not registered is another story; unknown can't tell
    let mut l = layers(up(), FAIL, FAIL);
    l[0] = judge_signal(Reg::Denied, NOW);
    assert_eq!(
        judge(&check(l.clone())),
        Verdict::None {
            why: "not_registered"
        }
    );
    l[0] = judge_signal(Reg::Unknown, NOW);
    assert_eq!(
        judge(&check(l)),
        Verdict::Unverified {
            why: "no_registration"
        }
    );
}

#[test]
fn data_layer_readings() {
    assert_eq!(judge_data(&up(), NOW).level, Level::Ok);
    assert_eq!(judge_data(&DataIn::default(), NOW).level, Level::Na);
    let no_ip = DataIn {
        has_ip: Some(false),
        ..up()
    };
    assert_eq!(judge_data(&no_ip, NOW).level, Level::Bad);
    let pdp_off = DataIn {
        pdp_active: Some(false),
        ..up()
    };
    assert_eq!(judge_data(&pdp_off, NOW).level, Level::Bad);
    let quiet = DataIn {
        connect_status: Some("disconnected".into()),
        ceer: Some("No cause information available".into()),
        ..up()
    };
    assert!(!judge_data(&quiet, NOW).detail.contains('（'));
    let cause = DataIn {
        ceer: Some("PDN rejected".into()),
        ..quiet
    };
    assert!(judge_data(&cause, NOW).detail.contains("PDN rejected"));
}

#[test]
fn missing_layers_count_as_na() {
    let c = check(vec![judge_signal(Reg::Home, NOW), judge_dns(FAIL, NOW)]);
    let f = open(judge(&c));
    assert_eq!((f.layer, f.confidence), (LayerId::Dns, Confidence::Likely));
    assert_eq!(f.evidence[1].level, Level::Na);
}

#[test]
fn a_typical_finding_stays_small() {
    let down = DataIn {
        connect_status: Some("disconnected".into()),
        fail_count: Some(12),
        ceer: Some("Requested service option not subscribed".into()),
        ..up()
    };
    let f = open(judge(&check(layers(down, None, None))));
    let n = serde_json::to_string(&Verdict::Open(f)).unwrap().len();
    assert!(n < 2048, "{n} bytes");
}
