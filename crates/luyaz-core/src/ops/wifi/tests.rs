use super::*;
use crate::ops::txn::{Confirm, NewTxn, SimId, Source, Txn};

const KEY: &str = "hunter2-secret-pass";
const NEW_KEY: &str = "brand-new-wifi-key";

fn vault() -> Vault {
    Vault::open_as(None, VAULT_FILE, VAULT_MAX)
}

/// B31 真机 wireless 的样子（节、选项都是 `uci show` 的键）。
fn uci() -> Uci {
    let mut m = Uci::new();
    for (k, v) in [
        ("wireless.wifi0", "wifi-device"),
        ("wireless.wifi1", "wifi-device"),
        ("wireless.main_2g", "wifi-iface"),
        ("wireless.main_5g", "wifi-iface"),
        ("wireless.guest_2g", "wifi-iface"),
        ("wireless.zte_mbb", "zte_mbb"),
        ("wireless.wifi0.channel", "0"),
        ("wireless.wifi0.htmode", "EHT40"),
        ("wireless.wifi0.disabled", "0"),
        ("wireless.wifi0.country", "US"),
        ("wireless.wifi0.txpowerpercent", "100"),
        ("wireless.wifi1.channel", "36"),
        ("wireless.wifi1.htmode", "EHT160"),
        ("wireless.wifi1.disabled", "0"),
        ("wireless.wifi1.country", "US"),
        ("wireless.wifi1.txpowerpercent", "100"),
        ("wireless.main_2g.ssid", "Home"),
        ("wireless.main_2g.key", KEY),
        ("wireless.main_2g.encryption", "psk2"),
        ("wireless.main_2g.disabled", "0"),
        ("wireless.main_5g.ssid", "Home-5G"),
        ("wireless.main_5g.key", KEY),
        ("wireless.main_5g.encryption", "psk2"),
        ("wireless.main_5g.disabled", "0"),
        ("wireless.guest_2g.ssid", "Guest"),
        ("wireless.guest_2g.key", "guestpass1"),
        ("wireless.guest_2g.encryption", "psk2"),
        ("wireless.guest_2g.disabled", "1"),
        ("wireless.zte_mbb.wifi_onoff", "1"),
        ("wireless.zte_mbb.wifi6_switch", "1"),
    ] {
        m.insert(k.into(), v.into());
    }
    m
}

fn body(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// 目标：和 `wifi_dev::resolve` 一样（计划 → 套上 → 同一个展开 → 只留变了的）。
fn target(kind: Kind, m: &Uci, plan: &Plan, v: &Vault) -> Map<String, Value> {
    let cur = value(kind, m, v).unwrap();
    changed(&cur, &value(kind, &simulate(m, plan), v).unwrap())
}

fn apply(m: &Uci, plan: &Plan) -> Uci {
    simulate(m, plan)
}

#[test]
fn main_plan_is_the_agents_write() {
    let m = uci();
    // 全身请求体（网页每次都带全部键）：只写变了的
    let p = main_plan(
        &body(json!({
            "ssid_2g": "Home", "ssid_5g": "Cafe;5G", "channel_5g": 149, "hidden_2g": true,
            "radio2_disabled": "0", "country": "jp", "wifi_onoff": "1", "wifi6_switch": "1",
            "unknown": "x"
        })),
        &m,
    )
    .unwrap();
    // 顺序：国家码在前（两个 radio）
    assert_eq!(
        p.set[0],
        ("wireless.wifi0.country".to_owned(), "JP".to_owned())
    );
    assert_eq!(
        p.set[1],
        ("wireless.wifi1.country".to_owned(), "JP".to_owned())
    );
    let set: std::collections::BTreeMap<_, _> = p.set.iter().cloned().collect();
    assert_eq!(set.len(), 5);
    assert_eq!(set["wireless.main_5g.ssid"], "Cafe5G");
    assert_eq!(set["wireless.wifi1.channel"], "149");
    assert_eq!(set["wireless.main_2g.hidden"], "1");
    assert_eq!(set["wireless.wifi0.country"], "JP");
    assert!(!p.vendor_on);
    // 同样的请求体再来一次：没有要写的
    assert!(
        main_plan(&body(json!({"ssid_2g": "Home", "wifi_onoff": "1"})), &m)
            .unwrap()
            .is_empty()
    );
    // Wi-Fi 6 要变：拒绝；没变照常
    assert_eq!(
        main_plan(&body(json!({"wifi6_switch": "0"})), &m),
        Err(WIFI6_REFUSED.into())
    );
    assert!(main_plan(&body(json!({"wifi6_switch": "1"})), &m).is_ok());
}

#[test]
fn master_switch_wins_over_the_radio_keys() {
    let mut m = uci();
    m.insert("wireless.main_2g.disabled".into(), "1".into());
    m.insert("wireless.main_5g.disabled".into(), "1".into());
    m.insert("wireless.wifi0.disabled".into(), "1".into());
    m.insert("wireless.zte_mbb.wifi_onoff".into(), "0".into());
    // 打开总开关：请求体同时说 2.4G radio 关着，总开关最后写、压过它；原厂开关也要开
    let p = main_plan(
        &body(json!({"radio2_disabled": "1", "wifi_onoff": "1"})),
        &m,
    )
    .unwrap();
    assert!(p.vendor_on);
    let after = apply(&m, &p);
    assert_eq!(after["wireless.wifi0.disabled"], "0");
    assert_eq!(after["wireless.main_2g.disabled"], "0");
    let v = vault();
    let t = target(Kind::Main, &m, &p, &v);
    assert_eq!(t["on"], json!(true));
    assert_eq!(t["band_2g.radio_on"], json!(true));
    // 关：两个 AP 关，radio 不动
    let p = main_plan(&body(json!({"wifi_onoff": "0"})), &uci()).unwrap();
    let set: std::collections::BTreeMap<_, _> = p.set.iter().cloned().collect();
    assert_eq!(set.len(), 2);
    assert_eq!(set["wireless.main_5g.disabled"], "1");
    assert!(!p.vendor_on);
}

#[test]
fn target_has_only_changed_keys_and_no_password() {
    let m = uci();
    let v = vault();
    let p = main_plan(&body(json!({"key_2g": NEW_KEY, "ssid_2g": "Home"})), &m).unwrap();
    assert_eq!(new_keys(&p), vec![NEW_KEY]);
    let t = target(Kind::Main, &m, &p, &v);
    assert_eq!(t.len(), 1);
    assert_eq!(t["band_2g.key_fp"], json!(key_fp(&v, NEW_KEY)));
    let s = serde_json::to_string(&t).unwrap();
    assert!(!s.contains(NEW_KEY) && !s.contains(KEY));
    // 现状的值：全部键，密码只有指纹
    let cur = value(Kind::Main, &m, &v).unwrap();
    assert_eq!(cur.len(), Kind::Main.keys().len());
    assert!(!serde_json::to_string(&cur).unwrap().contains(KEY));
    assert_eq!(cur["band_5g.key_fp"], cur["band_2g.key_fp"]);
    assert_eq!(key_fp(&v, ""), "");
}

/// 读回和目标走同一条路：按目标写（逆映射）再读，目标里的键都对上。
#[test]
fn writing_the_target_reads_back_as_the_target() {
    let m = uci();
    let v = vault();
    let keys: std::collections::HashMap<String, String> = [KEY, NEW_KEY]
        .iter()
        .map(|k| (key_fp(&v, k), (*k).to_owned()))
        .collect();
    let lookup = |fp: &str| keys.get(fp).cloned();
    for b in [
        json!({"key_2g": NEW_KEY, "hidden_5g": "1", "txpower_2g": 50}),
        json!({"wifi_onoff": "0"}),
        json!({"country": "de", "htmode_5g": "EHT80", "radio5_disabled": true}),
        json!({"ssid_5g": "x'y\"z", "encryption_5g": "sae"}),
    ] {
        let p = main_plan(&body(b.clone()), &m).unwrap();
        let t = target(Kind::Main, &m, &p, &v);
        assert!(!t.is_empty(), "{b}");
        let now = value(Kind::Main, &m, &v).unwrap();
        let w = plan_for(Kind::Main, &t, &m, &now, lookup).unwrap();
        let back = value(Kind::Main, &apply(&m, &w), &v).unwrap();
        assert!(
            crate::ops::txn::matches(&Value::Object(t.clone()), &Value::Object(back)),
            "{b}"
        );
        // 退回到旧值：整项都回来
        let after = apply(&m, &w);
        let now2 = value(Kind::Main, &after, &v).unwrap();
        let r = plan_for(Kind::Main, &now, &after, &now2, lookup).unwrap();
        assert_eq!(
            value(Kind::Main, &apply(&after, &r), &v).unwrap(),
            now,
            "{b}"
        );
    }
    // 旧密码不在保管箱：写不出来
    let p = main_plan(&body(json!({"key_2g": NEW_KEY})), &m).unwrap();
    let t = target(Kind::Main, &m, &p, &v);
    let now = value(Kind::Main, &m, &v).unwrap();
    assert_eq!(
        plan_for(Kind::Main, &t, &m, &now, |_| None),
        Err(KEY_LOST.into())
    );
    assert!(
        plan_for(
            Kind::Main,
            &json!({"nope": 1}).as_object().unwrap().clone(),
            &m,
            &now,
            lookup
        )
        .is_err()
    );
}

#[test]
fn guest_skips_a_missing_band() {
    let m = uci(); // 没有 guest_5g
    let v = vault();
    let p = guest_plan(
        &body(json!({"guest_disabled": false, "guest_ssid": "Cafe", "guest_key": NEW_KEY})),
        &m,
    )
    .unwrap();
    assert!(p.set.iter().all(|(k, _)| !k.contains("guest_5g")));
    let t = target(Kind::Guest, &m, &p, &v);
    assert_eq!(t["guest.on_2g"], json!(true));
    assert!(!t.contains_key("guest.on_5g"));
    assert_eq!(t["guest.ssid"], json!("Cafe"));
    assert!(!serde_json::to_string(&t).unwrap().contains(NEW_KEY));
    let now = value(Kind::Guest, &m, &v).unwrap();
    let lookup = |fp: &str| (fp == key_fp(&v, NEW_KEY)).then(|| NEW_KEY.to_owned());
    let w = plan_for(Kind::Guest, &t, &m, &now, lookup).unwrap();
    assert_eq!(w.set.len(), 3);
    let back = value(Kind::Guest, &apply(&m, &w), &v).unwrap();
    assert!(crate::ops::txn::matches(
        &Value::Object(t),
        &Value::Object(back)
    ));
    // 已经是这样：没有要写的
    assert!(
        guest_plan(&body(json!({"guest_ssid": "Guest"})), &m)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn txpower_alone_is_hot() {
    let m = uci();
    let p = main_plan(&body(json!({"txpower_2g": "50", "txpower_5g": 80})), &m).unwrap();
    let mut hot = p.hot_txpower().unwrap();
    hot.sort();
    assert_eq!(hot, vec![("wlan0", 50), ("wlan2", 80)]);
    let p = main_plan(&body(json!({"txpower_2g": "50", "channel_2g": "6"})), &m).unwrap();
    assert!(p.hot_txpower().is_none());
    assert!(Plan::default().hot_txpower().is_none());
}

#[test]
fn params_are_checked() {
    assert_eq!(check_main(&json!({"ssid_2g": "a"})), Ok(pending()));
    assert!(check_main(&json!({"key_2g": "short"})).is_err());
    assert!(check_main(&json!({"key_2g": ""})).is_ok());
    assert!(check_main(&json!({"ssid_2g": "a\nb"})).is_err());
    assert!(check_main(&json!({"ssid_2g": "x".repeat(129)})).is_err());
    assert!(check_guest(&json!({"guest_key": "1234567"})).is_err());
    assert!(check_guest(&json!({"guest_ssid": "ok", "guest_isolate": true})).is_ok());
    // 撤销：只认这一项的键和类型
    let r = json!({"restore": {"on": true, "band_2g.key_fp": "ab"}});
    assert_eq!(check_main(&r), Ok(r["restore"].clone()));
    assert!(check_main(&json!({"restore": {"on": "1"}})).is_err());
    assert!(check_main(&json!({"restore": {"guest.ssid": "x"}})).is_err());
    assert!(check_main(&json!({"restore": {}})).is_err());
    assert!(check_guest(&json!({"restore": {"guest.active_time": 3600}})).is_ok());
    let s = crate::ops::spec::find("wifi.settings").unwrap();
    assert_eq!(s.confirm, Confirm::Wifi);
    let old = json!({"band_2g.ssid": "a"});
    assert_eq!(s.target(&s.to_params(&old)), Ok(old));
}

#[test]
fn undo_restores_only_what_that_change_touched() {
    let old = json!({"on": true, "band_2g.ssid": "Home", "band_5g.ssid": "Home-5G"});
    let new = json!({"band_2g.ssid": "Cafe"});
    assert_eq!(project(&old, &new), json!({"band_2g.ssid": "Home"}));
}

#[test]
fn words_never_show_the_password() {
    let v = vault();
    let cur = Value::Object(value(Kind::Main, &uci(), &v).unwrap());
    let (zh, en) = words(&cur);
    assert!(
        zh.starts_with("Wi-Fi 开 · 2.4G Home · 5G Home-5G · 另 "),
        "{zh}"
    );
    assert!(en.ends_with(" more"), "{en}");
    assert!(!zh.contains(&key_fp(&v, KEY)));
    assert_eq!(words(&json!({"band_2g.key_fp": "ab"})).0, "1 项设置");
    assert_eq!(words(&json!({"guest.on_2g": false})).1, "guest off");
    assert_eq!(
        crate::ops::ui::value_words(crate::ops::spec::WIFI_MAIN, &json!({"on": false})).0,
        "Wi-Fi 关"
    );
}

/// 密码不进事务值、状态、流水账、落盘的事务、流水账参数。
#[test]
fn the_password_never_leaves_the_vault() {
    let m = uci();
    let v = vault();
    let p = main_plan(&body(json!({"key_2g": NEW_KEY, "key_5g": NEW_KEY})), &m).unwrap();
    let t = Value::Object(target(Kind::Main, &m, &p, &v));
    let old = Value::Object(value(Kind::Main, &m, &v).unwrap());
    let mut txn = Txn::new(
        NewTxn {
            op_id: "op1".into(),
            action: "wifi.settings".into(),
            item: crate::ops::spec::WIFI_MAIN.into(),
            source: Source::Web,
            approved_by: Source::Web,
            undo: false,
            target: t.clone(),
            old: old.clone(),
            rollback_to: old,
            sim: SimId {
                iccid: String::new(),
                slot: 0,
            },
            conn: None,
            confirm: Confirm::Wifi,
            rollback_enabled: true,
            deadline_ms: 90_000,
            boot_id: "b".into(),
            receipt: false,
        },
        1_000,
    );
    txn.begin_apply();
    txn.applied(crate::ops::txn::Applied::Done, 1_000);
    let dumps = [
        serde_json::to_string(&txn).unwrap(),
        crate::ops::record::txn_line(&txn).to_string(),
        crate::ops::ui::view(&txn, 2_000, Default::default()).to_string(),
        crate::ops::record::redact(
            "wifi.settings",
            &json!({"key_2g": NEW_KEY, "key_5g": KEY, "ssid_2g": "x"}),
        )
        .to_string(),
        crate::ops::record::redact("wifi.guest", &json!({"guest_key": NEW_KEY})).to_string(),
    ];
    for d in dumps {
        assert!(!d.contains(NEW_KEY) && !d.contains(KEY), "{d}");
    }
}

/// Wi-Fi：读回对上 + AP 已起来就确认，不看蜂窝数据、不探测；SIM 记空不算换卡。
#[test]
fn wifi_confirms_without_data() {
    use crate::ops::txn::{Next, Phase, Reading};
    let t = json!({"band_2g.ssid": "Cafe"});
    let mut txn = Txn::new(
        NewTxn {
            op_id: "op1".into(),
            action: "wifi.settings".into(),
            item: crate::ops::spec::WIFI_MAIN.into(),
            source: Source::Web,
            approved_by: Source::Web,
            undo: false,
            target: t.clone(),
            old: json!({"band_2g.ssid": "Home", "on": true}),
            rollback_to: json!({"band_2g.ssid": "Home", "on": true}),
            sim: SimId {
                iccid: String::new(),
                slot: 0,
            },
            conn: None,
            confirm: Confirm::Wifi,
            rollback_enabled: false,
            deadline_ms: 90_000,
            boot_id: "b".into(),
            receipt: false,
        },
        1_000,
    );
    txn.begin_apply();
    txn.applied(crate::ops::txn::Applied::Done, 1_000);
    let reading = |registered: bool| Reading {
        value: Some(json!({"band_2g.ssid": "Cafe", "on": true})),
        registered,
        sim: SimId {
            iccid: String::new(),
            slot: 0,
        },
        data: None,
        probe: None,
    };
    assert!(txn.wants_probe(&reading(true), 2_000).is_none());
    // 重载还没确认：等
    assert_eq!(txn.on_reading(&reading(false), 2_000), Next::Wait);
    assert_eq!(txn.phase, Phase::Verifying);
    txn.on_reading(&reading(true), 3_000);
    assert_eq!(txn.phase, Phase::Confirmed);
}

/// 改动记录里 Wi-Fi 那一行的撤销：只退回那次改过的键，带 expect；文字不带密码。
#[test]
fn journal_undo_request_is_projected() {
    let mut lines = vec![json!({
        "op_id": "op1", "action": "wifi.settings", "item": "wifi.main", "source": "web",
        "old": {"on": true, "band_2g.ssid": "Home", "band_2g.key_fp": "aa"},
        "new": {"band_2g.ssid": "Cafe", "band_2g.key_fp": "bb"},
        "rollback_to": {"on": true, "band_2g.ssid": "Home", "band_2g.key_fp": "aa"},
        "readback": {"on": true, "band_2g.ssid": "Cafe", "band_2g.key_fp": "bb"},
        "result": "confirmed", "reason": "verified",
    })];
    crate::ops::journal_view::decorate(&mut lines);
    let u = &lines[0]["undo_view"];
    assert_eq!(u["ok"], true);
    assert_eq!(
        u["request"],
        json!({"action": "wifi.settings", "undo": true, "params": {
            "restore": {"band_2g.ssid": "Home", "band_2g.key_fp": "aa"},
            "expect": {"band_2g.ssid": "Cafe", "band_2g.key_fp": "bb"},
        }})
    );
    assert_eq!(lines[0]["what_zh"], "Wi-Fi 设置");
    assert_eq!(
        lines[0]["change_zh"],
        "Wi-Fi 开 · 2.4G Home · 另 1 项 → 2.4G Cafe · 另 1 项"
    );
}

#[test]
fn layouts_the_firmware_cannot_run_are_seen_on_the_plan() {
    use luyaz_wifi::layout::{NO_2G, THREE_APS, refusal};
    let m = uci();
    // 只关 2.4G，5G 开着：Wi-Fi 全断
    for b in [
        json!({"radio2_disabled": "1"}),
        json!({"radio2_disabled": true}),
    ] {
        let p = main_plan(&body(b), &m).unwrap();
        assert_eq!(refusal(&m, &simulate(&m, &p)), Some(NO_2G));
    }
    // 只关 5G、总开关关：可以
    for b in [json!({"radio5_disabled": "1"}), json!({"wifi_onoff": "0"})] {
        let p = main_plan(&body(b), &m).unwrap();
        assert_eq!(refusal(&m, &simulate(&m, &p)), None);
    }
    // 访客两个频段都开着时关 Wi-Fi：访客跟着关，不拒
    let mut g = m.clone();
    for (k, v) in [
        ("wireless.guest_2g.disabled", "0"),
        ("wireless.guest_5g", "wifi-iface"),
        ("wireless.guest_5g.disabled", "0"),
    ] {
        g.insert(k.into(), v.into());
    }
    let p = main_plan(&body(json!({"wifi_onoff": "0"})), &g).unwrap();
    assert!(
        p.set
            .contains(&("wireless.guest_2g.disabled".into(), "1".into()))
    );
    assert!(
        p.set
            .contains(&("wireless.guest_5g.disabled".into(), "1".into()))
    );
    assert_eq!(refusal(&g, &simulate(&g, &p)), None);
    let want = body(json!({"on": false}));
    let now = value(Kind::Main, &g, &vault()).unwrap();
    let p = plan_for(Kind::Main, &want, &g, &now, |_| None).unwrap();
    assert_eq!(refusal(&g, &simulate(&g, &p)), None, "退回/撤销同样");
    // 只关一个频段的主网络、同频段访客还开着：拒
    let p = main_plan(&body(json!({"radio5_disabled": "0"})), &g).unwrap();
    assert!(p.is_empty());
    let mut one = g.clone();
    one.insert("wireless.main_5g.disabled".into(), "1".into());
    assert_eq!(refusal(&g, &one), Some(luyaz_wifi::layout::GUEST_ALONE));
    // 两个主网络 + 只开 2.4G 访客 = 3 个 AP
    let p = guest_plan(&body(json!({"guest_disabled_2g": false})), &m).unwrap();
    assert_eq!(refusal(&m, &simulate(&m, &p)), Some(THREE_APS));
}
