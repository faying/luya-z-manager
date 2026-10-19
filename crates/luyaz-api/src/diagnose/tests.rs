use super::*;

fn sta(signal: i32, rate: u32) -> Sta {
    Sta {
        mac: "02:00:00:00:00:01".into(),
        name: "iPhone".into(),
        signal: Some(signal),
        rate_mbps: Some(rate),
        ..Sta::default()
    }
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
        qos_stale: false,
        qos_nr: false,
        connected: true,
        rx_bps: 0,
        carriers: String::new(),
        bw_mhz: None,
    }
}

fn key(hour: u8) -> CellKey {
    CellKey {
        plmn: Some("460-11".into()),
        cell: Some(5_242_881_234),
        ch: Some(633_984),
        hour,
    }
}

fn probe(ms: &[u32], sent: usize) -> LinkProbe {
    LinkProbe {
        sent,
        rtts: ms.to_vec(),
    }
}

fn layer(id: &'static str, level: Level, detail: &str) -> Layer {
    Layer::new(id).set(level, detail, detail)
}

// ---- Wi-Fi ---------------------------------------------------------------------

#[test]
fn wifi_for_a_client() {
    let stas = [
        sta(-52, 866),
        Sta {
            mac: "02:00:00:00:00:02".into(),
            ..sta(-78, 24)
        },
    ];
    let l = judge_wifi(&Who::Client(Some("02:00:00:00:00:01".into())), &stas);
    assert_eq!(
        (l.level, l.detail.as_str()),
        (Level::Ok, "-52 dBm · 866 Mbps")
    );
    let l = judge_wifi(&Who::Client(Some("02:00:00:00:00:02".into())), &stas);
    assert_eq!(l.level, Level::Bad);
    // wired, Tailscale, or a lease that isn't on Wi-Fi
    for who in [
        Who::Client(None),
        Who::Client(Some("02:00:00:00:00:99".into())),
    ] {
        let l = judge_wifi(&who, &stas);
        assert_eq!(
            (l.level, l.detail.as_str(), l.counted),
            (Level::Na, "不是经 Wi-Fi 连的", true)
        );
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
    assert_eq!(
        one(Sta {
            tx_packets: Some(100),
            tx_retries: Some(11),
            ..sta(-50, 866)
        }),
        Level::Bad
    );
    assert_eq!(
        one(Sta {
            tx_packets: Some(100),
            tx_retries: Some(10),
            ..sta(-50, 866)
        }),
        Level::Ok
    );
    assert_eq!(
        one(Sta {
            tx_packets: Some(20),
            tx_retries: Some(15),
            ..sta(-50, 866)
        }),
        Level::Ok
    );
    let l = judge_wifi(
        &Who::Client(Some("02:00:00:00:00:01".into())),
        &[Sta {
            mac: "02:00:00:00:00:01".into(),
            ..Sta::default()
        }],
    );
    assert_eq!(l.level, Level::Na);
}

#[test]
fn wifi_from_the_screen_takes_the_worst() {
    let stas = [
        sta(-52, 866),
        Sta {
            mac: "m2".into(),
            name: "".into(),
            ..sta(-74, 300)
        },
        sta(-60, 40),
    ];
    let l = judge_wifi(&Who::Touch, &stas);
    assert_eq!(
        (l.level, l.detail.as_str()),
        (Level::Bad, "最差：m2 · -74 dBm · 300 Mbps")
    );
    assert_eq!(l.detail_en, "Worst: m2 · -74 dBm · 300 Mbps");
    let l = judge_wifi(&Who::Touch, &[sta(-52, 866)]);
    assert_eq!(l.detail, "iPhone · -52 dBm · 866 Mbps");
    // nobody on Wi-Fi: grey, and not counted in 3/6
    let l = judge_wifi(&Who::Touch, &[]);
    assert_eq!(
        (l.level, l.detail.as_str(), l.counted),
        (Level::Na, "没有设备连着", false)
    );
}

// ---- signal and cap -----------------------------------------------------------

#[test]
fn signal_layer() {
    let l = judge_signal(&radio());
    assert_eq!(
        (l.level, l.detail.as_str()),
        (Level::Ok, "信号强，干扰小")
    );
    assert_eq!(l.detail_en, "Strong signal, little noise");
    let mut r = radio();
    r.story = Some(story("weak", "bad", "warn"));
    assert_eq!(judge_signal(&r).level, Level::Bad);
    r.story = Some(story("noise", "ok", "warn"));
    assert_eq!(judge_signal(&r).level, Level::Warn);
    let mut st = story("narrow", "ok", "ok");
    st["cause"] = "narrow".into();
    r.story = Some(st);
    r.bw_mhz = Some(20);
    let l = judge_signal(&r);
    assert_eq!((l.level, l.detail.as_str()), (Level::Warn, "信号强，干扰小，此刻带宽只有 20 MHz"));
    // and the headline that goes with it
    assert_eq!(pick_main(&[l]).text, "此刻带宽窄");
    r.story = Some(story("nosvc", "", ""));
    let l = judge_signal(&r);
    assert_eq!(
        (l.level, l.detail.as_str(), l.detail_en.as_str()),
        (Level::Bad, "无服务", "No service")
    );
    r.story = None;
    assert_eq!(judge_signal(&r).level, Level::Na);
}

#[test]
fn cap_layer() {
    let mut r = radio();
    assert_eq!(judge_limit(&r).detail, "没有限速 · QCI 9");
    r.ambr_dl = Some(4.6);
    let l = judge_limit(&r);
    assert_eq!(
        (l.level, l.detail.as_str(), l.detail_en.as_str()),
        (
            Level::Bad,
            "限到 5 Mbps · QCI 9",
            "Capped at 5 Mbps · QCI 9"
        )
    );
    // a stale qos block: no "capped" verdict from history (b31-community Q6)
    r.qos_stale = true;
    let l = judge_limit(&r);
    assert_ne!(l.level, Level::Bad);
    assert!(!l.detail.contains("限到"), "{}", l.detail);
    r.qos_stale = false;
    // an NR PDU session's number is a 5QI
    r.qos_nr = true;
    assert_eq!(judge_limit(&r).detail, "限到 5 Mbps · 5QI 9");
    // a QCI without an AMBR (what the device reports, 10-18): no cap reported
    r.ambr_dl = None;
    let l = judge_limit(&r);
    assert_eq!((l.level, l.detail.as_str()), (Level::Ok, "没报限速 · 5QI 9"));
    r.qci = None;
    assert_eq!(judge_limit(&r).detail, "QoS 读不到");
    // the carriers go on the row too
    r.qci = Some(6);
    r.qos_nr = false;
    r.carriers = "n41 100M + B3 20M".into();
    assert_eq!(judge_limit(&r).detail, "没报限速 · n41 100M + B3 20M · QCI 6");
}

// ---- cellular link ---------------------------------------------------------------

#[test]
fn link_layer() {
    let l = judge_link(
        true,
        true,
        Some(&probe(&[40, 42, 45, 41, 39, 44, 43, 40, 41, 42], 10)),
    );
    assert_eq!(
        (l.level, l.detail.as_str()),
        (Level::Ok, "延迟 41 ms · 丢包 0/10")
    );
    // one lost in ten is still fine; two is not
    assert_eq!(
        judge_link(true, true, Some(&probe(&[40; 9], 10))).level,
        Level::Ok
    );
    assert_eq!(
        judge_link(true, true, Some(&probe(&[40; 8], 10))).level,
        Level::Bad
    );
    assert_eq!(
        judge_link(true, true, Some(&probe(&[151; 10], 10))).level,
        Level::Bad
    );
    assert_eq!(
        judge_link(true, true, Some(&probe(&[150; 10], 10))).level,
        Level::Ok
    );
    let l = judge_link(true, true, Some(&probe(&[], 10)));
    assert_eq!((l.level, l.detail.as_str()), (Level::Bad, "10 个探测全丢"));
    assert_eq!(judge_link(false, true, None).detail, "没有数据连接");
    assert_eq!(judge_link(true, false, None).detail, "没有运营商 DNS");
    assert_eq!(judge_link(true, true, None).detail, "超时");
}

// ---- cell load ------------------------------------------------------------------

fn idle_runs(k: &CellKey, ms: u32, n: usize) -> Vec<PastRun> {
    (0..n)
        .map(|_| PastRun {
            key: k.clone(),
            link_ms: ms,
            in_use: false,
        })
        .collect()
}

fn speeds(k: &CellKey, mbps: &[f64]) -> Vec<PastSpeed> {
    mbps.iter()
        .map(|m| PastSpeed {
            key: k.clone(),
            mbps: *m,
        })
        .collect()
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
    let l = judge_crowd(
        &r,
        &k,
        Some(&probe(&[90; 10], 10)),
        &idle_runs(&k, 40, 3),
        &[],
    );
    assert_eq!(l.level, Level::Warn);
    assert_eq!(l.detail, "疑似拥挤 · RSRQ -16 · 延迟是平时的 2.2 倍");
    // ② + ③: often busy at this hour
    r.rsrq = Some(-10.0);
    let l = judge_crowd(
        &r,
        &k,
        Some(&probe(&[90; 10], 10)),
        &idle_runs(&k, 40, 3),
        &speeds(&k, &[4.0, 6.0, 8.0]),
    );
    assert_eq!(l.level, Level::Bad);
    assert!(
        l.detail.starts_with("这个小区这个时段经常拥挤"),
        "{}",
        l.detail
    );
    // ① + ③
    r.rsrq = Some(-16.0);
    r.rx_bps = 0;
    assert_eq!(
        judge_crowd(&r, &k, None, &[], &speeds(&k, &[4.0, 6.0, 8.0])).level,
        Level::Warn
    );
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
    let busy: Vec<PastRun> = idle_runs(&k, 40, 3)
        .into_iter()
        .map(|p| PastRun { in_use: true, ..p })
        .collect();
    assert_eq!(st(&busy, &sp), Level::Ok);
    // another cell's runs don't count
    let other = CellKey {
        cell: Some(1),
        ..k.clone()
    };
    assert_eq!(st(&idle_runs(&other, 40, 3), &sp), Level::Ok);
    // speed tests at another hour don't count
    assert_eq!(
        st(&idle_runs(&k, 40, 3), &speeds(&key(9), &[4.0, 6.0, 8.0])),
        Level::Ok
    );
    // fast enough at this hour
    assert_eq!(
        st(&idle_runs(&k, 40, 3), &speeds(&k, &[4.0, 20.0, 30.0])),
        Level::Ok
    );
    // not in use: ② can't say anything
    let mut idle = radio();
    idle.rsrq = Some(-10.0);
    assert_eq!(
        judge_crowd(&idle, &k, Some(&slow), &idle_runs(&k, 40, 3), &sp).level,
        Level::Ok
    );
}

#[test]
fn crowd_without_cell_id_or_history() {
    let mut r = radio();
    r.rsrq = None;
    let none = CellKey {
        cell: None,
        ..key(20)
    };
    let l = judge_crowd(&r, &none, None, &[], &[]);
    assert_eq!((l.level, l.detail.as_str()), (Level::Na, "没有小区编号"));
    let l = judge_crowd(&r, &key(20), None, &[], &[]);
    assert_eq!((l.level, l.detail.as_str()), (Level::Na, "历史不够"));
    // no cell ID: ② and ③ never count, even with matching-looking history
    r.rsrq = Some(-16.0);
    r.rx_bps = 300_000;
    let l = judge_crowd(
        &r,
        &none,
        Some(&probe(&[90; 10], 10)),
        &idle_runs(&none, 40, 3),
        &speeds(&none, &[4.0, 6.0, 8.0]),
    );
    assert_eq!(l.level, Level::Ok);
}

// ---- proxy ----------------------------------------------------------------------

#[test]
fn proxy_against_direct() {
    let p = |n: Option<NodeProbe>, d: Option<u32>| ProxyProbe {
        node: "JP 03".into(),
        node_probe: n,
        direct_ms: d,
    };
    let l = judge_proxy(Some(&p(Some(NodeProbe::Ms(210)), Some(180))));
    assert_eq!(
        (l.level, l.detail.as_str()),
        (Level::Ok, "JP 03 210 ms · 直连 180 ms")
    );
    assert_eq!(
        judge_proxy(Some(&p(Some(NodeProbe::Ms(480)), Some(180)))).level,
        Level::Ok
    );
    let l = judge_proxy(Some(&p(Some(NodeProbe::Ms(481)), Some(180))));
    assert_eq!(
        (l.level, l.detail.as_str()),
        (Level::Bad, "JP 03 481 ms · 比直连慢 301 ms")
    );
    assert_eq!(
        judge_proxy(Some(&p(Some(NodeProbe::Timeout), Some(180)))).level,
        Level::Bad
    );
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
    let ls = vec![
        ok("wifi"),
        layer("signal", Level::Warn, "信号中 · 干扰中"),
        ok("limit"),
        layer("link", Level::Bad, "延迟 300 ms"),
        ok("crowd"),
    ];
    let m = pick_main(&ls);
    // the first bad beats an earlier warn
    assert_eq!((m.layer, m.level, m.more), ("link", Some(Level::Bad), 1));
    assert_eq!(m.text, "蜂窝链路不稳");
    let ls = vec![
        ok("wifi"),
        layer("signal", Level::Bad, "信号弱 · 干扰大"),
        layer("crowd", Level::Warn, "疑似拥挤"),
    ];
    let m = pick_main(&ls);
    assert_eq!(
        (m.text.as_str(), m.action_to, m.more),
        ("信号弱", "placement", 1)
    );
    let m = pick_main(&[
        layer("signal", Level::Warn, "信号强 · 干扰中"),
        layer("proxy", Level::Bad, "JP 03 超时"),
    ]);
    assert_eq!(
        (m.text.as_str(), m.action.as_str(), m.action_to),
        ("代理节点慢或不通", "换节点", "proxy")
    );
    let m = pick_main(&[layer("signal", Level::Bad, "无服务")]);
    assert_eq!(m.text, "无服务");
    let m = pick_main(&[
        layer("wifi", Level::Bad, "-80 dBm"),
        layer("signal", Level::Bad, "信号弱"),
    ]);
    assert_eq!(
        (m.text.as_str(), m.action.as_str()),
        ("Wi-Fi 信号差", "靠近一点，或换个频段")
    );
    // "can't tell" is never the main cause
    let m = pick_main(&[
        layer("wifi", Level::Na, "没有设备连着"),
        ok("signal"),
        layer("crowd", Level::Na, "历史不够"),
    ]);
    assert_eq!(
        (m.layer, m.level, m.text.as_str()),
        ("", None, "没查到问题")
    );
    assert_eq!(m.text_en, "No problem found");
}

#[test]
fn every_main_has_english_and_an_action() {
    for (id, d) in [
        ("wifi", ""),
        ("signal", "信号弱"),
        ("signal", "信号强 · 干扰大"),
        ("signal", "信号强，此刻带宽只有 20 MHz"),
        ("signal", "信号中"),
        ("limit", ""),
        ("link", ""),
        ("crowd", ""),
        ("proxy", ""),
    ] {
        let m = pick_main(&[layer(id, Level::Warn, d)]);
        assert!(
            !m.text.is_empty() && !m.text_en.is_empty() && m.text_en.is_ascii(),
            "{id} {d}: {m:?}"
        );
        assert!(
            !m.action.is_empty() && m.action_en.is_ascii(),
            "{id} {d}: {m:?}"
        );
    }
}

// ---- runs, history, gate ------------------------------------------------------------

#[test]
fn steps_count_only_what_applies() {
    let mut r = Run::new(1, 0, "touch", false);
    assert_eq!((r.step, r.steps, r.layers.len()), (0, 10, 10));
    let mut w = judge_wifi(&Who::Touch, &[]);
    w.counted = false;
    r.put(w);
    assert_eq!((r.step, r.steps), (0, 9));
    r.running("signal");
    r.put(judge_signal(&radio()));
    assert_eq!((r.step, r.steps), (1, 9));
    assert_eq!(Run::new(2, 0, "client", true).steps, 11);
}

#[test]
fn gave_up_after_waiting() {
    let mut r = Run::new(1, 0, "client", false);
    gave_up(&mut r, "scan");
    assert!(
        r.layers
            .iter()
            .all(|l| l.level == Level::Na && l.detail == "正在搜网")
    );
    assert_eq!(r.main.unwrap().text, "测不了：正在搜网");
}

#[test]
fn the_gate_refuses_while_a_diagnosis_runs() {
    assert!(refusal_at(&Gate::default(), 1000).is_none());
    let (code, body) = refusal_at(
        &Gate {
            diag_since: Some(1000),
            ..Gate::default()
        },
        1005,
    )
    .unwrap();
    assert_eq!(
        (code, body["busy"].as_str(), body["retry_after_s"].as_u64()),
        (409, Some("diagnose"), Some(15))
    );
    assert_eq!(body["error"], "正在诊断，约 15 秒后再试");
    assert_eq!(
        refusal_at(
            &Gate {
                diag_since: Some(1000),
                ..Gate::default()
            },
            1060
        )
        .unwrap()
        .1["retry_after_s"],
        1
    );
}

#[test]
fn a_finding_probe_is_refused_in_its_own_words_and_time() {
    let g = Gate {
        finding_since: Some(1000),
        ..Gate::default()
    };
    let (code, body) = refusal_at(&g, 1004).unwrap();
    assert_eq!(
        (code, body["busy"].as_str(), body["retry_after_s"].as_u64()),
        (409, Some("finding"), Some(10))
    );
    assert_eq!(body["error"], "正在检查网络，约 10 秒后再试");
    // a diagnosis going too: its words and its time, never the Finding's start
    let both = Gate {
        diag_since: Some(1003),
        finding_since: Some(1000),
        txn: None,
    };
    let (_, body) = refusal_at(&both, 1004).unwrap();
    assert_eq!(
        (body["busy"].as_str(), body["retry_after_s"].as_u64()),
        (Some("diagnose"), Some(19))
    );
}

// ---- v2 ----------------------------------------------------------------------------

fn sim(state: &str, channel: Option<EsimChannel>) -> SimIn {
    SimIn {
        state: Some(state.into()),
        channel,
    }
}

#[test]
fn sim_layer() {
    let j = |s: &str, c| judge_sim(&sim(s, c));
    assert_eq!(j("modem_sim_undetected", None).level, Level::Bad);
    assert_eq!(j("modem_sim_destroy", None).detail, "没有 SIM 卡");
    assert_eq!(j("modem_waitpin", None).detail, "SIM 卡要输入 PIN");
    assert_eq!(j("modem_waitpuk", None).detail, "SIM 卡被 PUK 锁住");
    assert_eq!(j("modem_init", None).level, Level::Na);
    assert_eq!(judge_sim(&SimIn::default()).level, Level::Na);
    // ZTE's sim_states wording counts as ready too (the hub's sim.state, 10-18)
    assert_eq!(j("sim ready", Some(EsimChannel::Free(2))).level, Level::Ok);
    let full = j("modem_init_complete", Some(EsimChannel::Full));
    assert_eq!((full.level, full.counted), (Level::Warn, true));
    assert!(full.detail.contains("eSIM 通道占满"));
    let free = j("modem_init_complete", Some(EsimChannel::Free(2)));
    assert_eq!((free.level, free.counted), (Level::Ok, true));
    assert_eq!(
        j("modem_init_complete", Some(EsimChannel::Busy)).level,
        Level::Ok
    );
    // not an eUICC we can check: shown, not counted
    for c in [None, Some(EsimChannel::Unknown)] {
        let l = j("modem_init_complete", c);
        assert_eq!(
            (l.level, l.counted, l.detail.as_str()),
            (Level::Ok, false, "已就绪")
        );
    }
}

fn bearer() -> BearerIn {
    BearerIn {
        connected: Some(true),
        has_ip: Some(true),
        iface: "rmnet_data0".into(),
        default_route: Some(true),
        rp_filter: Some(0),
        exits: 1,
    }
}

#[test]
fn bearer_layer() {
    let ok = judge_bearer(&bearer());
    assert_eq!(
        (ok.level, ok.detail.as_str()),
        (Level::Ok, "已连上")
    );
    let j = |f: &dyn Fn(&mut BearerIn)| {
        let mut b = bearer();
        f(&mut b);
        judge_bearer(&b)
    };
    assert_eq!(j(&|b| b.connected = Some(false)).detail, "没拨上号");
    assert_eq!(j(&|b| b.has_ip = Some(false)).detail, "拨上了，但没有地址");
    assert_eq!(j(&|b| b.default_route = Some(false)).detail, "没有默认路由");
    // unknowns never bad (bad blocks six layers)
    let unknown = j(&|b| {
        b.connected = None;
        b.has_ip = None;
        b.default_route = None;
    });
    assert_eq!(unknown.level, Level::Na);
    assert_eq!(j(&|b| b.default_route = None).level, Level::Ok);
    // rp_filter: strict with several ways out warns; loose (2), off (0) or one way out don't
    assert_eq!(
        j(&|b| {
            b.rp_filter = Some(1);
            b.exits = 2
        })
        .level,
        Level::Warn
    );
    assert_eq!(
        j(&|b| {
            b.rp_filter = Some(1);
            b.exits = 1
        })
        .level,
        Level::Ok
    );
    assert_eq!(
        j(&|b| {
            b.rp_filter = Some(2);
            b.exits = 3
        })
        .level,
        Level::Ok
    );
    assert_eq!(
        j(&|b| {
            b.rp_filter = None;
            b.exits = 3
        })
        .level,
        Level::Ok
    );
}

fn net(path: ResultState, dns: ResultState) -> crate::finding::NetworkEvidence {
    crate::finding::NetworkEvidence {
        path_reachable: path,
        dns,
        reason: "scoped_http_dns_only".into(),
        ..Default::default()
    }
}

use crate::finding::ResultState;

#[test]
fn dns_layer() {
    use ResultState::*;
    let d = |answered, ms| LocalDns {
        asked: 2,
        answered,
        ms,
    };
    let j = |l: Option<LocalDns>, c| judge_dns(l.as_ref(), Some(&net(Passed, c)));
    assert_eq!(
        j(Some(d(0, None)), Passed).detail,
        "设备的 DNS 坏了 · 运营商 DNS 能答"
    );
    assert_eq!(j(Some(d(0, None)), Failed).detail, "DNS 不通");
    assert_eq!(j(Some(d(0, None)), Unknown).level, Level::Bad);
    assert_eq!(j(Some(d(1, Some(30))), Passed).level, Level::Warn);
    assert_eq!(j(Some(d(2, Some(1000))), Passed).level, Level::Ok);
    assert_eq!(j(Some(d(2, Some(1001))), Passed).level, Level::Warn);
    assert_eq!(
        j(Some(d(2, Some(40))), Passed).detail,
        "能解析，40 ms · 运营商的也正常"
    );
    // no local probe: the carrier's alone (the normal reason adds no words)
    assert_eq!(j(None, Passed).detail, "运营商 DNS 能答");
    assert_eq!(j(None, Failed).level, Level::Bad);
    assert_eq!(j(None, Unknown).level, Level::Na);
    assert_eq!(judge_dns(None, None).level, Level::Na);
}

#[test]
fn e2e_layer() {
    use ResultState::*;
    assert_eq!(judge_e2e(None).level, Level::Na);
    let ok = judge_e2e(Some(&net(Passed, Unknown)));
    assert_eq!(
        (ok.level, ok.detail.as_str()),
        (Level::Ok, "能上网")
    );
    assert_eq!(judge_e2e(Some(&net(Failed, Unknown))).level, Level::Bad);
    let mut n = net(Unknown, Unknown);
    n.reason = "proxy_route_unverified".into();
    let l = judge_e2e(Some(&n));
    assert_eq!(
        (l.level, l.detail_en.as_str()),
        (Level::Na, "Not checked · traffic goes through the proxy")
    );
    assert_eq!(l.detail, "没测成 · 流量经代理");
    for (reason, zh_tail, en_tail) in [
        ("snapshot_or_connection_changed", "检测时连接变了", "connection changed during check"),
        ("snapshot_or_connection_changed_during_probe", "检测时连接变了", "connection changed during check"),
        ("probe_budget_unavailable", "刚测过，稍后再试", "checked moments ago; try shortly"),
        ("device_bound_policy_unverified", "出口绑定没核实", "route binding unverified"),
    ] {
        n.reason = reason.into();
        let l = judge_e2e(Some(&n));
        assert_eq!(l.detail, format!("没测成 · {zh_tail}"));
        assert_eq!(l.detail_en, format!("Not checked · {en_tail}"));
    }
    n.reason = "something_new".into();
    assert_eq!(judge_e2e(Some(&n)).detail, "没测成 · something_new");
}

#[test]
fn clock_layer() {
    use crate::clock::EvidenceStatus as E;
    let j = |s| judge_clock(Some(s), None).level;
    assert_eq!(j(E::Verified), Level::Ok);
    assert_eq!(j(E::Estimated), Level::Info);
    for s in [E::Stale, E::BootMismatch, E::WallChanged, E::Revoked] {
        assert_eq!(j(s), Level::Warn, "{s:?}");
    }
    assert_eq!(j(E::Unknown), Level::Na);
    assert_eq!(judge_clock(None, None).level, Level::Na);
    // an HTTP Date wins over the evidence
    let k = |d| judge_clock(Some(E::Unknown), Some(d));
    assert_eq!((k(0).level, k(0).detail.as_str()), (Level::Ok, "和网络时间一致"));
    assert_eq!((k(-12).level, k(-12).detail.as_str()), (Level::Ok, "慢 12 秒"));
    assert_eq!(k(CLOCK_OK_S + 1).level, Level::Warn);
    assert_eq!(k(CLOCK_BAD_S).level, Level::Warn);
    let bad = k(3600);
    assert_eq!((bad.level, bad.detail.as_str()), (Level::Bad, "快 3600 秒，HTTPS 会失败"));
}

#[test]
fn http_dates() {
    assert_eq!(http_date_unix("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
    assert_eq!(http_date_unix("Sun, 18 Oct 2026 10:50:00 GMT"), Some(1_792_320_600));
    assert_eq!(http_date_unix("Tue, 29 Feb 2028 23:59:59 GMT"), Some(1_835_481_599));
    assert_eq!(http_date_unix("Sun, 18 Oct 2026 10:50:00 UTC"), None);
    assert_eq!(http_date_unix("Sun, 18 Foo 2026 10:50:00 GMT"), None);
    assert_eq!(http_date_unix(""), None);
}

fn layers_at(levels: &[(&'static str, Level)]) -> Vec<Layer> {
    LAYERS
        .iter()
        .map(|id| {
            let lv = levels
                .iter()
                .find(|(i, _)| i == id)
                .map_or(Level::Ok, |(_, l)| *l);
            Layer::new(id).set(lv, "x", "x")
        })
        .collect()
}

#[test]
fn the_dependency_table() {
    // every layer named in it exists, and depends only on layers before it
    for (id, deps) in DEPS {
        let at = LAYERS.iter().position(|l| l == id).expect("known layer");
        for d in *deps {
            assert!(
                LAYERS.iter().position(|l| l == d).unwrap() < at,
                "{id} → {d}"
            );
        }
    }
    let ok = layers_at(&[]);
    assert!(LAYERS.iter().all(|id| blocked_by(&ok, id).is_none()));
    let l = layers_at(&[("sim", Level::Bad)]);
    assert_eq!(blocked_by(&l, "bearer"), Some("sim"));
    // a weak signal is bad but does not block: the bearer still gets checked
    let l = layers_at(&[("signal", Level::Bad)]);
    assert_eq!(blocked_by(&l, "bearer"), None);
    // warn and na never block
    let l = layers_at(&[("bearer", Level::Warn), ("e2e", Level::Na)]);
    assert!(
        ["link", "dns", "e2e", "clock", "proxy"]
            .iter()
            .all(|id| blocked_by(&l, id).is_none())
    );
    // a chain: blocked e2e blocks clock with the first bad one's name
    let mut l = layers_at(&[("bearer", Level::Bad)]);
    let e2e = l.iter().position(|x| x.id == "e2e").unwrap();
    l[e2e] = Layer::new("e2e").blocked("bearer");
    assert_eq!(blocked_by(&l, "clock"), Some("bearer"));
    assert_eq!(blocked_by(&l, "wifi"), None);
}

#[test]
fn bad_bearer_blocks_everything_after_it_and_is_the_main_cause() {
    let mut run = Run::new(1, 0, "touch", true);
    for id in LAYERS {
        let l = Layer::new(id);
        let l = match id {
            "bearer" => l.set(Level::Bad, "没拨上号", "Not connected"),
            _ => match blocked_by(&run.layers, id) {
                Some(by) => l.blocked(by),
                None => l.set(Level::Ok, "x", "x"),
            },
        };
        run.put(l);
    }
    for id in ["link", "dns", "e2e", "clock", "crowd", "proxy"] {
        let l = run.layers.iter().find(|l| l.id == id).unwrap();
        assert_eq!((l.level, l.counted), (Level::Blocked, false), "{id}");
        assert_eq!(l.detail, "前面的数据连接不通");
    }
    // N/M leaves them out: wifi sim signal limit bearer
    assert_eq!((run.step, run.steps), (5, 5));
    let m = pick_main(&run.layers);
    assert_eq!(
        (m.layer, m.more, m.text.as_str()),
        ("bearer", 0, "数据连接没建立")
    );
    assert_eq!(json!(Level::Blocked), json!("blocked"));
}

#[test]
fn main_cause_words_for_the_new_layers() {
    let m = pick_main(&layers_at(&[("sim", Level::Warn)]));
    assert_eq!((m.layer, m.text.as_str()), ("sim", "eSIM 通道占满"));
    let mut l = layers_at(&[]);
    l[1] = Layer::new("sim").set(Level::Bad, "没有 SIM 卡", "No SIM card");
    assert_eq!(pick_main(&l).text, "没有 SIM 卡");
    assert_eq!(
        pick_main(&layers_at(&[("bearer", Level::Warn)])).text,
        "回包可能被丢"
    );
    assert_eq!(
        pick_main(&layers_at(&[("dns", Level::Bad)])).text,
        "域名解析有问题"
    );
    assert_eq!(
        pick_main(&layers_at(&[("e2e", Level::Bad)])).text,
        "外网不通"
    );
    assert_eq!(
        pick_main(&layers_at(&[("clock", Level::Warn)])).text,
        "设备时间可能不准"
    );
    // a bad one wins over an earlier warn; blocked never counts
    let m = pick_main(&layers_at(&[
        ("sim", Level::Warn),
        ("dns", Level::Bad),
        ("crowd", Level::Blocked),
    ]));
    assert_eq!((m.layer, m.more), ("dns", 1));
}
