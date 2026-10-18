use super::*;
fn boot(seconds: u64) -> BootTime {
    BootTime {
        boot_id: "a".into(),
        seconds,
    }
}
fn blocks(at: u64, rx: u64) -> Vec<Observation> {
    [("signal", json!({"type":"SA","roaming":"Home"})),
     ("interfaces", json!({"cellular":{"enable":1,"roam_enable":0,"connect_status":"ipv6_connected","ipv6_address":"2001:db8::2","ipv6_dev_name":"cell0"}})),
     ("live", json!({"traffic":{"rx_bytes":rx}})),
     ("sim", json!({"iccid":"89860011"}))]
        .into_iter().map(|(name,data)| Observation { name,data,observed:Some(boot(at)),generation:1 }).collect()
}
fn reading(at: u64, rx: u64) -> Readings {
    Readings::from_observations(blocks(at, rx), &boot(at)).unwrap()
}
fn sample(p: &mut Passive, at: u64, rx: u64) -> (bool, bool) {
    let r = reading(at, rx);
    p.sample(Some(&r), Some(&boot(at)));
    p.admission(Some(&r), Some(&boot(at)))
}

#[test]
fn minute_hold_requires_continuous_five_second_observations() {
    let mut p = Passive::default();
    for at in (0..60).step_by(5) {
        assert_eq!(sample(&mut p, at, 10), (true, false));
    }
    assert_eq!(sample(&mut p, 60, 10), (true, true));
    assert_eq!(
        p.admission(Some(&reading(60, 10)), Some(&boot(71))),
        (true, false)
    );
    assert_eq!(sample(&mut p, 75, 10), (true, false));
    for at in (80..=135).step_by(5) {
        sample(&mut p, at, 10);
    }
    assert_eq!(sample(&mut p, 140, 10), (true, true));
    // A missing sample clears an otherwise completed hold.
    p.sample(None, Some(&boot(145)));
    assert_eq!(sample(&mut p, 150, 10), (true, false));
}

#[test]
fn traffic_at_a_five_second_tick_between_diagnoses_resets_hold() {
    let mut p = Passive::default();
    for at in (0..=30).step_by(5) {
        sample(&mut p, at, 10);
    }
    assert_eq!(sample(&mut p, 35, 11), (false, false));
    for at in (40..=65).step_by(5) {
        assert_eq!(sample(&mut p, at, 11), (false, false));
    }
    assert_eq!(sample(&mut p, 70, 11), (true, false));
    for at in (75..=125).step_by(5) {
        sample(&mut p, at, 11);
    }
    assert_eq!(sample(&mut p, 130, 11), (true, true));
    assert_eq!(
        sample(&mut p, 135, 0),
        (true, false),
        "counter reset is not receive activity or a fault"
    );
    assert_eq!(sample(&mut p, 140, 0), (true, false));
}

#[test]
fn gaps_suspend_boot_change_and_clock_failure_reset_hold() {
    let mut p = Passive::default();
    for at in (0..=60).step_by(5) {
        sample(&mut p, at, 10);
    }
    assert_eq!(
        sample(&mut p, 660, 10),
        (true, false),
        "suspend is not continuous observation"
    );
    let mut other = reading(665, 10);
    other.observed.boot_id = "b".into();
    p.sample(Some(&other), Some(&boot(665)));
    assert_eq!(sample(&mut p, 670, 10), (true, false));
    p.sample(Some(&reading(675, 10)), None);
    assert_eq!(sample(&mut p, 680, 10), (true, false));
    assert_eq!(
        sample(&mut p, 679, 10),
        (true, false),
        "backwards/duplicate elapsed samples reset"
    );
    assert_eq!(sample(&mut p, 679, 10), (true, false));
}

#[test]
fn unknown_status_counter_and_connection_change_reset_and_never_enable_probe() {
    let mut p = Passive::default();
    for at in (0..=60).step_by(5) {
        sample(&mut p, at, 10);
    }
    for field in ["connect_status", "rx_bytes", "generation", "iccid"] {
        let mut r = reading(65, 10);
        match field {
            "connect_status" => {
                r.state["interfaces"]["cellular"]["connect_status"] = "new_firmware_status".into()
            }
            "rx_bytes" => r.state["traffic"]["rx_bytes"] = Value::Null,
            "generation" => r.connection.generations[1] += 1,
            _ => r.connection.iccid = "other".into(),
        }
        p.sample(Some(&r), Some(&boot(65)));
        assert!(!p.admission(Some(&r), Some(&boot(65))).1);
        assert_eq!(sample(&mut p, 70, 10), (true, false));
    }
}

#[test]
fn freshness_uses_boot_observation_and_all_required_blocks() {
    let original = blocks(100, 10);
    assert!(Readings::from_observations(original.clone(), &boot(130)).is_some());
    for now in [99, 131, 100 + 28800] {
        assert!(Readings::from_observations(original.clone(), &boot(now)).is_none());
    }
    for index in 0..4 {
        let mut bad = original.clone();
        bad[index].observed = None;
        assert!(Readings::from_observations(bad, &boot(100)).is_none());
        let mut bad = original.clone();
        bad[index].observed.as_mut().unwrap().boot_id = "b".into();
        assert!(Readings::from_observations(bad, &boot(100)).is_none());
        let mut bad = original.clone();
        bad.remove(index);
        assert!(Readings::from_observations(bad, &boot(100)).is_none());
    }
    let mut blank = original;
    blank[3].data["iccid"] = "".into();
    assert!(Readings::from_observations(blank, &boot(100)).is_none());
}

#[test]
fn hub_reads_are_atomic_latest_and_changes_aba_or_failure_invalidate_identity() {
    use crate::block::{BlockSpec, LivePolicy, NoSink, OnChange, SignalPolicy};
    let hub = Hub::new(
        vec![
            BlockSpec::derived("signal", Box::new(SignalPolicy)),
            BlockSpec::derived("interfaces", Box::new(OnChange)),
            BlockSpec::derived("live", Box::new(LivePolicy)),
            BlockSpec::derived("sim", Box::new(OnChange)),
        ],
        Box::new(NoSink),
    );
    let now = tokio::time::Instant::now();
    for b in blocks(0, 10) {
        hub.record(b.name, Ok(b.data), now);
    }
    let a = Readings::capture(&hub).unwrap();
    assert_eq!(
        a.state["traffic"]["rx_bytes"], 10,
        "latest successful live read before publication"
    );
    hub.record("interfaces", Ok(a.state["interfaces"].clone()), now);
    assert!(
        Readings::capture(&hub).unwrap().connection == a.connection,
        "same-value refresh is not a switch"
    );
    let mut changed = a.state["interfaces"].clone();
    changed["cellular"]["ipv6_address"] = "2001:db8::3".into();
    hub.record("interfaces", Ok(changed), now);
    hub.record("interfaces", Ok(a.state["interfaces"].clone()), now);
    assert!(
        Readings::capture(&hub).unwrap().connection != a.connection,
        "A→B→A must invalidate old context"
    );
    let b = Readings::capture(&hub).unwrap();
    hub.record("sim", Err("unreadable".into()), now);
    assert!(Readings::capture(&hub).is_none());
    hub.record("sim", Ok(json!({"iccid":"89860011"})), now);
    assert!(Readings::capture(&hub).unwrap().connection != b.connection);
}
