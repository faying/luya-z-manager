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
}

impl Fake {
    fn io_calls(&self) -> usize {
        self.iface_reads + self.rings.len() + self.appends + self.prunes
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
    };
    assert_eq!(Row::decode(&r.encode()), r);
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
