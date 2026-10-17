use super::*;
use std::cell::Cell;
use std::io::{Read, Write};
use std::net::TcpListener;

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
    fn operator_dns(&self) -> Vec<String> {
        vec!["10.0.0.1".into()]
    }
    fn internet_probe(&self) -> Option<Probe> {
        self.probes.set(self.probes.get() + 1);
        self.internet
    }
    fn dns_probe(&self, _: &[String]) -> Option<Probe> {
        self.probes.set(self.probes.get() + 1);
        self.dns
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
    }
}

#[test]
fn proven_fault_opens_and_frees_the_gate() {
    let gate = Mutex::new(Gate::default());
    let f = fake();
    match check(&f, &gate).unwrap() {
        Verdict::Open(x) => assert_eq!((x.layer, x.confidence), (LayerId::Dns, Confidence::Sure)),
        v => panic!("{v:?}"),
    }
    assert_eq!(f.probes.get(), 2);
    assert!(gate.lock().unwrap().diag_since.is_none());
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
    assert!(gate.lock().unwrap().diag_since.is_none());
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

fn serve_once(reply: &'static [u8]) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let a = l.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut b = [0u8; 256];
            let _ = s.read(&mut b);
            let _ = s.write_all(reply);
        }
    });
    a
}

#[test]
fn http_204_reads_the_status_line() {
    let a = serve_once(b"HTTP/1.1 204 No Content\r\n\r\n");
    assert!(matches!(
        http_204(a, "connectivitycheck.example"),
        Probe::Ok { .. }
    ));
    let a = serve_once(b"HTTP/1.1 302 Found\r\nLocation: http://portal/\r\n\r\n");
    assert_eq!(http_204(a, "x"), Probe::Failed);
    // nothing listening
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let a = l.local_addr().unwrap();
    drop(l);
    assert_eq!(http_204(a, "x"), Probe::Failed);
    assert!(status_204(b"HTTP/1.0 204 "));
    assert!(!status_204(b"HTTP/1.1 200 OK"));
}

#[test]
fn any_status_line_counts_for_the_internet_targets() {
    assert!(any_status(b"HTTP/1.1 200 OK"));
    assert!(any_status(b"HTTP/1.1 301 Moved"));
    assert!(!any_status(b"HTTP/1.1 2"));
    assert!(!any_status(b"SSH-2.0-dropbear"));
    assert!(INTERNET_TARGETS
        .iter()
        .all(|(a, h, p)| a.parse::<SocketAddr>().is_ok()
            && a.starts_with(h)
            && p.starts_with('/')));
}
