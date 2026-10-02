use super::*;

fn sta(signal: i32, rate: u32) -> Sta {
    Sta { mac: "02:00:00:00:00:01".into(), name: "iPhone".into(), signal: Some(signal), rate_mbps: Some(rate), ..Sta::default() }
}

fn story(state: &str, sig_tone: &str, noise_tone: &str) -> Value {
    json!({
        "state": state, "cause": "none", "headline": "无服务", "headline_en": "No service",
        "sig": "强", "sig_en": "Strong", "sig_tone": sig_tone,
        "noise": "小", "noise_en": "low", "noise_tone": noise_tone,
    })
}

fn radio() -> Radio {
    Radio {
        story: Some(story("ok", "ok", "ok")),
        rsrp: Some(-87.0),
        sinr: Some(17.7),
        rsrq: Some(-11.0),
        tier: Some(2),
        nr: true,
        ambr_dl: Some(1668.0),
        qci: Some(9),
        connected: true,
        rx_bps: 0,
    }
}

fn key(hour: u8) -> CellKey {
    CellKey { plmn: Some("460-11".into()), cell: Some(5_242_881_234), ch: Some(633_984), hour }
}

fn probe(ms: &[u32], sent: usize) -> LinkProbe {
    LinkProbe { sent, rtts: ms.to_vec() }
}

fn layer(id: &'static str, level: Level, detail: &str) -> Layer {
    Layer::new(id).set(level, detail, detail)
}

// ---- Wi-Fi ---------------------------------------------------------------------

#[test]
fn wifi_for_a_client() {
    let stas = [sta(-52, 866), Sta { mac: "02:00:00:00:00:02".into(), ..sta(-78, 24) }];
    let l = judge_wifi(&Who::Client(Some("02:00:00:00:00:01".into())), &stas);
    assert_eq!((l.level, l.detail.as_str()), (Level::Ok, "-52 dBm · 866 Mbps"));
    let l = judge_wifi(&Who::Client(Some("02:00:00:00:00:02".into())), &stas);
    assert_eq!(l.level, Level::Bad);
    // wired, Tailscale, or a lease that isn't on Wi-Fi
    for who in [Who::Client(None), Who::Client(Some("02:00:00:00:00:99".into()))] {
        let l = judge_wifi(&who, &stas);
        assert_eq!((l.level, l.detail.as_str(), l.counted), (Level::Na, "不是经 Wi-Fi 连的", true));
    }
}

#[test]
fn wifi_thresholds() {
    let one = |s: Sta| judge_wifi(&Who::Client(Some(s.mac.clone())), &[s]).level;
    assert_eq!(one(sta(-70, 50)), Level::Ok);
    assert_eq!(one(sta(-71, 866)), Level::Bad);
    // a low rate alone is an idle station; it counts once the signal is middling
    assert_eq!(one(sta(-41, 29)), Level::Ok);
    assert_eq!(one(sta(-64, 49)), Level::Ok);
    assert_eq!(one(sta(-65, 49)), Level::Bad);
    // retries: > 10% of ≥ 50 frames
    assert_eq!(one(Sta { tx_packets: Some(100), tx_retries: Some(11), ..sta(-50, 866) }), Level::Bad);
    assert_eq!(one(Sta { tx_packets: Some(100), tx_retries: Some(10), ..sta(-50, 866) }), Level::Ok);
    assert_eq!(one(Sta { tx_packets: Some(20), tx_retries: Some(15), ..sta(-50, 866) }), Level::Ok);
    let l = judge_wifi(&Who::Client(Some("02:00:00:00:00:01".into())), &[Sta { mac: "02:00:00:00:00:01".into(), ..Sta::default() }]);
    assert_eq!(l.level, Level::Na);
}

#[test]
fn wifi_from_the_screen_takes_the_worst() {
    let stas = [sta(-52, 866), Sta { mac: "m2".into(), name: "".into(), ..sta(-74, 300) }, sta(-60, 40)];
    let l = judge_wifi(&Who::Touch, &stas);
    assert_eq!((l.level, l.detail.as_str()), (Level::Bad, "最差：m2 · -74 dBm · 300 Mbps"));
    assert_eq!(l.detail_en, "Worst: m2 · -74 dBm · 300 Mbps");
    let l = judge_wifi(&Who::Touch, &[sta(-52, 866)]);
    assert_eq!(l.detail, "iPhone · -52 dBm · 866 Mbps");
    // nobody on Wi-Fi: grey, and not counted in 3/6
    let l = judge_wifi(&Who::Touch, &[]);
    assert_eq!((l.level, l.detail.as_str(), l.counted), (Level::Na, "没有设备连着", false));
}

// ---- signal and cap -----------------------------------------------------------

#[test]
fn signal_layer() {
    let l = judge_signal(&radio());
    assert_eq!((l.level, l.detail.as_str()), (Level::Ok, "信号强 · 干扰小 · RSRP -87 · SINR 17.7"));
    assert_eq!(l.detail_en, "Signal strong · noise low · RSRP -87 · SINR 17.7");
    let mut r = radio();
    r.story = Some(story("weak", "bad", "warn"));
    assert_eq!(judge_signal(&r).level, Level::Bad);
    r.story = Some(story("noise", "ok", "warn"));
    assert_eq!(judge_signal(&r).level, Level::Warn);
    let mut st = story("narrow", "ok", "ok");
    st["cause"] = "narrow".into();
    r.story = Some(st);
    let l = judge_signal(&r);
    assert_eq!(l.level, Level::Warn);
    assert!(l.detail.contains("载波窄"), "{}", l.detail);
    r.story = Some(story("nosvc", "", ""));
    let l = judge_signal(&r);
    assert_eq!((l.level, l.detail.as_str(), l.detail_en.as_str()), (Level::Bad, "无服务", "No service"));
    r.story = None;
    assert_eq!(judge_signal(&r).level, Level::Na);
}

#[test]
fn cap_layer() {
    let mut r = radio();
    assert_eq!(judge_limit(&r).detail, "没有限速 · QCI 9");
    r.ambr_dl = Some(4.6);
    let l = judge_limit(&r);
    assert_eq!((l.level, l.detail.as_str(), l.detail_en.as_str()), (Level::Bad, "限到 5 Mbps · QCI 9", "Capped at 5 Mbps · QCI 9"));
    r.ambr_dl = None;
    assert_eq!(judge_limit(&r).detail, "QoS 读不到");
}

// ---- cellular link ---------------------------------------------------------------

#[test]
fn link_layer() {
    let l = judge_link(true, true, Some(&probe(&[40, 42, 45, 41, 39, 44, 43, 40, 41, 42], 10)));
    assert_eq!((l.level, l.detail.as_str()), (Level::Ok, "延迟 41 ms · 丢包 0/10"));
    // one lost in ten is still fine; two is not
    assert_eq!(judge_link(true, true, Some(&probe(&[40; 9], 10))).level, Level::Ok);
    assert_eq!(judge_link(true, true, Some(&probe(&[40; 8], 10))).level, Level::Bad);
    assert_eq!(judge_link(true, true, Some(&probe(&[151; 10], 10))).level, Level::Bad);
    assert_eq!(judge_link(true, true, Some(&probe(&[150; 10], 10))).level, Level::Ok);
    let l = judge_link(true, true, Some(&probe(&[], 10)));
    assert_eq!((l.level, l.detail.as_str()), (Level::Bad, "10 个探测全丢"));
    assert_eq!(judge_link(false, true, None).detail, "没有数据连接");
    assert_eq!(judge_link(true, false, None).detail, "没有运营商 DNS");
    assert_eq!(judge_link(true, true, None).detail, "超时");
}

// ---- cell load ------------------------------------------------------------------

fn idle_runs(k: &CellKey, ms: u32, n: usize) -> Vec<PastRun> {
    (0..n).map(|_| PastRun { key: k.clone(), link_ms: ms, in_use: false }).collect()
}

fn speeds(k: &CellKey, mbps: &[f64]) -> Vec<PastSpeed> {
    mbps.iter().map(|m| PastSpeed { key: k.clone(), mbps: *m }).collect()
}

#[test]
fn crowd_needs_two_pieces_of_evidence() {
    let k = key(20);
    let mut r = radio();
    // ① alone
    r.rsrq = Some(-16.0);
    assert_eq!(judge_crowd(&r, &k, None, &[], &[]).level, Level::Ok);
    // ① + ②: in use, latency ≥ 2× the idle baseline
    r.rx_bps = 300_000;
    let l = judge_crowd(&r, &k, Some(&probe(&[90; 10], 10)), &idle_runs(&k, 40, 3), &[]);
    assert_eq!(l.level, Level::Warn);
    assert_eq!(l.detail, "疑似拥挤 · RSRQ -16 · 延迟是平时的 2.2 倍");
    // ② + ③: often busy at this hour
    r.rsrq = Some(-10.0);
    let l = judge_crowd(&r, &k, Some(&probe(&[90; 10], 10)), &idle_runs(&k, 40, 3), &speeds(&k, &[4.0, 6.0, 8.0]));
    assert_eq!(l.level, Level::Bad);
    assert!(l.detail.starts_with("这个小区这个时段经常拥挤"), "{}", l.detail);
    // ① + ③
    r.rsrq = Some(-16.0);
    r.rx_bps = 0;
    assert_eq!(judge_crowd(&r, &k, None, &[], &speeds(&k, &[4.0, 6.0, 8.0])).level, Level::Warn);
}

#[test]
fn crowd_baseline_and_speed_rules() {
    let k = key(20);
    let mut r = radio();
    r.rx_bps = 300_000;
    let slow = probe(&[90; 10], 10);
    let sp = speeds(&k, &[4.0, 6.0, 8.0]);
    let st = |runs: &[PastRun], sp: &[PastSpeed]| judge_crowd(&r, &k, Some(&slow), runs, sp).level;
    // two idle runs are not a baseline
    assert_eq!(st(&idle_runs(&k, 40, 2), &sp), Level::Ok);
    // runs made while in use are not a baseline
    let busy: Vec<PastRun> = idle_runs(&k, 40, 3).into_iter().map(|p| PastRun { in_use: true, ..p }).collect();
    assert_eq!(st(&busy, &sp), Level::Ok);
    // another cell's runs don't count
    let other = CellKey { cell: Some(1), ..k.clone() };
    assert_eq!(st(&idle_runs(&other, 40, 3), &sp), Level::Ok);
    // speed tests at another hour don't count
    assert_eq!(st(&idle_runs(&k, 40, 3), &speeds(&key(9), &[4.0, 6.0, 8.0])), Level::Ok);
    // fast enough at this hour
    assert_eq!(st(&idle_runs(&k, 40, 3), &speeds(&k, &[4.0, 20.0, 30.0])), Level::Ok);
    // not in use: ② can't say anything
    let mut idle = radio();
    idle.rsrq = Some(-10.0);
    assert_eq!(judge_crowd(&idle, &k, Some(&slow), &idle_runs(&k, 40, 3), &sp).level, Level::Ok);
}

#[test]
fn crowd_without_cell_id_or_history() {
    let mut r = radio();
    r.rsrq = None;
    let none = CellKey { cell: None, ..key(20) };
    let l = judge_crowd(&r, &none, None, &[], &[]);
    assert_eq!((l.level, l.detail.as_str()), (Level::Na, "没有小区编号"));
    let l = judge_crowd(&r, &key(20), None, &[], &[]);
    assert_eq!((l.level, l.detail.as_str()), (Level::Na, "历史不够"));
    // no cell ID: ② and ③ never count, even with matching-looking history
    r.rsrq = Some(-16.0);
    r.rx_bps = 300_000;
    let l = judge_crowd(&r, &none, Some(&probe(&[90; 10], 10)), &idle_runs(&none, 40, 3), &speeds(&none, &[4.0, 6.0, 8.0]));
    assert_eq!(l.level, Level::Ok);
}

// ---- proxy ----------------------------------------------------------------------

#[test]
fn proxy_against_direct() {
    let p = |n: Option<NodeProbe>, d: Option<u32>| ProxyProbe { node: "JP 03".into(), node_probe: n, direct_ms: d };
    let l = judge_proxy(Some(&p(Some(NodeProbe::Ms(210)), Some(180))));
    assert_eq!((l.level, l.detail.as_str()), (Level::Ok, "JP 03 210 ms · 直连 180 ms"));
    assert_eq!(judge_proxy(Some(&p(Some(NodeProbe::Ms(480)), Some(180)))).level, Level::Ok);
    let l = judge_proxy(Some(&p(Some(NodeProbe::Ms(481)), Some(180))));
    assert_eq!((l.level, l.detail.as_str()), (Level::Bad, "JP 03 481 ms · 比直连慢 301 ms"));
    assert_eq!(judge_proxy(Some(&p(Some(NodeProbe::Timeout), Some(180)))).level, Level::Bad);
    // the direct request failed too: don't blame the node
    let l = judge_proxy(Some(&p(Some(NodeProbe::Timeout), None)));
    assert_eq!((l.level, l.detail.as_str()), (Level::Na, "直连也不通"));
    assert_eq!(judge_proxy(Some(&p(None, Some(180)))).detail, "代理没回应");
    assert_eq!(judge_proxy(None).detail, "超时");
}

// ---- main cause -------------------------------------------------------------------

#[test]
fn main_cause_order() {
    let ok = |id| layer(id, Level::Ok, "");
    let ls = vec![ok("wifi"), layer("signal", Level::Warn, "信号中 · 干扰中"), ok("limit"), layer("link", Level::Bad, "延迟 300 ms"), ok("crowd")];
    let m = pick_main(&ls);
    // the first bad beats an earlier warn
    assert_eq!((m.layer, m.level, m.more), ("link", Some(Level::Bad), 1));
    assert_eq!(m.text, "蜂窝链路不稳");
    let ls = vec![ok("wifi"), layer("signal", Level::Bad, "信号弱 · 干扰大"), layer("crowd", Level::Warn, "疑似拥挤")];
    let m = pick_main(&ls);
    assert_eq!((m.text.as_str(), m.action_to, m.more), ("信号弱", "placement", 1));
    let m = pick_main(&[layer("signal", Level::Warn, "信号强 · 干扰中"), layer("proxy", Level::Bad, "JP 03 超时")]);
    assert_eq!((m.text.as_str(), m.action.as_str(), m.action_to), ("代理节点慢或不通", "换节点", "proxy"));
    let m = pick_main(&[layer("signal", Level::Bad, "无服务")]);
    assert_eq!(m.text, "无服务");
    let m = pick_main(&[layer("wifi", Level::Bad, "-80 dBm"), layer("signal", Level::Bad, "信号弱")]);
    assert_eq!((m.text.as_str(), m.action.as_str()), ("Wi-Fi 信号差", "靠近一点，或换个频段"));
    // "can't tell" is never the main cause
    let m = pick_main(&[layer("wifi", Level::Na, "没有设备连着"), ok("signal"), layer("crowd", Level::Na, "历史不够")]);
    assert_eq!((m.layer, m.level, m.text.as_str()), ("", None, "没查到问题"));
    assert_eq!(m.text_en, "No problem found");
}

#[test]
fn every_main_has_english_and_an_action() {
    for (id, d) in [("wifi", ""), ("signal", "信号弱"), ("signal", "信号强 · 干扰大"), ("signal", "信号强 · 载波窄"), ("signal", "信号中"), ("limit", ""), ("link", ""), ("crowd", ""), ("proxy", "")] {
        let m = pick_main(&[layer(id, Level::Warn, d)]);
        assert!(!m.text.is_empty() && !m.text_en.is_empty() && m.text_en.is_ascii(), "{id} {d}: {m:?}");
        assert!(!m.action.is_empty() && m.action_en.is_ascii(), "{id} {d}: {m:?}");
    }
}

// ---- runs, history, gate ------------------------------------------------------------

#[test]
fn steps_count_only_what_applies() {
    let mut r = Run::new(1, 0, "touch", false);
    assert_eq!((r.step, r.steps, r.layers.len()), (0, 5, 5));
    let mut w = judge_wifi(&Who::Touch, &[]);
    w.counted = false;
    r.put(w);
    assert_eq!((r.step, r.steps), (0, 4));
    r.running("signal");
    r.put(judge_signal(&radio()));
    assert_eq!((r.step, r.steps), (1, 4));
    assert_eq!(Run::new(2, 0, "client", true).steps, 6);
}

#[test]
fn gave_up_after_waiting() {
    let mut r = Run::new(1, 0, "client", false);
    gave_up(&mut r, "scan");
    assert!(r.layers.iter().all(|l| l.level == Level::Na && l.detail == "正在搜网"));
    assert_eq!(r.main.unwrap().text, "测不了：正在搜网");
}

#[test]
fn history_from_events() {
    use crate::netwatch::record::Event;
    let mut d = key_json(&key(20));
    d["link_ms"] = json!(41);
    d["in_use"] = json!(false);
    let mut s = key_json(&key(20));
    s["mbps"] = json!(6.5);
    let no_cell = json!({"plmn": "460-11", "hour": 20, "mbps": 3.0});
    let evs = vec![Event::new(1, "diagnose", d), Event::new(2, "speed", s), Event::new(3, "speed", no_cell), Event::new(4, "data_down", json!({}))];
    let (runs, sp) = past_from_events(&evs);
    assert_eq!(runs, [PastRun { key: key(20), link_ms: 41, in_use: false }]);
    assert_eq!(sp, [PastSpeed { key: key(20), mbps: 6.5 }]);
}

#[test]
fn gate_refuses_both_ways() {
    // a local Gate: the real one is shared with tests running in parallel
    assert!(refusal(&Gate::default()).is_none());
    // a diagnosis is running: speed test / search / register get a 409 with a time
    let (code, body) = refusal(&Gate { diag_since: Some(now_unix()) }).unwrap();
    assert_eq!(code, 409);
    assert_eq!(body["error"], "正在诊断，约 20 秒后再试");
    assert_eq!(body["error_en"], "Diagnosing; try again in about 20 s");
    assert_eq!(refusal(&Gate { diag_since: Some(now_unix() - 60) }).unwrap().1["retry_after_s"], 1);
    assert_eq!(eta(100, 105), 15);
}

#[test]
fn speed_row_shows_the_route_and_no_verdict() {
    use crate::speedtest::{Phase, SpeedTestProgress};
    let l = speed_row(&SpeedTestProgress::ended(Phase::Complete, Some(86.4)));
    assert_eq!((l.level, l.detail.as_str(), l.detail_en.as_str()), (Level::Info, "直连 ↓ 86 Mbps", "Direct ↓ 86 Mbps"));
    assert_eq!(speed_row(&SpeedTestProgress::ended(Phase::Cancelled, Some(3.0))).detail, "被停下");
    assert_eq!(speed_row(&SpeedTestProgress::ended(Phase::Error, None)).detail, "没测成");
    // the main sentence never looks at the speed row
    let mut r = Run::new(1, 0, "client", false);
    for l in r.layers.clone() {
        r.put(l.set(Level::Ok, "", ""));
    }
    r.speed = Some(speed_row(&SpeedTestProgress::ended(Phase::Complete, Some(2.0))));
    assert_eq!(pick_main(&r.layers).text, "没查到问题");
}
