use super::*;

/// Counts every call; `ifaces` answers `iface`.
#[derive(Default)]
struct Fake {
    ifaces: std::collections::BTreeMap<String, (u64, u64)>,
    iface_reads: usize,
    rings: Vec<[u8; ROW]>,
    files: std::collections::BTreeMap<String, Vec<u8>>,
    appends: usize,
    prunes: usize,
    qdisc: [Option<diag::Qdisc>; 2],
    qdisc_reads: usize,
    conns: Option<Vec<diag::Tcp>>,
    tcp_reads: usize,
    tcp_us: u32,
}

impl Io for Fake {
    fn iface(&mut self, name: &str) -> Option<(u64, u64)> {
        self.iface_reads += 1;
        self.ifaces.get(name).copied()
    }
    fn ring(&mut self, row: &[u8; ROW]) {
        self.rings.push(*row);
    }
    fn day_size(&mut self, day: &str) -> u64 {
        self.files.get(day).map_or(0, |f| f.len() as u64)
    }
    fn append(&mut self, day: &str, data: &[u8]) {
        self.appends += 1;
        self.files
            .entry(day.into())
            .or_default()
            .extend_from_slice(data);
    }
    fn prune(&mut self, _today: &str) {
        self.prunes += 1;
    }
    fn qdiscs(&mut self) -> [Option<diag::Qdisc>; 2] {
        self.qdisc_reads += 1;
        self.qdisc
    }
    fn tcp(&mut self) -> Option<(Vec<diag::Tcp>, u32)> {
        self.tcp_reads += 1;
        self.conns.clone().map(|c| (c, self.tcp_us))
    }
}

impl Fake {
    fn io_calls(&self) -> usize {
        self.iface_reads
            + self.rings.len()
            + self.appends
            + self.prunes
            + self.qdisc_reads
            + self.tcp_reads
    }
}

fn pre(bytes: u64, clients: Option<usize>) -> Pre {
    Pre {
        vendor: Some(Vendor {
            rx: bytes,
            tx: 0,
            ..Default::default()
        }),
        clients,
        radio: Radio {
            rat: "SA".into(),
            band: "n78".into(),
            pci: 1,
            nr_snr: 150,
            ..Default::default()
        },
    }
}

/// Feeds rounds of `rate` bytes/s every `step` ms; returns the vendor byte counter.
struct Drive {
    rec: Recorder,
    io: Fake,
    bytes: u64,
    rmnet: u64,
    at: u64,
    wall: i64,
}

impl Drive {
    fn new() -> Drive {
        Drive {
            rec: Recorder::default(),
            io: Fake::default(),
            bytes: 1_000,
            rmnet: 0,
            at: 1_000,
            wall: 1_790_000_000,
        }
    }
    fn round(&mut self, rate: u64, step: u64, clients: Option<usize>) {
        self.bytes += rate * step / 1000;
        self.at += step;
        self.wall += (step / 1000) as i64;
        let p = pre(self.bytes, clients);
        self.rec.tick(
            &mut self.io,
            p,
            Some((self.rmnet, 0)),
            100,
            self.at,
            self.wall,
        );
    }
    fn open(&self) -> bool {
        self.rec.trigger.open()
    }
}

#[test]
fn thresholds_are_bytes_per_second() {
    assert_eq!(ON_BPS * 8, 1_000_000);
    assert_eq!(OFF_BPS * 8, 500_000);
}

#[test]
fn starts_after_three_rounds_at_one_mbps() {
    let mut t = Trigger::default();
    assert_eq!(t.step(1000, Some(ON_BPS), Some(1)), Act::Idle);
    assert_eq!(t.step(2000, Some(ON_BPS), Some(1)), Act::Idle);
    assert_eq!(t.step(3000, Some(ON_BPS), Some(1)), Act::Start);
    // a dip breaks the streak
    let mut t = Trigger::default();
    t.step(1000, Some(ON_BPS), Some(1));
    t.step(2000, Some(ON_BPS - 1), Some(1));
    t.step(3000, Some(ON_BPS), Some(1));
    assert_eq!(t.step(4000, Some(ON_BPS), Some(1)), Act::Idle);
    assert_eq!(t.step(5000, Some(ON_BPS), Some(1)), Act::Start);
}

#[test]
fn no_clients_never_starts_unknown_counts_as_some() {
    let mut t = Trigger::default();
    for i in 0..5 {
        assert_eq!(t.step(i * 1000, Some(ON_BPS * 10), Some(0)), Act::Idle);
    }
    let mut t = Trigger::default();
    t.step(0, Some(ON_BPS), None);
    t.step(1000, Some(ON_BPS), None);
    assert_eq!(t.step(2000, Some(ON_BPS), None), Act::Start);
}

#[test]
fn invalid_round_breaks_streak_and_keeps_session() {
    let mut t = Trigger::default();
    t.step(0, Some(ON_BPS), Some(1));
    t.step(1000, Some(ON_BPS), Some(1));
    assert_eq!(t.step(2000, None, Some(1)), Act::Idle);
    assert_eq!(t.step(3000, Some(ON_BPS), Some(1)), Act::Idle);
    // open: an invalid round neither stops nor resets the low timer
    let mut t = Trigger::default();
    for i in 0..3 {
        t.step(i * 1000, Some(ON_BPS), Some(1));
    }
    assert_eq!(t.step(10_000, Some(0), Some(1)), Act::Keep);
    assert_eq!(t.step(40_000, None, Some(1)), Act::Keep);
    assert_eq!(t.step(70_000, Some(0), Some(1)), Act::Stop("idle"));
}

#[test]
fn stops_after_sixty_seconds_low_mixed_intervals() {
    let mut t = Trigger::default();
    for i in 0..3 {
        t.step(i * 1000, Some(ON_BPS), Some(1));
    }
    // 0.5 Mbps exactly is not low; between thresholds keeps the session
    assert_eq!(t.step(3000, Some(OFF_BPS), Some(1)), Act::Keep);
    // low from 4 s on, 1 s and 5 s rounds mixed
    let mut at = 4000;
    assert_eq!(t.step(at, Some(OFF_BPS - 1), Some(1)), Act::Keep);
    while at < 4000 + 55_000 {
        at += if at % 2000 == 0 { 5000 } else { 1000 };
        assert_eq!(
            t.step(at.min(63_999), Some(10), Some(1)),
            Act::Keep,
            "at {at}"
        );
    }
    // a round back at the start threshold restarts the 60 s
    assert_eq!(t.step(63_999, Some(ON_BPS), Some(1)), Act::Keep);
    assert_eq!(t.step(65_000, Some(0), Some(1)), Act::Keep);
    assert_eq!(t.step(124_999, Some(0), Some(1)), Act::Keep);
    assert_eq!(t.step(125_000, Some(0), Some(1)), Act::Stop("idle"));
}

#[test]
fn clients_gone_stops() {
    let mut t = Trigger::default();
    for i in 0..3 {
        t.step(i * 1000, Some(ON_BPS), Some(2));
    }
    assert_eq!(t.step(4000, Some(ON_BPS * 50), None), Act::Keep);
    assert_eq!(
        t.step(5000, Some(ON_BPS * 50), Some(0)),
        Act::Stop("clients")
    );
}

#[test]
fn two_hour_cap_then_three_fresh_rounds() {
    let mut t = Trigger::default();
    for i in 0..3 {
        t.step(i * 1000, Some(ON_BPS), Some(1));
    }
    let open = 2000;
    assert_eq!(
        t.step(open + CAP_MS - 1, Some(ON_BPS * 9), Some(1)),
        Act::Keep
    );
    assert_eq!(
        t.step(open + CAP_MS, Some(ON_BPS * 9), Some(1)),
        Act::Stop("cap")
    );
    let at = open + CAP_MS;
    assert_eq!(t.step(at + 1000, Some(ON_BPS * 9), Some(1)), Act::Idle);
    assert_eq!(t.step(at + 2000, Some(ON_BPS * 9), Some(1)), Act::Idle);
    assert_eq!(t.step(at + 3000, Some(ON_BPS * 9), Some(1)), Act::Start);
}

#[test]
fn idle_path_does_no_io() {
    let mut d = Drive::new();
    // phone on Wi-Fi, a trickle: 0.9 Mbps never opens, never reads chill0 or writes
    for _ in 0..200 {
        d.round(ON_BPS - 1, 5000, Some(1));
    }
    for _ in 0..200 {
        d.round(1000, 1000, Some(3));
    }
    // no clients, any rate below start
    for _ in 0..50 {
        d.round(0, 5000, Some(0));
    }
    assert!(!d.open());
    assert_eq!(d.io.io_calls(), 0);
    // counters going backwards (redial) and failed reads cost nothing either
    d.bytes = 0;
    d.round(0, 1000, Some(1));
    d.rec
        .tick(&mut d.io, Pre::default(), None, 0, d.at + 1000, d.wall + 1);
    assert_eq!(d.io.io_calls(), 0);
}

#[test]
fn device_local_traffic_does_not_trigger() {
    let mut d = Drive::new();
    // 5 Mbps on the vendor counter, all of it on rmnet_data0 and none on chill0 = the device
    d.io.ifaces.insert("chill0".into(), (0, 0));
    for _ in 0..10 {
        d.rmnet += ON_BPS * 5;
        d.round(ON_BPS * 5, 1000, Some(1));
    }
    assert!(!d.open());
    // the same through chill0 (proxied clients) opens
    let mut d = Drive::new();
    let mut c = 0;
    for _ in 0..5 {
        d.rmnet += ON_BPS * 5;
        c += ON_BPS * 5;
        d.io.ifaces.insert("chill0".into(), (c, 0));
        d.round(ON_BPS * 5, 1000, Some(1));
    }
    assert!(d.open());
}

#[test]
fn chill_off_device_traffic_does_not_trigger() {
    let mut d = Drive::new();
    // no chill0 at all: a device speed test is all rmnet_data0
    for _ in 0..20 {
        d.rmnet += ON_BPS * 8;
        d.round(ON_BPS * 8, 1000, Some(1));
    }
    assert!(!d.open());
    // clients via IPA (vendor only) still open
    for _ in 0..4 {
        d.round(ON_BPS * 8, 1000, Some(1));
    }
    assert!(d.open());
}

#[test]
fn session_rows_and_summaries() {
    let mut d = Drive::new();
    d.io.ifaces.insert("chill0".into(), (0, 0));
    d.io.ifaces.insert("br-lan".into(), (5, 6));
    // the first round has no baseline: three usable rounds = four
    for _ in 0..4 {
        d.round(ON_BPS * 4, 1000, Some(1));
    }
    assert!(d.open());
    for _ in 0..25 {
        d.round(ON_BPS * 4, 1000, Some(1));
    }
    // buffered: nothing on flash yet (< 4 KiB)
    assert_eq!(d.io.appends, 0);
    for _ in 0..61 {
        d.round(0, 1000, Some(1));
    }
    assert!(!d.open());
    assert_eq!(d.io.appends, 1);
    let text = String::from_utf8(d.io.files.values().next().unwrap().clone()).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.first().unwrap()["kind"], "start");
    let end = lines.last().unwrap();
    assert_eq!(end["kind"], "end");
    assert_eq!(end["reason"], "idle");
    assert_eq!(end["trimmed"], false);
    let sums: Vec<&Value> = lines.iter().filter(|l| l["kind"] == "sum").collect();
    assert!(sums.len() >= 8, "{}", sums.len());
    assert_eq!(sums[0]["rate_max_bps"], 4_000_000);
    assert_eq!(d.io.rings.len(), 1 + 25 + 61);
    let r = Row::decode(&d.io.rings[3]);
    assert_eq!(r.brlan, Some((5, 6)));
    assert_eq!(r.radio.band, "n78");
    assert_eq!(r.radio.nr_snr, 150);
    // after it closed, idle again costs nothing
    let before = d.io.io_calls();
    for _ in 0..100 {
        d.round(0, 5000, Some(1));
    }
    assert_eq!(d.io.io_calls(), before);
}

#[test]
fn row_roundtrip() {
    let r = Row {
        t_ms: 12,
        wall: 1_790_000_000,
        session: 1_789_999_000,
        window_ms: 5000,
        ev: EV_CELL | EV_CA,
        cell: Vendor {
            rx: 1,
            tx: 2,
            rx_pkts: 3,
            tx_pkts: 4,
            rx_speed: 5,
            tx_speed: 6,
        },
        rmnet: Some((7, 8)),
        chill0: None,
        brlan: Some((9, 10)),
        rate: 0,
        radio: Radio {
            rat: "NSA".into(),
            band: "n41".into(),
            bandwidth: String::new(),
            pci: 500,
            cell_id: 0x9_1234_5678,
            nr_snr: -35,
            lte_snr: i64::MIN,
            rsrp: -110,
        },
        cpu_tenths: 123,
        q_rmnet: Some(diag::Qdisc {
            drops: 1,
            overlimits: 2,
            requeues: 3,
            backlog: 4,
            qlen: 5,
        }),
        q_chill0: None,
        tcp: Some(diag::TcpSum {
            n: 9,
            active: 3,
            retx: None,
            segs: Some(400),
            rtt_p50: 30_000,
            rtt_max: 90_000,
            min_rtt: 12_000,
            cwnd_min: 10,
            swnd_min: diag::NA,
            dump_us: 700,
        }),
    };
    assert_eq!(Row::decode(&r.encode()), r);
    let none = Row {
        tcp: None,
        ..r.clone()
    };
    assert_eq!(Row::decode(&none.encode()).tcp, None);
    let j = row_json(&r, None);
    assert_eq!(j["ev"], json!(["cell", "ca"]));
    assert_eq!(j["lte_snr"], Value::Null);
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("netrec-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn ring_wraps_and_resumes_after_restart() {
    let d = tmp("ring");
    let path = d.join("ring");
    let mut fsio = Fs::new(&path, &d.join("dir"), &d);
    let n = slots() + 10;
    for i in 0..n {
        let r = Row {
            t_ms: i,
            session: 7,
            ..Default::default()
        };
        fsio.ring(&r.encode());
    }
    assert_eq!(fs::metadata(&path).unwrap().len(), RING_BYTES);
    // a new process picks up the header
    let mut again = Fs::new(&path, &d.join("dir"), &d);
    again.ring(
        &Row {
            t_ms: n,
            session: 8,
            ..Default::default()
        }
        .encode(),
    );
    let v = ring_rows(&path, Some(7), 0, 2000);
    assert_eq!(v["total"], slots() - 1);
    assert_eq!(v["rows"][0]["t"], 11);
    let v = ring_rows(&path, None, 0, 10);
    assert_eq!(v["session"], 8);
    assert_eq!(v["rows"][0]["t"], n);
    // paging
    let v = ring_rows(&path, Some(7), 100, 50);
    assert_eq!(v["rows"].as_array().unwrap().len(), 50);
    assert_eq!(v["more"], true);
    // a broken header starts over
    fs::write(&path, b"garbage").unwrap();
    let mut fresh = Fs::new(&path, &d.join("dir"), &d);
    fresh.ring(
        &Row {
            t_ms: 1,
            session: 9,
            ..Default::default()
        }
        .encode(),
    );
    assert_eq!(ring_rows(&path, None, 0, 10)["total"], 1);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn directory_cap_drops_oldest_never_today() {
    let mb = 1 << 20;
    let sizes: Vec<(String, u64)> = (1..=10)
        .map(|i| (format!("202610{i:02}.jsonl"), mb))
        .collect();
    let gone = doomed(&sizes, "20261010");
    assert_eq!(gone, vec!["20261001.jsonl", "20261002.jsonl"]);
    // today alone over the cap stays
    let sizes = vec![
        ("20261001.jsonl".to_string(), 3 * mb),
        ("20261010.jsonl".to_string(), 9 * mb),
    ];
    assert_eq!(doomed(&sizes, "20261010"), vec!["20261001.jsonl"]);
    // real files
    let d = tmp("prune");
    for (n, s) in [
        ("20261001.jsonl", 5 * mb),
        ("20261002.jsonl", 4 * mb),
        ("20261003.jsonl", 1),
    ] {
        fs::write(d.join(n), vec![b'x'; s as usize]).unwrap();
    }
    fs::write(d.join("notes.txt"), b"keep").unwrap();
    let mut fsio = Fs::new(&d.join("ring"), &d, &d);
    fsio.prune("20261003");
    assert_eq!(day_files(&d), vec!["20261002.jsonl", "20261003.jsonl"]);
    assert!(d.join("notes.txt").exists());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn daily_budget_keeps_marks_and_trims() {
    let mut d = Drive::new();
    d.io.ifaces.insert("chill0".into(), (0, 0));
    let today = day(d.wall + 1);
    d.io.files
        .insert(today.clone(), vec![b'x'; (DAY_CAP - 100) as usize]);
    for _ in 0..3 {
        d.round(ON_BPS * 4, 1000, Some(1));
    }
    for _ in 0..40 {
        d.round(ON_BPS * 4, 1000, Some(1));
    }
    d.round(ON_BPS, 1000, Some(0));
    assert!(!d.open());
    let f = &d.io.files[&today];
    let tail = String::from_utf8_lossy(&f[(DAY_CAP - 100) as usize..]).into_owned();
    assert!(!tail.contains("\"kind\":\"sum\""), "{tail}");
    assert!(tail.contains("\"kind\":\"start\""));
    assert!(tail.contains("\"kind\":\"end\""));
    assert!(tail.contains("\"trimmed\":true"));
}

#[test]
fn session_across_midnight_splits_files() {
    let mut d = Drive::new();
    // 23:59:40 device time
    d.wall = 1_790_035_180;
    d.io.ifaces.insert("chill0".into(), (0, 0));
    for _ in 0..60 {
        d.round(ON_BPS * 4, 1000, Some(1));
    }
    d.round(0, 1000, Some(0));
    assert_eq!(d.io.files.len(), 2);
    let days: Vec<&String> = d.io.files.keys().collect();
    assert!(days[0] < days[1]);
}

#[test]
fn no_addresses_anywhere() {
    let mut d = Drive::new();
    d.io.ifaces.insert("chill0".into(), (0, 0));
    for _ in 0..30 {
        d.round(ON_BPS * 4, 1000, Some(1));
    }
    d.round(0, 1000, Some(0));
    let mut all = String::new();
    for f in d.io.files.values() {
        all.push_str(&String::from_utf8_lossy(f));
    }
    for r in &d.io.rings {
        all.push_str(&row_json(&Row::decode(r), None).to_string());
        all.push('\n');
    }
    for l in all.lines() {
        let v: Value = serde_json::from_str(l).unwrap();
        let o = v.as_object().unwrap();
        for k in o.keys() {
            assert!(
                !["ip", "addr", "mac", "dst", "host", "iccid", "imsi"]
                    .iter()
                    .any(|b| k.split('_').any(|w| w == *b)),
                "{k}"
            );
        }
        for val in o.values() {
            if let Some(s) = val.as_str() {
                assert!(
                    s.split('.').count() < 4 && !s.contains(':') || s.contains(' '),
                    "{s}"
                );
            }
        }
    }
}

#[test]
fn readers_over_files() {
    let d = tmp("readers");
    let a = "{\"kind\":\"start\",\"session\":100,\"ts\":100}\n{\"kind\":\"sum\",\"session\":100,\"ts\":110,\"cell_bytes\":5}\n{\"kind\":\"end\",\"session\":100,\"ts\":120,\"reason\":\"idle\",\"duration_ms\":20000,\"bytes\":9,\"trimmed\":false}\n";
    // a session cut by a restart: no end line
    let b = "{\"kind\":\"start\",\"session\":200,\"ts\":200}\n{\"kind\":\"sum\",\"session\":200,\"ts\":210,\"cell_bytes\":7}\n{\"kind\":\"sum\",\"session\":200,\"ts\":220,\"cell_bytes\":3}\n{\"partial";
    fs::write(d.join("20261025.jsonl"), a).unwrap();
    fs::write(d.join("20261026.jsonl"), b).unwrap();
    let s = sessions(&d, 10);
    let list = s["sessions"].as_array().unwrap();
    assert_eq!(list[0]["session"], 200);
    assert_eq!(list[0]["end"], 220);
    assert_eq!(list[0]["closed"], false);
    assert_eq!(list[0]["bytes"], 10);
    assert_eq!(list[1]["closed"], true);
    assert_eq!(list[1]["bytes"], 9);
    let (body, next) = summary_lines(&d, 0, Some("20261025:0"), 2);
    assert_eq!(body.lines().count(), 2);
    let (rest, next2) = summary_lines(&d, 0, next.as_deref(), 100);
    assert_eq!(rest.lines().count(), 4, "{rest}");
    assert!(next2.is_none());
    assert!(!rest.contains("partial"));
    let (since, _) = summary_lines(&d, 215, Some("20261025:0"), 100);
    assert_eq!(since.lines().count(), 1);
    let (on, _) = doctor_detail(&d, true, 1_790_000_000);
    assert!(on.starts_with("开"));
    assert_eq!(doctor_detail(&d, false, 0).0, "关");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn pre_reads_vendor_clients_radio() {
    let t: Result<Value, String> =
        Ok(json!({"real_rx_bytes":"100","real_tx_bytes":5,"real_rx_packets":1}));
    let lan = Ok(json!({"lan_access_list_info":[{"mac":"aa"},{"ip":"x"}]}));
    let wifi = Ok(json!({"wireless_access_list_info":[{"mac_address":"b"},{"mac_addr":"c"}]}));
    let net =
        Ok(json!({"network_type":"SA","nr5g_pci":"301","nr5g_snr":"12.5","wan_active_band":"n78"}));
    let p = Pre::from_reads(&t, &lan, &wifi, &net);
    assert_eq!(p.vendor.unwrap().rx, 100);
    assert_eq!(p.vendor.unwrap().tx, 5);
    assert_eq!(p.clients, Some(3));
    assert_eq!(p.radio.pci, 301);
    assert_eq!(p.radio.nr_snr, 125);
    let p = Pre::from_reads(&Err("x".into()), &Err("x".into()), &wifi, &net);
    assert!(p.vendor.is_none());
    assert_eq!(p.clients, None);
}

// ---------------------------------------------------------------------------------------------
// Second batch: qdisc + TCP_INFO

fn qdisc_msg(ifindex: i32, parent: u32, q: [u32; 5], stats2: bool) -> Vec<u8> {
    let mut p = vec![0u8; 20];
    p[4..8].copy_from_slice(&ifindex.to_ne_bytes());
    p[12..16].copy_from_slice(&parent.to_ne_bytes());
    crate::nl::put_attr(&mut p, 1, b"mq\0");
    if stats2 {
        let mut gq = Vec::new();
        for v in q {
            gq.extend_from_slice(&v.to_ne_bytes());
        }
        let mut nest = Vec::new();
        crate::nl::put_attr(&mut nest, 1, &[0u8; 16]); // basic
        crate::nl::put_attr(&mut nest, 3, &gq);
        crate::nl::put_attr(&mut p, 7 | 0x8000, &nest);
    } else {
        // legacy tc_stats: bytes u64, packets, drops, overlimits, bps, pps, qlen, backlog
        let mut t = vec![0u8; 36];
        t[12..16].copy_from_slice(&q[2].to_ne_bytes());
        t[16..20].copy_from_slice(&q[4].to_ne_bytes());
        t[28..32].copy_from_slice(&q[0].to_ne_bytes());
        t[32..36].copy_from_slice(&q[1].to_ne_bytes());
        crate::nl::put_attr(&mut p, 3, &t);
    }
    p
}

#[test]
fn qdisc_parse_roots_only() {
    // [qlen, backlog, drops, requeues, overlimits]
    let root = qdisc_msg(5, 0xFFFF_FFFF, [1, 1500, 7, 2, 9], true);
    let child = qdisc_msg(5, 0x0001_0001, [0, 0, 99, 0, 0], true);
    let other = qdisc_msg(6, 0xFFFF_FFFF, [0, 0, 3, 0, 0], false);
    let (i, par, q) = diag::parse_qdisc(&root).unwrap();
    assert_eq!((i, par), (5, 0xFFFF_FFFF));
    assert_eq!(
        q,
        diag::Qdisc {
            drops: 7,
            overlimits: 9,
            requeues: 2,
            backlog: 1500,
            qlen: 1
        }
    );
    let v = diag::roots(&[child, root, other], &[Some(5), Some(6), None]);
    assert_eq!(v[0].unwrap().drops, 7); // the child's 99 is not added
    assert_eq!(v[1].unwrap().drops, 3); // legacy TCA_STATS
    assert_eq!(v[2], None);
    assert!(diag::parse_qdisc(&[0u8; 10]).is_none());
}

#[allow(clippy::too_many_arguments)]
fn tcp_msg(
    cookie: u64,
    dst: [u8; 4],
    rtt: u32,
    cwnd: u32,
    retrans: u32,
    segs: u32,
    idle: u32,
    full: bool,
) -> Vec<u8> {
    let mut p = vec![0u8; 72];
    p[0] = libc::AF_INET as u8;
    p[1] = 1;
    p[4..6].copy_from_slice(&40000u16.to_be_bytes());
    p[6..8].copy_from_slice(&443u16.to_be_bytes());
    p[24..28].copy_from_slice(&dst);
    p[44..52].copy_from_slice(&cookie.to_ne_bytes());
    let mut i = vec![0u8; if full { 232 } else { 104 }];
    let mut put = |at: usize, v: u32| i[at..at + 4].copy_from_slice(&v.to_ne_bytes());
    put(44, idle);
    put(52, idle + 10);
    put(68, rtt);
    put(80, cwnd);
    put(100, retrans);
    if full {
        put(148, rtt / 2);
        put(156, segs);
        put(228, 65535);
    }
    crate::nl::put_attr(&mut p, 2, &i);
    p
}

#[test]
fn tcp_info_parse_and_truncated() {
    let t = diag::parse_tcp(&tcp_msg(42, [1, 2, 3, 4], 30_000, 10, 5, 100, 0, true)).unwrap();
    assert_eq!((t.cookie, t.rtt_us, t.cwnd, t.retrans), (42, 30_000, 10, 5));
    assert_eq!(
        (t.min_rtt_us, t.segs_out, t.snd_wnd),
        (Some(15_000), Some(100), Some(65535))
    );
    assert_eq!(t.dport, 443);
    // an older kernel's shorter tcp_info: the late fields are None, the rest still parse
    let s = diag::parse_tcp(&tcp_msg(1, [1, 2, 3, 4], 9, 8, 7, 0, 0, false)).unwrap();
    assert_eq!((s.min_rtt_us, s.segs_out, s.snd_wnd), (None, None, None));
    assert_eq!(s.retrans, 7);
    // no INET_DIAG_INFO attribute
    assert!(diag::parse_tcp(&[0u8; 72]).is_none());
    let r = diag::tcp_request(libc::AF_INET as u8);
    assert_eq!((r[1], r[2]), (libc::IPPROTO_TCP as u8, 2));
    assert_eq!(u32::from_ne_bytes(r[4..8].try_into().unwrap()), 2);
}

#[test]
fn wan_filter() {
    let w = |s: &str| diag::wan(&s.parse().unwrap());
    for s in ["1.1.1.1", "203.0.113.9", "2001:db8::1", "198.20.0.1"] {
        assert!(w(s), "{s}");
    }
    for s in [
        "127.0.0.1",
        "192.168.0.5",
        "192.168.1.2",
        "172.20.0.1",
        "100.100.1.1",
        "198.18.0.7",
        "198.19.255.1",
        "169.254.1.1",
        "::1",
        "fe80::1",
        "fd7a:115c::1",
    ] {
        assert!(!w(s), "{s}");
    }
}

#[test]
fn tcp_summary_deltas_by_cookie() {
    let c = |cookie, dst, rtt, cwnd, retrans, segs, idle| {
        diag::parse_tcp(&tcp_msg(cookie, dst, rtt, cwnd, retrans, segs, idle, true)).unwrap()
    };
    let first = vec![
        c(1, [1, 1, 1, 1], 40_000, 20, 3, 1000, 0),
        c(2, [8, 8, 8, 8], 10_000, 50, 0, 10, 60_000), // idle: counted, not in rtt stats
        c(3, [10, 0, 0, 2], 1, 1, 99, 99, 0),          // LAN: ignored
    ];
    let (s, map) = diag::summarize(&first, None, 300);
    assert_eq!((s.n, s.active, s.retx, s.segs), (2, 1, None, None));
    assert_eq!((s.rtt_p50, s.min_rtt, s.cwnd_min), (40_000, 20_000, 20));
    assert_eq!(s.dump_us, 300);
    // conn 2 closed, conn 1 retransmitted 2 more, conn 4 is new with 1
    let second = vec![
        c(1, [1, 1, 1, 1], 60_000, 8, 5, 1500, 0),
        c(4, [9, 9, 9, 9], 20_000, 30, 1, 40, 0),
    ];
    let (s, map2) = diag::summarize(&second, Some(&map), 300);
    assert_eq!((s.n, s.active, s.retx, s.segs), (2, 2, Some(3), Some(540)));
    assert_eq!((s.rtt_max, s.cwnd_min, s.swnd_min), (60_000, 8, 65535));
    assert_eq!(map2.len(), 2);
    // nothing active
    let (s, _) = diag::summarize(&[], Some(&map2), 1);
    assert_eq!((s.n, s.rtt_p50, s.retx), (0, diag::NA, Some(0)));
}

#[test]
fn session_qdisc_and_tcp_cadence() {
    let mut d = Drive::new();
    d.io.ifaces.insert("chill0".into(), (0, 0));
    d.io.qdisc = [
        Some(diag::Qdisc::default()),
        Some(diag::Qdisc {
            drops: 10,
            backlog: 3000,
            ..Default::default()
        }),
    ];
    d.io.conns = Some(vec![
        diag::parse_tcp(&tcp_msg(1, [1, 1, 1, 1], 40_000, 20, 3, 1000, 0, true)).unwrap(),
    ]);
    // open a session without CPU-path traffic: qdisc every row, no TCP dump
    for _ in 0..5 {
        d.round(ON_BPS * 4, 1000, Some(1));
    }
    assert!(d.open());
    let rows0 = d.io.rings.len();
    assert_eq!(d.io.qdisc_reads, rows0);
    assert_eq!(d.io.tcp_reads, 0);
    // CPU-path traffic: a dump at most every 5 s
    for k in 0..12 {
        d.rmnet += 100_000;
        if k == 6 {
            d.io.qdisc[1].as_mut().unwrap().drops = 25;
        }
        d.round(ON_BPS * 4, 1000, Some(1));
    }
    assert_eq!(d.io.tcp_reads, 3, "at 0, 5, 10 s");
    let rows: Vec<Row> = d.io.rings.iter().map(|b| Row::decode(b)).collect();
    let sampled: Vec<&Row> = rows.iter().filter(|r| r.tcp.is_some()).collect();
    assert_eq!(sampled.len(), 3);
    assert_eq!(sampled[0].tcp.unwrap().retx, None);
    assert_eq!(sampled[1].tcp.unwrap().retx, Some(0));
    let j = row_json(&rows[rows0 + 6], Some(&rows[rows0 + 5]));
    assert_eq!(j["d_q_chill0_drops"], 15);
    assert_eq!(j["q_chill0"]["backlog"], 3000);
    // close: the summaries carry the window's drops and tcp aggregates
    for _ in 0..61 {
        d.round(0, 1000, Some(1));
    }
    assert!(!d.open());
    let text = String::from_utf8(d.io.files.values().next().unwrap().clone()).unwrap();
    let sums: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|l| l["kind"] == "sum")
        .collect();
    let drops: u64 = sums
        .iter()
        .filter_map(|s| s["q_chill0_drops"].as_u64())
        .sum();
    assert_eq!(drops, 15);
    assert!(sums.iter().any(|s| s["tcp_rtt_p50_us"] == 40_000));
    assert!(sums.iter().any(|s| s["tcp_min_rtt_us"] == 20_000));
    // idle again: no qdisc or tcp reads
    let before = d.io.io_calls();
    for _ in 0..50 {
        d.round(0, 5000, Some(1));
    }
    assert_eq!(d.io.io_calls(), before);
}

#[test]
fn slow_dump_stretches_cadence() {
    let mut d = Drive::new();
    d.io.conns = Some(vec![]);
    d.io.tcp_us = TCP_SLOW_US + 1;
    for _ in 0..5 {
        d.round(ON_BPS * 4, 1000, Some(1));
    }
    assert!(d.open());
    for _ in 0..21 {
        d.rmnet += 1;
        d.round(ON_BPS * 4, 1000, Some(1));
    }
    assert_eq!(d.io.tcp_reads, 3, "at 0, 10, 20 s");
}

/// Real netlink sockets (run in the build container; skipped where netlink is unavailable).
#[test]
fn real_netlink_qdisc_and_tcp_info() {
    use std::io::{Read, Write as _};
    let Ok(mut route) = crate::nl::NlSock::open(crate::nl::NETLINK_ROUTE, 256 << 10) else {
        eprintln!("no NETLINK_ROUTE here, skipped");
        return;
    };
    let p = diag::dump_qdiscs(&mut route).unwrap();
    assert!(
        p.iter().all(|m| diag::parse_qdisc(m).is_some()),
        "every qdisc parses"
    );
    let lo = diag::ifindex("lo");
    eprintln!(
        "qdiscs: {}, lo root: {:?}",
        p.len(),
        diag::roots(&p, &[lo])[0]
    );

    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let (mut sv, _) = l.accept().unwrap();
    c.write_all(&[7u8; 4096]).unwrap();
    let mut buf = [0u8; 4096];
    sv.read_exact(&mut buf).unwrap();
    let mut sd = crate::nl::NlSock::open(crate::nl::NETLINK_SOCK_DIAG, 1 << 20).unwrap();
    let all = diag::dump_tcp(&mut sd).unwrap();
    let mine = all
        .iter()
        .find(|t| t.dport == port)
        .expect("the client socket");
    eprintln!("{mine:?}");
    assert!(mine.cwnd > 0);
    assert!(mine.rtt_us > 0);
    assert!(mine.segs_out.unwrap_or(0) >= 1);
    assert!(mine.idle_ms <= diag::ACTIVE_MS);
    // loopback never reaches the summary
    let (s, _) = diag::summarize(&all, None, 0);
    assert!(all.iter().filter(|t| diag::wan(&t.dst)).count() == usize::from(s.n));
    // the Fs seam answers the same way
    let mut f = Fs::new(
        Path::new("/nonexistent"),
        Path::new("/nonexistent"),
        Path::new("/sys/class/net"),
    );
    assert!(f.tcp().is_some());
    let _ = f.qdiscs();
}
