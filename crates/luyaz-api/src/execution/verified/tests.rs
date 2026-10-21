use super::*;
use serde_json::json;

fn ep(conn: &str) -> Epochs {
    Epochs {
        sim: Some("s1".into()),
        conn: Some(conn.into()),
        radio: Some("r1".into()),
    }
}

fn pass() -> Recheck {
    Recheck {
        layers: vec![("dns".into(), Check::Pass), ("exit".into(), Check::Pass)],
        valid: true,
    }
}

fn proof<'a>(r: &'a Recheck, then: &'a Epochs, now: &'a Epochs) -> Proof<'a> {
    Proof {
        txn: Txn::None,
        recheck: r,
        then,
        now,
        iccid_then: Some("8986"),
        iccid_now: Some("8986"),
        mcc: Some(460),
    }
}

#[test]
fn records_only_a_clean_pass() {
    let (r, a) = (pass(), ep("c1"));
    assert_eq!(refuse_record(&proof(&r, &a, &a)), None);
    let p = Proof {
        txn: Txn::Final(SETTLE_MS),
        ..proof(&r, &a, &a)
    };
    assert_eq!(refuse_record(&p), None);
    let cases: Vec<(Proof, &str)> = vec![
        (Proof { txn: Txn::Active, ..proof(&r, &a, &a) }, "txn_active"),
        (Proof { txn: Txn::Final(SETTLE_MS - 1), ..proof(&r, &a, &a) }, "settling"),
        (Proof { iccid_now: Some("8987"), ..proof(&r, &a, &a) }, "sim_changed"),
        (Proof { iccid_then: None, ..proof(&r, &a, &a) }, "no_sim"),
        (Proof { mcc: None, ..proof(&r, &a, &a) }, "no_country"),
    ];
    for (p, why) in cases {
        assert_eq!(refuse_record(&p), Some(why));
    }
}

#[test]
fn a_failed_unknown_or_moved_check_is_not_a_pass() {
    let a = ep("c1");
    let b = ep("c2");
    let mut r = pass();
    assert_eq!(refuse_record(&proof(&r, &a, &b)), Some("connection_changed"));
    r.layers[1].1 = Check::Unknown;
    assert_eq!(refuse_record(&proof(&r, &a, &a)), Some("check_not_passed"));
    let r = Recheck { valid: false, ..pass() };
    assert_eq!(refuse_record(&proof(&r, &a, &a)), Some("check_not_passed"));
    let r = Recheck { layers: vec![], valid: true };
    assert_eq!(refuse_record(&proof(&r, &a, &a)), Some("check_not_passed"));
}

fn items(mode: &str) -> BTreeMap<String, Value> {
    [
        ("cell.lte", json!({"off": true})),
        ("band.lte", json!("1,3,8")),
        ("network.mode", json!(mode)),
        ("apn", json!({"mode": 1, "manual": "manu1", "fp": "ab"})),
        ("wifi.main", json!("ignored")),
        ("cell.nr", Value::Null),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect()
}

#[test]
fn next_keeps_known_items_and_the_one_before() {
    let s1 = next(None, 100, 460, "c1", &items("WL_AND_5G")).unwrap();
    assert_eq!(s1.items.len(), 4, "unknown and unread items are left out");
    assert!(s1.prev.is_none());
    let s2 = next(Some(&s1), 200, 460, "c2", &items("Only_5G")).unwrap();
    assert_eq!(s2.prev.as_ref().unwrap().at, 100);
    // the same settings again keep the older prev
    let s3 = next(Some(&s2), 300, 460, "c3", &items("Only_5G")).unwrap();
    assert_eq!(s3.prev, s2.prev);
    assert!(next(None, 1, 460, "c", &BTreeMap::new()).is_none());
    let back: Set = serde_json::from_value(serde_json::to_value(&s2).unwrap()).unwrap();
    assert_eq!(back, s2);
}

#[test]
fn diff_in_restore_order_and_apn_by_fingerprint() {
    let set = next(None, 1, 460, "c", &items("WL_AND_5G")).unwrap();
    let mut now = items("Only_5G");
    now.insert("apn".into(), json!({"mode": 1, "manual": "manu3", "fp": "ab"}));
    now.insert("cell.lte".into(), json!({"pci": "1", "earfcn": "100"}));
    let steps = diff(&set, &now);
    let order: Vec<_> = steps.iter().map(|s| s.action).collect();
    assert_eq!(order, ["cell.lock_lte", "network.set_mode"], "same fp on another profile number is the same");
    now.remove("band.lte");
    let steps = diff(&set, &now);
    assert_eq!(steps[1].item, "band.lte");
    assert_eq!(steps[1].now, Value::Null);
}

#[test]
fn apn_target_finds_the_profile_by_fingerprint() {
    let list = vec![("manu2".to_owned(), "zz".to_owned()), ("manu5".to_owned(), "ab".to_owned())];
    let to = json!({"mode": 1, "manual": "manu1", "fp": "ab"});
    assert_eq!(apn_target(&to, &list).unwrap()["manual"], "manu5");
    assert_eq!(apn_target(&json!({"mode": 1, "manual": "manu1", "fp": "gone"}), &list), None);
    assert_eq!(apn_target(&json!({"mode": 0, "manual": "", "fp": ""}), &[]).unwrap()["mode"], 0);
}

#[test]
fn restore_refusals_in_order() {
    let set = next(None, 1, 460, "c", &items("WL_AND_5G")).unwrap();
    let steps = diff(&set, &items("Only_5G"));
    assert_eq!(refuse_restore(None, Some(&set), false, &steps), Some("no_country"));
    assert_eq!(refuse_restore(Some(460), None, false, &steps), Some("no_record"));
    assert_eq!(refuse_restore(Some(460), Some(&set), true, &steps), Some("txn_active"));
    assert_eq!(refuse_restore(Some(460), Some(&set), false, &[]), Some("already_there"));
    assert_eq!(refuse_restore(Some(460), Some(&set), false, &steps), None);
    for item in ITEMS {
        assert!(action_of(item).is_some());
    }
}

#[test]
fn unlocks_first_and_locks_after_bands_and_mode() {
    let mut want = items("WL_AND_5G");
    want.insert("cell.nr".into(), json!({"pci": "5", "arfcn": "627264", "band": "78"}));
    let set = next(None, 1, 460, "c", &want).unwrap();
    let mut now = items("Only_5G");
    now.insert("cell.lte".into(), json!({"pci": "1", "earfcn": "100"}));
    now.insert("band.lte".into(), json!("3"));
    now.insert("apn".into(), json!({"mode": 0, "manual": "", "fp": ""}));
    let order: Vec<_> = diff(&set, &now).iter().map(|s| s.item).collect();
    assert_eq!(order, ["cell.lte", "band.lte", "network.mode", "cell.nr", "apn"]);
}
