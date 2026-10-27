//! Parity with the screen's C (touch-ui tests/parity/gen.py): every case in
//! tests/fixtures/screen_net_corpus.jsonl must come out field for field the same.
//! Rule changes since then are re-recorded with `bless_corpus` below.

use super::*;
use std::path::PathBuf;

fn merge(base: &mut Value, patch: &Value) {
    let (Some(b), Some(p)) = (base.as_object_mut(), patch.as_object()) else {
        return;
    };
    for (k, v) in p {
        if v.is_null() {
            b.remove(k);
        } else if v.is_object() && b.get(k).is_some_and(Value::is_object) {
            merge(b.get_mut(k).unwrap(), v);
        } else {
            b.insert(k.clone(), v.clone());
        }
    }
}

#[test]
fn matches_the_screens_c_on_the_corpus() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let template: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("tests/fixtures/snapshot-normal.json")).unwrap(),
    )
    .unwrap();
    let corpus =
        std::fs::read_to_string(root.join("tests/fixtures/screen_net_corpus.jsonl")).unwrap();
    // the stall window must not change any old verdict: unknown (what `net_view`
    // gives) and a healthy one (traffic both ways) both match the fixture
    let healthy = CellWindow {
        tx_packets: 50,
        rx_packets: 40,
        span_ms: 30_000,
    };
    let mut n = 0;
    let mut bad = Vec::new();
    for (line, win) in corpus
        .lines()
        .skip(1)
        .flat_map(|l| [(l, None), (l, Some(healthy))])
    {
        let case: Value = serde_json::from_str(line).unwrap();
        let mut state = template.clone();
        merge(&mut state, &case["patch"]);
        let got = serde_json::to_value(net_view_with(&state, win)).unwrap();
        let mut want = case["view"].clone();
        want.as_object_mut().unwrap().remove("parsed");
        // fields added after the corpus was frozen: tested on their own below
        let got = strip_added_fields(got);
        n += 1;
        if got != want {
            let (g, w) = (got.as_object().unwrap(), want.as_object().unwrap());
            let mut diff: Vec<String> = w
                .iter()
                .filter(|(k, v)| g.get(*k) != Some(v))
                .map(|(k, v)| {
                    format!(
                        "{k}: want {v} got {}",
                        g.get(k).map_or("∅".into(), |x| x.to_string())
                    )
                })
                .collect();
            diff.extend(
                g.keys()
                    .filter(|k| !w.contains_key(*k))
                    .map(|k| format!("{k}: extra")),
            );
            let w = if win.is_some() {
                " (healthy window)"
            } else {
                ""
            };
            bad.push(format!("case {}{w}: {}", case["id"], diff.join("; ")));
        }
    }
    assert!(n > 2000, "corpus too small: {n}");
    assert!(
        bad.is_empty(),
        "{} of {n} cases differ:\n{}",
        bad.len(),
        bad.iter().take(15).cloned().collect::<Vec<_>>().join("\n")
    );
}

/// Drop what was added after the corpus was frozen (mode_word/mode_auto, and the
/// L2 English siblings + story.state), so the Chinese still compares field by field.
fn strip_added_fields(mut got: Value) -> Value {
    let o = got.as_object_mut().unwrap();
    o.retain(|k, _| !k.ends_with("_en") && k != "mode_word" && k != "mode_auto" && k != "home");
    let st = o.get_mut("story").unwrap().as_object_mut().unwrap();
    st.retain(|k, _| !k.ends_with("_en") && k != "state" && k != "fix");
    got
}

#[test]
fn c_scalar_reads() {
    assert_eq!(strtol("  -12x"), Some(-12));
    assert_eq!(strtol("x"), None);
    assert_eq!(atoi("18.0"), 18);
    assert_eq!(atof("-3.25dB"), -3.25);
    assert_eq!(atof(""), 0.0);
    let mut f = [0f64; 11];
    assert_eq!(scan_floats("263,3,0,1750,20", 11, &mut f), 5);
    assert_eq!(scan_floats("1,2,x", 11, &mut f), 2);
    assert_eq!(
        scan_floats("0,17,0,78,627264,100,0,-90,-10,15.5,-60,9", 11, &mut f),
        11
    );
    assert_eq!(band_short("LTE BAND 3", false), "B3");
    assert_eq!(band_short("GSM 900", false), "GSM 900");
    assert_eq!(band_short("", true), "-");
    assert_eq!(atoi_after_first_byte("-"), 0);
    assert_eq!(atoi_after_first_byte("n78"), 78);
    assert_eq!(cstr("abcdef".into(), 4), "abc");
    assert_eq!(cstr("中国移动".into(), 5), "中");
}

// ---- named cases ----

fn base() -> NetIn<'static> {
    NetIn {
        sim_state: "sim ready",
        airplane: false,
        net_type: "SA",
        bars: 5,
        data_up: true,
        connected: true,
        win: None,
        roaming: 0,
        n_active: 3,
        nr_active: 3,
        lte_active: 0,
        mhz: 220,
        sinr_valid: true,
        sinr: 17.7,
        rsrp_valid: true,
        rsrp: -87,
        rsrq_valid: true,
        rsrq: -11,
        mcc: 460,
        mnc: 11,
        nr_band: 78,
        nr_mhz: 220,
        rx_bps: 0,
        ambr_dl: 1668.64,
        net_select: "WL_AND_5G",
        data_sw: Sw::Unknown,
        roam_sw: Sw::Unknown,
        hot: false,
    }
}

fn with(f: impl FnOnce(&mut NetIn<'static>)) -> Story {
    let mut x = base();
    f(&mut x);
    story(&x)
}

#[test]
fn radio_names_and_bands() {
    assert_eq!(rat_of("ENDC"), Rat::Nsa);
    assert_eq!(rat_of("LTE-A"), Rat::G4);
    assert_eq!(rat_of("WCDMA"), Rat::G3);
    assert_eq!(rat_of("GSM"), Rat::G2);
    assert_eq!(rat_of("UNREGISTERED"), Rat::None);
    assert_eq!(rat_of("LIMITED_SERVICE_SA"), Rat::None);
    assert_eq!(rat_of("NR5G_SA"), Rat::Sa);
    for (raw, lte, want) in [
        ("NSA", 1, "5G NSA · 4G 锚点"),
        ("SA", 1, "5G SA"),
        ("HSPA+", 1, "3G HSPA+"),
        ("EDGE", 1, "2G EDGE"),
        ("CDMA2000", 1, "3G CDMA2000"),
        ("LIMITED_SERVICE", 1, ""),
        ("LTE", 1, "4G LTE"),
        ("LTE", 2, "4G LTE-A"),
        ("WCDMA", 1, "3G WCDMA"),
        ("GSM", 1, "2G GSM"),
    ] {
        assert_eq!(rat_long(raw, lte).0, want, "{raw}");
    }
    for (raw, nr, want) in [
        ("LTE BAND 3", false, "B3"),
        ("NR5G BAND 78", true, "n78"),
        ("n78", true, "n78"),
        ("B41", false, "B41"),
        ("", false, "-"),
        ("DCS", false, "DCS"),
        ("GSM 900", false, "GSM 900"),
        ("n261", true, "n261"),
    ] {
        assert_eq!(band_short(raw, nr), want, "{raw}");
    }
}

#[test]
fn story_every_situation_in_priority_order() {
    let o = with(|_| {});
    assert_eq!((o.headline.as_str(), o.tone), ("顺畅", Tone::Ok));
    assert_eq!(
        (o.rat.as_str(), o.link.as_str()),
        ("5G-A", "3 条载波聚合 · 带宽很宽")
    );
    assert_eq!(
        (
            o.sig.as_str(),
            o.sig_tone,
            o.noise.as_str(),
            o.load.as_str(),
            o.limit.as_str(),
            o.cause
        ),
        ("强", Tone::Ok, "小", "", "无", Cause::None)
    );
    assert_eq!(o.hint, "");

    let h = |f: fn(&mut NetIn<'static>)| {
        let o = with(f);
        (o.headline, o.tone)
    };
    assert_eq!(
        h(|x| {
            x.sim_state = "sim absent";
            x.bars = 0
        }),
        ("无 SIM".into(), Tone::Bad)
    );
    assert_eq!(
        h(|x| {
            x.airplane = true;
            x.bars = 0
        }),
        ("移动网络已关".into(), Tone::Neutral)
    );
    assert_eq!(
        h(|x| {
            x.net_type = "LIMITED_SERVICE";
            x.bars = 0
        }),
        ("只能紧急呼叫".into(), Tone::Bad)
    );
    assert_eq!(
        h(|x| {
            x.net_type = "";
            x.bars = 0
        }),
        ("无服务".into(), Tone::Bad)
    );
    assert_eq!(h(|x| x.data_up = false), ("没连上网".into(), Tone::Bad));

    assert!(
        with(|x| {
            x.data_up = false;
            x.roaming = 1
        })
        .hint
        .contains("数据漫游")
    );
    let o = with(|x| {
        x.data_up = false;
        x.roaming = 1;
        x.roam_sw = Sw::On
    });
    assert!(o.hint.contains("正在拨号") && !o.hint.contains("打开"));
    assert!(
        with(|x| {
            x.data_up = false;
            x.roaming = 1;
            x.roam_sw = Sw::Off
        })
        .hint
        .contains("数据漫游关着")
    );
    assert!(
        with(|x| {
            x.data_up = false;
            x.roaming = 1;
            x.roam_sw = Sw::Off;
            x.data_sw = Sw::Off
        })
        .hint
        .contains("移动数据关着")
    );

    assert_eq!(h(|x| x.bars = 2), ("慢：信号弱".into(), Tone::Warn));
    assert_eq!(h(|x| x.sinr = -2.5), ("慢：干扰大".into(), Tone::Warn));
    // 信号弱只看格数：5 格满时 RSRP 再低也不说「信号弱」（以前 RSRP < -110 也判弱）
    assert_eq!(h(|x| x.rsrp = -115), ("顺畅".into(), Tone::Ok));
    let o = with(|x| {
        x.bars = 2;
        x.rsrp = -115
    });
    assert!(o.headline == "慢：信号弱" && o.sig == "弱" && o.hint.contains("RSRP -115"));
    assert_eq!(
        h(|x| {
            x.bars = 1;
            x.roaming = 1
        }),
        ("慢：信号弱".into(), Tone::Warn)
    );
    assert_eq!(
        h(|x| {
            x.bars = 2;
            x.sinr = -3.0
        }),
        ("慢：信号弱".into(), Tone::Warn)
    );
    let o = with(|x| x.sinr = -2.5);
    assert!(
        o.sig == "强" && o.noise == "大" && o.cause == Cause::Noise && o.hint.contains("SINR -2.5")
    );
    assert_eq!([5, 4, 3, 2, 1, 0].map(bars_tier), [2, 2, 1, 0, 0, -1]);

    assert_eq!(h(|x| x.roaming = 1), ("顺畅".into(), Tone::Ok));
    assert_eq!(
        h(|x| {
            x.net_type = "WCDMA";
            x.n_active = 0;
            x.sinr_valid = false;
            x.rsrp_valid = false
        }),
        ("只有 3G".into(), Tone::Warn)
    );
    assert_eq!(
        h(|x| {
            x.net_type = "EDGE";
            x.n_active = 0;
            x.sinr_valid = false;
            x.rsrp_valid = false
        }),
        ("只有 2G".into(), Tone::Warn)
    );
    let o = with(|x| {
        x.net_type = "WCDMA";
        x.n_active = 0;
        x.sinr_valid = false;
        x.rsrp_valid = false;
        x.net_select = "Only_WCDMA"
    });
    assert!(o.hint.contains("限定") && o.link == "这个制式没有载波聚合");
    let o = with(|x| {
        x.net_type = "NSA";
        x.n_active = 2;
        x.nr_active = 1;
        x.lte_active = 1;
        x.mhz = 120
    });
    assert!(o.headline == "顺畅" && o.rat == "5G" && o.link == "4G 锚点 + 5G，2 条载波 · 带宽充足");
    let o = with(|x| {
        x.net_type = "LTE";
        x.n_active = 3;
        x.nr_active = 0;
        x.lte_active = 3;
        x.mhz = 60
    });
    assert!(o.rat == "4G+" && o.link == "3 条载波聚合 · 带宽一般");
    let o = with(|x| {
        x.net_type = "LTE";
        x.n_active = 1;
        x.nr_active = 0;
        x.lte_active = 1;
        x.mhz = 20;
        x.net_select = "Only_LTE"
    });
    assert!(
        o.rat == "4G"
            && o.link == "单载波 · 带宽偏窄"
            && o.headline == "慢：载波窄"
            && o.cause == Cause::Narrow
    );
    let o = with(|x| {
        x.net_type = "LTE";
        x.n_active = 2;
        x.nr_active = 0;
        x.lte_active = 2;
        x.mhz = 40;
        x.net_select = "Only_LTE"
    });
    assert!(o.headline == "顺畅" && o.hint.contains("只用 4G") && o.tone == Tone::Ok);
    assert_eq!(
        with(|x| {
            x.net_type = "LTE";
            x.n_active = 1;
            x.nr_active = 0;
            x.lte_active = 1;
            x.mhz = 0
        })
        .link,
        "单载波"
    );
    let o = with(|x| x.net_select = "TCHGWL_5G");
    assert!(o.hint.is_empty() || !o.hint.contains("只用"));
    let o = with(|x| x.bars = 3);
    assert!(o.sig == "中" && o.sig_tone == Tone::Warn);
    let o = with(|x| x.sinr = 5.0);
    assert!(o.noise == "中" && o.sig == "强");
    let o = with(|x| x.sinr = -3.0);
    assert!(o.noise == "大" && o.noise_tone == Tone::Bad);

    // why it is slow
    assert_eq!(h(|x| x.ambr_dl = 5.0), ("慢：限速".into(), Tone::Warn));
    let o = with(|x| x.ambr_dl = 5.0);
    assert!(o.cause == Cause::Limit && o.limit == "有" && o.hint.contains("5 Mbps"));
    let o = with(|x| x.ambr_dl = 0.0);
    assert!(o.limit == "—" && o.headline == "顺畅");
    assert_eq!(
        h(|x| {
            x.ambr_dl = 5.0;
            x.sinr = -3.0
        }),
        ("慢：限速".into(), Tone::Warn)
    );
    assert_eq!(
        h(|x| {
            x.rsrq = -18;
            x.rx_bps = 400_000
        }),
        ("慢：疑似拥挤".into(), Tone::Warn)
    );
    let o = with(|x| {
        x.rsrq = -18;
        x.rx_bps = 400_000
    });
    assert!(
        o.cause == Cause::Crowd && o.load == "高" && o.sig == "强" && o.hint.contains("RSRQ -18")
    );
    assert_eq!(
        h(|x| {
            x.rsrq = -18;
            x.rx_bps = 20_000
        }),
        ("顺畅".into(), Tone::Ok)
    );
    assert_eq!(with(|x| x.rsrq = -18).load, "");
    assert_eq!(with(|x| x.rx_bps = 400_000).load, "正常");
    // 判不判拥挤也看格数，不看 RSRP：5 格、RSRP -116 照样判；2 格（信号弱）、RSRP -95 不判
    assert_eq!(
        with(|x| {
            x.rsrp = -116;
            x.rx_bps = 400_000
        })
        .load,
        "正常"
    );
    let o = with(|x| {
        x.bars = 2;
        x.rsrp = -95;
        x.rsrq = -18;
        x.rx_bps = 400_000
    });
    assert!(o.headline == "慢：信号弱" && o.load.is_empty());
    assert_eq!(
        h(|x| {
            x.net_type = "LTE";
            x.n_active = 2;
            x.lte_active = 2;
            x.nr_active = 0;
            x.mhz = 40;
            x.rsrq = -13;
            x.rx_bps = 400_000
        }),
        ("慢：疑似拥挤".into(), Tone::Warn)
    );
    assert_eq!(
        h(|x| {
            x.rsrp = -104;
            x.sinr = -1.0;
            x.rsrq = -18;
            x.rx_bps = 400_000
        }),
        ("慢：干扰大".into(), Tone::Warn)
    );
    assert_eq!(
        h(|x| {
            x.n_active = 1;
            x.nr_active = 1;
            x.mhz = 20
        }),
        ("慢：载波窄".into(), Tone::Warn)
    );
    assert_eq!(
        with(|x| {
            x.n_active = 1;
            x.nr_active = 1;
            x.mhz = 100
        })
        .headline,
        "顺畅"
    );
    assert_eq!(
        h(|x| {
            x.n_active = 1;
            x.nr_active = 1;
            x.mhz = 15;
            x.roaming = 1
        }),
        ("慢：载波窄".into(), Tone::Warn)
    );

    // 2026-09-25 on the device: SA, one n5 15 MHz carrier, SINR -1.9, RSRP -102, RSRQ -17
    let o = with(|x| {
        x.bars = 3;
        x.n_active = 1;
        x.nr_active = 1;
        x.mhz = 15;
        x.sinr = -1.9;
        x.rsrp = -102;
        x.rsrq = -17;
        x.rx_bps = 0
    });
    assert!(
        o.headline == "慢：干扰大"
            && o.sig == "中"
            && o.noise == "大"
            && o.load.is_empty()
            && o.limit == "无"
            && o.cause == Cause::Noise
            && o.hint.contains("SINR -1.9")
    );
    assert!(o.headline.len() <= 18 && o.hint.len() <= 60);
}

#[test]
fn status_bar_label_by_the_network_you_are_on() {
    let z = || {
        let mut x = base();
        x.net_type = "SA";
        x.mcc = 460;
        x.mnc = 11;
        x.nr_active = 1;
        x.nr_band = 78;
        x.nr_mhz = 100;
        x.mhz = 100;
        x
    };
    let b = |f: fn(&mut NetIn<'static>)| {
        let mut x = z();
        f(&mut x);
        net_badge(&x)
    };
    assert_eq!(b(|_| {}), "5G");
    // 移动 2CC 200 MHz 不算 5G-A（只有电信/联通的 2CC ≥200 MHz 算）
    assert_eq!(
        b(|x| {
            x.mnc = 0;
            x.nr_active = 2;
            x.nr_mhz = 200
        }),
        "5G"
    );
    assert_eq!(b(|x| x.nr_active = 3), "5G-A");
    assert_eq!(
        b(|x| {
            x.net_type = "NSA";
            x.nr_active = 3
        }),
        "5G-A"
    );
    assert_eq!(
        b(|x| {
            x.mnc = 1;
            x.nr_active = 2;
            x.nr_mhz = 200
        }),
        "5G-A"
    );
    assert_eq!(
        b(|x| {
            x.mnc = 1;
            x.nr_active = 2;
            x.nr_mhz = 160
        }),
        "5G"
    );
    assert_eq!(
        b(|x| {
            x.mcc = 310;
            x.mnc = 260;
            x.nr_band = 41
        }),
        "5G UC"
    );
    assert_eq!(
        b(|x| {
            x.mcc = 310;
            x.mnc = 260;
            x.nr_band = 71
        }),
        "5G"
    );
    assert_eq!(
        b(|x| {
            x.mcc = 311;
            x.mnc = 480;
            x.nr_band = 77
        }),
        "5G UW"
    );
    assert_eq!(
        b(|x| {
            x.mcc = 311;
            x.mnc = 480;
            x.nr_band = 48
        }),
        "5G UW"
    );
    assert_eq!(
        b(|x| {
            x.mcc = 311;
            x.mnc = 480;
            x.nr_band = 5;
            x.mhz = 10
        }),
        "5G"
    );
    assert_eq!(
        b(|x| {
            x.mcc = 310;
            x.mnc = 410;
            x.nr_band = 77
        }),
        "5G+"
    );
    assert_eq!(
        b(|x| {
            x.mcc = 310;
            x.mnc = 410;
            x.nr_band = 5;
            x.mhz = 50
        }),
        "5G+"
    );
    assert_eq!(
        b(|x| {
            x.mcc = 310;
            x.mnc = 410;
            x.nr_band = 5;
            x.mhz = 10
        }),
        "5G"
    );
    // 电信和联通共建共享：2CC ≥200 MHz 两家都算
    assert_eq!(
        b(|x| {
            x.mnc = 11;
            x.nr_active = 2;
            x.nr_mhz = 200
        }),
        "5G-A"
    );
    // docomo：n78/n79 是 5G+，按频段不按载波数
    assert_eq!(
        b(|x| {
            x.mcc = 440;
            x.mnc = 10
        }),
        "5G+"
    );
    assert_eq!(
        b(|x| {
            x.mcc = 440;
            x.mnc = 10;
            x.nr_band = 28;
            x.nr_active = 3
        }),
        "5G"
    );
    assert_eq!(
        b(|x| {
            x.net_type = "LTE";
            x.mcc = 440;
            x.mnc = 10;
            x.nr_active = 0;
            x.lte_active = 1
        }),
        "4G+"
    );
    assert_eq!(
        b(|x| {
            x.net_type = "LTE";
            x.mcc = 440;
            x.mnc = 20;
            x.nr_active = 0;
            x.lte_active = 1
        }),
        "4G"
    );
    assert_eq!(
        b(|x| {
            x.net_type = "LTE";
            x.nr_active = 0;
            x.lte_active = 2
        }),
        "4G+"
    );
    assert_eq!(
        b(|x| {
            x.net_type = "LTE";
            x.nr_active = 0;
            x.lte_active = 1
        }),
        "4G"
    );
    assert_eq!(
        b(|x| {
            x.mcc = 466;
            x.mnc = 92;
            x.nr_active = 3
        }),
        "5G"
    );
    assert_eq!(
        b(|x| {
            x.mcc = 454;
            x.mnc = 12;
            x.nr_active = 3
        }),
        "5G"
    );
    assert_eq!(
        b(|x| {
            x.mcc = 234;
            x.mnc = 30;
            x.nr_active = 3
        }),
        "5G"
    );
    assert_eq!(
        b(|x| {
            x.net_type = "LTE";
            x.mcc = 440;
            x.nr_active = 0;
            x.lte_active = 2
        }),
        "4G+"
    );
    assert_eq!(
        b(|x| {
            x.net_type = "LTE";
            x.mcc = 466;
            x.nr_active = 0;
            x.lte_active = 3
        }),
        "4G+"
    );
    assert_eq!(
        b(|x| {
            x.net_type = "LTE";
            x.mcc = 466;
            x.nr_active = 0;
            x.lte_active = 1
        }),
        "4G"
    );
    assert_eq!(
        b(|x| {
            x.net_type = "LTE";
            x.nr_active = 0;
            x.lte_active = 3
        }),
        "4G+"
    );
    assert_eq!(
        b(|x| {
            x.net_type = "LTE";
            x.mcc = 234;
            x.nr_active = 0;
            x.lte_active = 2
        }),
        "4G"
    );
    assert_eq!(
        b(|x| {
            x.net_type = "LTE";
            x.mcc = 310;
            x.mnc = 260;
            x.nr_active = 0;
            x.lte_active = 1
        }),
        "LTE"
    );
    assert_eq!(
        b(|x| {
            x.net_type = "LTE";
            x.mcc = 311;
            x.mnc = 480;
            x.nr_active = 0;
            x.lte_active = 2
        }),
        "LTE"
    );
    assert_eq!(
        b(|x| {
            x.net_type = "HSPA+";
            x.nr_active = 0
        }),
        "3G"
    );
    assert_eq!(
        b(|x| {
            x.net_type = "EDGE";
            x.nr_active = 0
        }),
        "2G"
    );
    assert_eq!(b(|x| x.net_type = "LIMITED_SERVICE"), "SOS");
    assert_eq!(
        b(|x| {
            x.roaming = 1;
            x.mcc = 310;
            x.mnc = 260;
            x.nr_band = 41
        }),
        "5G UC"
    );
    assert_eq!(
        b(|x| {
            x.roaming = 1;
            x.mcc = 440;
            x.mnc = 20;
            x.nr_active = 3
        }),
        "5G"
    );
    let o = with(|x| {
        x.net_type = "";
        x.bars = 0;
        x.data_up = false
    });
    assert!(o.rat == "无服务" && o.headline == "无服务");
}

#[test]
fn logos_and_imsi() {
    assert_eq!(operator_logo(460, 0), Some("china-mobile"));
    assert_eq!(operator_logo(460, 13), Some("china-mobile"));
    assert_eq!(imsi_plmn("460131234567802"), Some((460, 13)));
    assert_eq!(operator_logo(460, 11), Some("china-telecom"));
    assert_eq!(operator_logo(311, 480), Some("verizon"));
    assert_eq!(sim_logo(454, 12, "CMLink"), Some("cmlink"));
    assert_eq!(sim_logo(454, 12, ""), Some("cmhk"));
    assert_eq!(operator_logo(455, 1), Some("ctm"));
    assert_eq!(operator_logo(454, 3), Some("three-hk"));
    assert_eq!(imsi_plmn("460011234567890"), Some((460, 1)));
    assert_eq!(imsi_plmn("310260123456789"), Some((310, 260)));
    assert_eq!(imsi_plmn("466920123456789"), Some((466, 92)));
    assert_eq!(imsi_plmn(""), None);
    assert_eq!(operator_logo(0, 0), None);
    assert_eq!(operator_logo(234, 15), None);
}

#[test]
fn phone_style_labels_and_families() {
    for (raw, want, fam) in [
        ("GSM", "2G", "GSM"),
        ("GPRS", "2G", "GPRS"),
        ("EDGE", "2G", "EDGE"),
        ("CDMA", "2G", "CDMA 1X"),
        ("1xRTT", "2G", "CDMA 1X"),
        ("WCDMA", "3G", "WCDMA"),
        ("UMTS", "3G", "WCDMA"),
        ("HSPA", "3G", "HSPA"),
        ("HSPA+", "3G", "HSPA+"),
        ("DC-HSPA+", "3G", "HSPA+"),
        ("TD-SCDMA", "3G", "TD-SCDMA"),
        ("CDMA2000", "3G", "CDMA2000"),
        ("EVDO", "3G", "CDMA2000"),
        ("eHRPD", "3G", "CDMA2000"),
        ("LTE", "4G", "LTE"),
        ("TD-LTE", "4G", "LTE"),
        ("FDD-LTE", "4G", "LTE"),
        ("4G", "4G", "LTE"),
        ("LTE", "4G", "LTE"),
        ("LTE-A", "4G", "LTE"),
        ("LTE_CA", "4G", "LTE"),
        ("4G+", "4G", "LTE"),
        ("SA", "5G", "NR"),
        ("NSA", "5G", "NR"),
        ("ENDC", "5G", "NR"),
        ("SA", "5G", "NR"),
        ("NSA", "5G", "NR"),
        ("SA", "5G", "NR"),
        ("5G-A", "5G-A", "NR"),
        ("LIMITED_SERVICE", "", ""),
        ("LIMITED_SERVICE_SA", "", ""),
        ("", "", ""),
    ] {
        assert_eq!((net_label(raw), rat_family(raw)), (want, fam), "{raw}");
    }
}

#[test]
fn radio_mode_words() {
    let w = |s: &str| net_select_word(s).0;
    assert_eq!(w("TCHGWL_5G"), "自动");
    assert_eq!(w("WL_AND_5G"), "自动");
    assert_eq!(w("NETWORK_auto"), "自动");
    assert_eq!(w("Only_5G"), "只用 5G SA");
    assert_eq!(w("LTE_AND_5G"), "只用 5G NSA");
    assert_eq!(w("Only_GSM_WCDMA"), "只用 3G 和 2G");
    assert_eq!(w(""), "-");
    assert_eq!(w("SOMETHING_NEW"), "SOMETHING_NEW");
    assert!(
        net_select_is_auto("TCHGWL_5G")
            && net_select_is_auto("WL_AND_5G")
            && net_select_is_auto("NETWORK_auto")
            && !net_select_is_auto("Only_LTE")
            && !net_select_is_auto("WCDMA_AND_LTE")
    );
    let mut state: Value = serde_json::from_str(
        &std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/snapshot-normal.json"),
        )
        .unwrap(),
    )
    .unwrap();
    state["net"]["net_select"] = "Only_LTE".into();
    let v = net_view(&state);
    assert_eq!((v.mode_word.as_str(), v.mode_auto), ("只用 4G", false));
}

fn corpus_cases() -> (Value, Vec<String>) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let template: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("tests/fixtures/snapshot-normal.json")).unwrap(),
    )
    .unwrap();
    let corpus =
        std::fs::read_to_string(root.join("tests/fixtures/screen_net_corpus.jsonl")).unwrap();
    (template, corpus.lines().map(str::to_string).collect())
}

/// 用户真机截图：状态栏 5 格满、右边「信号强」，大字却是「慢：信号弱 / RSRP -116」。
#[test]
fn full_bars_never_say_weak_signal() {
    let (mut state, _) = corpus_cases();
    state["net"]["bars"] = 5.into();
    state["net"]["nr_rsrp"] = (-116).into();
    state["net"]["nr_bw"] = "100".into();
    state["net"]["nr_channel"] = 627264.into();
    state["net"]["wan_status"] = "connected".into();
    let v = net_view(&state);
    assert_eq!(v.carriers[0].rsrp, "-116");
    assert_eq!(v.story.sig, "强");
    assert!(
        !v.story.headline.contains("信号弱") && v.story.cause != Cause::Weak,
        "{:?}",
        v.story
    );
}

/// 在 LTE 上时 nr_* 留着上次 5G 的旧读数（实测 nr_rsrp -116、lte_rsrp -95）：
/// 不能出 NR 载波，主信号要用 LTE 的。
#[test]
fn stale_nr_readings_ignored_off_5g() {
    let (mut state, _) = corpus_cases();
    let net = &mut state["net"];
    net["type"] = "LTE".into();
    net["band"] = "LTE BAND 3".into();
    net["bars"] = 4.into();
    net["nr_rsrp"] = (-116).into();
    net["nr_snr"] = "-3.0".into();
    net["nr_channel"] = 627264.into();
    net["nr_bw"] = "100".into();
    net["nrca"] = "1,78,1,627264,100,-116,-12,-3.0,0,0,0".into();
    net["lte_rsrp"] = (-95).into();
    net["lte_rsrq"] = (-10).into();
    net["lte_snr"] = "12.0".into();
    net["lte_pci"] = 101.into();
    net["channel"] = 1850.into();
    net["bandwidth"] = "20".into();
    net["wan_status"] = "connected".into();
    let v = net_view(&state);
    assert!(
        v.carriers.iter().all(|c| c.kind == "lte"),
        "{:?}",
        v.carriers
    );
    assert_eq!((v.act_nr, v.nr_band0, v.nr_mhz), (0, 0, 0));
    let c0 = &v.carriers[0];
    assert_eq!((c0.rsrp.as_str(), c0.sinr.as_str()), ("-95", "12.0"));
    assert!(!v.story.headline.contains("信号弱") && !v.story.hint.contains("-116"));
    assert_eq!(v.story.noise, "中");

    // B27 固件的 5 段 lteca（没有信号值）：PCI 对上 lte_pci 时主信号取 lte_*
    state["net"]["lteca"] = "101,3,0,1850,20".into();
    let v = net_view(&state);
    let c0 = &v.carriers[0];
    assert_eq!(
        (c0.kind, c0.rsrp.as_str(), c0.sinr.as_str()),
        ("lte", "-95", "12.0")
    );
    state["net"]["lteca"] = "".into();

    // 到了 3G/2G，lteca 里的也是旧读数：不出载波，结论是「只有 3G」，不拿旧 SINR 判干扰
    state["net"]["type"] = "WCDMA".into();
    state["net"]["lteca"] = "0,3,0,1850,20,0,-118,-14,-2.5,-60,0".into();
    let v = net_view(&state);
    assert!(
        v.carriers.is_empty() && v.ca_val == "无聚合",
        "{:?}",
        v.carriers
    );
    assert_eq!(v.story.headline, "只有 3G");
    state["net"]["lteca"] = "".into();

    // 同一份读数换成 5G（NSA）时 NR 载波照常排第一
    state["net"]["type"] = "NSA".into();
    let v = net_view(&state);
    assert_eq!(v.carriers[0].kind, "nr");
}

/// 遍历全部对照样本：「信号弱」和右边的 sig（= 状态栏格数）不能互相矛盾。
/// 限速、没连上网、无服务等更高优先级的结论会先返回，所以不是严格的「⇔」：
/// (a) 大字或说明提到信号弱 ⇒ sig 是「弱」；
/// (b) sig 是「弱」⇒ 大字不是排在「信号弱」后面的结论；
/// (c) 信号弱时不判负载（load 为空）。
#[test]
fn weak_signal_agrees_with_bars_on_the_corpus() {
    let (template, lines) = corpus_cases();
    let later = [
        "顺畅",
        "慢：干扰大",
        "慢：疑似拥挤",
        "慢：载波窄",
        "只有 2G",
        "只有 3G",
    ];
    let (mut n, mut weak) = (0, 0);
    for line in lines.iter().skip(1) {
        let case: Value = serde_json::from_str(line).unwrap();
        let mut state = template.clone();
        merge(&mut state, &case["patch"]);
        let s = net_view(&state).story;
        let says_weak = s.headline.contains("信号弱") || s.hint.contains("离基站远");
        let id = &case["id"];
        assert!(!says_weak || s.sig == "弱", "case {id}: {s:?}");
        if s.sig == "弱" {
            weak += 1;
            assert!(!later.contains(&s.headline.as_str()), "case {id}: {s:?}");
            assert!(s.load.is_empty(), "case {id}: {s:?}");
        }
        n += 1;
    }
    assert!(n > 1000 && weak > 10, "n={n} weak={weak}");
}

/// 改规则后重录对照样本：`SCREEN_CORPUS_BLESS=1 cargo test screen::tests::bless_corpus -- --ignored`。
/// 每条的 patch 和 parsed 原样保留，只换 view（字段顺序同原样本，不含后加的 mode_word/mode_auto、
/// `*_en` 和 story.state）。
#[test]
#[ignore]
fn bless_corpus() {
    if std::env::var_os("SCREEN_CORPUS_BLESS").is_none() {
        return;
    }
    let (template, lines) = corpus_cases();
    let mut out = String::new();
    for (k, line) in lines.iter().enumerate() {
        if k == 0 || line.is_empty() {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let case: Value = serde_json::from_str(line).unwrap();
        let mut state = template.clone();
        merge(&mut state, &case["patch"]);
        let v = net_view(&state);
        // the corpus has story last (after bars_tier), the struct has it in the middle;
        // story's L2 fields (from `state` on) and net's (after mode_word) are cut off
        let story = format!(",\"story\":{}", serde_json::to_string(&v.story).unwrap());
        let view = serde_json::to_string(&v).unwrap().replacen(&story, "", 1);
        let legacy = format!("{}}}", &story[..story.find(",\"state\":").unwrap()]);
        let view = format!(
            "{}{legacy}",
            &view[..view.rfind(",\"mode_word\":").unwrap()]
        );
        let head = &line[..line.find(",\"view\":").unwrap()];
        let parsed = &line[line.rfind(",\"parsed\":").unwrap()..line.len() - 2];
        out.push_str(&format!("{head},\"view\":{view}{parsed}}}}}\n"));
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("tests/fixtures/screen_net_corpus.jsonl");
    let old = std::fs::read_to_string(&path).unwrap();
    let out = if old.ends_with('\n') {
        out
    } else {
        out.trim_end_matches('\n').to_string()
    };
    std::fs::write(path, out).unwrap();
}

// ---- L2: English siblings and story.state ----------------------------------

/// The headline table, hard-coded from manager docs/DESIGN.md §4「首页结论表」and
/// docs/ui-glossary.md §4 (this repo can't read them; touch-ui's cross-repo test
/// compares the docs with screen.rs). (state, 中文大字, English state word, tone)
const HEADLINES: [(&str, &str, &str, Tone); 15] = [
    ("nosim", "无 SIM", "No SIM", Tone::Bad),
    ("airplane", "移动网络已关", "Airplane", Tone::Neutral),
    ("sos", "只能紧急呼叫", "SOS only", Tone::Bad),
    ("nosvc", "无服务", "No service", Tone::Bad),
    ("nodata", "没连上网", "Offline", Tone::Bad),
    ("stall", "连上了但不通", "No traffic", Tone::Bad),
    ("hot", "慢：过热限速", "Slow", Tone::Warn),
    ("limit", "慢：限速", "Slow", Tone::Warn),
    ("weak", "慢：信号弱", "Slow", Tone::Warn),
    ("noise", "慢：干扰大", "Slow", Tone::Warn),
    ("crowd", "慢：疑似拥挤", "Slow", Tone::Warn),
    ("only2g", "只有 2G", "2G only", Tone::Warn),
    ("only3g", "只有 3G", "3G only", Tone::Warn),
    ("narrow", "慢：载波窄", "Slow", Tone::Warn),
    ("ok", "顺畅", "All good", Tone::Ok),
];

fn state_name(s: State) -> String {
    serde_json::to_value(s)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

/// English text may only be ASCII plus the few symbols both languages share.
fn english_ok(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii() || matches!(c, '·' | '—' | '↓' | '↑'))
}

/// The rules every English string follows (glossary + DESIGN.md §1).
fn check_en(what: &str, en: &str) {
    assert!(english_ok(en), "{what}: non-English text in {en:?}");
    assert!(!en.ends_with('.'), "{what}: trailing period in {en:?}");
    assert!(
        !en.to_ascii_lowercase().contains("please"),
        "{what}: \"Please\" in {en:?}"
    );
}

/// (zh, en, C buffer of the zh field on the screen) for every text field the
/// touch screen reads (luyaz-ui: touch-ui/rs/ui/src/net_view.rs, logic.rs).
fn story_pairs(s: &Story) -> Vec<(&'static str, &str, &str, usize)> {
    vec![
        ("headline", s.headline.as_str(), s.headline_en.as_str(), 32),
        ("hint", s.hint.as_str(), s.hint_en.as_str(), 128),
        ("rat", s.rat.as_str(), s.rat_en.as_str(), 32),
        ("link", s.link.as_str(), s.link_en.as_str(), 96),
        ("sig", s.sig.as_str(), s.sig_en.as_str(), 8),
        ("noise", s.noise.as_str(), s.noise_en.as_str(), 8),
        ("load", s.load.as_str(), s.load_en.as_str(), 12),
        ("limit", s.limit.as_str(), s.limit_en.as_str(), 8),
    ]
}

fn net_pairs(v: &NetView) -> Vec<(&'static str, &str, &str, usize)> {
    vec![
        ("fine", v.fine.as_str(), v.fine_en.as_str(), 32),
        ("name", v.name.as_str(), v.name_en.as_str(), 48),
        ("where", v.r#where.as_str(), v.where_en.as_str(), 64),
        ("ca_val", v.ca_val.as_str(), v.ca_val_en.as_str(), 48),
        ("ca_sub", v.ca_sub.as_str(), v.ca_sub_en.as_str(), 160),
        (
            "mode_word",
            v.mode_word.as_str(),
            v.mode_word_en.as_str(),
            32,
        ),
    ]
}

/// `*_en` present ⇔ the Chinese has non-ASCII; fits the same C buffer; English rules.
/// `passthrough` = fields allowed to carry a non-English broadcast name as-is.
fn check_pairs(ctx: &str, pairs: &[(&'static str, &str, &str, usize)], passthrough: &[&str]) {
    for &(k, zh, en, cap) in pairs {
        if zh.is_ascii() {
            assert!(en.is_empty(), "{ctx}: {k}_en {en:?} for ASCII {zh:?}");
            continue;
        }
        assert!(!en.is_empty(), "{ctx}: {k}_en missing for {zh:?}");
        assert!(
            en.len() < cap,
            "{ctx}: {k}_en {en:?} over char[{cap}] on the screen"
        );
        if !passthrough.contains(&k) {
            check_en(&format!("{ctx}: {k}_en"), en);
        }
    }
}

fn check_story(ctx: &str, s: &Story) {
    let name = state_name(s.state);
    let row = HEADLINES
        .iter()
        .find(|r| r.0 == name)
        .unwrap_or_else(|| panic!("{ctx}: state {name} not in the table"));
    assert_eq!(
        (s.headline.as_str(), s.headline_en.as_str(), s.tone),
        (row.1, row.2, row.3),
        "{ctx}: state {name}"
    );
    assert!(s.headline_en.chars().count() <= 10, "{ctx}: {s:?}");
    check_pairs(ctx, &story_pairs(s), &[]);
}

/// One case per say() call, plus the hint variants inside a call; the English
/// hint is the glossary §5 text with the numbers filled in.
#[test]
fn every_branch_has_its_state_and_english() {
    type Case = (fn(&mut NetIn<'static>), &'static str, &'static str);
    let cases: &[Case] = &[
        (
            |x| {
                x.sim_state = "sim absent";
                x.bars = 0
            },
            "nosim",
            "Insert a SIM, or enable an eSIM profile in SIM & eSIM",
        ),
        (
            |x| {
                x.airplane = true;
                x.bars = 0
            },
            "airplane",
            "Turn off airplane mode in the web admin",
        ),
        (
            |x| {
                x.net_type = "LIMITED_SERVICE";
                x.bars = 0;
                x.roaming = 1
            },
            "sos",
            "Ask your carrier to enable roaming, or use a local SIM",
        ),
        (
            |x| {
                x.net_type = "LIMITED_SERVICE";
                x.bars = 0
            },
            "sos",
            "Check your balance or line status; this carrier may have no coverage here",
        ),
        (
            |x| {
                x.net_type = "";
                x.bars = 0;
                x.net_select = "Only_LTE"
            },
            "nosvc",
            "Searching; set network mode to Auto in Cellular",
        ),
        (
            |x| {
                x.net_type = "";
                x.bars = 0
            },
            "nosvc",
            "Searching; try another spot",
        ),
        (
            |x| {
                x.data_up = false;
                x.data_sw = Sw::Off
            },
            "nodata",
            "Turn on mobile data in Cellular",
        ),
        (
            |x| {
                x.data_up = false;
                x.roaming = 1;
                x.roam_sw = Sw::Off
            },
            "nodata",
            "Turn on data roaming in Cellular; your plan must allow it too",
        ),
        (
            |x| {
                x.data_up = false;
                x.roaming = 1;
                x.roam_sw = Sw::On
            },
            "nodata",
            "Connecting, ~30 s after a network change; if stuck, your plan may not roam here",
        ),
        (
            |x| {
                x.data_up = false;
                x.roaming = 1
            },
            "nodata",
            "Check data roaming in Cellular; your plan must allow it too",
        ),
        (
            |x| x.data_up = false,
            "nodata",
            "Check mobile data, APN, or your balance",
        ),
        (
            |x| x.win = Some(win(20, 0)),
            "stall",
            "Signal and data are up, but nothing came back for 30 s",
        ),
        (
            |x| x.hot = true,
            "hot",
            "Too hot; speed limited until it cools",
        ),
        (
            |x| x.ambr_dl = 4.6,
            "limit",
            "Carrier caps speed at 5 Mbps; moving won't help",
        ),
        (
            |x| {
                x.bars = 1;
                x.rsrp = -118
            },
            "weak",
            "Weak signal, RSRP -118 dBm; try near a window",
        ),
        (
            |x| {
                x.bars = 1;
                x.rsrp = -118;
                x.roaming = 1
            },
            "weak",
            "Weak signal, RSRP -118 dBm; try near a window; roaming",
        ),
        (
            |x| {
                x.bars = 2;
                x.rsrp_valid = false
            },
            "weak",
            "Weak signal; try near a window",
        ),
        (
            |x| x.sinr = -3.4,
            "noise",
            "Noisy signal, SINR -3.4 dB; move or rotate the device",
        ),
        (
            |x| {
                x.sinr = -3.4;
                x.roaming = 1
            },
            "noise",
            "Noisy signal, SINR -3.4 dB; move or rotate the device; roaming",
        ),
        (
            |x| {
                x.rx_bps = 200_000;
                x.rsrq = -19
            },
            "crowd",
            "Cell busy, RSRQ -19 dB; moving won't help much",
        ),
        (
            |x| {
                x.net_type = "GSM";
                x.net_select = "Only_GSM"
            },
            "only2g",
            "Network mode is 2G only; set it to Auto in Cellular",
        ),
        (
            |x| x.net_type = "GSM",
            "only2g",
            "Very slow; probably no 4G/5G nearby",
        ),
        (
            |x| {
                x.net_type = "WCDMA";
                x.net_select = "Only_WCDMA"
            },
            "only3g",
            "Network mode is 3G only; set it to Auto in Cellular",
        ),
        (
            |x| x.net_type = "WCDMA",
            "only3g",
            "Online but slow; probably no 4G/5G nearby",
        ),
        (
            |x| {
                x.n_active = 1;
                x.nr_active = 1;
                x.mhz = 20
            },
            "narrow",
            "Only one 20 MHz carrier here",
        ),
        (
            |x| {
                x.net_type = "LTE";
                x.net_select = "Only_LTE"
            },
            "ok",
            "Network mode is 4G only; set it to Auto in Cellular",
        ),
        (|_| {}, "ok", ""),
    ];
    let mut seen = std::collections::BTreeMap::new();
    for (k, (f, want_state, want_hint)) in cases.iter().enumerate() {
        let o = with(*f);
        let ctx = format!("case {k} ({want_state})");
        assert_eq!(state_name(o.state), *want_state, "{ctx}: {o:?}");
        assert_eq!(o.hint_en, *want_hint, "{ctx}: {o:?}");
        check_story(&ctx, &o);
        // a state always means the same headline
        let head = (o.headline.clone(), o.headline_en.clone(), o.tone);
        assert_eq!(
            seen.entry(*want_state).or_insert_with(|| head.clone()),
            &head,
            "{ctx}"
        );
    }
    // every code in the table is reachable, and no two codes share a headline
    let codes: Vec<&str> = HEADLINES.iter().map(|r| r.0).collect();
    assert_eq!(
        seen.keys().copied().collect::<Vec<_>>(),
        {
            let mut c = codes.clone();
            c.sort();
            c
        },
        "state codes"
    );
    let heads: std::collections::BTreeSet<&str> = HEADLINES.iter().map(|r| r.1).collect();
    assert_eq!(heads.len(), HEADLINES.len(), "headline per code is unique");
    let codes_set: std::collections::BTreeSet<&str> = codes.iter().copied().collect();
    assert_eq!(codes_set.len(), HEADLINES.len(), "codes are unique");

    // the hints that point at a page name it as it is now (蜂窝 / Cellular)
    let o = with(|x| {
        x.net_type = "";
        x.bars = 0;
        x.net_select = "Only_LTE"
    });
    assert_eq!(o.hint, "正在搜网；制式被限定，去「蜂窝」改回自动");
    assert_eq!(o.rat_en, "No svc");
    let o = with(|x| x.sim_state = "sim absent");
    assert_eq!(o.hint, "插卡，或在「蜂窝 → SIM 与 eSIM」启用");
}

#[test]
fn right_column_words_in_english() {
    let o = with(|_| {});
    assert_eq!(
        (o.sig_en.as_str(), o.noise_en.as_str(), o.limit_en.as_str()),
        ("Strong", "low", "none")
    );
    assert_eq!(o.link_en, "3-carrier CA · Very wide");
    assert_eq!(o.load_en, "", "idle: no load word");
    let o = with(|x| {
        x.bars = 3;
        x.sinr = 5.0;
        x.rx_bps = 200_000;
        x.rsrq = -10;
        x.mhz = 100;
        x.ambr_dl = 0.0
    });
    assert_eq!(
        (
            o.sig_en.as_str(),
            o.noise_en.as_str(),
            o.load_en.as_str(),
            o.limit_en.as_str(),
            o.link_en.as_str()
        ),
        ("Fair", "mid", "normal", "—", "3-carrier CA · Wide")
    );
    let o = with(|x| {
        x.net_type = "NSA";
        x.n_active = 2;
        x.mhz = 60
    });
    assert_eq!(o.link_en, "4G anchor + 5G, 2 carriers · Fair");
    let o = with(|x| {
        x.net_type = "LTE";
        x.n_active = 1;
        x.mhz = 30
    });
    assert_eq!(o.link_en, "Single carrier · Narrow");
    let o = with(|x| x.net_type = "WCDMA");
    assert_eq!(o.link_en, "No CA on this network");
    // the badge is ASCII except 无服务: no rat_en then
    assert_eq!((o.rat.as_str(), o.rat_en.as_str()), ("3G", ""));
}

#[test]
fn net_fields_in_english() {
    let (mut state, _) = corpus_cases();
    state["net"]["type"] = "NSA".into();
    state["net"]["mcc"] = 460.into();
    state["net"]["mnc"] = 1.into();
    state["net"]["net_select"] = "Only_GSM_WCDMA".into();
    let v = net_view(&state);
    assert_eq!(
        (
            v.fine_en.as_str(),
            v.name.as_str(),
            v.name_en.as_str(),
            v.where_en.as_str()
        ),
        ("5G NSA · 4G anchor", "中国联通", "China Unicom", "Local")
    );
    assert_eq!(v.mode_word_en, "3G & 2G only");
    for (sel, en) in [
        ("WL_AND_5G", "Auto"),
        ("Only_5G", "5G SA only"),
        ("LTE_AND_5G", "5G NSA only"),
        ("Only_LTE", "4G only"),
        ("Only_WCDMA", "3G only"),
        ("Only_TDSCDMA", "TD-SCDMA only"),
        ("Only_GSM", "2G only"),
    ] {
        assert_eq!(net_select_word(sel).1, en, "{sel}");
    }
    for (mnc, en) in [
        (0, "China Mobile"),
        (1, "China Unicom"),
        (3, "China Telecom"),
        (15, "China Broadnet"),
    ] {
        assert_eq!(mainland_operator(460, mnc, "").unwrap().1, en);
    }

    // roaming on someone else's network, unregistered, carrier counts
    state["net"]["type"] = "SA".into();
    state["net"]["mcc"] = 440.into();
    state["net"]["mnc"] = 20.into();
    state["net"]["operator"] = "SoftBank".into();
    state["net"]["roaming"] = "Roaming".into();
    state["net"]["nrca"] =
        "1,17,1,78,627264,100,0,-90,-10,15.5,-60;2,18,1,78,627300,100,0,-140,-10,15.5,-60".into();
    let v = net_view(&state);
    assert_eq!(
        (v.r#where.as_str(), v.where_en.as_str(), v.name_en.as_str()),
        ("漫游到SoftBank", "Roaming on SoftBank", "")
    );
    assert!(v.ca_val_en.starts_with("Active "), "{}", v.ca_val_en);
    assert_eq!(v.ca_sub_en, v.ca_sub);
    state["net"]["operator"] = "".into();
    state["net"]["type"] = "".into();
    state["net"]["bars"] = 0.into();
    state["net"]["nrca"] = "".into();
    state["net"]["nr_rsrp"] = 0.into();
    let v = net_view(&state);
    assert_eq!(
        (
            v.name_en.as_str(),
            v.ca_val_en.as_str(),
            v.where_en.as_str()
        ),
        ("Not registered", "No cell", "")
    );
}

/// nosvc now comes from story.state; on every corpus case it must be what the
/// old rule (comparing the Chinese headline) gave.
#[test]
fn nosvc_from_state_matches_the_old_headline_rule() {
    let (template, lines) = corpus_cases();
    let mut n = (0, 0);
    for line in lines.iter().skip(1) {
        let case: Value = serde_json::from_str(line).unwrap();
        let mut state = template.clone();
        merge(&mut state, &case["patch"]);
        let v = net_view(&state);
        let old = v.story.headline == "无服务" || v.story.headline == "只能紧急呼叫";
        assert_eq!(v.nosvc, old, "case {}", case["id"]);
        assert_eq!(
            Value::Bool(v.nosvc),
            case["view"]["nosvc"],
            "case {}",
            case["id"]
        );
        n.0 += 1;
        n.1 += usize::from(v.nosvc);
    }
    assert!(n.0 > 1000 && n.1 > 100, "{n:?}");
}

/// Every corpus case: the English follows the rules and the headline table.
#[test]
fn english_on_the_corpus() {
    let (template, lines) = corpus_cases();
    let mut states = std::collections::BTreeSet::new();
    for line in lines.iter().skip(1) {
        let case: Value = serde_json::from_str(line).unwrap();
        let mut state = template.clone();
        merge(&mut state, &case["patch"]);
        let v = net_view(&state);
        let ctx = format!("case {}", case["id"]);
        check_story(&ctx, &v.story);
        // a broadcast name outside the 4 mainland carriers passes through as-is
        let pass: &[&str] = if v.name_en == v.name {
            &["name", "where"]
        } else {
            &[]
        };
        check_pairs(&ctx, &net_pairs(&v), pass);
        states.insert(state_name(v.story.state));
    }
    assert!(states.len() >= 10, "{states:?}");
}

// ---- size: luyaz-ui must still read the bigger /v2/screen -----------------

/// luyaz-ui's read caps: data/screen_feed.rs takes at most 16384 bytes of HTTP reply
/// (cut short → the fetch fails) and 8192 of `net`; net_view.rs parse() reads 1024
/// of the story, 4096 of the carriers and 768 per carrier. json::get() keeps at
/// most cap-1 bytes and still says found,
/// so a value must be < cap or its tail fields silently go missing.
const RESP_BUF: usize = 16384;
const HTTP_HEADROOM: usize = 512;
const NET_BUF: usize = 8192;
const NET_BUDGET: usize = 6 * 1024;
const STORY_BUF: usize = 1024;
const CARRIERS_BUF: usize = 4096;
const CARRIER_BUF: usize = 768;

fn check_sizes(ctx: &str, v: &NetView) -> (usize, usize) {
    let net = serde_json::to_string(v).unwrap();
    let story = serde_json::to_string(&v.story).unwrap();
    let carriers = serde_json::to_string(&v.carriers).unwrap();
    // what server.rs v2_screen sends
    let resp = serde_json::to_string(&serde_json::json!({
        "v": SCREEN_VERSION,
        "ts": 1_759_300_000_000u64,
        "net": serde_json::to_value(v).unwrap(),
    }))
    .unwrap();
    assert!(
        net.len() < NET_BUDGET && net.len() < NET_BUF,
        "{ctx}: net {}",
        net.len()
    );
    assert!(
        story.len() < STORY_BUF,
        "{ctx}: story {} {story}",
        story.len()
    );
    assert!(
        carriers.len() < CARRIERS_BUF,
        "{ctx}: carriers {}",
        carriers.len()
    );
    for c in &v.carriers {
        let c = serde_json::to_string(c).unwrap();
        assert!(c.len() < CARRIER_BUF, "{ctx}: carrier {}", c.len());
    }
    assert!(
        resp.len() + HTTP_HEADROOM <= RESP_BUF,
        "{ctx}: response {}",
        resp.len()
    );
    (net.len(), story.len())
}

/// The longest fields at once: NSA, 5 carriers, a 47-byte non-ASCII broadcast
/// name roamed onto, the longest hint (dialing while roaming), load and limit set.
fn worst_case_state() -> Value {
    let (mut state, _) = corpus_cases();
    let name = "テストモバイル株式会社テストモバイル"; // 3-byte chars, cut to 47 bytes
    let p = serde_json::json!({
        "net": {
            "type": "NSA 5G-A", "bars": 4, "operator": name, "mcc": 440, "mnc": 99,
            "roaming": "Roaming", "wan_status": "disconnected",
            "nr_rsrp": -139, "nr_rsrq": -43, "nr_snr": "-19.75", "nr_band": "NR5G BAND 78",
            "nr_bw": "100", "nr_channel": 2_016_667, "nr_pci": 1007,
            "nrca": "1,1007,1,258,2016667,400,0,-139.5,-43.5,-19.75,-120;2,1006,1,257,2016666,400,0,-139.5,-43.5,-19.75,-120;3,1005,1,261,2016665,400,0,-139.5,-43.5,-19.75,-120",
            "lteca": "1,503,1,66,66986,20,0,-139.5,-43.5,-19.75,-120;2,502,1,66,66987,20,0,-139.5,-43.5,-19.75,-120",
            "net_select": "WL_AND_NSA", "lte_rsrp": -139, "lte_pci": 503
        },
        "interfaces": {"cellular": {"enable": 1, "roam_enable": 1}},
        "traffic": {"rx_speed": 999_999_999},
        "qos": {"ambr_dl": "4.4"},
        "sim": {"imsi": "460001234567890", "spn": "CMLink Worldwide Roaming SPN 01"}
    });
    merge(&mut state, &p);
    state
}

#[test]
fn response_fits_the_old_screens_buffers() {
    let state = worst_case_state();
    let v = net_view(&state);
    assert_eq!(v.carriers.len(), CA_MAX, "{:?}", v.carriers);
    assert!(v.other && v.where_en.starts_with("Roaming on "), "{v:?}");
    assert_eq!(state_name(v.story.state), "nodata");
    assert!(v.story.hint_en.starts_with("Connecting"));
    check_story("worst", &v.story);
    let (worst, worst_story) = check_sizes("worst", &v);

    let (template, lines) = corpus_cases();
    let (mut max, mut max_story) = (0, 0);
    for line in lines.iter().skip(1) {
        let case: Value = serde_json::from_str(line).unwrap();
        let mut state = template.clone();
        merge(&mut state, &case["patch"]);
        let (n, s) = check_sizes(&format!("case {}", case["id"]), &net_view(&state));
        (max, max_story) = (max.max(n), max_story.max(s));
    }
    eprintln!(
        "net bytes: worst case {worst} (story {worst_story}), corpus max {max} (story {max_story})"
    );
    assert!(max <= worst, "corpus max {max} vs worst {worst}");
}

// ---- stall: sending for 30 s, nothing back (slow-diagnosis §4.1, eng review D5) ----

fn win(tx_packets: u64, rx_packets: u64) -> CellWindow {
    CellWindow {
        tx_packets,
        rx_packets,
        span_ms: 30_000,
    }
}

#[test]
fn stall_needs_twenty_sent_and_nothing_back() {
    let st = |f: fn(&mut NetIn<'static>)| state_name(with(f).state);
    let o = with(|x| x.win = Some(win(20, 0)));
    assert_eq!(
        (o.headline.as_str(), o.headline_en.as_str(), o.tone, o.cause),
        ("连上了但不通", "No traffic", Tone::Bad, Cause::None)
    );
    assert_eq!(o.hint, "有信号、已拨号，但 30 秒没收到任何数据");
    // the right column still reads as usual
    assert_eq!((o.sig.as_str(), o.noise.as_str()), ("强", "小"));
    assert_eq!(st(|x| x.win = Some(win(19, 0))), "ok");
    assert_eq!(st(|x| x.win = Some(win(20, 1))), "ok");
    assert_eq!(st(|x| x.win = Some(win(0, 0))), "ok");
    assert_eq!(st(|x| x.win = None), "ok");
    // wan_status empty counts as up for nodata, but stall wants it said connected
    assert_eq!(
        st(|x| {
            x.connected = false;
            x.win = Some(win(500, 0))
        }),
        "ok"
    );
}

#[test]
fn stall_sits_after_nodata_and_before_the_slow_ones() {
    let st = |f: fn(&mut NetIn<'static>)| state_name(with(f).state);
    // before it
    assert_eq!(
        st(|x| {
            x.win = Some(win(300, 0));
            x.sim_state = "sim absent"
        }),
        "nosim"
    );
    assert_eq!(
        st(|x| {
            x.win = Some(win(300, 0));
            x.airplane = true
        }),
        "airplane"
    );
    assert_eq!(
        st(|x| {
            x.win = Some(win(300, 0));
            x.net_type = "";
            x.bars = 0
        }),
        "nosvc"
    );
    assert_eq!(
        st(|x| {
            x.win = Some(win(300, 0));
            x.data_up = false
        }),
        "nodata"
    );
    // after it
    assert_eq!(
        st(|x| {
            x.win = Some(win(300, 0));
            x.ambr_dl = 4.6
        }),
        "stall"
    );
    assert_eq!(
        st(|x| {
            x.win = Some(win(300, 0));
            x.bars = 1
        }),
        "stall"
    );
    assert_eq!(
        st(|x| {
            x.win = Some(win(300, 0));
            x.rx_bps = 200_000;
            x.rsrq = -18
        }),
        "stall"
    );
    assert_eq!(
        st(|x| {
            x.win = Some(win(300, 0));
            x.net_type = "WCDMA"
        }),
        "stall"
    );
}

#[test]
fn stall_through_net_view_with() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let state: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("tests/fixtures/snapshot-normal.json")).unwrap(),
    )
    .unwrap();
    let mut state = state;
    // the fixture leaves wan_status empty: up for nodata, not said connected
    assert_eq!(state["net"]["wan_status"], "");
    let quiet = Some(win(40, 0));
    assert_ne!(
        state_name(net_view_with(&state, quiet).story.state),
        "stall"
    );
    state["net"]["wan_status"] = "ipv4_ipv6_connected".into();
    assert_eq!(
        state_name(net_view_with(&state, quiet).story.state),
        "stall"
    );
    assert_ne!(state_name(net_view(&state).story.state), "stall");
    state["net"]["wan_status"] = "ipv4_ipv6_disconnected".into();
    assert_eq!(
        state_name(net_view_with(&state, quiet).story.state),
        "nodata"
    );
}

// ---- E4 T13：写操作叠在首页结论上（STATE_V2.md V2-38） ----

fn live_op() -> ScreenOp {
    ScreenOp {
        kind: ScreenOpKind::Live,
        head: ("正在确认".into(), "Checking".into()),
        hint: (
            "1:42 后没通就退回到自动".into(),
            "Back to Auto in 1:42 if no data".into(),
        ),
    }
}

fn with_op_story(f: fn(&mut NetIn<'static>), op: &ScreenOp) -> Story {
    let mut x = base();
    f(&mut x);
    with_op(story(&x), Some(op))
}

#[test]
fn story_changing_beats_no_service_and_offline() {
    let cases: &[fn(&mut NetIn<'static>)] = &[
        |_| {},
        // 换制式时常会先经过无服务、只能紧急呼叫、没连上网：都不出红色
        |x| {
            x.bars = 0;
            x.net_type = "No Service"
        },
        |x| x.net_type = "Limited Service",
        |x| x.data_up = false,
        |x| {
            x.bars = 1;
            x.sinr = -3.0
        },
    ];
    for (i, f) in cases.iter().enumerate() {
        let o = with_op_story(*f, &live_op());
        assert_eq!(o.state, State::Changing, "case {i}");
        assert_eq!((o.tone, o.cause), (Tone::Neutral, Cause::None), "case {i}");
        assert_eq!(
            (o.headline.as_str(), o.headline_en.as_str()),
            ("正在确认", "Checking")
        );
        assert_eq!(o.hint, "1:42 后没通就退回到自动");
        assert!(o.headline_en.chars().count() <= 10);
        check_pairs(&format!("changing {i}"), &story_pairs(&o), &[]);
    }
    // 状态栏那些照旧按网络算
    let o = with_op_story(|x| x.bars = 0, &live_op());
    assert_eq!(o.rat, "无服务");
    // 无 SIM、移动网络已关不叠（写操作那时没有意义）
    let o = with_op_story(|x| x.sim_state = "sim absent", &live_op());
    assert_eq!(o.state, State::Nosim);
    let o = with_op_story(|x| x.airplane = true, &live_op());
    assert_eq!(o.state, State::Airplane);
    // 没有写操作：和以前一模一样
    let mut x = base();
    x.data_up = false;
    assert_eq!(with_op(story(&x), None), story(&x));
}

#[test]
fn story_revert_fail_takes_the_card() {
    let op = ScreenOp {
        kind: ScreenOpKind::Alert,
        head: ("退回也没通".into(), "Failed".into()),
        hint: (
            "当前设置未知 · 上次确认是自动 · 再试一次退回或重启设备".into(),
            "Current setting unknown · last good Auto · retry revert or restart".into(),
        ),
    };
    let o = with_op_story(|_| {}, &op);
    assert_eq!((o.state, o.tone), (State::RevertFail, Tone::Bad));
    assert_eq!(o.headline, "退回也没通");
    assert_eq!(o.headline_en, "Failed");
    assert!(o.hint.starts_with("当前设置未知"));
    check_pairs("revert_fail", &story_pairs(&o), &[]);
    assert_eq!(state_name(State::RevertFail), "revert_fail");
    assert_eq!(state_name(State::Changing), "changing");
    // 触屏 C 读 state 的缓冲是 char[12]
    assert!(state_name(State::RevertFail).len() < 12);
}

#[test]
fn story_sticky_result_rides_on_the_hint() {
    let op = ScreenOp {
        kind: ScreenOpKind::Sticky,
        head: (String::new(), String::new()),
        hint: ("没通 · 已退回自动".into(), "No data · back to Auto".into()),
    };
    // 网络正常：大字照旧，提示就是那一句
    let o = with_op_story(|_| {}, &op);
    assert_eq!((o.state, o.headline.as_str()), (State::Ok, "顺畅"));
    assert_eq!(o.hint, "没通 · 已退回自动");
    assert_eq!(o.hint_en, "No data · back to Auto");
    // 有提示的：接在后面
    let o = with_op_story(|x| x.data_up = false, &op);
    assert_eq!(o.state, State::Nodata);
    assert!(o.hint.ends_with("；没通 · 已退回自动"), "{}", o.hint);
    assert!(
        o.hint_en.ends_with("; No data · back to Auto"),
        "{}",
        o.hint_en
    );
    check_pairs("sticky", &story_pairs(&o), &[]);
}

/// E4 T13：有写操作时（`net.home` + 顶层 `op`）旧触屏的缓冲也放得下：最长的值、
/// 进行中 + 退回也没通的结果、整机重启过、撤销过。
#[test]
fn response_with_op_fits_the_old_screens_buffers() {
    use crate::ops::{
        spec::NETWORK_MODE,
        txn::{Confirm, NewTxn, Phase, Reason, SimId, Source, Txn},
        ui,
    };
    let mk = |phase: Phase, reason: Option<Reason>| {
        let mut t = Txn::new(
            NewTxn {
                op_id: "x".repeat(64),
                action: "network.set_mode".into(),
                item: NETWORK_MODE.into(),
                source: Source::Guard,
                approved_by: Source::Guard,
                undo: true,
                target: "Only_GSM_WCDMA".into(),
                old: "TDSCDMA_AND_LTE".into(),
                rollback_to: "WL_AND_NSA".into(),
                sim: SimId {
                    iccid: "8".repeat(20),
                    slot: 2,
                },
                conn: None,
                confirm: Confirm::Registered,
                rollback_enabled: true,
                deadline_ms: 120_000,
                boot_id: "b".repeat(36),
                receipt: false,
            },
            0,
        );
        t.phase = phase;
        t.reason = reason;
        t.rollback_reason = Some(Reason::RebootLoop);
        t.readback = Some("Only_TDSCDMA".into());
        t.boots = 2;
        t
    };
    let active = mk(Phase::Verifying, None);
    let last = mk(Phase::RollbackFailed, Some(Reason::RollbackTimeout));
    let strip = |mut v: Value| {
        v.as_object_mut().unwrap().remove("age_ms");
        v
    };
    let mut l = strip(ui::view(&last, 0, ui::Ctx::default()));
    l["acked"] = Value::Bool(false);
    l["needs_ack"] = Value::Bool(true);
    let op = serde_json::json!({
        "rollback_enabled": true,
        "active": strip(ui::view(&active, 0, ui::Ctx::default())),
        "last": l,
        "notice": "rollback_on",
    });
    let state = worst_case_state();
    let mut worst = (0, 0);
    for so in [
        ui::screen_op(Some(&active), None, 0).unwrap(),
        ui::screen_op(None, Some((&last, false)), 0).unwrap(),
        ui::screen_op(
            None,
            Some((&mk(Phase::Cancelled, Some(Reason::Preempted)), false)),
            0,
        )
        .unwrap(),
        // D40：最长的常驻结果
        ui::screen_op(
            None,
            Some((&mk(Phase::Cancelled, Some(Reason::OtherChange)), false)),
            0,
        )
        .unwrap(),
    ] {
        let v = net_view_op(&state, None, Some(&so));
        let home = v.home.clone().unwrap();
        check_pairs("home", &story_pairs(&home), &[]);
        let net = serde_json::to_string(&v).unwrap();
        let resp = serde_json::to_string(&serde_json::json!({
            "v": SCREEN_VERSION,
            "ts": 1_759_300_000_000u64,
            "net": v,
            "op": op,
        }))
        .unwrap();
        assert!(net.len() < NET_BUF, "net {}", net.len());
        assert!(serde_json::to_string(&home).unwrap().len() < STORY_BUF);
        assert!(
            resp.len() + HTTP_HEADROOM <= RESP_BUF,
            "response {}",
            resp.len()
        );
        worst = (worst.0.max(net.len()), worst.1.max(resp.len()));
    }
    eprintln!("with op: net {} bytes, response {} bytes", worst.0, worst.1);
}

#[test]
fn hot_says_the_firmware_limits_speed() {
    let s = with(|x| x.hot = true);
    assert_eq!(state_name(s.state), "hot");
    assert_eq!((s.tone, s.cause), (Tone::Warn, Cause::None));
    assert_eq!(s.headline, "慢：过热限速");
    assert_eq!(s.headline_en, "Slow");
    assert_eq!(s.hint, "机身太热，固件在限速，凉下来自动恢复");
    assert_eq!(s.hint_en, "Too hot; speed limited until it cools");
    let r = with(|x| {
        x.hot = true;
        x.roaming = 1
    });
    assert_eq!(r.hint, "机身太热，固件在限速，凉下来自动恢复；漫游中");
    assert_eq!(r.hint_en, "Too hot; speed limited until it cools; roaming");
    assert_eq!(state_name(with(|_| {}).state), "ok");
}

#[test]
fn hot_sits_after_stall_and_before_the_slow_ones() {
    let st = |f: fn(&mut NetIn<'static>)| state_name(with(f).state);
    // before it
    assert_eq!(
        st(|x| {
            x.hot = true;
            x.data_up = false
        }),
        "nodata"
    );
    assert_eq!(
        st(|x| {
            x.hot = true;
            x.win = Some(win(300, 0))
        }),
        "stall"
    );
    // after it
    assert_eq!(
        st(|x| {
            x.hot = true;
            x.ambr_dl = 4.6
        }),
        "hot"
    );
    assert_eq!(
        st(|x| {
            x.hot = true;
            x.bars = 1
        }),
        "hot"
    );
    assert_eq!(
        st(|x| {
            x.hot = true;
            x.net_type = "WCDMA"
        }),
        "hot"
    );
}

#[test]
fn hot_through_net_view_reads_thermal_hightemp_limit() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut state: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("tests/fixtures/snapshot-normal.json")).unwrap(),
    )
    .unwrap();
    let base = state_name(net_view(&state).story.state);
    let base = base.as_str();
    assert_ne!(base, "hot");
    for (v, want) in [
        (serde_json::json!(1), "hot"),
        (serde_json::json!(0), base),
        (Value::Null, base),
        (serde_json::json!("1"), base),
    ] {
        state["thermal"]["hightemp_limit"] = v.clone();
        assert_eq!(state_name(net_view(&state).story.state), want, "{v}");
    }
}

/// A stale qos (bearer of the other core, e.g. an LTE bearer still in key.log
/// after moving to SA) must not make the home screen say 限速.
#[test]
fn stale_qos_does_not_conclude_limit() {
    let (mut state, _) = corpus_cases();
    state["qos"] = serde_json::json!({"qci": 9, "ambr_dl": "4.400", "ambr_ul": "1.000"});
    assert_eq!(state_name(net_view(&state).story.state), "limit");
    state["qos"]["stale"] = true.into();
    assert_ne!(state_name(net_view(&state).story.state), "limit");
}

#[test]
fn a_sure_finding_goes_over_the_story_not_over_a_write() {
    let state = worst_case_state();
    let sym = (
        "已注册，但数据连接没建立",
        "Registered, but the data connection is down",
    );
    for layer in ["data", "internet", "dns"] {
        let mut v = net_view_op(&state, None, None);
        let story = v.story.clone();
        with_finding(&mut v, layer, sym);
        let home = v.home.clone().unwrap();
        assert_eq!(v.story, story, "story stays the network verdict");
        assert_eq!(home.tone, Tone::Bad);
        assert!(home.hint.starts_with(sym.0) && home.hint_en.starts_with(sym.1));
        check_pairs("home", &story_pairs(&home), &[]);
        assert!(serde_json::to_string(&home).unwrap().len() < STORY_BUF);
    }
    let mut v = net_view_op(&state, None, None);
    with_finding(&mut v, "later", sym);
    assert!(v.home.is_none());
    let so = ScreenOp {
        kind: ScreenOpKind::Live,
        head: ("正在确认".into(), "Confirming".into()),
        hint: ("x".into(), "x".into()),
    };
    let mut v = net_view_op(&state, None, Some(&so));
    let before = v.home.clone();
    with_finding(&mut v, "data", sym);
    assert_eq!(v.home, before);
}

/// `story.fix` is set exactly where the touch offered an action (an abnormal verdict, no SIM,
/// an op), naming the target; never where it showed the carriers.
#[test]
fn fix_where_the_touch_had_an_action() {
    let i = base();
    let acts = [
        State::Limit,
        State::Weak,
        State::Noise,
        State::Crowd,
        State::Narrow,
        State::Nodata,
        State::Stall,
        State::Nosim,
        State::Changing,
        State::RevertFail,
    ];
    for s in [
        State::Ok,
        State::Nosim,
        State::Airplane,
        State::Sos,
        State::Nosvc,
        State::Nodata,
        State::Stall,
        State::Limit,
        State::Weak,
        State::Noise,
        State::Crowd,
        State::Only2g,
        State::Only3g,
        State::Narrow,
        State::Hot,
        State::Changing,
        State::RevertFail,
    ] {
        assert_eq!(fix_for(s, &i).is_some(), acts.contains(&s), "{s:?}");
    }
    let fix = |f: fn(&mut NetIn<'static>)| with(f).fix;
    assert_eq!(fix(|_| {}), None);
    assert_eq!(
        fix(|x| x.sim_state = "sim absent").as_deref(),
        Some("sim_enable")
    );
    assert_eq!(fix(|x| x.bars = 1).as_deref(), Some("placement"));
    assert_eq!(fix(|x| x.sinr = -3.0).as_deref(), Some("placement"));
    assert_eq!(fix(|x| x.ambr_dl = 2.0).as_deref(), Some("diag:limit"));
    assert_eq!(
        fix(|x| {
            x.data_up = false;
            x.data_sw = Sw::Off
        })
        .as_deref(),
        Some("cell")
    );
    assert_eq!(fix(|x| x.data_up = false).as_deref(), Some("diag:data"));
    // pinned to 4G: a hint, but no footer action (as before)
    assert_eq!(
        fix(|x| {
            x.net_type = "LTE";
            x.net_select = "Only_LTE"
        }),
        None
    );
    assert!(
        !serde_json::to_string(&with(|_| {}))
            .unwrap()
            .contains("\"fix\"")
    );
}

#[test]
fn fix_for_a_finding_and_an_op() {
    let state = worst_case_state();
    let sym = ("x", "x");
    for (layer, want) in [
        ("data", Some("diag:data")),
        ("internet", Some("diag:internet")),
        ("dns", Some("diag:dns")),
        ("signal", None),
    ] {
        let mut v = net_view_op(&state, None, None);
        with_finding(&mut v, layer, sym);
        assert_eq!(v.home.unwrap().fix.as_deref(), want, "{layer}");
    }
    let so = ScreenOp {
        kind: ScreenOpKind::Alert,
        head: ("退回也没通".into(), "Revert failed".into()),
        hint: ("x".into(), "x".into()),
    };
    let v = net_view_op(&state, None, Some(&so));
    assert_eq!(v.home.unwrap().fix.as_deref(), Some("op"));
}
