use super::*;
use crate::hub::Hub;
use crate::sse::Frame;
use std::sync::Arc;

fn blk(data: Value) -> Value {
    json!({"revision": 1, "observed_at": 1, "stale": false, "data": data})
}

/// Synthetic values only.
fn wifi(guest: Value) -> Value {
    json!({"on": true, "wifi6": true, "country": "CN", "band_2g": null, "band_5g": null, "guest": guest})
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

fn form(g: &Guest) -> FormIn {
    FormIn {
        on_2g: g.on_2g,
        on_5g: g.on_5g,
        ssid: g.ssid.clone(),
        pw: String::new(),
        enc: g.encryption.clone(),
        hidden: g.hidden,
        isolate: g.isolate,
        time: json!(g.active_time.to_string()),
    }
}

#[test]
fn view_reads_the_guest_part_without_the_password() {
    let h = hub_with(wifi(guest_on()));
    let v = h.read(|st| view(st, Lang::Zh));
    assert_eq!(v.tone, "ok");
    assert_eq!(v.state, "访客 Wi-Fi 已开启");
    assert_eq!(v.reason.as_deref(), Some("2.4 GHz"));
    assert_eq!(v.meta.as_deref(), Some("剩余 1 小时 02 分 05 秒"));
    assert_eq!(v.left, Some(3725));
    let html = Leaves { v: &v }.render().unwrap();
    assert!(html.contains("已设密码") && html.contains("Cafe-Guest") && html.contains("120 分钟"));
}

#[test]
fn view_warns_when_wifi_is_off_and_when_there_is_no_guest_network() {
    let mut w = wifi(guest_on());
    w["on"] = json!(false);
    let v = hub_with(w).read(|st| view(st, Lang::En));
    assert_eq!(
        (v.tone, v.state.as_str()),
        ("warn", "Guest Wi-Fi set on, but Wi-Fi is off")
    );
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
fn only_changed_keys_go_and_an_empty_password_is_kept() {
    let c = cur();
    let mut f = form(&c);
    assert!(
        matches!(
            check(&f, Some(&c), Lang::Zh),
            Err(write::Outcome::NotYet(_))
        ),
        "no change"
    );
    f.on_5g = true;
    f.ssid = "Cafe 2".into();
    f.time = json!("0");
    let s = check(&f, Some(&c), Lang::Zh).unwrap();
    assert_eq!(
        Value::Object(s.params.clone()),
        json!({"guest_disabled_5g": "0", "guest_ssid": "Cafe 2", "guest_active_time": "0"})
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
}

#[test]
fn checks_refuse_what_the_device_would_drop_or_reject() {
    let c = cur();
    let bad = |f: &FormIn| check(f, Some(&c), Lang::En).unwrap_err().text(Lang::En);
    let mut f = form(&c);
    f.ssid = "a;b".into();
    assert!(bad(&f).contains("silently drop"));
    f.ssid = "x".repeat(33);
    assert!(bad(&f).contains("32 bytes"));
    f.ssid = "ok".into();
    f.pw = "short".into();
    assert!(bad(&f).contains("8–63"));
    f.pw = "pass&word".into();
    assert!(bad(&f).contains("silently drop"));
    f.pw = "pässwort1".into();
    assert!(bad(&f).contains("ASCII"));
    f.pw = String::new();
    f.time = json!("1441");
    assert!(bad(&f).contains("1440"));
    f.time = json!("10");
    f.enc = "wep".into();
    assert!(bad(&f).contains("security"));
    // open → WPA2 with no password on file
    let mut open = c.clone();
    open.encryption = "none".into();
    open.has_password = false;
    let mut f = form(&open);
    f.enc = "psk2+ccmp".into();
    assert!(
        check(&f, Some(&open), Lang::En)
            .unwrap_err()
            .text(Lang::En)
            .contains("Set a password")
    );
    f.pw = "goodpass1".into();
    let s = check(&f, Some(&open), Lang::En).unwrap();
    assert_eq!(s.params["guest_key"], "goodpass1");
    assert!(s.key_sent);
    // switching to open drops a typed password
    let mut f = form(&c);
    f.enc = "none".into();
    f.pw = "ignored-pw".into();
    let s = check(&f, Some(&c), Lang::En).unwrap();
    assert!(!s.params.contains_key("guest_key") && s.params["guest_encryption"] == "none");
    // 64 hex digits is a raw key
    let mut f = form(&c);
    f.pw = "a".repeat(64);
    assert!(check(&f, Some(&c), Lang::En).is_ok());
    f.pw = "g".repeat(64);
    assert!(check(&f, Some(&c), Lang::En).is_err());
    // one band's guest = three APs with the mains: both bands or neither (a layout already
    // like that — this fixture, 2.4 only — is left alone)
    let mut off = c.clone();
    off.on_2g = false;
    let mut f = form(&off);
    f.on_5g = true;
    assert!(check(&f, Some(&off), Lang::En).unwrap_err().text(Lang::En).contains("both bands"));
    f.on_2g = true;
    assert!(check(&f, Some(&off), Lang::En).is_ok());
    let mut f = form(&c);
    f.ssid = "renamed".into();
    assert!(check(&f, Some(&c), Lang::En).is_ok(), "unchanged bands: not refused");
    f.on_2g = false;
    assert!(check(&f, Some(&c), Lang::En).is_ok(), "turning the lone band off is fine");
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
