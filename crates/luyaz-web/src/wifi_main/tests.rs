use super::*;
use crate::hub::Hub;
use crate::sse::Frame;
use std::sync::Arc;

fn blk(data: Value) -> Value {
    json!({"revision": 1, "observed_at": 1, "stale": false, "data": data})
}

/// Synthetic values only.
fn wifi_data() -> Value {
    json!({
        "on": true, "module_on": true, "wifi6": true, "country": "CN",
        "band_2g": {"ap_on": true, "radio_on": true, "ssid": "Home", "encryption": "psk2+ccmp", "has_password": true,
                    "hidden": false, "channel": "0", "txpower": "100", "htmode": "HT20"},
        "band_5g": {"ap_on": true, "radio_on": true, "ssid": "Home-5G", "encryption": "psk2+psk3+ccmp", "has_password": true,
                    "hidden": false, "channel": "149", "txpower": "100", "htmode": "VHT80"},
        "guest": {"ssid": "Guest", "encryption": "psk2+ccmp", "has_password": true, "on_2g": false, "on_5g": true,
                  "hidden": false, "isolate": true, "active_time": 0, "left_s": null}
    })
}

fn hub_with(wifi: Value, op: Value) -> Arc<Hub> {
    let hub = Hub::new();
    hub.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch": "e", "blocks": {
            "wifi": blk(wifi),
            "clients": blk(json!({"lan": 0, "total": 3, "wifi": 3, "list": []})),
            "nfc": blk(json!({"switch": 0})),
            "op": blk(op),
        }})
        .to_string(),
    });
    hub.set_link_for_test(Link::Live);
    hub
}

fn cur() -> Wifi {
    wifi_of(&wifi_data()).unwrap()
}

fn seed() -> Fields {
    Fields::seed(&cur())
}

#[test]
fn reads_the_block_and_seeds_the_form_normalised() {
    let w = cur();
    assert!(w.on == Some(true) && w.wifi6 && w.country == "CN");
    let s = seed();
    assert_eq!(s.b[0].ch, "auto", "channel 0 = automatic");
    // the seed round-trips through the signals
    let mut m = Map::new();
    s.signals('g', &mut m);
    assert_eq!(Fields::from_signals(&Value::Object(m), 'g'), Some(s));
    assert_eq!(Fields::from_signals(&json!({}), 'f'), None);
}

#[test]
fn only_changed_keys_are_sent_and_an_empty_password_never() {
    let s = seed();
    let mut d = s.clone();
    d.b[0].ssid = "New name".into();
    let c = check(&s, &d, ["", ""], Some(&cur()), Lang::Zh).unwrap();
    assert_eq!(Value::Object(c.params.clone()), json!({"ssid_2g": "New name"}));
    assert!(!c.txpower_only() && c.rejoin() && !c.master_off());
    // untouched form: nothing to send ("auto" in the draft vs "0" on the device is no change)
    let mut d = s.clone();
    d.b[0].ch = "auto".into();
    assert!(matches!(check(&s, &d, ["", ""], Some(&cur()), Lang::Zh), Err(write::Outcome::NotYet(t)) if t.contains("还没有改动")));
    // a typed password goes, under its band only
    let c = check(&s, &s, ["", "abcdefgh1"], Some(&cur()), Lang::En).unwrap();
    assert_eq!(Value::Object(c.params.clone()), json!({"key_5g": "abcdefgh1"}));
    assert_eq!(c.keys, [false, true]);
    // switches as the agent's "0"/"1"
    let mut d = s.clone();
    d.on = false;
    d.b[1].radio = false;
    d.b[1].hidden = true;
    assert!(check(&s, &d, ["", ""], Some(&cur()), Lang::En).unwrap_err().text(Lang::En).contains("separately"));
    d.b[1].radio = true;
    let c = check(&s, &d, ["", ""], Some(&cur()), Lang::En).unwrap();
    assert_eq!(
        Value::Object(c.params.clone()),
        json!({"wifi_onoff": "0", "hidden_5g": "1", "expect": {"on": true, "module_on": true}})
    );
    assert!(c.master_off());
    // not read / not current: nothing is checked
    assert!(matches!(check(&s, &d, ["", ""], None, Lang::En), Err(write::Outcome::NotYet(_))));
}

#[test]
fn passwords_and_names_are_checked_stricter_than_u60d() {
    let s = seed();
    let bad = |k: &str| check(&s, &s, [k, ""], Some(&cur()), Lang::En).is_err();
    assert!(bad("short"));
    assert!(bad(&"x".repeat(65)));
    assert!(bad(&"g".repeat(64)), "64 characters must be hex");
    assert!(!bad(&"a1".repeat(32)), "64 hex digits are a PSK");
    assert!(!bad(&"y".repeat(63)));
    assert!(bad("pass;word1"), "dropped characters");
    assert!(bad("pässwörd12"), "not ASCII");
    let mut d = s.clone();
    d.b[1].ssid = "a|b".into();
    assert!(check(&s, &d, ["", ""], Some(&cur()), Lang::Zh).is_err());
    d.b[1].ssid = String::new();
    assert!(check(&s, &d, ["", ""], Some(&cur()), Lang::Zh).is_err());
    d.b[1].ssid = "x".repeat(33);
    assert!(check(&s, &d, ["", ""], Some(&cur()), Lang::Zh).is_err());
}

#[test]
fn open_to_wpa_needs_a_password_and_open_sends_none() {
    let mut w = wifi_data();
    w["band_2g"]["encryption"] = json!("none");
    w["band_2g"]["has_password"] = json!(false);
    let cur = wifi_of(&w).unwrap();
    let s = Fields::seed(&cur);
    let mut d = s.clone();
    d.b[0].enc = "psk2+ccmp".into();
    let e = check(&s, &d, ["", ""], Some(&cur), Lang::En).unwrap_err();
    assert!(e.text(Lang::En).contains("needs a password"), "{e:?}");
    let c = check(&s, &d, ["goodpass1", ""], Some(&cur), Lang::En).unwrap();
    assert_eq!(c.params["key_2g"], "goodpass1");
    // to open: a typed password is not sent
    let s = seed();
    let mut d = s.clone();
    d.b[1].enc = "none".into();
    let c = check(&s, &d, ["", "ignored12"], Some(&self::cur()), Lang::En).unwrap();
    assert_eq!(Value::Object(c.params.clone()), json!({"encryption_5g": "none"}));
    assert!(what(&c, Lang::Zh).contains("开放网络"));
}

#[test]
fn wifi6_and_us_channel_13_are_refused_before_sending() {
    let s = seed();
    let mut d = s.clone();
    d.w6 = false;
    let e = check(&s, &d, ["", ""], Some(&cur()), Lang::Zh).unwrap_err();
    assert!(e.text(Lang::Zh).contains("Wi-Fi 6 在这里改不了"));
    let mut d = s.clone();
    d.cc = "US".into();
    d.b[0].ch = "13".into();
    let e = check(&s, &d, ["", ""], Some(&cur()), Lang::En).unwrap_err();
    assert!(e.text(Lang::En).contains("12 and 13"));
    let mut d = s.clone();
    d.b[1].ch = "7".into();
    assert!(check(&s, &d, ["", ""], Some(&cur()), Lang::En).is_err(), "not a 5 GHz channel");
}

#[test]
fn tier_two_for_txpower_alone_three_for_a_reload_with_remote_warning() {
    let s = seed();
    let mut d = s.clone();
    d.b[0].tx = "50".into();
    d.b[1].tx = "25".into();
    let c = check(&s, &d, ["", ""], Some(&cur()), Lang::Zh).unwrap();
    assert!(c.txpower_only());
    let cf = confirm(&c, Lang::Zh);
    assert_eq!(cf.tier, write::Tier::Inline);
    assert!(cf.what.contains("不重启"));
    let mut d = s.clone();
    d.b[0].ch = "6".into();
    let c = check(&s, &d, ["", ""], Some(&cur()), Lang::En).unwrap();
    let cf = confirm(&c, Lang::En);
    assert_eq!(cf.tier, write::Tier::Dialog);
    assert!(cf.cuts_uplink, "a remote path may depend on Wi-Fi");
    assert!(cf.downtime.unwrap().contains("10–60 seconds"));
    let r = cf.recovery.unwrap();
    assert!(r.contains("reconnects by itself"), "{r}");
    // a new name: join again, named
    let mut d = s.clone();
    d.b[0].ssid = "Home".into();
    d.b[1].ssid = "Home".into();
    let c = check(&s, &d, ["", "newpass12"], Some(&cur()), Lang::Zh).unwrap();
    let r = confirm(&c, Lang::Zh).recovery.unwrap();
    assert!(r.contains("用新密码重新连到「Home」"), "{r}");
    // master off: the touchscreen
    let mut d = s.clone();
    d.on = false;
    let c = check(&s, &d, ["", ""], Some(&cur()), Lang::Zh).unwrap();
    let cf = confirm(&c, Lang::Zh);
    assert!(cf.recovery.unwrap().contains("触屏") && cf.downtime.unwrap().contains("一直关着"));
    // the password never goes into the dialog
    assert!(!format!("{}{}", cf.what, cf.title).contains("newpass12"));
}

#[test]
fn refusals_say_what_holds_the_device() {
    let doing = json!({"ok": false, "error": {"code": "busy", "message": "another change is in progress"},
        "doing": {"say_zh": "正在改访客 Wi-Fi（看门狗发起，12 秒），稍等", "say_en": "Busy: guest wi-fi (watchdog)"}});
    let o = refusal(409, &doing, Lang::Zh);
    assert!(o.text(Lang::Zh).contains("正在改访客 Wi-Fi"), "{o:?}");
    assert_eq!(o.tone(), "warn");
    let lock = json!({"ok": false, "error": {"code": "busy", "message": "Wi-Fi is being changed by another writer; try again"}});
    assert!(refusal(503, &lock, Lang::Zh).text(Lang::Zh).contains("Wi-Fi 正被别的程序修改"));
    let w6 = json!({"ok": false, "error": {"code": "refused", "message": "this firmware changes Wi-Fi 6 together with the radio mode; switching it here is not supported yet"}});
    let o = refusal(409, &w6, Lang::Zh);
    assert_eq!(o.tone(), "bad");
    assert!(o.text(Lang::Zh).contains("Wi-Fi 6 在这里改不了"));
    let inv = json!({"ok": false, "error": {"code": "invalid_parameter", "message": "key_2g must be 8-64 characters"}});
    assert!(refusal(400, &inv, Lang::En).text(Lang::En).contains("key_2g must be 8-64"));
    // the core's AP layout refusals (luyaz_wifi::layout) in the page's language
    let lay = json!({"ok": false, "error": {"code": "invalid_parameter", "message": "this device cannot run exactly three Wi-Fi networks; turn the guest network on for both bands, or off"}});
    assert!(refusal(400, &lay, Lang::Zh).text(Lang::Zh).contains("不能刚好开三个"));
}

#[test]
fn only_2g_off_with_5g_on_goes_to_the_dialog() {
    // 2.4 GHz off alone is the radio switch — the layout the firmware supports (CN B11, measured
    // 2026-11-18); it used to be refused before the dialog.
    let (s, c) = (seed(), cur());
    let mut d = s.clone();
    d.b[0].radio = false;
    let ch = check(&s, &d, ["", ""], Some(&c), Lang::Zh).unwrap();
    assert_eq!(ch.params["radio2_disabled"], json!("1"));
    // only 5 GHz off, or both: fine
    let mut d = s.clone();
    d.b[1].radio = false;
    assert!(check(&s, &d, ["", ""], Some(&c), Lang::Zh).is_ok());
    d.b[0].radio = false;
    assert!(check(&s, &d, ["", ""], Some(&c), Lang::Zh).is_ok());
}

#[test]
fn the_agent_reply_keeps_only_the_live_fields() {
    let v = json!({"key_2g": "secret-pass-1", "key_5g": "secret-pass-2", "actual_channel_2g": "6", "actual_bw_2g": "HT20",
        "actual_channel_5g": "", "clients_2g": 2, "clients_5g": "1", "ssid_2g": "x"});
    let l = Live::of(&v);
    assert_eq!(l.ch, ["6".to_string(), String::new()]);
    assert_eq!(l.n, [Some(2), Some(1)]);
    assert!(!format!("{l:?}").contains("secret"));
}

#[test]
fn readback_holds_on_the_block() {
    let s = seed();
    let mut d = s.clone();
    d.b[0].ch = "auto".into();
    d.b[1].ch = "36".into();
    d.b[0].hidden = true;
    let c = check(&s, &d, ["", "goodpass1"], Some(&cur()), Lang::En).unwrap();
    assert!(!holds(&c, &cur()));
    let mut w = wifi_data();
    w["band_5g"]["channel"] = json!("36");
    w["band_2g"]["hidden"] = json!(true);
    assert!(holds(&c, &wifi_of(&w).unwrap()));
}

#[test]
fn status_bands_and_the_last_change() {
    let h = hub_with(wifi_data(), json!({"rollback_enabled": true, "active": null,
        "last": {"op_id": "x", "item": "wifi.main", "mark": "ok", "say_zh": "Wi-Fi 设置已生效", "say_en": "Wi-Fi settings in effect"}}));
    let live = Live {
        ch: ["6".into(), "149".into()],
        bw: ["HT20".into(), "VHT80".into()],
        n: [Some(1), Some(2)],
    };
    let v = h.read(|st| view(st, Some(&live), None, Lang::Zh));
    assert_eq!((v.tone, v.state.as_str()), ("ok", "Wi-Fi 已开启"));
    assert_eq!(v.reason.as_deref(), Some("3 台设备连着"));
    assert_eq!(v.meta[0], "2.4 GHz：信道 6 · HT20 · 1 台设备");
    assert_eq!(v.op, Some(("上次改动：Wi-Fi 设置已生效".into(), "ok")));
    let html = Leaves { v: &v }.render().unwrap();
    assert!(html.contains("已设密码") && html.contains("访客 Wi-Fi") && html.contains("/router/wifi-guest"));
    assert!(html.contains("NFC 碰一碰") && !html.contains("Wi-Fi 节能"), "power save only with the agent's answer");
    // on but no band up in iw: not broadcasting
    let dark = Live::default();
    let psm = Psm::of(&json!({"enabled": true, "preferred": true, "saved": true, "live": null}));
    let v = h.read(|st| view(st, Some(&dark), Some(psm), Lang::En));
    assert_eq!((v.tone, v.state.as_str()), ("warn", "Wi-Fi not broadcasting"));
    assert_eq!(v.psm_sub(), Some(psm_deferred(Lang::En)));
    // a running wifi.main transaction shows its progress; a guest one does not
    let h = hub_with(wifi_data(), json!({"active": {"op_id": "y", "item": "wifi.main", "say_zh": "正在确认", "say_en": "Checking"}}));
    let v = h.read(|st| view(st, None, None, Lang::Zh));
    assert_eq!(v.op.unwrap().0, "正在改 Wi-Fi 设置：正在确认");
    let h = hub_with(wifi_data(), json!({"active": {"op_id": "y", "item": "wifi.guest", "say_zh": "正在确认"}}));
    assert!(h.read(|st| view(st, None, None, Lang::Zh)).op.is_none());
    // master off
    let mut w = wifi_data();
    w["on"] = json!(false);
    w["module_on"] = json!(false);
    let h = hub_with(w, json!({}));
    let v = h.read(|st| view(st, None, None, Lang::Zh));
    assert_eq!(v.state, "Wi-Fi 已关闭");
    assert!(v.meta.iter().all(|s| s.contains("广播状态未读取")));
}

#[test]
fn wifi_psm_selects_saved_value_and_requires_active_or_deferred_readback() {
    let h = hub_with(wifi_data(), json!({}));
    let show = |psm: Psm| h.read(|st| view(st, None, Some(psm), Lang::En));
    let psm = Psm::of(&json!({"enabled": false, "preferred": true, "saved": true, "live": false}));
    let v = show(psm);
    assert_eq!(v.psm_sub(), Some("Saved, but not active"));
    let html = Leaves { v: &v }.render().unwrap();
    assert!(html.contains("aria-labelledby=\"wf-psm-l\" aria-checked=\"true\""));
    assert!(html.contains("Saved, but not active"));
    assert!(!psm.holds(true, true));

    let active = Psm::of(&json!({"preferred": true, "saved": true, "live": true}));
    assert!(active.holds(true, false));
    assert_eq!(show(active).psm_sub(), None);
    let live_only = Psm::of(&json!({"preferred": true, "saved": null, "live": true}));
    assert!(!live_only.holds(true, false));
    assert_eq!(show(live_only).psm_sub(), None);
    let saved_only = Psm::of(&json!({"preferred": true, "saved": true, "live": null}));
    assert!(!saved_only.holds(true, false));
    assert!(saved_only.holds(true, true));
    assert_eq!(show(saved_only).psm_sub(), Some(psm_deferred(Lang::En)));
    let live = Live { ch: ["6".into(), "".into()], ..Live::default() };
    assert_eq!(h.read(|st| view(st, Some(&live), Some(saved_only), Lang::En)).psm_sub(),
        Some(psm_driver_unknown(Lang::En)));
    assert_eq!(show(Psm::default()).psm_sub(), Some("Power-save setting unknown"));
}

#[test]
fn wifi_nfc_only_sends_with_current_value_and_confirms_new_current_read() {
    let h = Hub::new();
    let snapshot = |epoch, revision, stale, switch: Value| h.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch": epoch, "blocks": {"nfc": {
            "revision": revision, "stale": stale, "data": {"switch": switch}
        }}}).to_string(),
    });
    assert!(matches!(h.read(|st| nfc_start(st, true, Lang::En)), Err(write::Outcome::NotYet(_))));
    snapshot("e", 6, false, json!(0));
    assert!(matches!(h.read(|st| nfc_start(st, false, Lang::En)), Err(write::Outcome::AcceptedSaid(_))));
    let (epoch, rev) = h.read(|st| nfc_start(st, true, Lang::En)).unwrap();
    let confirms = || h.read(|st| nfc_readback(st, &epoch, rev, true));

    snapshot("e", 6, false, json!(1));
    assert!(!confirms());
    snapshot("e", 7, true, json!(1));
    assert!(!confirms());
    h.read(|st| {
        assert!(matches!(nfc_start(st, true, Lang::En), Err(write::Outcome::NotYet(_))));
        let v = view(st, None, None, Lang::En);
        let html = Leaves { v: &v }.render().unwrap();
        assert!(html.contains("id=\"wf-nfc-unknown\" disabled>—"));
        assert!(!html.contains("/router/wifi/nfc/set?on="));
    });
    snapshot("e", 8, false, Value::Null);
    assert!(!confirms());
    snapshot("e", 9, false, json!(2));
    assert!(!confirms());
    snapshot("e", 10, false, json!(1));
    assert!(confirms());
    h.set_link_for_test(Link::Down);
    assert!(!confirms());
    snapshot("new", 1, true, json!(1));
    assert!(!confirms());
    snapshot("new", 1, false, json!(1));
    assert!(confirms());
}

#[test]
fn wifi_main_guest_summary_labels_saved_and_blocked() {
    let mut w = wifi_data();
    w["band_5g"]["radio_on"] = json!(false);
    let h = hub_with(w, json!({}));
    let v = h.read(|st| view(st, None, None, Lang::En));
    assert_eq!(v.guest.as_ref().unwrap().1, "Set to on · 5 GHz: radio off");
    let html = Leaves { v: &v }.render().unwrap();
    assert!(html.contains("Set to on · 5 GHz: radio off"));
}

#[test]
fn the_form_renders_seeded_and_never_with_a_password() {
    let h = hub_with(wifi_data(), json!({}));
    let closed = h.read(|st| form(false, st, Lang::Zh));
    assert!(closed.contains("更改设置") && closed.contains("data-init=\"$_wfo = false\""));
    let open = h.read(|st| form(true, st, Lang::En));
    assert!(open.contains("data-bind=\"_fs2\"") && open.contains("type=\"password\"") && open.contains("filterSignals"));
    assert!(open.contains("_gc2&#34;:&#34;auto&#34;"), "seed signals: {open}");
    assert!(open.contains("Set, leave empty to keep it"));
    // the current value stays an option even if not in the list
    let mut w = wifi_data();
    w["band_5g"]["htmode"] = json!("HE80");
    let h = hub_with(w, json!({}));
    let open = h.read(|st| form(true, st, Lang::En));
    assert!(open.contains("<option value=\"HE80\" selected>HE80</option>"));
}

#[test]
fn wifi_split_switches_keep_saved_bands_and_report_live_channels() {
    let mut w = wifi_data();
    w["on"] = json!(false);
    w["module_on"] = json!(true);
    w["band_2g"]["radio_on"] = json!(false);
    w["band_2g"]["ap_on"] = json!(false);
    w["band_5g"]["ap_on"] = json!(false);
    let h = hub_with(w.clone(), json!({}));
    let live = Live { ch: [String::new(), "149".into()], ..Live::default() };
    let v = h.read(|st| view(st, Some(&live), None, Lang::En));
    assert_eq!(v.state, "Wi-Fi on");
    assert!(v.meta.iter().any(|s| s.contains("Master switch set to off; module currently on")));
    assert!(v.meta.iter().any(|s| s.contains("5 GHz: channel 149")));
    assert_eq!(v.global[0].value, "Off");
    assert_eq!(v.bands[0].rows[0].value, "Off");
    assert_eq!(v.bands[1].rows[0].value, "On · Main network not enabled");
    let seed = Fields::seed(&wifi_of(&w).unwrap());
    assert!(!seed.on && !seed.b[0].radio && seed.b[1].radio);

    w["on"] = json!(true);
    w["module_on"] = json!(false);
    let h = hub_with(w, json!({}));
    let v = h.read(|st| view(st, None, None, Lang::Zh));
    assert_eq!(v.state, "Wi-Fi 已关闭");
    assert!(v.meta.iter().any(|s| s.contains("总开关设置为开，模块当前关闭")));
    assert!(v.meta.iter().any(|s| s.contains("广播状态未读取")));
}

#[test]
fn wifi_form_requires_fresh_known_master_and_radios() {
    for path in ["on", "band_2g", "band_5g"] {
        let mut w = wifi_data();
        if path == "on" {
            w["on"] = Value::Null;
        } else {
            w[path]["radio_on"] = Value::Null;
        }
        let h = hub_with(w, json!({}));
        h.read(|st| {
            assert!(editable(st).is_none());
            assert!(!form(true, st, Lang::En).contains("data-bind=\"_fs2\""));
            let mut d = seed();
            d.on = false;
            assert!(check(&seed(), &d, ["", ""], wifi(st).as_ref(), Lang::En).is_err());
        });
    }
    let h = Hub::new();
    h.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch": "e", "blocks": {"wifi": {"revision": 1, "observed_at": 1, "stale": true, "data": wifi_data()}}}).to_string(),
    });
    h.set_link_for_test(Link::Live);
    h.read(|st| assert!(editable(st).is_none()));

    let mut w = wifi_data();
    w["module_on"] = Value::Null;
    w["band_2g"]["ap_on"] = Value::Null;
    let h = hub_with(w, json!({}));
    h.read(|st| {
        assert!(editable(st).is_some());
        let v = view(st, None, None, Lang::En);
        assert_eq!(v.state, "Wi-Fi module status unknown");
        assert!(v.meta.iter().any(|s| s.contains("broadcast status not read")));
    });
}

#[test]
fn wifi_switch_conflicts_and_band_gates_use_seed_and_current_state() {
    let s = seed();
    let mut d = s.clone();
    d.b[0].radio = false;
    let c = check(&s, &d, ["", ""], Some(&cur()), Lang::En).unwrap();
    assert_eq!(c.params["expect"], json!({"band_2g.radio_on": true}));
    assert!(!c.txpower_only());
    let mut now = cur();
    now.bands[0].radio_on = Some(false);
    let err = check(&s, &d, ["", ""], Some(&now), Lang::En).unwrap_err();
    assert!(err.text(Lang::En).contains("changed since the form was opened"));
    now.bands[0].radio_on = Some(true);
    now.module_on = Some(false);
    let err = check(&s, &d, ["", ""], Some(&now), Lang::En).unwrap_err();
    assert!(err.text(Lang::En).contains("turn Wi-Fi on"));
    d.on = false;
    let err = check(&s, &d, ["", ""], Some(&cur()), Lang::En).unwrap_err();
    assert!(err.text(Lang::En).contains("separately"));

    let mut d = s.clone();
    d.on = false;
    now.on = Some(false);
    let err = check(&s, &d, ["", ""], Some(&now), Lang::En).unwrap_err();
    assert!(err.text(Lang::En).contains("changed since the form was opened"));
    let mut split = cur();
    split.on = Some(false);
    let split_seed = Fields::seed(&split);
    let mut split_draft = split_seed.clone();
    split_draft.on = true;
    let c = check(&split_seed, &split_draft, ["", ""], Some(&split), Lang::En).unwrap();
    assert_eq!(c.params["expect"], json!({"on": false, "module_on": true}));
    assert!(!holds(&c, &split));
    split.on = Some(true);
    assert!(holds(&c, &split));
}

#[test]
fn wifi_form_gate_keeps_seed_and_disables_bands_until_master_is_on() {
    let mut w = wifi_data();
    w["module_on"] = json!(false);
    let h = hub_with(w, json!({}));
    let html = h.read(|st| form(true, st, Lang::En));
    assert!(html.contains("data-attr:disabled=\"!$_gon || !$_gmodule || !$_fon\""));
    assert!(html.contains("aria-labelledby=\"wf-fr2-l\" aria-checked=\"true\" data-selected=\"true\" disabled"));
    assert!(html.contains("_gmodule&#34;:false"));
}

#[test]
fn wifi_explicit_noop_requires_fresh_matching_state_and_ignores_expect() {
    let mut d = seed();
    d.b[0].radio = false;
    let c = check(&seed(), &d, ["", ""], Some(&cur()), Lang::En).unwrap();
    let mut w = wifi_data();
    w["band_2g"]["radio_on"] = json!(false);
    let h = hub_with(w.clone(), json!({}));
    assert!(explicit_no_changes(&json!({"ok":true,"result":{"note":"no changes"}})));
    assert!(!explicit_no_changes(&json!({"ok":true})));
    assert!(!explicit_no_changes(&json!({"result":{"note":"no changes"},"op":{}})));
    assert!(h.read(|st| fresh(st) && holds(&c, &wifi(st).unwrap())));
    h.set_link_for_test(Link::Down);
    assert!(!h.read(fresh));
    let mut w = wifi_of(&w).unwrap();
    w.bands[0].radio_on = Some(true);
    assert!(!holds(&c, &w));
}

#[test]
fn wifi_pending_or_stale_at_confirmation_blocks_send() {
    let pending = hub_with(wifi_data(), json!({"active": {"item": "wifi.main", "op_id": "other", "say_en": "Working"}}));
    assert!(pending.read(|st| write_gate(st, Lang::En).unwrap().text(Lang::En).contains("still in progress")));
    let idle = hub_with(wifi_data(), json!({}));
    assert!(idle.read(|st| write_gate(st, Lang::En).is_none()));
    idle.set_link_for_test(Link::Down);
    assert!(idle.read(|st| write_gate(st, Lang::En).unwrap().text(Lang::En).contains("not current")));
}

#[test]
fn wifi_undo_from_the_change_log_is_tier_three_in_wifi_words() {
    for a in ["wifi.settings", "wifi.guest"] {
        assert!(crate::changes::UNDO_ACTIONS.contains(&a));
        assert_eq!(crate::changes::tier(a), write::Tier::Dialog);
    }
    let e = json!({"what_zh": "Wi-Fi 设置", "change_zh": "", "undo_view": {"ok": true, "label_zh": "撤销",
        "request": {"action": "wifi.settings", "undo": true, "params": {"restore": {}}}}});
    let c = crate::changes::confirm(&e, Lang::Zh);
    assert!(c.cuts_uplink && c.downtime.unwrap().contains("Wi-Fi 重新加载"));
}

fn dfs_hub(d: Value) -> Arc<Hub> {
    let hub = Hub::new();
    hub.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch": "e", "blocks": {"wifi_dfs": blk(d)}}).to_string(),
    });
    hub
}

#[test]
fn the_dfs_line_says_only_what_matters() {
    let base = json!({"want_5g": true, "up": true, "channel": 36, "width": 160, "on_dfs": true, "auto": true,
                      "down_since": null, "override_channel": null, "override_htmode": null, "override_at": null,
                      "restore_at": null, "last_event": "", "last_event_at": null});
    let line = |d: &Value, l| dfs_hub(d.clone()).read(|st| dfs_line(st, l));
    assert!(line(&base, Lang::Zh).unwrap().contains("DFS"));
    let mut d = base.clone();
    d["on_dfs"] = json!(false);
    assert_eq!(line(&d, Lang::En), None, "a non-DFS channel: nothing to say");
    d["want_5g"] = json!(false);
    assert_eq!(line(&d, Lang::En), None);
    let mut d = base.clone();
    d["up"] = json!(false);
    d["down_since"] = json!(3600);
    assert!(line(&d, Lang::En).unwrap().contains("moving it"));
    d["auto"] = json!(false);
    assert!(line(&d, Lang::Zh).unwrap().contains("还没恢复"));
    let mut d = base.clone();
    d["override_channel"] = json!("0");
    d["override_at"] = json!(0);
    d["restore_at"] = json!(1800);
    let t = line(&d, Lang::Zh).unwrap();
    assert!(t.contains("00:30") && t.contains("自动"), "{t}");
}
