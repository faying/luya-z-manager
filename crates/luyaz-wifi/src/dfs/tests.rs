use super::*;

fn up(chan: u32, width: u32, stations: u32) -> Obs {
    Obs { want_5g: true, up: true, chan: Some(chan), width: Some(width), stations: Some(stations), in_call: false }
}

fn down() -> Obs {
    Obs { want_5g: true, up: false, stations: Some(0), ..Default::default() }
}

fn fell_back_at(g: &mut Guard, at: u64) {
    g.fell_back(Override { channel: "0".into(), htmode: "EHT160".into(), at });
}

#[test]
fn ap_stopped_on_dfs_falls_back_after_the_grace() {
    let mut g = Guard::default();
    assert!(g.step(&up(36, 160, 1), "EHT160", 0, true).events.is_empty());
    let s = g.step(&down(), "EHT160", 10, true);
    assert_eq!(s.events, vec![Event::Down]);
    assert_eq!(s.action, None, "not inside the grace");
    let s = g.step(&down(), "EHT160", 10 + GRACE_S, true);
    assert_eq!(s.action, Some(Action::Fallback { htmode: "EHT80".into() }));
    assert!(!s.alert);
    // tried: not again before RETRY_S, and the alert comes once at ALERT_S
    assert_eq!(g.step(&down(), "EHT160", 20 + GRACE_S, true).action, None);
    let s = g.step(&down(), "EHT160", 10 + ALERT_S, true);
    assert!(s.alert);
    assert!(!g.step(&down(), "EHT160", 20 + ALERT_S, true).alert, "once per outage");
}

#[test]
fn auto_off_only_reports_and_alerts() {
    let mut g = Guard::default();
    g.step(&up(52, 80, 0), "EHT160", 0, false);
    g.step(&down(), "EHT160", 1, false);
    let s = g.step(&down(), "EHT160", 1 + ALERT_S, false);
    assert_eq!(s.action, None);
    assert!(s.alert);
}

#[test]
fn stopped_off_dfs_is_not_radar() {
    let mut g = Guard::default();
    g.step(&up(149, 80, 0), "EHT80", 0, true);
    g.step(&down(), "EHT80", 1, true);
    assert_eq!(g.step(&down(), "EHT80", 1 + GRACE_S, true).action, None);
}

#[test]
fn narrowed_is_reported_only() {
    let mut g = Guard::default();
    g.step(&up(36, 160, 2), "EHT160", 0, true);
    let s = g.step(&up(36, 80, 2), "EHT160", 60, true);
    assert_eq!(s.events, vec![Event::Narrowed { from: 160, to: 80 }]);
    assert_eq!(s.action, None);
}

#[test]
fn a_channel_switch_is_reported() {
    let mut g = Guard::default();
    g.step(&up(52, 160, 2), "EHT160", 0, true);
    let s = g.step(&up(149, 80, 2), "EHT160", 60, true);
    assert_eq!(s.events, vec![Event::Moved { from: 52, to: 149 }]);
    assert_eq!(s.action, None);
}

#[test]
fn back_after_down_clears_the_outage() {
    let mut g = Guard::default();
    g.step(&up(36, 160, 0), "EHT160", 0, true);
    g.step(&down(), "EHT160", 1, true);
    let s = g.step(&up(36, 160, 0), "EHT160", 30, true);
    assert_eq!(s.events, vec![Event::Back]);
    assert_eq!(g.down_since, None);
}

#[test]
fn a_restarted_core_still_restores() {
    let mut g = Guard::default();
    fell_back_at(&mut g, 1000);
    let saved = g.to_json();
    let mut g = Guard::from_json(&saved);
    assert_eq!(g.ov.as_ref().map(|o| o.at), Some(1000));
    assert_eq!(g.step(&up(36, 80, 0), "EHT80", 1000 + RESTORE_S - 1, true).action, None);
    let s = g.step(&up(36, 80, 0), "EHT80", 1000 + RESTORE_S, true);
    assert_eq!(
        s.action,
        Some(Action::Restore { ov: Override { channel: "0".into(), htmode: "EHT160".into(), at: 1000 }, reload: true })
    );
    g.restored();
    assert_eq!(g.to_json(), json!({}));
    assert_eq!(Guard::from_json(&json!({"override": {"channel": "0"}})).ov, None, "half a record is none");
}

#[test]
fn restore_waits_for_nobody_and_no_call() {
    let mut g = Guard::default();
    fell_back_at(&mut g, 0);
    let t = RESTORE_S + 5;
    assert_eq!(g.step(&up(36, 80, 1), "EHT80", t, true).action, None, "a station is connected");
    let unknown = Obs { stations: None, ..up(36, 80, 0) };
    assert_eq!(g.step(&unknown, "EHT80", t, true).action, None, "unreadable counts as somebody");
    let call = Obs { in_call: true, ..up(36, 80, 0) };
    assert_eq!(g.step(&call, "EHT80", t, true).action, None, "a call");
    assert!(matches!(g.step(&up(36, 80, 0), "EHT80", t, true).action, Some(Action::Restore { reload: true, .. })));
}

#[test]
fn restore_while_off_is_uci_only() {
    let mut g = Guard::default();
    fell_back_at(&mut g, 0);
    let off = Obs { want_5g: false, stations: Some(3), ..Default::default() };
    assert!(matches!(g.step(&off, "EHT80", RESTORE_S, true).action, Some(Action::Restore { reload: false, .. })));
    // off is no outage
    g.down_since = Some(0);
    let s = g.step(&off, "EHT80", ALERT_S * 10, true);
    assert!(!s.alert);
    assert_eq!(g.down_since, None);
}

#[test]
fn no_second_fallback_while_overridden() {
    let mut g = Guard::default();
    fell_back_at(&mut g, 0);
    g.step(&down(), "EHT80", 10, true);
    assert_eq!(g.step(&down(), "EHT80", 10 + GRACE_S, true).action, None);
}

#[test]
fn lock_held_by_the_engine_defers_the_fallback() {
    let path = std::env::temp_dir().join(format!("luyaz-wifi-dfs-lock-{}", std::process::id()));
    let path = path.to_str().unwrap().to_string();
    let mut g = Guard::default();
    g.step(&down(), "EHT160", 0, true);
    assert!(matches!(g.step(&down(), "EHT160", GRACE_S, true).action, Some(Action::Fallback { .. })));
    // the scenario engine holds the lock: ours does not come within the wait
    let held = crate::lock_at(&path, Duration::from_secs(1)).expect("first lock");
    let ours = std::thread::spawn({
        let path = path.clone();
        move || crate::lock_at(&path, Duration::from_millis(300)).is_err()
    })
    .join()
    .unwrap();
    assert!(ours, "busy lock must not be taken");
    drop(held);
    g.deferred();
    assert!(g.ov.is_none());
    assert!(matches!(g.step(&down(), "EHT160", GRACE_S + 30, true).action, Some(Action::Fallback { .. })), "tried again next look");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn spans_and_dfs() {
    assert_eq!(span(36, 160), vec![36, 40, 44, 48, 52, 56, 60, 64]);
    assert_eq!(span(44, 80), vec![36, 40, 44, 48]);
    assert_eq!(span(56, 40), vec![52, 56]);
    assert_eq!(span(157, 80), vec![149, 153, 157, 161]);
    assert!(on_dfs(36, 160), "36 at 160 MHz covers 52–64");
    assert!(!on_dfs(36, 80));
    assert!(on_dfs(100, 20));
    assert!(!on_dfs(149, 80));
}

#[test]
fn htmode_family_kept() {
    assert_eq!(htmode_80("EHT160"), "EHT80");
    assert_eq!(htmode_80("HE160"), "HE80");
    assert_eq!(htmode_80("EHT320"), "EHT80");
    assert_eq!(htmode_80("EHT40"), "EHT40");
    assert_eq!(htmode_80(""), "EHT80");
}

#[test]
fn parses_iw() {
    let info = "Interface wlan2\n\tifindex 30\n\ttype AP\n\tchannel 36 (5180 MHz), width: 160 MHz, center1: 5250 MHz\n";
    assert_eq!(parse_iw_info(info), (Some(36), Some(160)));
    assert_eq!(parse_iw_info("Interface wlan2\n\ttype AP\n"), (None, None));
    let dev = "phy#4\n\tInterface wlan2\n\t\ttype AP\n\tInterface wlan0\n\t\ttype AP\n\tInterface scan0\n\t\ttype managed\n";
    assert_eq!(parse_ap_ifaces(dev), vec!["wlan2", "wlan0"]);
}

#[test]
fn kmsg_hints() {
    assert!(kmsg_hint("wlan: [1:I:dfs] dfs_process_radarevent: Radar detected on 5260"));
    assert!(kmsg_hint("wlan: CSA started to chan 149"));
    assert!(!kmsg_hint("mlme_init_dfs_cfg: dfs: zte_qdf_wlan_dfs_cac_timeout=3."));
    assert!(!kmsg_hint("hdd_hostapd_channel_allow_suspend: DFS: allowing suspend (chan_freq: 5180)"));
    assert!(!kmsg_hint("PM: suspend entry"));
}
