use super::*;
fn boot(id: &str, seconds: u64) -> BootTime {
    BootTime {
        boot_id: id.into(),
        seconds,
    }
}
fn fixture() -> PathBuf {
    std::env::temp_dir()
        .join(format!("finding-budget-{}", rand::random::<u64>()))
        .join("budget.json")
}

#[test]
fn reservation_survives_crash_and_core_restarts_with_no_completion_write() {
    let path = fixture();
    let mut b = Budget::load(path.clone(), Some(&boot("a", 0)));
    assert!(!b.due(Some(&boot("a", 599))));
    assert!(b.reserve(Some(&boot("a", 600))));
    assert!(!b.due(Some(&boot("a", 1199))));
    let mut restarted = Budget::load(path.clone(), Some(&boot("a", 601)));
    assert!(!restarted.reserve(Some(&boot("a", 1199))));
    assert!(restarted.reserve(Some(&boot("a", 1200))));
    assert!(!Budget::load(path.clone(), Some(&boot("a", 1201))).due(Some(&boot("a", 1799))));
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn missing_bad_future_and_old_version_get_conservative_holds() {
    for text in [
        None,
        Some("{}"),
        Some("{broken"),
        Some(r#"{"version":2,"last":{"boot_id":"a","seconds":0}}"#),
        Some(r#"{"version":1,"last":{"boot_id":"a","seconds":99999}}"#),
    ] {
        let path = fixture();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        if let Some(text) = text {
            fs::write(&path, text).unwrap();
        }
        let mut b = Budget::load(path.clone(), Some(&boot("a", 50)));
        assert!(!b.due(None));
        assert!(!b.due(Some(&boot("a", 649))));
        assert!(!b.due(Some(&boot("b", 1000))));
        assert!(!b.due(Some(&boot("a", 49))));
        assert!(b.reserve(Some(&boot("a", 650))));
        // A short core restart does not reset a successfully persisted reservation.
        assert!(!Budget::load(path.clone(), Some(&boot("a", 651))).due(Some(&boot("a", 1249))));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    let path = fixture();
    assert!(!Budget::load(path, None).due(Some(&boot("a", 9999))));
}

#[test]
fn device_reboot_uses_new_boot_and_suspend_counts_for_cooldown() {
    let path = fixture();
    let mut b = Budget::load(path.clone(), Some(&boot("a", 0)));
    assert!(b.reserve(Some(&boot("a", 50000))));
    let mut rebooted = Budget::load(path.clone(), Some(&boot("b", 5)));
    assert!(!rebooted.due(Some(&boot("b", 599))));
    assert!(rebooted.reserve(Some(&boot("b", 600))));
    assert!(
        rebooted.due(Some(&boot("b", 1200))),
        "no periodic ticks needed during suspend"
    );
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn two_instances_cannot_spend_one_budget_and_lock_is_nonblocking() {
    let path = fixture();
    let mut a = Budget::load(path.clone(), Some(&boot("a", 0)));
    let mut b = Budget::load(path.clone(), Some(&boot("a", 0)));
    assert!(a.reserve(Some(&boot("a", 600))));
    assert!(!b.reserve(Some(&boot("a", 600))));
    assert!(!b.due(Some(&boot("a", 1199))));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    assert!(!a.reserve(Some(&boot("a", 1200))));
    drop(lock);
    assert!(
        !a.due(Some(&boot("a", 1799))),
        "failed lock must not retry every sampler tick"
    );
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn unwritable_budget_sends_no_permission_and_retry_is_bounded() {
    let path = fixture();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path.parent().unwrap().join("not-dir"), "x").unwrap();
    let mut b = Budget::load(
        path.parent().unwrap().join("not-dir/budget"),
        Some(&boot("a", 0)),
    );
    assert!(!b.reserve(Some(&boot("a", 600))));
    assert!(!b.due(Some(&boot("a", 601))));
    assert!(b.due(Some(&boot("a", 1200))));
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn a_new_future_reservation_from_another_instance_is_not_overwritten() {
    let path = fixture();
    let mut a = Budget::load(path.clone(), Some(&boot("a", 0)));
    let mut b = Budget::load(path.clone(), Some(&boot("a", 0)));
    assert!(a.reserve(Some(&boot("a", 601))));
    assert!(!b.reserve(Some(&boot("a", 600))));
    let record = Budget::read(&path).unwrap();
    assert_eq!(record.last.seconds, 601);
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
