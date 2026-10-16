use super::*;
use crate::hub::Hub;
use crate::sse::Frame;

fn cjk(s: &str) -> Vec<char> {
    s.chars()
        .filter(|c| {
            ('\u{4e00}'..='\u{9fff}').contains(c)
                || ('\u{3000}'..='\u{303f}').contains(c)
                || ('\u{ff00}'..='\u{ffef}').contains(c)
        })
        .collect()
}

fn m(id: u64, number: &str, text: &str, tag: &str, store: Option<Store>) -> Msg {
    Msg {
        id,
        number: number.into(),
        text: text.into(),
        date: "2026-10-12 09:30".into(),
        tag: tag.into(),
        store,
    }
}

fn hub_with(sms: Option<Value>) -> std::sync::Arc<Hub> {
    let hub = Hub::new();
    let mut blocks = serde_json::Map::new();
    if let Some(d) = sms {
        blocks.insert(
            "sms".into(),
            json!({"revision":1,"observed_at":1,"stale":false,"data":d}),
        );
    }
    hub.apply(&Frame {
        event: "snapshot".into(),
        data: json!({"epoch":"e","blocks":blocks}).to_string(),
    });
    hub.set_link_for_test(Link::Live);
    hub
}

fn got(list: Vec<Msg>) -> Got {
    Got {
        ok: Some((list, SystemTime::now())),
        ..Got::default()
    }
}

#[test]
fn rows_decode_like_the_old_page() {
    assert_eq!(decode_text("6D4B8BD50041"), "测试A");
    assert_eq!(decode_text("not hex"), "not hex");
    assert_eq!(decode_text("00410000"), "A", "trailing NULs go");
    // numbers: plain stays plain, even when it looks like hex
    assert_eq!(decode_number("10086"), "10086");
    assert_eq!(decode_number("861380000000"), "861380000000");
    assert_eq!(decode_number("95588123"), "95588123");
    assert_eq!(decode_number("+8613800138000"), "+8613800138000");
    // out of the vendor's envelope: UCS-2 hex
    assert_eq!(decode_number("00310030003000380036"), "10086");
    assert_eq!(decode_number("0043004D0048004B"), "CMHK");
    assert_eq!(decode_number("4E2D56FD79FB52A8"), "中国移动");
    assert_eq!(format_date("26,05,29,10,56,56,+32"), "2026-05-29 10:56");
    assert_eq!(format_date("x"), "x");
    let a = msg(&json!({"id":"9","number":"10086","content":"0041","date":"26,10,12,09,30,05,+32","tag":"1","mem_store":0})).unwrap();
    assert_eq!(
        a,
        Msg {
            id: 9,
            number: "10086".into(),
            text: "A".into(),
            date: "2026-10-12 09:30".into(),
            tag: "1".into(),
            store: Some(Store::Sim)
        }
    );
    let b = msg(&json!({"id":10,"number":"x","content":"","tag":2,"mem_store":1})).unwrap();
    assert_eq!((b.tag.as_str(), b.store), ("2", Some(Store::Device)));
    assert_eq!(
        msg(&json!({"id":11,"tag":"0"})).unwrap().store,
        None,
        "older u60d: no store"
    );
    assert!(msg(&json!({"number":"1"})).is_none());
}

#[test]
fn ids_are_digits_only() {
    assert_eq!(parse_ids("3;1;x;2;;1"), vec![1, 2, 3]);
    assert_eq!(parse_ids("1) OR 1=1;--"), Vec::<u64>::new());
    assert_eq!(id_list(&[1, 2]), "1;2;");
}

#[test]
fn capacity_reads_the_per_box_totals() {
    let cap = json!({"sms_sim_total":"50","sms_sim_rev_total":"3","sms_sim_send_total":1,
        "sms_nv_total":100,"sms_nv_rev_total":"92","sms_nv_draftbox_total":"0","sms_nvused_total":0});
    assert_eq!(store_usage(&cap, "sim"), Some((4, 50)));
    assert_eq!(store_usage(&cap, "nv"), Some((92, 100)));
    assert_eq!(
        store_usage(&json!({"sms_sim_total":50,"sms_simused_total":7}), "sim"),
        Some((7, 50))
    );
    assert_eq!(store_usage(&json!({}), "sim"), None);
    // ≥ 90 %: the warning, with the old words
    let hub = hub_with(Some(json!({"unread":2,"max_id":31,"count":6})));
    let v = hub.read(|st| view(st, &got(vec![]), Some(&cap), Lang::Zh, true));
    assert_eq!((v.tone, v.state.as_str()), ("warn", "设备 短信存储快满了"));
    assert_eq!(
        v.reason.as_deref(),
        Some("已用 92 / 100。满了以后收不到新短信，删掉一些旧短信。")
    );
    let line: Vec<String> = v.capacity.iter().map(|(k, x)| format!("{k} {x}")).collect();
    assert_eq!(
        line,
        [
            "SIM 已用 4 / 50",
            "设备已用 92 / 100",
            "总已用 96 / 150",
            "未读 2"
        ]
    );
}

#[test]
fn the_status_reads_loading_failed_unread_and_stopped() {
    let hub = hub_with(Some(json!({"unread":2,"max_id":31,"count":6})));
    let v = hub.read(|st| view(st, &Got::default(), None, Lang::Zh, true));
    assert_eq!((v.tone, v.state.as_str()), ("neutral", "正在读取短信…"));
    assert!(matches!(v.list, List::Loading));
    assert_eq!(v.capacity[3].1, "—", "unread only once there is a list");
    let failed = Got {
        err: Some((ReadErr::Unreachable, Instant::now())),
        ..Got::default()
    };
    let v = hub.read(|st| view(st, &failed, None, Lang::Zh, true));
    assert_eq!(
        (v.tone, v.state.as_str(), v.retry),
        ("bad", "读不到短信", true)
    );
    assert_eq!(v.reason.as_deref(), Some("连不上核心服务"));
    assert!(matches!(v.list, List::Failed));
    // unread from the block (live), not the list
    let g = got(vec![m(1, "10086", "x", "1", Some(Store::Device))]);
    let v = hub.read(|st| view(st, &g, None, Lang::Zh, true));
    assert_eq!((v.tone, v.state.as_str()), ("ok", "2 条未读"));
    let none = hub_with(Some(json!({"unread":0,"max_id":31,"count":6})));
    assert_eq!(
        none.read(|st| view(st, &g, None, Lang::En, true)).state,
        "No unread messages"
    );
    // a list, then a failed read: stale, "list stopped at"
    let mut g2 = got(vec![m(1, "10086", "x", "0", Some(Store::Device))]);
    g2.err = Some((ReadErr::NoReply, Instant::now()));
    let v = hub.read(|st| view(st, &g2, None, Lang::Zh, true));
    assert!(v.stale && v.tone == "stale");
    assert!(
        v.stopped.as_deref().unwrap().starts_with("列表停在 "),
        "{:?}",
        v.stopped
    );
    // the page's own render never carries rows
    assert!(matches!(
        hub.read(|st| view(st, &g, None, Lang::Zh, false)).list,
        List::Loading
    ));
}

#[test]
fn rows_say_their_tag_and_store_and_the_filter_needs_every_store() {
    let hub = hub_with(None);
    let long = "字".repeat(300);
    let g = got(vec![
        m(5, "10086", "a\n b", "1", Some(Store::Sim)),
        m(4, "", &long, "2", Some(Store::Device)),
        m(3, "10010", "c", "3", Some(Store::Device)),
        m(2, "10010", "d", "4", Some(Store::Device)),
        m(1, "10010", "e", "0", Some(Store::Device)),
    ]);
    let v = hub.read(|st| view(st, &g, None, Lang::Zh, true));
    let List::Rows(rows) = &v.list else { panic!() };
    let words: Vec<_> = rows.iter().map(|r| r.word.map(|w| w.1)).collect();
    assert_eq!(
        words,
        [
            Some("未读"),
            Some("已发送"),
            Some("发送失败"),
            Some("草稿"),
            None
        ]
    );
    assert_eq!(rows[0].preview, "a b", "one line");
    assert_eq!(rows[1].from, "未知");
    assert_eq!(rows[1].preview.chars().count(), PREVIEW + 1);
    assert_eq!((rows[0].store, rows[0].s), (Some("SIM"), "sim"));
    assert!(v.stores && v.n_sim == 1 && v.n_dev == 4);
    let older = got(vec![m(1, "10086", "x", "0", None)]);
    let v2 = hub.read(|st| view(st, &older, None, Lang::Zh, true));
    assert!(!v2.stores, "an older u60d: no filter");
    assert_ne!(v.rev, v2.rev);
    assert!(v.init().contains("$_st = true") && v2.init().contains("$_st = false"));
}

fn leaves_html(v: &View) -> String {
    Leaves { v }.render().unwrap()
}

#[test]
fn device_text_never_lands_in_an_expression() {
    let hub = hub_with(None);
    let evil = "x'); alert(1); ('";
    let g = got(vec![m(7, evil, evil, "1", Some(Store::Sim))]);
    let v = hub.read(|st| view(st, &g, None, Lang::Zh, true));
    let html = leaves_html(&v);
    for (attr, val) in attrs(&html) {
        if attr.starts_with("data-") {
            assert!(!val.contains("alert"), "{attr}={val}");
        }
    }
    assert!(html.contains("@post('/sms/open?id=7', {retryMaxCount: 0})"));
    let one = g.ok.as_ref().unwrap().0[0].clone();
    let dlg = view_html(&one, Lang::Zh, tag_mark(&one, Lang::Zh));
    for (attr, val) in attrs(&dlg) {
        if attr.starts_with("data-") {
            assert!(!val.contains("alert"), "{attr}={val}");
        }
    }
}

/// (name, value) of every attribute, entity-decoded the way the browser hands it to Datastar.
fn attrs(html: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find("=\"") {
        let name_start = rest[..i].rfind(' ').map_or(0, |s| s + 1);
        let name = rest[name_start..i].to_string();
        let after = &rest[i + 2..];
        let end = after.find('"').unwrap_or(after.len());
        let val = after[..end]
            .replace("&#39;", "'")
            .replace("&#x27;", "'")
            .replace("&quot;", "\"")
            .replace("&#34;", "\"")
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">");
        out.push((name, val));
        rest = &after[end.min(after.len())..];
    }
    out
}

#[test]
fn the_three_tiers() {
    // tier 1: mark read posts once, its area hears a lost reply
    let p = Page {
        shell: crate::shell::Shell::new(&HeaderMap::new(), PATH),
        l: Lang::Zh,
        leaves: vec![],
        read_op: read_area(Lang::Zh, &write::Phase::Idle),
        del_op: del_area(Lang::Zh, "/sms/delete", &write::Phase::Idle),
    };
    assert!(
        p.read_js().starts_with("@post('/sms/read?ids=' + ")
            && p.read_js().ends_with("{retryMaxCount: 0})")
    );
    let page = p.render().unwrap();
    assert!(
        page.contains("data-write=\"sms-read\"") && page.contains("/sms/read/cancel?lost=true")
    );
    let main = &page[page.find("<main").unwrap()..page.find("</main>").unwrap()];
    assert!(!main.contains("<dialog"), "no dialog for tier 1");
    // tier 3: a dialog naming how many and whose; its confirm sends exactly those ids, once
    let list = vec![m(2, "10086", "x", "0", None), m(3, "95588", "y", "1", None)];
    let html = del_area(
        Lang::Zh,
        "/sms/delete?ids=2;3",
        &write::Phase::Ask(del_confirm(Lang::Zh, 2, &senders(&list, &[2, 3], Lang::Zh))),
    );
    assert!(
        html.contains("<dialog") && html.contains("el.showModal()"),
        "{html}"
    );
    assert!(html.contains("删除 2 条短信？"));
    assert!(html.contains("来自 10086、95588 的 2 条短信会被删除，删除后没法恢复。"));
    assert!(
        html.contains("@post(&#39;/sms/delete?ids=2;3&#39;, {retryMaxCount: 0})"),
        "{html}"
    );
    assert!(html.contains("nd-btn--danger") && html.contains("删除 (2)"));
    let many: Vec<Msg> = (1..=5)
        .map(|i| m(i, &format!("1000{i}"), "x", "0", None))
        .collect();
    assert_eq!(
        senders(&many, &[1, 2, 3, 4, 5], Lang::En),
        "10001, 10002, 10003 …"
    );
}

#[test]
fn the_english_page_has_no_chinese() {
    let hub = hub_with(Some(json!({"unread":1,"max_id":31,"count":6})));
    let cap = json!({"sms_sim_total":10,"sms_sim_rev_total":9});
    let mut all = Vec::new();
    for g in [
        Got::default(),
        Got {
            err: Some((ReadErr::Unreachable, Instant::now())),
            ..Got::default()
        },
        got(vec![]),
        got(vec![
            m(5, "10086", "hi", "1", Some(Store::Sim)),
            m(4, "", "x", "3", Some(Store::Device)),
        ]),
    ] {
        let v = hub.read(|st| view(st, &g, Some(&cap), Lang::En, true));
        all.push(leaves_html(&v));
    }
    let msg1 = m(5, "10086", "hi", "1", Some(Store::Sim));
    all.push(view_html(
        &msg1,
        Lang::En,
        mark("neutral", "Marking as read…", false),
    ));
    all.push(del_area(
        Lang::En,
        "/x",
        &write::Phase::Ask(del_confirm(Lang::En, 2, "10086")),
    ));
    for o in [
        write::Outcome::Accepted,
        write::Outcome::Unknown,
        write::Outcome::Busy,
    ] {
        all.push(read_area(Lang::En, &write::Phase::Done(o)));
    }
    all.push(read_area(
        Lang::En,
        &write::Phase::Done(partial(Lang::En, 1, 1).unwrap()),
    ));
    let p = Page {
        shell: crate::shell::Shell::new(&HeaderMap::new(), PATH),
        l: Lang::En,
        leaves: vec![],
        read_op: String::new(),
        del_op: String::new(),
    };
    let page = p.render().unwrap();
    // the page's own text (the frame has the language switch, which says 中文)
    let main = &page[page.find("<main").unwrap()..page.find("</main>").unwrap()];
    all.push(main.to_owned());
    let html = all.join("\n");
    assert_eq!(cjk(&html), Vec::<char>::new(), "{html}");
}

// ---- the writes, against the mock u60d ----

async fn app_on_mock() -> (Shared, String) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, crate::mock::router("", 1.0)).await.unwrap() });
    let app = std::sync::Arc::new(crate::pages::App {
        hub: Hub::new(),
        control_url: format!("{base}/control"),
        u60d_url: base.clone(),
        side_kick: std::sync::Arc::new(tokio::sync::Notify::new()),
        http: reqwest::Client::new(),
        proxy_http: reqwest::Client::new(),
        sessions: crate::auth::Sessions::default(),
        password_file: None,
        pages: vec![PATH.into()],
        kick: tokio::sync::broadcast::channel(16).0,
        logo_dir: std::path::PathBuf::new(),
    });
    (app, base)
}

async fn stats(base: &str) -> Value {
    reqwest::get(format!("{base}/mock/stats"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn post(url: String) {
    reqwest::Client::new().post(url).send().await.unwrap();
}

/// One test for the flows: they share the module's list cache.
#[tokio::test]
async fn mark_read_and_delete_read_back_through_the_list() {
    let (app, base) = app_on_mock().await;
    let list = read_now(&app).await.unwrap();
    assert_eq!(
        list.iter().map(|m| m.id).collect::<Vec<_>>(),
        [31, 30, 29, 28, 27, 26]
    );
    assert_eq!(list[4].number, "CMHK", "hex number decoded");
    assert_eq!(list[1].text, "Hello from the SIM card.\nSecond line.");
    assert_eq!(list[1].store, Some(Store::Sim));

    // mark read: applied once the list shows it
    let o = mark_read(&app, &[31, 30], Lang::Zh).await;
    assert_eq!(o, write::Outcome::Applied("已把 2 条标为已读".into()));
    // u60d takes it but the list never shows it: "设备已接受", never success
    post(format!("{base}/mock/sms?count=0")).await;
    post(format!("{base}/mock/control?noreadback=true")).await;
    let o = mark_read_within(&app, &[31], Lang::Zh, Duration::from_secs(2)).await;
    assert_eq!(o, write::Outcome::Accepted);
    assert!(o.text(Lang::Zh).contains("设备已接受") && !o.text(Lang::Zh).contains("成功"));
    // refused: u60d's words after 没有生效
    post(format!("{base}/mock/control?fail=true")).await;
    let o = mark_read(&app, &[31], Lang::Zh).await;
    assert!(o.text(Lang::Zh).starts_with("没有生效："), "{o:?}");

    // delete NV 29 and SIM 27: the firmware leaves 27, db_delete takes it
    let before = stats(&base).await["sms_ops"].as_array().unwrap().len();
    let o = delete(&app, &[29, 27], Lang::Zh).await;
    assert_eq!(o, write::Outcome::Applied("已删除 2 条短信".into()));
    let s = stats(&base).await;
    let ops: Vec<&str> = s["sms_ops"].as_array().unwrap()[before..]
        .iter()
        .filter_map(Value::as_str)
        .filter(|a| *a != "sms.list_after")
        .collect();
    assert_eq!(
        ops,
        ["sms.delete", "sms.db_delete"],
        "db_delete after, for the survivor only"
    );
    assert_eq!(s["sms_ids"], json!([31, 30, 28, 26]));
    // NV only: no db_delete
    let before = stats(&base).await["sms_ops"].as_array().unwrap().len();
    assert_eq!(
        delete(&app, &[26], Lang::En).await,
        write::Outcome::Applied("Deleted 1 message(s)".into())
    );
    let s = stats(&base).await;
    assert!(
        !s["sms_ops"].as_array().unwrap()[before..]
            .iter()
            .any(|a| a == "sms.db_delete")
    );
    // every write had its own op_id
    let ids: Vec<&str> = s["op_ids"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let mut uniq = ids.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), ids.len());
    assert!(ids.iter().all(|i| i.starts_with("lw-")), "{ids:?}");
}
