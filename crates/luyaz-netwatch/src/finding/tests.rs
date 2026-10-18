use super::*;
use std::cell::Cell;

struct Fake {
    premises: Premises,
    down: bool,
    can_probe: bool,
    busy: Option<&'static str>,
    reg: Reg,
    data: DataIn,
    internet: Option<Probe>,
    dns: Option<Probe>,
    probes: Cell<u32>,
    expired: bool,
    panic_probe: bool,
    /// the gate as the probe saw it: (diag_since, finding_since)
    gate: Option<std::sync::Arc<Mutex<Gate>>>,
    seen: Cell<Option<(Option<u64>, Option<u64>)>>,
}

impl FindingHost for Fake {
    fn premises(&self) -> Premises {
        self.premises
    }
    fn passive_down(&self) -> bool {
        self.down
    }
    fn can_probe(&self) -> bool {
        self.can_probe
    }
    fn iccid(&self) -> String {
        "8986".into()
    }
    fn others_busy(&self) -> Option<&'static str> {
        self.busy
    }
    fn registration(&self) -> Reg {
        self.reg
    }
    fn data(&self) -> DataIn {
        self.data.clone()
    }
    fn readings_current(&self) -> bool {
        !self.expired
    }
    fn network_probe(&self) -> NetworkEvidence {
        assert!(!self.panic_probe, "fake transport panic");
        self.probes.set(self.probes.get() + 1);
        if let Some(g) = &self.gate {
            let g = g.lock().unwrap();
            self.seen.set(Some((g.diag_since, g.finding_since)));
        }
        let state = |p| match p {
            Some(Probe::Ok { .. }) => ResultState::Passed,
            Some(Probe::Failed) => ResultState::Failed,
            None => ResultState::Unknown,
        };
        NetworkEvidence {
            path_reachable: state(self.internet),
            dns: state(self.dns),
            reason: "offline_fixture".into(),
            ..NetworkEvidence::default()
        }
    }
}

fn fake() -> Fake {
    Fake {
        premises: Premises {
            sim_present: Some(true),
            airplane: Some(false),
            data_on: Some(true),
            roaming: Some(false),
            roam_allowed: Some(false),
        },
        down: true,
        can_probe: true,
        busy: None,
        reg: Reg::Home,
        data: DataIn {
            connect_status: Some("ipv4_connected".into()),
            has_ip: Some(true),
            pdp_active: Some(true),
            ..DataIn::default()
        },
        internet: Some(Probe::Ok { ms: 30 }),
        dns: Some(Probe::Failed),
        probes: Cell::new(0),
        expired: false,
        panic_probe: false,
        gate: None,
        seen: Cell::new(None),
    }
}

#[test]
fn proven_fault_opens_and_frees_the_gate() {
    let gate = Mutex::new(Gate::default());
    let f = fake();
    match check(&f, &gate).unwrap() {
        Verdict::Open(x) => assert_eq!((x.layer, x.confidence), (LayerId::Dns, Confidence::Likely)),
        v => panic!("{v:?}"),
    }
    assert_eq!(f.probes.get(), 1);
    assert!(gate.lock().unwrap().diag_since.is_none());
    assert!(gate.lock().unwrap().finding_since.is_none());
}

#[test]
fn the_probe_holds_finding_since_not_diag_since() {
    let gate = std::sync::Arc::new(Mutex::new(Gate::default()));
    let f = Fake {
        gate: Some(gate.clone()),
        ..fake()
    };
    check(&f, &gate).unwrap();
    let (diag, finding) = f.seen.get().expect("probed");
    assert!(diag.is_none() && finding.is_some());
    let g = gate.lock().unwrap();
    assert!(g.diag_since.is_none() && g.finding_since.is_none());
}

#[test]
fn no_packets_unless_it_could_open() {
    let gate = Mutex::new(Gate::default());
    for f in [
        Fake {
            down: false,
            ..fake()
        },
        Fake {
            can_probe: false,
            ..fake()
        },
        Fake {
            premises: Premises {
                data_on: Some(false),
                ..fake().premises
            },
            ..fake()
        },
        Fake {
            premises: Premises {
                airplane: None,
                ..fake().premises
            },
            ..fake()
        },
        Fake {
            reg: Reg::Searching,
            ..fake()
        },
    ] {
        let v = check(&f, &gate).unwrap();
        assert!(!matches!(v, Verdict::Open(_)), "{v:?}");
        assert_eq!(f.probes.get(), 0, "{v:?}");
    }
}

#[test]
fn busy_gate_is_refused_without_probing() {
    let gate = Mutex::new(Gate {
        diag_since: Some(1),
        ..Gate::default()
    });
    let f = fake();
    assert_eq!(check(&f, &gate), Err(Busy("diagnose")));
    assert_eq!(gate.lock().unwrap().diag_since, Some(1));
    let gate = Mutex::new(Gate::default());
    let f = Fake {
        busy: Some("speedtest"),
        ..fake()
    };
    assert_eq!(check(&f, &gate), Err(Busy("speedtest")));
    assert_eq!(f.probes.get(), 0);
    assert!(gate.lock().unwrap().finding_since.is_none());
}

#[test]
fn data_down_still_probes_and_blames_data() {
    let gate = Mutex::new(Gate::default());
    let f = Fake {
        data: DataIn {
            connect_status: Some("disconnected".into()),
            ..DataIn::default()
        },
        internet: Some(Probe::Failed),
        ..fake()
    };
    match check(&f, &gate).unwrap() {
        Verdict::Open(x) => assert_eq!(x.layer, LayerId::Data),
        v => panic!("{v:?}"),
    }
}

#[test]
fn http_status_means_path_only() {
    assert!(any_status(b"HTTP/1.1 200 OK"));
    assert!(any_status(b"HTTP/1.1 302 Found"));
    assert!(!any_status(b"HTTP/1.1 2"));
    assert!(!any_status(b"SSH-2.0-dropbear"));
}

#[test]
fn expiry_or_transport_panic_releases_gate_and_cannot_make_a_sure_network_finding() {
    let gate = Mutex::new(Gate::default());
    let f = Fake {
        expired: true,
        ..fake()
    };
    assert_eq!(
        check(&f, &gate).unwrap(),
        Verdict::Unverified {
            why: "snapshot_expired"
        }
    );
    assert!(gate.lock().unwrap().finding_since.is_none());
    let f = Fake {
        panic_probe: true,
        ..fake()
    };
    assert!(matches!(
        check(&f, &gate).unwrap(),
        Verdict::Unverified { .. }
    ));
    assert!(gate.lock().unwrap().finding_since.is_none());
}
