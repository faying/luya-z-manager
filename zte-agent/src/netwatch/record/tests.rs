use super::*;
use serde_json::json;

/// 2026-10-02 12:00:00 on the device clock.
const T0: u64 = 1_790_942_400;

fn sa_state(rsrp: i64) -> Value {
    json!({
        "net": {
            "type": "SA", "band": "n78", "wan_status": "ipv4_ipv6_connected",
            "nr_rsrp": rsrp, "nr_rsrq": -11, "nr_snr": "17.5", "nr_pci": 312, "nr_cell_id": 5_242_881_234i64,
            "nr_channel": 633_984, "lte_rsrp": -95, "lte_rsrq": -9, "lte_snr": "12.0", "lte_pci": 7, "lte_cell_id": 99,
            "mcc": 460, "mnc": 11, "nrca": "1,78,100;1,78,60", "lteca": "", "HSR": false
        },
        "qos": {"qci": 9, "ambr_dl": "1000000", "ambr_ul": "200000"}
    })
}

fn screen(state: &str) -> Value {
    json!({"v": 1, "net": {"story": {"state": state}}})
}

fn rd(at: u64, state: &Value, verdict: &str, rx: u64) -> Reading {
    Reading::parse(at, Some(state), Some(&screen(verdict)), Some(rx))
}

fn kinds(ev: &[Event]) -> Vec<&str> {
    ev.iter().map(|e| e.kind.as_str()).collect()
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("netwatch-record-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    d
}

// ---- parsing ----------------------------------------------------------------

#[test]
fn parse_sa_ignores_leftover_lte() {
    let r = rd(T0, &sa_state(-90), "ok", 0);
    assert!(r.ok && r.connected);
    assert_eq!(r.rat, "SA");
    assert_eq!((r.pci, r.cell, r.channel), (Some(312), Some(5_242_881_234), Some(633_984)));
    assert_eq!(r.plmn.as_deref(), Some("460-11"));
    assert_eq!((r.nr_rsrp, r.nr_sinr, r.nr_rsrq), (Some(-90.0), Some(17.5), Some(-11.0)));
    assert_eq!((r.lte_rsrp, r.lte_sinr, r.lteca), (None, None, None));
    assert_eq!(r.nrca, Some(2));
    assert_eq!(r.ambr.as_deref(), Some("1000000/200000"));
    assert_eq!(r.verdict.as_deref(), Some("ok"));
}

#[test]
fn parse_nsa_lte_and_3g() {
    let mut s = sa_state(-90);
    s["net"]["type"] = "NSA".into();
    s["net"]["nr_rsrp"] = 0.into();
    s["net"]["nr_snr"] = "--".into();
    let r = Reading::parse(T0, Some(&s), None, None);
    assert_eq!(r.rat, "NSA");
    // NSA: the anchor LTE cell is the serving one
    assert_eq!((r.pci, r.cell), (Some(7), Some(99)));
    assert_eq!((r.nr_rsrp, r.nr_sinr), (None, None));
    assert_eq!(r.lte_rsrp, Some(-95.0));
    assert_eq!(r.verdict, None);

    s["net"]["type"] = "WCDMA".into();
    let r = Reading::parse(T0, Some(&s), None, None);
    assert_eq!(r.rat, "3G");
    assert_eq!((r.pci, r.cell, r.channel, r.nr_rsrp, r.lte_rsrp), (None, None, None, None, None));

    s["net"]["type"] = "LTE".into();
    s["net"]["lte_cell_id"] = 0.into();
    s["net"]["mcc"] = 0.into();
    let r = Reading::parse(T0, Some(&s), None, None);
    assert_eq!((r.rat, r.cell, r.plmn), ("LTE", None, None));
}

#[test]
fn parse_unreadable() {
    let r = Reading::parse(T0, None, Some(&screen("ok")), Some(5));
    assert!(!r.ok);
    assert_eq!(r.verdict.as_deref(), Some("ok"));
    let r = Reading::parse(T0, Some(&json!({"net": {}})), None, None);
    assert!(!r.ok);
    assert_eq!(r.plmn, None);
    let mut s = sa_state(-90);
    s["net"]["mnc"] = 0.into();
    s["net"]["mcc"] = 460.into();
    assert_eq!(Reading::parse(T0, Some(&s), None, None).plmn.as_deref(), Some("460-00"));
}

// ---- minutes ----------------------------------------------------------------

#[test]
fn minute_summary() {
    let mut rec = Recorder::default();
    let s = sa_state(-90);
    let mut rx = 0;
    for k in 0..12 {
        let mut st = s.clone();
        st["net"]["nr_rsrp"] = (-90 - k).into();
        rx += if k == 3 { 12_500_000 } else { 1000 };
        let v = if k < 8 { "ok" } else { "weak" };
        let (_, closed) = rec.feed(&rd(T0 + k as u64 * 5, &st, v, rx));
        assert!(closed.is_none());
    }
    let (_, closed) = rec.feed(&rd(T0 + 60, &s, "ok", rx));
    let m = closed.unwrap();
    assert_eq!((m.t, m.rat.as_str(), m.n, m.nr_n, m.lte_n), (T0, "SA", 12, 12, 0));
    assert_eq!((m.nr_rsrp_min, m.nr_rsrp_max, m.nr_rsrp_avg), (Some(-101.0), Some(-90.0), Some(-95.5)));
    assert_eq!((m.cell, m.plmn.as_deref(), m.ch), (Some(5_242_881_234), Some("460-11"), Some(633_984)));
    assert_eq!(m.verdict, "ok");
    // 12.5 MB in 5 s = 20 Mbps
    assert_eq!(m.rx_peak_mbps, Some(20.0));
    assert_eq!(m.down_s, 0);
    assert_eq!(rec.minutes.len(), 1);
}

#[test]
fn minute_with_datad_down() {
    let mut rec = Recorder::default();
    for k in 0..12 {
        let r = if k < 4 { rd(T0 + k * 5, &sa_state(-90), "ok", 0) } else { Reading::parse(T0 + k * 5, None, None, None) };
        rec.feed(&r);
    }
    let (_, m) = rec.feed(&Reading::parse(T0 + 60, None, None, None));
    let m = m.unwrap();
    assert_eq!((m.n, m.verdict.as_str()), (4, "ok"));

    let mut rec = Recorder::default();
    for k in 0..12 {
        rec.feed(&Reading::parse(T0 + k * 5, None, None, None));
    }
    let (ev, m) = rec.feed(&Reading::parse(T0 + 60, None, None, None));
    let m = m.unwrap();
    assert_eq!((m.n, m.verdict.as_str(), m.rat.as_str()), (0, "-", "none"));
    // never had datad, so nothing was "lost"
    assert!(ev.is_empty() && rec.events.is_empty());
}

#[test]
fn nsa_with_nr_coming_and_going() {
    let mut rec = Recorder::default();
    let mut s = sa_state(-90);
    s["net"]["type"] = "NSA".into();
    for k in 0..12u64 {
        let mut st = s.clone();
        if k % 2 == 1 {
            st["net"]["nr_rsrp"] = 0.into();
            st["net"]["nr_snr"] = "--".into();
        }
        rec.feed(&rd(T0 + k * 5, &st, "ok", 0));
    }
    let (_, m) = rec.feed(&rd(T0 + 60, &s, "ok", 0));
    let m = m.unwrap();
    assert_eq!((m.nr_n, m.lte_n, m.n), (6, 12, 12));
    assert_eq!((m.lte_rsrp_avg, m.nr_rsrp_avg), (Some(-95.0), Some(-90.0)));
}

#[test]
fn down_seconds() {
    let mut rec = Recorder::default();
    let mut off = sa_state(-90);
    off["net"]["wan_status"] = "disconnected".into();
    let on = sa_state(-90);
    for k in 0..12u64 {
        rec.feed(&rd(T0 + k * 5, if k >= 6 { &off } else { &on }, "nodata", 0));
    }
    let (_, m) = rec.feed(&rd(T0 + 60, &sa_state(-90), "ok", 0));
    assert_eq!(m.unwrap().down_s, 30);
}

#[test]
fn keeps_at_most_a_day_of_minutes_and_2000_events() {
    let mut rec = Recorder::default();
    for k in 0..(KEEP_MINUTES as u64 + 10) * 2 {
        rec.feed(&rd(T0 + k * 30, &sa_state(-90), "ok", 0));
    }
    assert_eq!(rec.minutes.len(), KEEP_MINUTES);
    for k in 0..KEEP_EVENTS as u64 + 5 {
        rec.push_event(Event::new(T0 + k, "diagnose", json!({"k": k})));
    }
    assert_eq!(rec.events.len(), KEEP_EVENTS);
    assert_eq!(rec.events.front().unwrap().data["k"], 5);
}

#[test]
fn clock_jump_drops_the_minute_and_timers() {
    let mut rec = Recorder::default();
    let mut off = sa_state(-90);
    off["net"]["wan_status"] = "disconnected".into();
    rec.feed(&rd(1000, &sa_state(-90), "ok", 0));
    rec.feed(&rd(1005, &off, "nodata", 0));
    // SNTP sets the clock
    let (ev, closed) = rec.feed(&rd(T0, &sa_state(-90), "ok", 0));
    assert!(closed.is_none(), "no minute across a jump");
    let up = ev.iter().find(|e| e.kind == "data_up").unwrap();
    assert!(!up.data.contains_key("down_s"), "no duration across a jump: {up:?}");
}

// ---- events -----------------------------------------------------------------

#[test]
fn first_reading_makes_no_events() {
    let mut rec = Recorder::default();
    let (ev, _) = rec.feed(&rd(T0, &sa_state(-120), "stall", 0));
    assert!(ev.is_empty());
}

#[test]
fn data_down_and_up_with_duration() {
    let mut rec = Recorder::default();
    let mut off = sa_state(-90);
    off["net"]["wan_status"] = "disconnected".into();
    rec.feed(&rd(T0, &sa_state(-90), "ok", 0));
    let (ev, _) = rec.feed(&rd(T0 + 5, &off, "nodata", 0));
    assert_eq!(kinds(&ev), ["data_down"]);
    rec.feed(&rd(T0 + 10, &off, "nodata", 0));
    // datad gone for a bit: still compares with the last good reading
    rec.feed(&Reading::parse(T0 + 15, None, None, None));
    let (ev, _) = rec.feed(&rd(T0 + 43, &sa_state(-90), "ok", 0));
    assert_eq!(kinds(&ev), ["datad_back", "data_up"]);
    assert_eq!(ev[1].data["down_s"], 38);
    assert_eq!(kinds(&rec.events.iter().cloned().collect::<Vec<_>>()), ["data_down", "datad_lost", "datad_back", "data_up"]);
}

#[test]
fn stall_and_stall_end_from_the_verdict() {
    let mut rec = Recorder::default();
    rec.feed(&rd(T0, &sa_state(-90), "ok", 0));
    let (ev, _) = rec.feed(&rd(T0 + 5, &sa_state(-90), "stall", 0));
    assert_eq!(kinds(&ev), ["stall"]);
    assert_eq!(ev[0].data["cell"]["cell"], 5_242_881_234i64);
    // /v2/screen missed one round: no flip-flop
    let (ev, _) = rec.feed(&Reading::parse(T0 + 10, Some(&sa_state(-90)), None, Some(0)));
    assert!(ev.is_empty());
    let (ev, _) = rec.feed(&rd(T0 + 20, &sa_state(-90), "stall", 0));
    assert!(ev.is_empty());
    let (ev, _) = rec.feed(&rd(T0 + 25, &sa_state(-90), "ok", 0));
    assert_eq!(kinds(&ev), ["stall_end"]);
    assert_eq!((ev[0].data["secs"].clone(), ev[0].data["now"].clone()), (json!(20), json!("ok")));
}

#[test]
fn cell_rat_ca_and_qos_changes() {
    let mut rec = Recorder::default();
    let a = sa_state(-90);
    rec.feed(&rd(T0, &a, "ok", 0));
    let mut b = a.clone();
    b["net"]["nr_pci"] = 77.into();
    b["net"]["nrca"] = "1,78,100".into();
    let (ev, _) = rec.feed(&rd(T0 + 5, &b, "ok", 0));
    assert_eq!(kinds(&ev), ["cell_change", "ca_change"]);
    assert_eq!((ev[0].data["from"]["pci"].clone(), ev[0].data["to"]["pci"].clone()), (json!(312), json!(77)));
    let mut c = b.clone();
    c["net"]["type"] = "LTE".into();
    let (ev, _) = rec.feed(&rd(T0 + 10, &c, "ok", 0));
    assert_eq!(kinds(&ev), ["rat_change", "cell_change"]);
    let mut d = c.clone();
    d["qos"]["ambr_dl"] = "5000".into();
    let (ev, _) = rec.feed(&rd(T0 + 15, &d, "limit", 0));
    assert_eq!(kinds(&ev), ["qos_change"]);
    assert_eq!(ev[0].data["to"]["ambr"], "5000/200000");
    // unknown QoS on one side: no event
    let mut e = d.clone();
    e["qos"]["ambr_dl"] = "".into();
    let (ev, _) = rec.feed(&rd(T0 + 20, &e, "ok", 0));
    assert!(ev.is_empty());
}

#[test]
fn weak_signal_hysteresis() {
    let mut rec = Recorder::default();
    let mut t = T0;
    let mut feed = |rsrp: Option<i64>| {
        t += 5;
        let mut s = sa_state(-90);
        s["net"]["nr_rsrp"] = rsrp.unwrap_or(0).into();
        kinds(&rec.feed(&rd(t, &s, "ok", 0)).0).into_iter().map(str::to_string).collect::<Vec<_>>()
    };
    feed(Some(-100));
    // 25 s below, then one above: no event
    for _ in 0..6 {
        assert!(feed(Some(-112)).is_empty());
    }
    assert!(feed(Some(-108)).is_empty());
    // 30 s below, with a missing reading in the middle
    let mut got = Vec::new();
    for r in [-112, -115, 0, -113, -111, -114, -112] {
        got.extend(feed(if r == 0 { None } else { Some(r) }));
    }
    assert_eq!(got, ["weak_enter"]);
    // −107 is inside the band: stays weak
    for _ in 0..10 {
        assert!(feed(Some(-107)).is_empty());
    }
    let mut got = Vec::new();
    for _ in 0..7 {
        got.extend(feed(Some(-100)));
    }
    assert_eq!(got, ["weak_exit"]);
    let exit = rec.events.back().unwrap();
    assert_eq!(exit.data["worst"], -115.0);
}

// ---- disk -------------------------------------------------------------------

#[test]
fn day_names() {
    assert_eq!(day_name(0), "19700101");
    assert_eq!(day_name(T0), "20261002");
    assert_eq!(day_name(951_782_400), "20000229");
    assert_eq!(day_name(T0 + 12 * 3600 - 1), "20261002");
    assert_eq!(day_name(T0 + 12 * 3600), "20261003");
}

#[test]
fn append_after_a_cut_off_line() {
    let dir = tmp("cut");
    let mut st = Store::new(&dir);
    st.append_event(&Event::new(T0, "data_down", json!({}))).unwrap();
    // power cut half way through the next line
    let p = dir.join("20261002.events.jsonl");
    let mut f = OpenOptions::new().append(true).open(&p).unwrap();
    f.write_all(br#"{"t":1790942405,"kind":"data_"#).unwrap();
    drop(f);
    // a new process appends
    let mut st = Store::new(&dir);
    st.append_event(&Event::new(T0 + 10, "data_up", json!({"down_s": 10}))).unwrap();
    st.append_event(&Event::new(T0 + 20, "stall", json!({}))).unwrap();
    let (_, es) = st.read_range(T0, T0 + 3600);
    assert_eq!(kinds(&es), ["data_down", "data_up", "stall"]);
    assert_eq!(es[1].data["down_s"], 10);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn minutes_across_midnight_go_to_two_files() {
    let dir = tmp("midnight");
    let mut st = Store::new(&dir);
    let m = |t| Minute { t, rat: "SA".into(), verdict: "ok".into(), ..Default::default() };
    let midnight = T0 + 12 * 3600;
    st.append_minutes(&[m(midnight - 120), m(midnight - 60), m(midnight), m(midnight + 60)]).unwrap();
    let count = |d: &str| fs::read_to_string(dir.join(format!("{d}.jsonl"))).unwrap().lines().count();
    assert_eq!((count("20261002"), count("20261003")), (2, 2));
    let (ms, _) = st.read_range(midnight - 90, midnight + 3600);
    assert_eq!(ms.iter().map(|x| x.t).collect::<Vec<_>>(), [midnight - 60, midnight, midnight + 60]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn prune_keeps_seven_days() {
    let dir = tmp("prune");
    fs::create_dir_all(&dir).unwrap();
    for d in ["20260924", "20260925", "20261002", "notes"] {
        fs::write(dir.join(format!("{d}.jsonl")), "").unwrap();
    }
    fs::write(dir.join("20260924.events.jsonl"), "").unwrap();
    Store::new(&dir).prune(T0);
    let mut left: Vec<String> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, ["20260925.jsonl", "20261002.jsonl", "notes.jsonl"]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn log_writes_events_at_once_and_minutes_every_ten() {
    let dir = tmp("log");
    let mut log = Log::new(&dir);
    let mut off = sa_state(-90);
    off["net"]["wan_status"] = "disconnected".into();
    log.feed(&rd(T0, &sa_state(-90), "ok", 0));
    log.feed(&rd(T0 + 5, &off, "nodata", 0));
    let ev_file = dir.join("20261002.events.jsonl");
    assert_eq!(fs::read_to_string(&ev_file).unwrap().lines().count(), 1, "event on disk at once");
    let min_file = dir.join("20261002.jsonl");
    for k in 2..(9 * 12) {
        log.feed(&rd(T0 + k * 5, &sa_state(-90), "ok", 0));
    }
    assert!(!min_file.exists(), "no minutes before 10 min");
    for k in (9 * 12)..(11 * 12) {
        log.feed(&rd(T0 + k * 5, &sa_state(-90), "ok", 0));
    }
    assert_eq!(fs::read_to_string(&min_file).unwrap().lines().count(), 10);
    assert_eq!(log.write_errors, 0);

    // restart: the day comes back from disk
    let mut log2 = Log::new(&dir);
    log2.feed(&rd(T0 + 700, &sa_state(-90), "ok", 0));
    assert_eq!(log2.rec.minutes.len(), 10);
    assert_eq!(kinds(&log2.rec.events.iter().cloned().collect::<Vec<_>>()), ["data_down", "data_up"]);
    let (ms, es) = log2.range(T0, T0 + 3600);
    assert_eq!((ms.len(), es.len()), (10, 2));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn nothing_written_before_the_clock_is_set() {
    let dir = tmp("clock");
    let mut log = Log::new(&dir);
    let mut off = sa_state(-90);
    off["net"]["wan_status"] = "disconnected".into();
    let boot = 1_000_000;
    let on = sa_state(-90);
    for k in 0..(15 * 12) {
        log.feed(&rd(boot + k * 5, if k == 3 { &off } else { &on }, "ok", 0));
    }
    assert!(!dir.exists(), "nothing on disk with an unset clock");
    assert!(!log.rec.events.is_empty() && !log.rec.minutes.is_empty());
    log.push_event(Event::new(boot + 900, "diagnose", json!({})));
    assert!(!dir.exists());
    // the clock is set: what came before goes, recording carries on
    log.feed(&rd(T0, &sa_state(-90), "ok", 0));
    assert!(log.rec.minutes.is_empty() && log.rec.events.is_empty());
    log.push_event(Event::new(T0 + 1, "diagnose", json!({"cause": "cell"})));
    assert_eq!(fs::read_to_string(dir.join("20261002.events.jsonl")).unwrap().lines().count(), 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn bad_dir_counts_errors_and_keeps_recording() {
    let mut log = Log::new("/proc/no-such-dir/signal-log");
    let mut off = sa_state(-90);
    off["net"]["wan_status"] = "disconnected".into();
    log.feed(&rd(T0, &sa_state(-90), "ok", 0));
    log.feed(&rd(T0 + 5, &off, "nodata", 0));
    assert_eq!(log.write_errors, 1);
    assert_eq!(log.rec.events.len(), 1);
}
