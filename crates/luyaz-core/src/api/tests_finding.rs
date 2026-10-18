use super::*;
use luyaz_api::diagnose::Level;
use luyaz_api::finding::{
    Confidence, Evidence, Expected, Finding, KIND_NO_INTERNET, LayerId, expected_outage,
};
use serde_json::json;

const T: u64 = record::CLOCK_SET + 1_000_000;

fn inputs(state: Value, story: &str) -> Inputs {
    Inputs {
        state: Some(state),
        screen: Some(json!({"net": {"story": {"state": story}}})),
        iccid: "89860011".into(),
        passive_down: true,
        clients: true,
        probe_due: true,
        held_down: true,
        busy: None,
        probed: AtomicBool::new(false),
        observed: BootTime::read(),
        root: "/nonexistent-luyaz-fixture".into(),
        network: Mutex::new(None),
        current_connection: Arc::new(|| true),
        reserve_probe: Arc::new(|| true),
    }
}

fn device(net: Value, cell: Value) -> Value {
    json!({"net": net, "interfaces": {"cellular": cell}})
}

#[test]
fn premises_from_the_blocks_and_the_screen() {
    let st = device(
        json!({"type": "SA", "roaming": "Home"}),
        json!({"enable": 1, "roam_enable": 0}),
    );
    let p = inputs(st.clone(), "nodata").premises();
    assert_eq!(expected_outage(&p), Ok(None));
    assert_eq!(
        expected_outage(&inputs(st.clone(), "nosim").premises()),
        Ok(Some(Expected::NoSim))
    );
    assert_eq!(
        expected_outage(&inputs(st, "airplane").premises()),
        Ok(Some(Expected::Airplane))
    );
    let off = device(
        json!({"type": "SA", "roaming": "Home"}),
        json!({"enable": 0, "roam_enable": 1, "connect_status": "disconnected"}),
    );
    assert_eq!(
        expected_outage(&inputs(off, "nodata").premises()),
        Ok(Some(Expected::DataOff))
    );
    let roam = device(
        json!({"type": "LTE", "roaming": "Roaming"}),
        json!({"enable": 1, "roam_enable": 0}),
    );
    assert_eq!(
        expected_outage(&inputs(roam, "nodata").premises()),
        Ok(Some(Expected::RoamingOff))
    );
    let text = device(
        json!({"type": "SA", "roaming": "Home"}),
        json!({"enable": "0", "roam_enable": "1", "connect_status": "disconnected"}),
    );
    assert_eq!(
        expected_outage(&inputs(text, "nodata").premises()),
        Ok(Some(Expected::DataOff))
    );
    // after a boot: enable 0 but connected is on
    let boot = device(
        json!({"type": "SA", "roaming": "Home"}),
        json!({"enable": 0, "roam_enable": 0, "connect_status": "ipv4_ipv6_connected"}),
    );
    assert_eq!(expected_outage(&inputs(boot, "stall").premises()), Ok(None));
    // no screen view: can't tell, so nothing opens
    let mut i = inputs(device(json!({}), json!({"enable": 1})), "ok");
    i.screen = None;
    assert!(expected_outage(&i.premises()).is_err());
}

#[test]
fn registration_and_data_readings() {
    let r = |t: &str, roam: &str| {
        inputs(device(json!({"type": t, "roaming": roam}), json!({})), "ok").registration()
    };
    assert_eq!(r("SA", "Home"), Reg::Home);
    assert_eq!(r("LTE", "Roaming"), Reg::Roaming);
    assert_eq!(r("Limited Service", "Home"), Reg::NotRegistered);
    assert_eq!(r("", "Home"), Reg::Unknown);
    let d = inputs(
        device(
            json!({}),
            json!({"connect_status": "disconnected", "connect_fail_count": 3, "ipv4_address": "", "ipv6_address": ""}),
        ),
        "ok",
    )
    .data();
    assert_eq!(d.connect_status.as_deref(), Some("disconnected"));
    assert_eq!((d.fail_count, d.has_ip), (Some(3), Some(false)));
}

#[test]
fn ipv6_address_is_not_a_missing_ipv4_fault_and_unknown_stays_unknown() {
    let mut i = inputs(
        device(
            json!({"type":"SA", "roaming":"Home"}),
            json!({
                "enable":1, "connect_status":"ipv6_connected", "ipv4_address":"", "ipv6_address":"2001:db8::2"
            }),
        ),
        "stall",
    );
    assert_eq!(i.data().has_ip, Some(true));
    assert_eq!(
        luyaz_api::finding::judge_data(&i.data(), T).level,
        Level::Ok
    );
    i.state.as_mut().unwrap()["interfaces"]["cellular"]["ipv6_address"] = Value::Null;
    assert_eq!(i.data().has_ip, None);
    assert_eq!(
        luyaz_api::finding::judge_data(&i.data(), T).level,
        Level::Na
    );
    i.observed = None;
    assert!(!i.can_probe());
    assert_eq!(i.data(), DataIn::default());
    assert!(expected_outage(&i.premises()).is_err());
}

#[test]
fn stale_snapshot_and_unverified_proxy_do_not_use_the_host_network() {
    let root = std::env::temp_dir().join(format!("finding-path-{}", rand::random::<u64>()));
    std::fs::create_dir_all(root.join("tmp")).unwrap();
    let mut i = inputs(
        device(
            json!({}),
            json!({
                "ipv4_address":"192.0.2.2", "ipv4_dev_name":"no_such_cell", "ipv4_dns_prefer":"192.0.2.53"
            }),
        ),
        "stall",
    );
    i.root = root.to_string_lossy().into();
    std::fs::write(root.join("tmp/chill.state"), r#"{"state":"running"}"#).unwrap();
    let n = i.network_probe();
    assert_eq!(n.reason, "proxy_route_unverified");
    assert_eq!(n.path_reachable, luyaz_api::finding::ResultState::Unknown);
    assert!(n.paths.is_empty());
    i.observed.as_mut().unwrap().seconds = i.observed.as_ref().unwrap().seconds.saturating_sub(31);
    assert!(!i.can_probe());
    // A different boot always fails, even on a freshly booted test container.
    i.observed.as_mut().unwrap().boot_id = "other".into();
    assert!(!i.can_probe());
    assert_eq!(i.network_probe().reason, "snapshot_or_connection_changed");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn no_probe_without_client_budget_or_hold() {
    let mut i = inputs(json!({}), "ok");
    assert!(i.can_probe());
    i.clients = false;
    assert!(!i.can_probe());
    i.clients = true;
    i.probe_due = false;
    assert!(!i.can_probe());
    i.probe_due = true;
    i.held_down = false;
    assert!(!i.can_probe());
    i.held_down = true;
    i.current_connection = Arc::new(|| false);
    assert!(!i.can_probe());
}

fn open(iccid: &str) -> Verdict {
    Verdict::Open(Finding {
        kind: KIND_NO_INTERNET,
        iccid: iccid.into(),
        layer: LayerId::Internet,
        confidence: Confidence::Sure,
        symptom: "已连上，但外网不通".into(),
        symptom_en: "Connected, but the internet is unreachable".into(),
        evidence: vec![Evidence::new(LayerId::Internet, Level::Bad, "x", "x", T)],
        at: T,
    })
}

#[test]
fn store_survives_a_restart_and_transitions_are_events() {
    let dir = std::env::temp_dir().join(format!("luyaz-findings-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("findings.json");
    let _ = std::fs::remove_file(&path);
    let w = Watch::new();
    w.open(dir.join("log"));
    let f = Findings::load(path.clone());
    f.observe(&open("A"), "A", &w, T);
    assert!(f.dismiss_shown(KIND_NO_INTERNET, Some("A"), &w, T + 1));
    f.observe(&open("B"), "B", &w, T + 2);
    let kinds: Vec<String> = w
        .history(0, u64::MAX)
        .1
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, ["finding.open", "finding.close", "finding.open"]);

    // "restart": a new store from the file
    let back = Findings::load(path.clone());
    assert_eq!(back.value(), f.value());
    let v = back.value();
    assert_eq!(v["open"][0]["iccid"], "B");
    assert_eq!(v["suppressed"][0]["iccid"], "A");
    assert!(!dir.join("findings.json.tmp").exists());
    // a broken file: empty, not a panic
    std::fs::write(&path, "{not json").unwrap();
    assert_eq!(Findings::load(path).value(), Store::default().to_value());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn home_shows_only_sure_and_ignore_keeps_it_away() {
    let dir = std::env::temp_dir().join(format!("luyaz-findings-home-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("findings.json");
    let _ = std::fs::remove_file(&path);
    let w = Watch::new();
    let f = Findings::load(path);
    let data_open = || match open("89860011") {
        Verdict::Open(mut x) => {
            x.layer = LayerId::Data;
            Verdict::Open(x)
        }
        v => v,
    };
    let likely = match data_open() {
        Verdict::Open(mut x) => {
            x.confidence = Confidence::Likely;
            Verdict::Open(x)
        }
        v => v,
    };
    f.observe(&likely, "89860011", &w, T);
    assert_eq!(f.home(), None);
    assert_eq!(f.view()["iccid_tail"], "0011");
    f.observe(&data_open(), "89860011", &w, T + 30);
    let h = f.home().unwrap();
    assert_eq!(
        (h.layer.as_str(), h.symptom.as_str()),
        ("data", "已连上，但外网不通")
    );
    assert!(!h.to_value().to_string().contains("8986"));

    // the wrong card's tail or kind: nothing
    assert!(!f.dismiss_shown(KIND_NO_INTERNET, Some("9999"), &w, T + 40));
    assert!(!f.dismiss_shown("other", None, &w, T + 40));
    assert!(f.dismiss_shown(KIND_NO_INTERNET, Some("0011"), &w, T + 40));
    assert_eq!((f.home(), f.view()), (None, Value::Null));
    // the same card still down inside 24 h: stays away
    f.observe(&open("89860011"), "89860011", &w, T + 3600);
    assert_eq!(f.view(), Value::Null);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn legacy_network_findings_cannot_reappear_as_sure_on_home() {
    let f = Findings::load(
        std::env::temp_dir().join(format!("finding-legacy-{}", rand::random::<u64>())),
    );
    f.observe(&open("89860011"), "89860011", &Watch::new(), T);
    assert_eq!(f.home(), None);
    let v = f.view();
    assert_eq!(v["confidence"], "guess");
    assert_eq!(v["verification"], "legacy_path_unverified");
    assert!(!v.to_string().contains("89860011"));
    let _ = std::fs::remove_file(&f.path);
}

#[test]
fn network_observation_survives_without_an_open_finding_and_expires_on_read() {
    use luyaz_api::finding::ResultState;
    let path = std::env::temp_dir().join(format!("finding-report-{}", rand::random::<u64>()));
    let f = Findings::load(path.clone());
    let mut i = inputs(json!({}), "ok");
    i.clients = false;
    // Replay a completed local probe; the closed client gate prevents all real packets.
    i.probed.store(true, Ordering::SeqCst);
    *lock(&i.network) = Some(NetworkEvidence {
        observed_at: BootTime::read(),
        path_reachable: ResultState::Passed,
        reason: "offline_replay".into(),
        ..NetworkEvidence::default()
    });
    f.round(&i, &Mutex::new(Gate::default()), &Watch::new(), T);
    assert!(f.view().is_null());
    let v = f.network_view();
    assert_eq!(v["fresh"], true);
    assert_eq!(v["observation"]["path_reachable"], "passed");
    assert_eq!(v["observation"]["internet_usable"], "unknown");
    lock(&f.network)
        .as_mut()
        .unwrap()
        .evidence
        .observed_at
        .as_mut()
        .unwrap()
        .boot_id = "other".into();
    assert_eq!(f.network_view()["fresh"], false);
    assert!(
        !path.exists(),
        "observations alone must not add flash writes"
    );
}

fn path_inputs() -> Inputs {
    inputs(
        device(
            json!({"type":"SA","roaming":"Home"}),
            json!({
                "enable":1,"roam_enable":0,"connect_status":"ipv4_connected",
                "ipv4_address":"192.0.2.2","ipv4_dev_name":"cell0","ipv4_dns_prefer":"192.0.2.53"
            }),
        ),
        "stall",
    )
}

struct StubTransport {
    calls: std::sync::atomic::AtomicUsize,
    on_request: Box<dyn Fn()>,
}
impl nf::path::Transport for StubTransport {
    fn http(&self, _: &nf::path::Path, _: std::net::SocketAddr) -> luyaz_api::finding::ResultState {
        self.calls.fetch_add(1, Ordering::SeqCst);
        (self.on_request)();
        luyaz_api::finding::ResultState::Passed
    }
    fn dns(&self, p: &nf::path::Path, t: std::net::SocketAddr) -> luyaz_api::finding::ResultState {
        self.http(p, t)
    }
}
fn stub(f: impl Fn() + 'static) -> StubTransport {
    StubTransport {
        calls: 0.into(),
        on_request: Box::new(f),
    }
}

#[test]
fn no_packets_when_budget_or_connection_is_unknown() {
    let io = stub(|| {});
    let mut i = path_inputs();
    i.reserve_probe = Arc::new(|| false);
    let n = i.network_probe_with(&io);
    assert_eq!(n.reason, "probe_budget_unavailable");
    assert!(!i.probed.load(Ordering::SeqCst));
    i.current_connection = Arc::new(|| false);
    i.reserve_probe = Arc::new(|| panic!("changed connection must not claim budget"));
    assert_eq!(
        i.network_probe_with(&io).reason,
        "snapshot_or_connection_changed"
    );
    assert_eq!(io.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn connection_change_during_http_discards_success_and_stops_remaining_requests() {
    let current = Arc::new(AtomicBool::new(true));
    let mut i = path_inputs();
    let c = current.clone();
    i.current_connection = Arc::new(move || c.load(Ordering::SeqCst));
    let io = stub(move || current.store(false, Ordering::SeqCst));
    let n = i.network_probe_with(&io);
    assert_eq!(io.calls.load(Ordering::SeqCst), 1);
    assert!(i.probed.load(Ordering::SeqCst));
    assert_eq!(n.reason, "snapshot_or_connection_changed_during_probe");
    assert_eq!(n.path_reachable, luyaz_api::finding::ResultState::Unknown);
    assert_eq!(n.dns, luyaz_api::finding::ResultState::Unknown);
    assert!(n.paths.is_empty(), "discard per-path success too");
    assert!(!i.readings_current());
}

#[test]
fn budget_is_durable_before_request_and_panic_does_not_refund_it() {
    let root =
        std::env::temp_dir().join(format!("finding-before-packet-{}", rand::random::<u64>()));
    let path = root.join("budget.json");
    let boot = |seconds| BootTime {
        boot_id: "fixture".into(),
        seconds,
    };
    let b = Arc::new(Mutex::new(budget::Budget::load(
        path.clone(),
        Some(&boot(0)),
    )));
    let mut i = path_inputs();
    let b2 = b.clone();
    let at = boot(600);
    i.reserve_probe = Arc::new(move || lock(&b2).reserve(Some(&at)));
    let p = path.clone();
    let io = stub(move || {
        let v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(v["last"]["seconds"], 600);
        panic!("simulated request crash after durable reservation");
    });
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| i.network_probe_with(&io)))
            .is_err()
    );
    assert!(!budget::Budget::load(path.clone(), Some(&boot(601))).due(Some(&boot(1199))));
    assert_eq!(
        i.network_probe_with(&stub(|| panic!("must not issue a second request")))
            .reason,
        "probe_budget_unavailable"
    );
    assert_eq!(io.calls.load(Ordering::SeqCst), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn changed_connection_does_not_resolve_finding_or_bless_saved_report() {
    let path = std::env::temp_dir().join(format!("finding-switch-{}", rand::random::<u64>()));
    let f = Findings::load(path.clone());
    f.observe(&open("89860011"), "89860011", &Watch::new(), T);
    let current = Arc::new(AtomicBool::new(true));
    let c = current.clone();
    let mut i = path_inputs();
    i.current_connection = Arc::new(move || c.load(Ordering::SeqCst));
    i.clients = false; // No real requests in this replay.
    *lock(&i.network) = Some(NetworkEvidence {
        observed_at: BootTime::read(),
        reason: "offline_replay".into(),
        ..NetworkEvidence::default()
    });
    f.round(&i, &Mutex::new(Gate::default()), &Watch::new(), T);
    assert_eq!(f.network_view()["fresh"], true);
    current.store(false, Ordering::SeqCst);
    assert_eq!(f.network_view()["fresh"], false);
    i.passive_down = false; // Even a stale "up" cannot resolve an old finding.
    for _ in 0..2 {
        assert!(matches!(
            f.round(&i, &Mutex::new(Gate::default()), &Watch::new(), T),
            Some(Verdict::Unverified { .. })
        ));
    }
    assert_eq!(f.value()["open"].as_array().unwrap().len(), 1);
    let _ = std::fs::remove_file(path);
}

#[test]
fn stale_card_context_cannot_expire_another_cards_current_finding() {
    let path = std::env::temp_dir().join(format!("finding-old-card-{}", rand::random::<u64>()));
    let f = Findings::load(path.clone());
    f.observe(&open("NEW"), "NEW", &Watch::new(), T);
    let mut i = path_inputs();
    i.iccid = "OLD".into();
    i.current_connection = Arc::new(|| false);
    assert!(matches!(
        f.round(&i, &Mutex::new(Gate::default()), &Watch::new(), T),
        Some(Verdict::Unverified { .. })
    ));
    assert_eq!(f.value()["open"][0]["iccid"], "NEW");
    let _ = std::fs::remove_file(path);
}

#[test]
fn guard_never_resumes_requests_after_an_observed_connection_failure() {
    use nf::path::Transport;
    let current = Arc::new(AtomicBool::new(true));
    let c = current.clone();
    let mut i = path_inputs();
    i.current_connection = Arc::new(move || c.load(Ordering::SeqCst));
    let io = stub(|| {});
    let guarded = Guarded {
        inputs: &i,
        io: &io,
        invalid: AtomicBool::new(false),
    };
    let path = nf::path::paths(i.state.as_ref().unwrap()).remove(0);
    let target = "192.0.2.53:53".parse().unwrap();
    guarded.http(&path, target);
    current.store(false, Ordering::SeqCst);
    guarded.dns(&path, target);
    current.store(true, Ordering::SeqCst);
    guarded.http(&path, target);
    assert_eq!(io.calls.load(Ordering::SeqCst), 1);
    assert!(!guarded.current());
}
