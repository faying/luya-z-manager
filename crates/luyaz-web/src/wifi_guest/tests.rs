use super::*;
use crate::hub::Hub;
use crate::sse::Frame;
use std::sync::Arc;

fn blk(data: Value) -> Value {
    json!({"revision": 1, "observed_at": 1, "stale": false, "data": data})
}

/// Synthetic values only.
fn wifi(guest: Value) -> Value {
    json!({"on": true, "module_on": true, "wifi6": true, "country": "CN", "band_2g": {"radio_on": true, "ap_on": true}, "band_5g": {"radio_on": true, "ap_on": true}, "guest": guest})
}

fn guest_on() -> Value {
    json!({"ssid": "Cafe-Guest", "encryption": "psk2+ccmp", "has_password": true, "on_2g": true, "on_5g": false,
           "hidden": false, "isolate": true, "active_time": 120, "left_s": 3725})
}

fn hub_with(w: Value) -> Arc<Hub> {
    let hub = Hub::new();
    hub.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch": "e", "blocks": {"wifi": blk(w)}}).to_string(),
    });
    hub.set_link_for_test(Link::Live);
    hub
}

fn cur() -> Guest {
    Guest::of(&guest_on()).unwrap()
}

#[test]
fn wifi_guest_view_reads_the_guest_part_without_the_password() {
    let h = hub_with(wifi(guest_on()));
    let v = h.read(|st| view(st, Lang::Zh));
    assert_eq!(v.tone, "ok");
    assert_eq!(v.state, "访客 Wi-Fi 已设置开启");
    assert_eq!(v.reason.as_deref(), Some("2.4 GHz"));
    assert_eq!(v.meta.as_deref(), Some("剩余 1 小时 02 分 05 秒"));
    assert_eq!(v.left, Some(3725));
    let html = Leaves { v: &v }.render().unwrap();
    assert!(html.contains("已设密码") && html.contains("Cafe-Guest") && html.contains("120 分钟"));
}

#[test]
fn wifi_guest_reads_module_radio_and_ap_flags_without_filling_unknowns() {
    let mut w = wifi(guest_on());
    w["on"] = json!(false);
    w["band_2g"]["radio_on"] = Value::Null;
    w["band_5g"]["ap_on"] = json!(false);
    let h = hub_with(w);
    h.read(|st| {
        let r = read(st).unwrap();
        assert_eq!(r.module_on, Some(true));
        assert_eq!(r.radios, [None, Some(true)]);
        assert_eq!(r.main_aps, [Some(true), Some(false)]);
    });
}

#[test]
fn wifi_guest_view_warns_when_eligibility_is_blocked_or_unknown() {
    let mut w = wifi(guest_on());
    w["module_on"] = json!(false);
    let blocked = |w: Value, description: &str| {
        hub_with(w).read(|st| {
            let v = view(st, Lang::En);
            assert_eq!((v.tone, v.state.as_str()), ("warn", "Guest Wi-Fi set to on"));
            assert_eq!(v.left, None);
            let html = Leaves { v: &v }.render().unwrap();
            assert!(html.to_lowercase().contains(description));
            assert!(!html.contains("broadcasting"));
            assert!(summary(st, Lang::En).unwrap().1.to_lowercase().contains(description));
            let g = gate(st, Lang::En).unwrap();
            assert!(form_html(true, Some(&g), Lang::En).contains("data-bind=\"_wgtime\""));
        });
    };
    blocked(w, "wi-fi module off");
    let mut w = wifi(guest_on());
    w["band_2g"]["radio_on"] = json!(false);
    blocked(w, "2.4 ghz: radio off");
    let mut w = wifi(guest_on());
    w["band_2g"]["ap_on"] = json!(false);
    blocked(w, "2.4 ghz: main network off");
    let mut w = wifi(guest_on());
    w["module_on"] = Value::Null;
    blocked(w, "availability unknown");
    let v = hub_with(wifi(
        json!({"ssid": "", "encryption": "", "has_password": false, "on_2g": false, "on_5g": false,
        "hidden": false, "isolate": true, "active_time": 0, "left_s": null}),
    ))
    .read(|st| view(st, Lang::En));
    assert_eq!(v.state, "No guest network found");
    let v = hub_with(Value::Null).read(|st| view(st, Lang::En));
    assert_eq!((v.tone, v.retry), ("bad", true));
}

#[test]
fn wifi_guest_sends_only_edited_keys_and_keeps_empty_password() {
    let c = cur();
    let mut f = c.signals();
    assert!(
        matches!(
            check(&f, &c, Lang::Zh),
            Err(write::Outcome::NotYet(_))
        ),
        "no change"
    );
    f["_wgon5"] = json!(true);
    f["_wgssid"] = json!("Cafe 2");
    f["_wgtime"] = json!("0");
    let s = check(&f, &c, Lang::Zh).unwrap();
    assert_eq!(
        Value::Object(s.params.clone()),
        json!({"guest_disabled_5g": "0", "guest_ssid": "Cafe 2", "guest_active_time": "0",
               "expect": {"guest.on_5g": false, "guest.ssid": "Cafe-Guest", "guest.active_time": 120}})
    );
    assert!(!s.params.contains_key("guest_key"));
    assert!(!s.key_sent);
    let mut g = c.clone();
    g.on_5g = true;
    g.ssid = "Cafe 2".into();
    g.active_time = 0;
    assert!(s.holds(&g) && !s.holds(&c));
    // the reload drops the main network, and guests must reconnect to the new name
    assert!(
        s.what.contains("主网络")
            && s.reconnect.as_deref() == Some("访客设备要重新连接「Cafe 2」。")
    );
    assert!(confirm(&s, Lang::Zh).cuts_uplink, "a remote path may depend on Wi-Fi");
}

#[test]
fn wifi_guest_checks_refuse_what_the_device_would_drop_or_reject() {
    let c = cur();
    let bad = |f: &Value| check(f, &c, Lang::En).unwrap_err().text(Lang::En);
    let mut f = c.signals();
    f["_wgssid"] = json!("a;b");
    assert!(bad(&f).contains("silently drop"));
    f["_wgssid"] = json!("x".repeat(33));
    assert!(bad(&f).contains("32 bytes"));
    f["_wgssid"] = json!("ok");
    f["_wgpw"] = json!("short");
    assert!(bad(&f).contains("8–63"));
    f["_wgpw"] = json!("pass&word");
    assert!(bad(&f).contains("silently drop"));
    f["_wgpw"] = json!("pässwort1");
    assert!(bad(&f).contains("ASCII"));
    f["_wgpw"] = json!("");
    f["_wgtime"] = json!("1441");
    assert!(bad(&f).contains("1440"));
    f["_wgtime"] = json!("10");
    f["_wgenc"] = json!("wep");
    assert!(bad(&f).contains("security"));
    // open → WPA2 with no password on file
    let mut open = c.clone();
    open.encryption = "none".into();
    open.has_password = false;
    let mut f = open.signals();
    f["_wgenc"] = json!("psk2+ccmp");
    assert!(
        check(&f, &open, Lang::En)
            .unwrap_err()
            .text(Lang::En)
            .contains("Set a password")
    );
    f["_wgpw"] = json!("goodpass1");
    let s = check(&f, &open, Lang::En).unwrap();
    assert_eq!(s.params["guest_key"], "goodpass1");
    assert!(s.key_sent);
    // switching to open drops a typed password
    let mut f = c.signals();
    f["_wgenc"] = json!("none");
    f["_wgpw"] = json!("ignored-pw");
    let s = check(&f, &c, Lang::En).unwrap();
    assert!(!s.params.contains_key("guest_key") && s.params["guest_encryption"] == "none");
    // 64 hex digits is a raw key
    let mut f = c.signals();
    f["_wgpw"] = json!("a".repeat(64));
    assert!(check(&f, &c, Lang::En).is_ok());
    f["_wgpw"] = json!("g".repeat(64));
    assert!(check(&f, &c, Lang::En).is_err());
    // one band's guest = three APs with the mains: both bands or neither (a layout already
    // like that — this fixture, 2.4 only — is left alone)
    let mut off = c.clone();
    off.on_2g = false;
    let mut f = off.signals();
    f["_wgon5"] = json!(true);
    assert!(check(&f, &off, Lang::En).unwrap_err().text(Lang::En).contains("both bands"));
    f["_wgon2"] = json!(true);
    assert!(check(&f, &off, Lang::En).is_ok());
    let mut f = c.signals();
    f["_wgssid"] = json!("renamed");
    assert!(check(&f, &c, Lang::En).is_ok(), "unchanged bands: not refused");
    f["_wgon2"] = json!(false);
    assert!(check(&f, &c, Lang::En).is_ok(), "turning the lone band off is fine");
}

#[test]
fn wifi_guest_form_preserves_other_edits_and_rejects_same_field_conflicts() {
    let mut seed = cur();
    seed.isolate = false;
    let html = form_html(true, Some(&seed), Lang::En);
    assert!(html.contains("_wgs_iso&#34;:false"));
    assert!(html.contains("_wgs_ssid&#34;:&#34;Cafe-Guest&#34;"));
    assert!(!html.contains("_wgs_pw"));
    let mut f = seed.signals();
    let mut now = seed.clone();
    now.isolate = true;
    f["_wgssid"] = json!("Renamed");
    let s = check(&f, &now, Lang::En).unwrap();
    assert_eq!(json!(s.params), json!({"guest_ssid": "Renamed", "expect": {"guest.ssid": "Cafe-Guest"}}));
    assert!(s.want.isolate);
    assert!(!s.what.contains("isolation"));
    assert!(!s.holds(&now));
    now.ssid = "Renamed".into();
    now.isolate = false;
    assert!(s.holds(&now));
    assert!(check(&f, &now, Lang::En).unwrap_err().text(Lang::En).contains("reopen the form"));
    f.as_object_mut().unwrap().remove("_wgs_ssid");
    assert!(check(&f, &seed, Lang::En).is_err());
}

#[test]
fn replies_say_what_u60d_said() {
    let l = Lang::Zh;
    assert_eq!(
        reply_of(200, &json!({"ok": true, "op": {"op_id": "x"}}), l),
        Ok(Reply::Took(write::Sent::Ok, true))
    );
    assert_eq!(
        reply_of(
            200,
            &json!({"ok": true, "result": {"status": "ok", "note": "no changes"}}),
            l
        ),
        Ok(Reply::NoChanges)
    );
    let busy = json!({"ok": false, "error": {"code": "busy", "message": "another change is in progress"},
        "doing": {"action": "network.set_mode", "say_zh": "正在换制式", "say_en": "Busy: network mode"}});
    assert_eq!(
        reply_of(409, &busy, l).unwrap_err().text(l),
        "没有生效：另一项改动正在进行（正在换制式），等它结束再试"
    );
    let lock = json!({"ok": false, "error": {"code": "busy", "message": "Wi-Fi is being changed by another writer; try again"}});
    assert!(
        reply_of(503, &lock, l)
            .unwrap_err()
            .text(l)
            .contains("Wi-Fi 正在被别处修改")
    );
    let refused =
        json!({"ok": false, "error": {"code": "refused", "message": "Wi-Fi 6 can't change here"}});
    assert_eq!(
        reply_of(409, &refused, l).unwrap_err(),
        write::Outcome::Failed("Wi-Fi 6 can't change here".into())
    );
}

#[test]
fn left_text_matches_the_countdown_words() {
    assert_eq!(left_text(Lang::En, 65), "1 min 05 s left");
    assert_eq!(left_text(Lang::En, 3600), "1 h 00 min 00 s left");
    assert_eq!(left_text(Lang::Zh, 0), "已到期");
}
