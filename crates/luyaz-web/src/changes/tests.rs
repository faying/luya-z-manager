use super::*;
use crate::sse::Frame;
use std::sync::Arc;

/// The rows as u60d decorates them (the old web's mock fixture, fixtures/ops.ts).
fn entries() -> Vec<Value> {
    serde_json::from_value(json!([
        {"source": "screen", "action": "op.ack", "result": "ok", "hide": true, "what_zh": "op.ack", "what_en": "op.ack"},
        {"op_id": "web-20", "action": "network.set_mode", "item": "network.mode", "source": "web", "undo": false,
         "new": "Only_LTE", "old": "WL_AND_5G", "t": "2026-10-03 14:32:07", "what_zh": "制式", "what_en": "Network mode",
         "change_zh": "自动 → 只用 4G", "change_en": "Auto → 4G only", "result_zh": "已切到只用 4G", "result_en": "Now 4G only",
         "mark": "ok", "source_zh": "网页", "source_en": "Web", "hide": false,
         "undo_view": {"ok": true, "label_zh": "撤销", "label_en": "Undo", "why_zh": null, "why_en": null,
                       "request": {"action": "network.set_mode", "undo": true, "params": {"mode": "WL_AND_5G"}}}},
        {"action": "cellular.set", "source": "screen", "params": {"enabled": 0}, "result": "failed", "t": "2026-10-03 09:01:00",
         "what_zh": "移动数据", "what_en": "Mobile data", "change_zh": "关掉数据", "change_en": "Turn data off",
         "result_zh": "没改成", "result_en": "Not changed", "mark": "bad", "source_zh": "触屏", "source_en": "Screen",
         "hide": false, "undo_view": null},
        {"action": "wifi.apply", "item": "wifi", "source": "scenario", "result": "skipped", "skip": "end", "count": 5,
         "t": "2026-10-02 22:10:00", "what_zh": "Wi-Fi", "what_en": "Wi-Fi", "change_zh": "", "change_en": "",
         "result_zh": "情景跳过 ×5（你手动改过）", "result_en": "Scene skipped ×5 (you changed it by hand)",
         "mark": "warn", "source_zh": "情景", "source_en": "Scene", "hide": false, "undo_view": null},
        {"op_id": "screen-3-1", "action": "network.set_mode", "item": "network.mode", "source": "screen", "undo": false,
         "new": "WL_AND_5G", "old": "Only_5G", "t": "2026-10-01 19:45:00", "what_zh": "制式", "what_en": "Network mode",
         "change_zh": "只用 5G SA → 自动", "change_en": "5G SA only → Auto", "result_zh": "已切到自动", "result_en": "Now Auto",
         "mark": "ok", "source_zh": "触屏", "source_en": "Screen", "hide": false,
         "undo_view": {"ok": false, "label_zh": "撤销", "label_en": "Undo", "why_zh": "之后又改过", "why_en": "Changed since",
                       "request": {"action": "network.set_mode", "undo": true, "params": {"mode": "Only_5G"}}}}
    ]))
    .unwrap()
}

fn got(e: Vec<Value>) -> Got {
    Got {
        ok: Some((e, SystemTime::now())),
        ..Got::default()
    }
}

fn html(v: &View) -> String {
    leaves(v).into_iter().map(|(_, h)| h).collect()
}

fn op(id: &str) -> Want {
    Want {
        op: Some(id.into()),
        row: None,
    }
}

#[test]
fn rows_are_u60d_words_in_two_lines() {
    let e = entries();
    let vis = visible(&e);
    assert_eq!(vis.len(), 4, "the hidden op.ack is not a row");
    let r = row(vis[0], Lang::Zh);
    assert_eq!(
        (r.what.as_str(), r.change.as_str()),
        ("制式", "自动 → 只用 4G")
    );
    assert_eq!(r.line2, "10-03 14:32 · 网页 · 已切到只用 4G");
    assert_eq!(r.href, "/changes?op=web-20");
    assert_eq!(r.tone, "ok");
    // the source is not repeated when the result starts with it
    assert_eq!(
        row(vis[2], Lang::Zh).line2,
        "10-02 22:10 · 情景跳过 ×5（你手动改过）"
    );
    assert_eq!(row(vis[1], Lang::Zh).tone, "bad");
    assert_eq!(row(vis[2], Lang::Zh).tone, "warn");
    // a row without an op_id gets a stable key of its own
    let k = row(vis[1], Lang::Zh).href;
    assert!(
        k.starts_with("/changes?row=") && k.len() == "/changes?row=".len() + 16,
        "{k}"
    );
    assert_eq!(k, row(&vis[1].clone(), Lang::En).href);
    assert_eq!(short_when("2026-10-03 14:32:07"), "10-03 14:32");
    assert_eq!(short_when("x"), "x");
}

#[test]
fn list_detail_and_states() {
    let mut snap = None;
    let v = view(
        &got(entries()),
        &Want::default(),
        &mut snap,
        false,
        Lang::Zh,
    );
    let h = html(&v);
    assert_eq!(h.matches("class=\"nd-row nd-row--two\" href=").count(), 4);
    assert!(h.contains("只显示最近 50 条") && !h.contains("撤销"), "{h}");

    // ?op= opens the detail: result, what · change, time · source, the undo button
    let v = view(&got(entries()), &op("web-20"), &mut snap, false, Lang::Zh);
    let h = html(&v);
    for s in [
        "‹ 改动记录",
        "已切到只用 4G",
        "制式 · <span data-data>自动 → 只用 4G</span>",
        "10-03 14:32 · 网页",
        "/changes/undo/ask?op=web-20",
    ] {
        assert!(h.contains(s), "{s}: {h}");
    }
    assert!(!h.contains(" disabled"), "can be undone");
    // stuck: off, with the reason
    let h = html(&view(
        &got(entries()),
        &op("web-20"),
        &mut None,
        true,
        Lang::Zh,
    ));
    assert!(h.contains(" disabled") && h.contains("数据服务没响应 · 暂时不能改设置"));
    // can't be undone: off, u60d's reason
    let h = html(&view(
        &got(entries()),
        &op("screen-3-1"),
        &mut None,
        false,
        Lang::Zh,
    ));
    assert!(h.contains(" disabled") && h.contains("之后又改过"));
    // no undo_view: no button
    let w = Want::of(&entries()[2]);
    let h = html(&view(&got(entries()), &w, &mut None, false, Lang::Zh));
    assert!(
        h.contains("没改成")
            && !h.contains("<button type=\"button\" class=\"nd-btn nd-btn--secondary\""),
        "{h}"
    );

    // fell out of the latest 50: what was shown, without undo
    let rest: Vec<Value> = entries()
        .into_iter()
        .filter(|e| e["op_id"] != "web-20")
        .collect();
    let h = html(&view(
        &got(rest.clone()),
        &op("web-20"),
        &mut snap,
        false,
        Lang::Zh,
    ));
    assert!(
        h.contains("已切到只用 4G") && !h.contains("/changes/undo/ask"),
        "{h}"
    );
    // an op that was never in the list: the list (the old page did the same)
    let h = html(&view(&got(rest), &op("nope-1"), &mut None, false, Lang::Zh));
    assert!(h.contains("只显示最近 50 条"));

    // empty, loading, unreadable, unreadable with old data, old u60d
    let h = html(&view(
        &got(vec![]),
        &Want::default(),
        &mut None,
        false,
        Lang::Zh,
    ));
    assert!(
        h.contains("还没有改动") && h.contains("触屏、网页、情景、定时任务改的设置都会记在这里。")
    );
    let h = html(&view(
        &Got::default(),
        &Want::default(),
        &mut None,
        false,
        Lang::Zh,
    ));
    assert!(h.contains("正在读取改动记录…") && h.contains("nd-skel"));
    let failed = Got {
        err: Some(ReadErr::Unreachable),
        ..Got::default()
    };
    let h = html(&view(&failed, &Want::default(), &mut None, false, Lang::Zh));
    assert!(
        h.contains("读不到改动记录 · 数据服务没响应")
            && h.contains("/changes/refresh")
            && h.contains("role=\"alert\"")
    );
    let mut stale = got(entries());
    stale.err = Some(ReadErr::NoReply);
    let h = html(&view(&stale, &Want::default(), &mut None, false, Lang::Zh));
    assert!(
        h.contains("读不到改动记录")
            && h.contains("数字停在 ")
            && h.contains("nd-group nd-stale")
            && h.contains("已切到只用 4G")
    );
}

#[test]
fn english_pages_have_no_chinese() {
    let cjk = |s: &str| {
        s.chars().any(|c| {
            ('\u{3000}'..='\u{9fff}').contains(&c) || ('\u{ff00}'..='\u{ffef}').contains(&c)
        })
    };
    let states = [
        view(
            &got(entries()),
            &Want::default(),
            &mut None,
            false,
            Lang::En,
        ),
        view(&got(entries()), &op("web-20"), &mut None, true, Lang::En),
        view(
            &got(entries()),
            &op("screen-3-1"),
            &mut None,
            false,
            Lang::En,
        ),
        view(&got(vec![]), &Want::default(), &mut None, false, Lang::En),
        view(
            &Got::default(),
            &Want::default(),
            &mut None,
            false,
            Lang::En,
        ),
        view(
            &Got {
                err: Some(ReadErr::Unreachable),
                ..Got::default()
            },
            &Want::default(),
            &mut None,
            false,
            Lang::En,
        ),
        view(
            &Got {
                err: Some(ReadErr::NoReply),
                ..Got::default()
            },
            &Want::default(),
            &mut None,
            false,
            Lang::En,
        ),
    ];
    for v in &states {
        let h = html(v);
        assert!(!cjk(&h), "{h}");
    }
    let e = &entries()[1];
    for c in [confirm(e, Lang::En), confirm(&made_up_tier2(), Lang::En)] {
        for s in [&c.title, &c.what, &c.action] {
            assert!(!cjk(s), "{s}");
        }
        for s in [&c.downtime, &c.recovery].into_iter().flatten() {
            assert!(!cjk(s), "{s}");
        }
    }
    let live = Seen::Live(json!({"say_zh": "正在确认", "say_en": "Checking"}));
    assert!(!cjk(
        &outcome(&live, write::Sent::Ok, Lang::En).text(Lang::En)
    ));
    assert!(!cjk(&gone(Lang::En).text(Lang::En)));
}

/// A transaction action that does not cut the mobile link. u60d has none today (only
/// network.set_mode has a spec), so this is made up: it checks the tier rule, never sent.
fn made_up_tier2() -> Value {
    json!({"op_id": "web-30", "action": "nfc.set", "what_zh": "碰一碰", "what_en": "NFC",
           "change_zh": "关 → 开", "change_en": "off → on", "result_zh": "已改", "result_en": "Done", "mark": "ok",
           "undo_view": {"ok": true, "label_zh": "撤销", "label_en": "Undo",
                         "request": {"action": "nfc.set", "undo": true, "params": {"enabled": 0}}}})
}

#[test]
fn tier_per_action() {
    assert_eq!(tier("network.set_mode"), write::Tier::Dialog);
    assert_eq!(tier("nfc.set"), write::Tier::Inline);
    let c = confirm(&entries()[1], Lang::Zh);
    assert_eq!(c.tier, write::Tier::Dialog);
    assert_eq!(c.title, "撤销：制式？");
    assert_eq!(c.what, "撤回这次改动：自动 → 只用 4G");
    assert_eq!(
        c.downtime.as_deref(),
        Some("调制解调器重新附着，移动网络断开约 30 秒。")
    );
    assert!(
        c.recovery
            .as_deref()
            .unwrap()
            .starts_with("在「网络模式」页改回来")
    );
    assert!(c.cuts_uplink && c.action == "撤销" && c.word.is_none());
    let c = confirm(&made_up_tier2(), Lang::Zh);
    assert_eq!(c.tier, write::Tier::Inline);
    assert_eq!(c.what, "撤销：碰一碰 · 关 → 开");
    assert!(!c.cuts_uplink && c.downtime.is_none());
    // the dialog renders the three facts
    let a = write::Area {
        id: "ch-undo",
        l: Lang::Zh,
        remote: Via::Tailnet,
        set: "/changes/undo?op=web-20",
        cancel: "/changes/undo/cancel",
        phase: &write::Phase::Ask(confirm(&entries()[1], Lang::Zh)),
    }
    .html();
    assert!(
        a.contains("<dialog")
            && a.contains("断开多久")
            && a.contains("Tailscale")
            && a.contains("{retryMaxCount: 0}"),
        "{a}"
    );
}

#[test]
fn undo_body_is_a_new_op_with_undo_set() {
    let e = &entries()[1];
    let a = write::next_op_id();
    let b = write::next_op_id();
    let x = undo_body(e, &a, false, Lang::Zh).unwrap();
    let y = undo_body(e, &b, false, Lang::Zh).unwrap();
    assert_ne!(x["op_id"], y["op_id"], "a fresh op_id per undo");
    for v in [&x, &y] {
        assert_ne!(v["op_id"], "web-20", "never the row's own id");
        assert_eq!(v["undo"], true);
        assert_eq!(v["source"], "web");
        assert_eq!(v["action"], "network.set_mode");
        assert_eq!(v["params"], json!({"mode": "WL_AND_5G"}));
        assert!(v["params"].get("op_id").is_none());
    }
    // the row's own id is refused outright
    assert!(matches!(
        undo_body(e, "web-20", false, Lang::Zh),
        Err(write::Outcome::Failed(_))
    ));
    // u60d says no: its reason, nothing sent
    let o = undo_body(&entries()[4], &a, false, Lang::Zh).unwrap_err();
    assert_eq!(o, write::Outcome::NotYet("之后又改过".into()));
    // stuck executor
    assert_eq!(
        undo_body(e, &a, true, Lang::Zh).unwrap_err(),
        write::Outcome::NotYet("数据服务没响应 · 暂时不能改设置".into())
    );
    // only u60d's transaction actions go out (the agent's /api/ops/write guard)
    assert!(matches!(
        undo_body(&made_up_tier2(), &a, false, Lang::Zh),
        Err(write::Outcome::Failed(_))
    ));
    // no undo_view
    assert!(matches!(
        undo_body(&entries()[2], &a, false, Lang::Zh),
        Err(write::Outcome::NotYet(_))
    ));
}

fn op_block(rev: u64, data: Value) -> Frame {
    Frame {
        event: "block".into(),
        data: json!({"epoch": "e", "name": "op", "revision": rev, "observed_at": rev, "stale": false, "data": data})
            .to_string(),
    }
}

fn hub_with(data: Value) -> Arc<Hub> {
    let hub = Hub::new();
    hub.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch": "e", "blocks": {"op": {"epoch": "e", "name": "op", "revision": 1, "observed_at": 1, "stale": false, "data": data}}})
            .to_string(),
    });
    hub
}

#[tokio::test]
async fn readback_follows_the_new_op_id_and_its_phase() {
    // `last` still holds the row's own transaction, final: that is not this undo's readback
    let old_last = json!({"rollback_enabled": true, "active": null,
        "last": {"op_id": "web-20", "phase": "confirmed", "mark": "ok", "say_zh": "已切到只用 4G", "say_en": "Now 4G only"}});
    let hub = hub_with(old_last);
    let s = follow(&hub, "lw-1-1", Duration::from_millis(150), hub.watch()).await;
    assert_eq!(s, Seen::Nothing);
    assert_eq!(
        outcome(&s, write::Sent::Ok, Lang::Zh),
        write::Outcome::Accepted
    );

    // live, then final ok: u60d's words, applied
    let hub = hub_with(json!({"active": null, "last": null}));
    let h2 = hub.clone();
    let rx = hub.watch();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        h2.apply(&op_block(2, json!({"active": {"op_id": "lw-1-2", "phase": "verifying", "say_zh": "正在确认", "say_en": "Checking"}, "last": null})));
        tokio::time::sleep(Duration::from_millis(50)).await;
        h2.apply(&op_block(3, json!({"active": null, "last": {"op_id": "lw-1-2", "phase": "confirmed", "mark": "ok", "say_zh": "已切到自动", "say_en": "Now Auto"}})));
    });
    let s = follow(&hub, "lw-1-2", Duration::from_secs(5), rx).await;
    let o = outcome(&s, write::Sent::Ok, Lang::Zh);
    assert_eq!(o, write::Outcome::Applied("已切到自动".into()));

    // final but rolled back: u60d's words with its tone, not "applied", not "没有生效：…"
    let s = Seen::Final(
        json!({"op_id": "x", "phase": "rolled_back", "mark": "warn", "say_zh": "没通 · 已退回只用 4G"}),
    );
    let o = outcome(&s, write::Sent::Ok, Lang::Zh);
    assert_eq!(o.text(Lang::Zh), "没通 · 已退回只用 4G");
    assert_eq!((o.tone(), o.role()), ("warn", "alert"));
    let o = outcome(
        &Seen::Final(json!({"mark": "bad", "say_zh": "退回也没通"})),
        write::Sent::Ok,
        Lang::Zh,
    );
    assert_eq!(o.tone(), "bad");

    // still live at the deadline: accepted, the bar follows (never "成功")
    let hub = hub_with(
        json!({"active": {"op_id": "lw-1-3", "phase": "verifying", "say_zh": "正在确认", "say_en": "Checking"}, "last": null}),
    );
    let s = follow(&hub, "lw-1-3", Duration::from_millis(100), hub.watch()).await;
    let o = outcome(&s, write::Sent::Ok, Lang::Zh);
    assert_eq!(
        o.text(Lang::Zh),
        "设备已接受 · 正在确认，结果会显示在顶部横条"
    );
    assert_eq!(o.tone(), "neutral");
    assert!(!o.text(Lang::Zh).contains("成功"));
    // no reply and nothing seen: not confirmed
    assert_eq!(
        outcome(&Seen::Nothing, write::Sent::NoReply, Lang::Zh),
        write::Outcome::Unknown
    );
}

#[tokio::test]
async fn one_post_per_undo_each_with_its_own_op_id() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let s = seen.clone();
    let app = axum::Router::new().route(
        "/control",
        axum::routing::post(move |axum::Json(b): axum::Json<Value>| {
            let s = s.clone();
            async move {
                s.lock().unwrap().push(b);
                axum::Json(json!({"ok": true, "op": {"phase": "applying"}}))
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/control", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let http = reqwest::Client::new();
    let e = &entries()[1];
    for _ in 0..2 {
        let body = undo_body(e, &write::next_op_id(), false, Lang::Zh).unwrap();
        let r = write::send_body(&http, &url, &body, None, Lang::Zh).await;
        assert_eq!(r, Ok(write::Sent::Ok));
    }
    let b = seen.lock().unwrap().clone();
    assert_eq!(b.len(), 2, "one POST per undo, no retries");
    assert_ne!(b[0]["op_id"], b[1]["op_id"]);
    assert!(
        b.iter()
            .all(|v| v["op_id"] != "web-20" && v["undo"] == true)
    );
    // busy is its own sentence
    let busy = axum::Router::new().route(
        "/control",
        axum::routing::post(|| async {
            (
                axum::http::StatusCode::CONFLICT,
                axum::Json(json!({"ok": false, "error": {"code": "busy", "message": "another change is in progress"}})),
            )
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/control", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, busy).await.unwrap() });
    let body = undo_body(e, &write::next_op_id(), false, Lang::Zh).unwrap();
    assert_eq!(
        write::send_body(&http, &url, &body, None, Lang::Zh).await,
        Err(write::Outcome::Busy)
    );
}

#[test]
fn want_keys_are_safe() {
    assert!(valid_id("web-20") && valid_id("lw-1791-3") && !valid_id("a'b") && !valid_id(""));
    let w = Want {
        op: Some("x');alert(1)//".into()),
        row: None,
    };
    assert_eq!(w.query(), "", "an op that isn't an op_id goes nowhere");
    let w = Want {
        op: None,
        row: Some("zz<".into()),
    };
    assert_eq!(w.query(), "");
}
