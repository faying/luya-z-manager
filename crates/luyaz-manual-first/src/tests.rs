//! The runtime's tests (the machine's own are u60-policy's `manual_first/tests.rs`).

use super::*;

use std::sync::atomic::{AtomicU32, Ordering};

const JP: &str = "🇯🇵 日本";
const NODE: &str = "JP 03";

#[derive(Default)]
struct Fake {
    in_use: bool,
    probes: Vec<Option<Probe>>,
    cell: bool,
    direct: Option<u32>,
    region: Option<String>,
    retest: Option<(String, bool)>,
    selected: Vec<String>,
    dropped: u32,
    saved: u32,
    probed: u32,
}

impl Io for Fake {
    fn in_use(&mut self) -> bool {
        self.in_use
    }
    fn probe(&mut self, _n: &str) -> Option<Probe> {
        self.probed += 1;
        if self.probes.is_empty() { Some(Probe::Ms(100)) } else { self.probes.remove(0) }
    }
    fn cell_alive(&mut self) -> bool {
        self.cell
    }
    fn direct_ms(&mut self) -> Option<u32> {
        self.direct
    }
    fn region_of(&mut self, _n: &str) -> Option<String> {
        self.region.clone()
    }
    fn retest(&mut self, _g: &str) -> Option<(String, bool)> {
        self.retest.clone()
    }
    fn select_main(&mut self, m: &str) -> bool {
        self.selected.push(m.to_string());
        true
    }
    fn drop_manual_conns(&mut self) {
        self.dropped += 1;
    }
    fn save(&mut self, _p: &Persist) {
        self.saved += 1;
    }
}

fn healthy() -> Fake {
    Fake {
        in_use: true,
        cell: true,
        direct: Some(60),
        region: Some(JP.into()),
        retest: Some(("JP 01".into(), true)),
        ..Default::default()
    }
}

/// A host whose mihomo is nowhere (nothing listens on port 9 here); `active` for `ticks` checks.
struct FakeHost {
    ticks: AtomicU32,
    running: AtomicU32,
    slept: AtomicU32,
}

impl FakeHost {
    fn new(ticks: u32) -> Self {
        FakeHost { ticks: AtomicU32::new(ticks), running: AtomicU32::new(0), slept: AtomicU32::new(0) }
    }
}

impl Host for FakeHost {
    fn mihomo_api(&self) -> String {
        "http://127.0.0.1:9".into()
    }
    fn chill_running(&self) -> bool {
        self.running.fetch_add(1, Ordering::SeqCst);
        false
    }
    fn cell_alive(&self) -> bool {
        true
    }
    fn direct_dns_ms(&self) -> Option<u32> {
        Some(50)
    }
    fn tick(&self, _record: Value) {}
    fn active(&self) -> bool {
        let left = self.ticks.load(Ordering::SeqCst);
        left > 0 && self.ticks.compare_exchange(left, left - 1, Ordering::SeqCst, Ordering::SeqCst).is_ok()
    }
    fn sleep(&self, _d: Duration) {
        self.slept.fetch_add(1, Ordering::SeqCst);
    }
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("u60-manual-first-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Records as u60d gets them: every tick of a run goes through the recorder, the record
/// parses into u60-policy's types, and replaying it there gives the machine after.
#[test]
fn shadow_records_replay_in_u60_policy() {
    use luyaz_policy::manual_first as pol;
    let mut m = Machine::default();
    let mut io = healthy();
    io.probes = vec![Some(Probe::Timeout), Some(Probe::Ms(900)), Some(Probe::Timeout), None];
    let mut published = 0;
    let mut t = 1000;
    for i in 0..40 {
        // on 🎯 until it leaves, then on the backup (back after 10 good minutes), then off
        let main = if i < 3 || (24..32).contains(&i) { MANUAL_GROUP } else if i < 24 { JP } else { "DIRECT" };
        let o = Obs { now: t, active: i % 9 != 8, main_now: main.into(), manual_now: NODE.into() };
        let before = m.clone();
        let mut rec = Recorder { inner: &mut io, events: Vec::new() };
        m.tick(&o, &mut rec);
        let events = rec.events;
        let text = tick_record(&o, Some((main.into(), before.needs_detail(main))), &before, &events, &m, 0).to_string();
        let v: Value = serde_json::from_str(&text).unwrap();
        let b: pol::Machine = serde_json::from_value(v["before"].clone()).unwrap();
        let a: pol::Machine = serde_json::from_value(v["after"].clone()).unwrap();
        let obs: pol::Obs = serde_json::from_value(v["obs"].clone()).unwrap();
        let evs: Vec<pol::Event> = serde_json::from_value(v["io"].clone()).unwrap();
        let out = pol::replay(&b, &obs, evs);
        assert_eq!((out.diverged, out.unused), (None, 0), "tick {i}");
        assert_eq!(out.after, a, "tick {i}");
        assert_eq!(serde_json::to_value(&out.after).unwrap(), v["after"], "tick {i}: same JSON");
        assert_eq!(b.needs_detail(main), v["detail"]["needs"].as_bool().unwrap());
        published += worth_a_record(&before, &m, &events) as u32;
        t += 60;
    }
    assert_eq!(io.selected, vec![JP, MANUAL_GROUP], "left and came back");
    assert!(published > 0 && published < 40, "{published}");
}

#[test]
fn idle_ticks_are_not_recorded() {
    let m = Machine::default();
    assert!(!worth_a_record(&m, &m, &[]));
    assert!(!worth_a_record(&m, &m, &[json!({"call": "in_use", "got": false})]));
    assert!(worth_a_record(&m, &m, &[json!({"call": "in_use", "got": true})]));
    let mut n = m.clone();
    n.bad = 1;
    assert!(worth_a_record(&m, &n, &[]), "counts reset/changed without I/O");
}

#[test]
fn exit_switch_remembers_manual_while_on_backup() {
    let mf = ManualFirst::new(Arc::new(FakeHost::new(0)), "/nonexistent/manual-first.json");
    assert_eq!(mf.backup(), "", "nothing published yet");
    mf.publish(&Persist { backup: JP.into(), ..Default::default() });
    assert_eq!(luyaz_api::chill::main_to_remember(JP, &mf.backup()), MANUAL_GROUP);
    assert_eq!(luyaz_api::chill::main_to_remember("🇹🇼 台湾", &mf.backup()), "🇹🇼 台湾");
    mf.publish(&Persist::default());
    assert_eq!(luyaz_api::chill::main_to_remember(JP, &mf.backup()), JP);
}

#[test]
fn provider_map_skips_groups() {
    let all = json!({"providers": {
        "snap1": {"vehicleType": "File", "proxies": [{"name": "JP 03"}]},
        "🚀 节点选择": {"vehicleType": "Compatible", "proxies": [{"name": "JP 03"}]},
        "sub": {"vehicleType": "HTTP", "proxies": [{"name": "US 1"}]},
    }});
    let m = node_providers(&all);
    assert_eq!(m.get("JP 03").map(String::as_str), Some("snap1"));
    assert_eq!(m.get("US 1").map(String::as_str), Some("sub"));
    assert_eq!(m.len(), 2);
}

#[test]
fn connection_filters() {
    let c = json!({"connections": [
        {"id": "a", "upload": 10, "chains": [NODE, MANUAL_GROUP, MAIN_GROUP], "metadata": {"sourceIP": "192.168.0.5"}},
        {"id": "b", "upload": 5, "chains": [NODE, JP, "🤖 AI"], "metadata": {"sourceIP": "192.168.0.5"}},
        {"id": "c", "upload": 7, "chains": ["X", JP, MAIN_GROUP], "metadata": {"sourceIP": "127.0.0.1"}},
    ]});
    assert_eq!(manual_conn_ids(&c), vec!["a"], "AI connection on the same node is left alone");
    let up = main_uploads(&c);
    assert_eq!(up.len(), 1, "the device's own 127.0.0.1 lookups don't count");
    assert!(sent_since(&HashMap::new(), &up));
    assert!(!sent_since(&up, &up));
    let mut more = up.clone();
    more.insert("a".into(), 11);
    assert!(sent_since(&up, &more));
}

/// What the loop saves is what the other side reads back, and the summary/backup from the file
/// equal the published ones; a missing or broken file reads as nothing taken over.
#[test]
fn state_file_round_trip_and_read_helpers() {
    let d = dir("file");
    let path = d.join("manual-first.json");
    assert_eq!(read_persist(&path), Persist::default(), "missing");
    fs::write(&path, "{not json").unwrap();
    assert_eq!(read_persist(&path), Persist::default(), "unparsable");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

    let host: Arc<dyn Host> = Arc::new(FakeHost::new(0));
    let mut io = Mihomo::new(&host, &path);
    let p = Persist {
        backup: JP.into(),
        manual: NODE.into(),
        switched_at: 1000,
        switches: vec![1000],
        notice: "手选的 JP 03 不通，已临时换到 🇯🇵 日本（自动）".into(),
        notice_en: "JP 03 is down; using 🇯🇵 日本 for now (auto)".into(),
        notice_at: 1000,
        notice_kind: "backup".into(),
        ..Default::default()
    };
    io.save(&p);
    assert_eq!(read_persist(&path), p);
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640, "mode kept");
    assert!(!d.join("manual-first.json.tmp").exists());

    let mf = ManualFirst::new(host, &path);
    mf.publish(&p);
    let from_file = read_persist(&path);
    assert_eq!(backup_of(&from_file), mf.backup());
    assert_eq!(summary_of(&from_file, 1001), summary_of(&p, 1001));
    assert_eq!(summary_of(&from_file, 1001)["backup"], JP);
    let _ = fs::remove_dir_all(&d);
}

/// The loop reads the file at start and publishes it, checks `active` before each observation,
/// and with CHILL off does nothing but sleep.
#[test]
fn run_publishes_the_file_and_stops_when_handed_over() {
    let d = dir("run");
    let path = d.join("manual-first.json");
    let p = Persist { backup: JP.into(), manual: NODE.into(), ..Default::default() };
    fs::write(&path, serde_json::to_vec(&p).unwrap()).unwrap();
    let host = Arc::new(FakeHost::new(3));
    let mf = ManualFirst::new(host.clone(), &path);
    mf.run();
    assert_eq!(mf.backup(), JP);
    assert_eq!(host.running.load(Ordering::SeqCst), 3, "observed once per active tick");
    assert_eq!(host.slept.load(Ordering::SeqCst), 3);
    assert_eq!(read_persist(&path), p, "nothing written while CHILL is off");

    let host = Arc::new(FakeHost::new(0));
    ManualFirst::new(host.clone(), &path).run();
    assert_eq!(host.running.load(Ordering::SeqCst), 0, "inactive: not one observation");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn notice_logged_when_set_not_when_cleared() {
    let set = |at: u64, text: &str| Persist { notice: text.into(), notice_at: at, ..Default::default() };
    assert_eq!(notice_set(&Persist::default(), &set(10, "x")), Some("x"));
    assert_eq!(notice_set(&set(10, "x"), &set(70, "x")), Some("x"), "the same notice again, a minute later");
    assert_eq!(notice_set(&set(10, "x"), &set(10, "x")), None);
    assert_eq!(notice_set(&set(10, "x"), &Persist::default()), None, "cleared");
    // through a real tick: leaving says it, the owner taking over clears it silently
    let mut m = Machine::default();
    let mut io = healthy();
    io.probes = vec![Some(Probe::Timeout); 2];
    let o = |now, main: &str| Obs { now, active: true, main_now: main.into(), manual_now: NODE.into() };
    m.tick(&o(1000, MANUAL_GROUP), &mut io);
    let before = m.clone();
    m.tick(&o(1060, MANUAL_GROUP), &mut io);
    assert!(notice_set(&before.p, &m.p).is_some_and(|t| t.contains(NODE)));
    let before = m.clone();
    m.tick(&o(1120, "DIRECT"), &mut io);
    assert_eq!(notice_set(&before.p, &m.p), None);
}
