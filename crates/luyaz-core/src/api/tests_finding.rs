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
            json!({"connect_status": "disconnected", "connect_fail_count": 3, "ipv4_address": ""}),
        ),
        "ok",
    )
    .data();
    assert_eq!(d.connect_status.as_deref(), Some("disconnected"));
    assert_eq!((d.fail_count, d.has_ip), (Some(3), Some(false)));
}

#[test]
fn no_probe_without_a_client_or_inside_ten_minutes() {
    let mut i = inputs(json!({}), "ok");
    assert!(i.can_probe());
    i.clients = false;
    assert!(!i.can_probe());
    let f = Findings::load(std::env::temp_dir().join("luyaz-findings-none.json"));
    assert!(f.probe_due(T));
    f.last_probe.store(T, Ordering::SeqCst);
    assert!(!f.probe_due(T + RECHECK_SECS - 1));
    assert!(f.probe_due(T + RECHECK_SECS));
}

/// What the sampler would decide at `now`: the same steps as `watch::finding_round`.
fn may_probe(f: &Findings, down: bool, now: u64) -> bool {
    let mut i = inputs(json!({}), "ok");
    i.passive_down = down;
    i.probe_due = f.probe_due(now);
    i.held_down = f.held_down(down, now);
    i.can_probe()
}

#[test]
fn probes_only_after_a_minute_of_downs() {
    let f = Findings::load(std::env::temp_dir().join("luyaz-findings-hold.json"));
    // down 5 s, then back: no probe
    assert!(!may_probe(&f, true, T));
    assert!(!may_probe(&f, true, T + 5));
    assert!(!may_probe(&f, false, T + 10));
    // down 30 s: no probe
    assert!(!may_probe(&f, true, T + 100));
    assert!(!may_probe(&f, true, T + 130));
    // back in between: the count starts again
    assert!(!may_probe(&f, false, T + 150));
    assert!(!may_probe(&f, true, T + 160));
    assert!(!may_probe(&f, true, T + 160 + DOWN_HOLD_SECS - 1));
    // a minute in a row: it probes
    assert!(may_probe(&f, true, T + 160 + DOWN_HOLD_SECS));
}

#[test]
fn short_outages_keep_the_recheck_wait_free() {
    let f = Findings::load(std::env::temp_dir().join("luyaz-findings-short.json"));
    // three short outages (`round` only starts the wait when the probe ran)
    for k in 0..3 {
        let t = T + k * 100;
        assert!(!may_probe(&f, true, t));
        assert!(!may_probe(&f, true, t + 30));
        assert!(!may_probe(&f, false, t + 40));
    }
    assert_eq!(f.last_probe.load(Ordering::SeqCst), 0);
    // the next long one probes as soon as it has lasted a minute
    let t = T + 1000;
    assert!(!may_probe(&f, true, t));
    assert!(may_probe(&f, true, t + DOWN_HOLD_SECS));
    // after a probe the 10-minute wait applies, held or not
    f.last_probe.store(t + DOWN_HOLD_SECS, Ordering::SeqCst);
    assert!(!may_probe(&f, true, t + DOWN_HOLD_SECS + 30));
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
    let likely = match open("89860011") {
        Verdict::Open(mut x) => {
            x.confidence = Confidence::Likely;
            Verdict::Open(x)
        }
        v => v,
    };
    f.observe(&likely, "89860011", &w, T);
    assert_eq!(f.home(), None);
    assert_eq!(f.view()["iccid_tail"], "0011");
    f.observe(&open("89860011"), "89860011", &w, T + 30);
    let h = f.home().unwrap();
    assert_eq!(
        (h.layer.as_str(), h.symptom.as_str()),
        ("internet", "已连上，但外网不通")
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
