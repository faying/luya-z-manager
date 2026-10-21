//! The home page's tailnet-poller banner (docs/designs/tailnet-poller.md §4): the core's
//! `power.tailnet_poller` event, newest one not acknowledged and not allowed.
//!
//! ```text
//!  poll  GET /api/poller (u60d, 30 s; a write asks again at once)
//!  leaf  hm-poller (hidden when there is nothing to say)
//!  write POST /home/poller/allow?peer=&port=  ─▶ /control poller.allow (24 h) ─┐ tier 1, no confirm;
//!        POST /home/poller/ack?id=            ─▶ /control poller.ack ────────┴ the banner again,
//!                                                                              with the result line
//! ```
//! The allow list itself (make permanent, delete) is on /settings (settings.rs).

use std::time::Instant;

use axum::Router;
use axum::extract::{Query, Request, State};
use axum::response::IntoResponse;
use axum::routing::post;
use serde::Deserialize;
use serde_json::{Value, json};

use super::Src;
use crate::agent::{self, Caller, esc};
use crate::i18n::Lang;
use crate::pages::Shared;
use crate::write;

pub(crate) const STATUS: &str = "/api/poller";

pub(super) fn routes(r: Router<Shared>) -> Router<Shared> {
    r.route("/home/poller/allow", post(allow))
        .route("/home/poller/ack", post(ack))
}

/// The banner's empty leaf and, outside it, the write's result line: only a write's own reply
/// patches `#hm-poller-op`, so a push (the banner hiding after "是我自己的") never wipes it.
pub(super) fn frame() -> String {
    format!("{}{}", hidden(), op_html(Lang::Zh, None))
}

fn hidden() -> String {
    "<section id=\"hm-poller\" hidden></section>".into()
}

/// `#hm-poller-op`: a write's result (`None`: empty).
pub(crate) fn op_html(l: Lang, op: Option<&write::Outcome>) -> String {
    match op {
        Some(o) => format!(
            "<p id=\"hm-poller-op\" class=\"hm-o1 hm-poller__op t-note t-note--{}\" role=\"status\">{}</p>",
            o.tone(),
            esc(&o.text(l))
        ),
        None => "<p id=\"hm-poller-op\" class=\"hm-o1 hm-poller__op t-note\" role=\"status\"></p>"
            .into(),
    }
}

/// The event the banner is about: active ones first, then the newest.
fn pick(v: &Value) -> Option<&Value> {
    let evs = v["events"].as_array()?;
    let open = |e: &&Value| e["acked"] != true && e["allowed"] != true;
    evs.iter()
        .filter(open)
        .filter(|e| e["state"] == "active")
        .max_by_key(|e| e["since"].as_i64())
        .or_else(|| evs.iter().filter(open).max_by_key(|e| e["since"].as_i64()))
}

/// The banner for the core's `/api/poller` reply (`None`: not read yet).
pub(crate) fn banner(l: Lang, v: Option<&Value>) -> String {
    let Some(e) = v.and_then(pick) else {
        return hidden();
    };
    let peer = e["peer"].as_str().unwrap_or("?");
    let port = e["port"].as_u64().unwrap_or(0);
    let id = e["id"].as_str().unwrap_or("");
    let minutes = e["minutes"].as_u64().unwrap_or(0);
    let ppm = e["pkts_per_min"].as_u64().unwrap_or(0);
    let gw = e["via_gateway"] == true;
    let ended = e["state"] == "ended";
    let who = match (e["peer_name"].as_str(), gw, l) {
        (Some(n), _, _) => format!("{n}（{peer}）"),
        (None, true, Lang::Zh) => format!("{peer}（经家里网关转发，具体是哪台设备要在家里那边查）"),
        (None, true, Lang::En) => format!(
            "{peer} (forwarded by a gateway; which device is behind it has to be looked up there)"
        ),
        (None, false, _) => peer.to_string(),
    };
    let title = match (ended, l) {
        (false, Lang::Zh) => "有设备在经 Tailscale 持续访问管理接口",
        (true, Lang::Zh) => "有设备经 Tailscale 持续访问过管理接口（已停）",
        (false, Lang::En) => "A device keeps polling the admin ports over Tailscale",
        (true, Lang::En) => "A device kept polling the admin ports over Tailscale (stopped)",
    };
    let line = match l {
        Lang::Zh => format!(
            "来自 {who}，:{port}，{}{minutes} 分钟（约 {ppm} 包/分钟）。息屏约多耗 0.3–0.6 W。",
            if ended { "持续了 " } else { "已 " }
        ),
        Lang::En => format!(
            "From {who}, :{port}, {minutes} min{} (about {ppm} packets a minute). About 0.3–0.6 W more with the screen off.",
            if ended { " in all" } else { " so far" }
        ),
    };
    let note = match (gw, l) {
        (true, Lang::Zh) => {
            "「是我自己的」会把这个地址到这个端口放行 24 小时：它是网关，等于放行网关后面的整个网段。设置页可以改成永久或删除。"
        }
        (false, Lang::Zh) => {
            "「是我自己的」会把这个地址到这个端口放行 24 小时，设置页可以改成永久或删除。"
        }
        (true, Lang::En) => {
            "\u{201c}It's mine\u{201d} allows this address on this port for 24 hours. It is a gateway, so everything behind it is allowed too. Make it permanent or delete it in Settings."
        }
        (false, Lang::En) => {
            "\u{201c}It's mine\u{201d} allows this address on this port for 24 hours. Make it permanent or delete it in Settings."
        }
    };
    let q = format!("peer={}&port={port}", url_enc(peer));
    format!(
        "<section id=\"hm-poller\" class=\"nd-group hm-o1 hm-poller\" role=\"status\" aria-labelledby=\"hm-poller-t\"><div class=\"hm-poller__body\">\
<p id=\"hm-poller-t\" class=\"nd-row__label\"><span class=\"nd-mark nd-mark--warn\"><span class=\"nd-mark__sym\" aria-hidden=\"true\">▲</span><span>{}</span></span></p>\
<p class=\"nd-body\" data-data>{}</p><p class=\"nd-aux\">{}</p>\
<div class=\"hm-poller__acts\"><button type=\"button\" id=\"hm-poller-allow\" class=\"nd-btn nd-btn--secondary nd-btn--sm\" data-on:click=\"{}\">{}</button>\
<button type=\"button\" id=\"hm-poller-ack\" class=\"nd-btn nd-btn--ghost nd-btn--sm\" data-on:click=\"{}\">{}</button></div></div></section>",
        esc(title),
        esc(&line),
        esc(note),
        write::post_once(&format!("/home/poller/allow?{q}")),
        l.pick("It's mine", "是我自己的"),
        write::post_once(&format!("/home/poller/ack?id={}", url_enc(id))),
        l.pick("Got it", "知道了"),
    )
}

fn url_enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'.' | b'-' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub(super) struct Poller {
    src: Src<Value>,
}

impl Poller {
    pub(super) fn new() -> Poller {
        Poller { src: Src::new(30) }
    }

    pub(super) fn next(&self) -> Instant {
        self.src.next()
    }

    pub(super) async fn poll(&mut self, app: &Shared, who: &Caller, now: Instant, force: bool) {
        if !force && !self.src.due(now) {
            return;
        }
        let r = agent::get(app, who, STATUS).await.ok();
        self.src.set(Instant::now(), r);
    }

    pub(super) fn leaves(&self, l: Lang) -> Vec<(String, String)> {
        vec![("hm-poller".into(), banner(l, self.src.val.as_ref()))]
    }
}

/// One `/control` write of the poller (tier 1: sent once, the core's reply is the readback).
/// Send one poller `/control` write once; `Ok` = the core took it (not yet read back).
pub(crate) async fn send(
    app: &Shared,
    l: Lang,
    action: &str,
    params: Value,
) -> Result<write::Sent, write::Outcome> {
    let body = json!({
        "action": action,
        "params": params,
        "source": "web",
        "op_id": write::next_op_id(),
    });
    write::send_body(&app.http, &app.control_url, &body, None, l).await
}

/// After [`send`]: the fresh `/api/poller` read decides. Read back = `applied`; taken but not seen
/// in the read = accepted (or unknown when there was no reply), never "成功".
pub(crate) fn outcome(
    sent: Result<write::Sent, write::Outcome>,
    held: bool,
    applied: &str,
) -> write::Outcome {
    match sent {
        Err(o) => o,
        Ok(_) if held => write::Outcome::Applied(applied.into()),
        Ok(s) => s.not_read(),
    }
}

pub(crate) fn allowed_in<'a>(v: &'a Value, peer: &str, port: u64) -> Option<&'a Value> {
    v["allow"]
        .as_array()?
        .iter()
        .find(|a| a["peer"] == peer && a["port"].as_u64() == Some(port))
}

#[derive(Deserialize)]
struct AllowQuery {
    peer: String,
    port: u16,
}

#[derive(Deserialize)]
struct AckQuery {
    id: String,
}

/// The banner (fresh read) and the result line, after a write.
async fn reply(
    app: &Shared,
    l: Lang,
    v: Option<Value>,
    o: write::Outcome,
) -> axum::response::Response {
    let _ = app.kick.send("/");
    let html = format!("{}{}", banner(l, v.as_ref()), op_html(l, Some(&o)));
    axum::response::sse::Sse::new(futures_util::stream::once(async move {
        Ok::<_, std::convert::Infallible>(crate::pages::patch(&html))
    }))
    .into_response()
}

async fn allow(
    State(app): State<Shared>,
    Query(q): Query<AllowQuery>,
    req: Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, "/").lang;
    let who = Caller::of(&app, &req);
    let sent = send(
        &app,
        l,
        "poller.allow",
        json!({"peer": q.peer, "port": q.port}),
    )
    .await;
    let v = agent::get(&app, &who, STATUS).await.ok();
    let held = v
        .as_ref()
        .is_some_and(|v| allowed_in(v, &q.peer, q.port.into()).is_some());
    let o = outcome(sent, held, l.pick("Allowed for 24 hours", "已放行 24 小时"));
    reply(&app, l, v, o).await
}

async fn ack(
    State(app): State<Shared>,
    Query(q): Query<AckQuery>,
    req: Request,
) -> impl IntoResponse {
    let l = crate::topbar::Ctx::of_request(&req, "/").lang;
    let who = Caller::of(&app, &req);
    let sent = send(&app, l, "poller.ack", json!({"id": q.id})).await;
    let v = agent::get(&app, &who, STATUS).await.ok();
    let held = v
        .as_ref()
        .and_then(|v| v["events"].as_array())
        .is_some_and(|evs| {
            evs.iter()
                .any(|e| e["id"] == q.id.as_str() && e["acked"] == true)
        });
    if held {
        read_alert(&app, &who).await;
    }
    let o = outcome(sent, held, l.pick("Dismissed", "已收起"));
    reply(&app, l, v, o).await
}

/// The alert queue's read cursor up to the newest unread `tailnet-poller` alert, so 「知道了」
/// dismisses the alert bar's copy too. Only when every unread alert up to it is a poller one
/// (the cursor marks everything below it read).
pub(crate) fn alert_seq(alerts: &Value) -> Option<u64> {
    let evs = alerts["events"].as_array()?;
    let unread = |e: &&Value| e["unread"] == true;
    let s = evs
        .iter()
        .filter(unread)
        .filter(|e| e["kind"] == "tailnet-poller")
        .filter_map(|e| e["seq"].as_u64())
        .max()?;
    evs.iter()
        .filter(unread)
        .filter(|e| e["seq"].as_u64().is_some_and(|q| q <= s))
        .all(|e| e["kind"] == "tailnet-poller")
        .then_some(s)
}

async fn read_alert(app: &Shared, who: &Caller) {
    let Ok(a) = agent::get(app, who, "/api/alerts").await else {
        return;
    };
    if let Some(seq) = alert_seq(&a) {
        let _ = agent::post_raw(app, who, "/api/alerts/read", &json!({"seq": seq})).await;
        app.side_kick.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(state: &str, acked: bool, since: i64) -> Value {
        json!({"id": format!("0123abcd-{since}"), "peer": "192.168.50.88", "port": 80, "since": since,
               "minutes": 12, "pkts_per_min": 550, "via_gateway": true, "state": state,
               "acked": acked, "allowed": false, "peer_name": null})
    }

    #[test]
    fn got_it_reads_the_poller_alert_only_when_nothing_else_is_below() {
        let e = |seq: u64, kind: &str, unread: bool| json!({"seq": seq, "kind": kind, "unread": unread});
        let a = json!({"events": [e(5, "tailnet-poller", true), e(4, "tailnet-poller", true), e(3, "core-crash", false)]});
        assert_eq!(alert_seq(&a), Some(5));
        let a = json!({"events": [e(6, "core-crash", true), e(5, "tailnet-poller", true)]});
        assert_eq!(alert_seq(&a), Some(5), "a newer other alert stays unread");
        let a = json!({"events": [e(5, "tailnet-poller", true), e(4, "core-crash", true)]});
        assert_eq!(
            alert_seq(&a),
            None,
            "an older unread one would be marked too"
        );
        assert_eq!(
            alert_seq(&json!({"events": [e(5, "tailnet-poller", false)]})),
            None
        );
    }

    #[test]
    fn banner_says_who_where_how_long_and_the_cost() {
        let v = json!({"events": [ev("active", false, 179_000_000_000)]});
        let h = banner(Lang::Zh, Some(&v));
        assert!(h.contains("有设备在经 Tailscale 持续访问管理接口"));
        assert!(h.contains("来自 192.168.50.88（经家里网关转发"));
        assert!(h.contains(":80，已 12 分钟（约 550 包/分钟）"));
        assert!(h.contains("0.3–0.6 W"));
        assert!(h.contains("/home/poller/allow?peer=192.168.50.88&port=80"));
        assert!(h.contains("/home/poller/ack?id=0123abcd-179000000000"));
        assert!(
            h.contains("整个网段"),
            "the gateway caveat beside the button"
        );
        assert!(!h.contains(" hidden"));
        let en = banner(Lang::En, Some(&v));
        assert!(en.contains("It's mine") && en.contains("Got it") && !en.contains("知道了"));
    }

    #[test]
    fn nothing_to_say_is_hidden() {
        assert!(banner(Lang::Zh, None).contains("hidden"));
        let v = json!({"events": [ev("active", true, 1), {"state": "active", "allowed": true, "acked": false}]});
        assert!(banner(Lang::Zh, Some(&v)).contains("hidden"));
        assert!(banner(Lang::Zh, Some(&json!({"events": []}))).contains("hidden"));
    }

    #[test]
    fn active_before_ended_and_a_result_line_after_a_write() {
        let v = json!({"events": [ev("ended", false, 5), ev("active", false, 3)]});
        let h = banner(Lang::Zh, Some(&v));
        assert!(h.contains("ack?id=0123abcd-3") && h.contains("已 12 分钟"));
        let v = json!({"events": [ev("ended", false, 5)]});
        assert!(banner(Lang::Zh, Some(&v)).contains("（已停）"));
        // the result line is its own element, outside the pushed leaf
        let done = op_html(
            Lang::Zh,
            Some(&write::Outcome::Applied("已放行 24 小时".into())),
        );
        assert!(done.contains("id=\"hm-poller-op\"") && done.contains("已放行 24 小时"));
        assert!(!banner(Lang::Zh, Some(&json!({"events": []}))).contains("hm-poller-op"));
        assert!(frame().contains("id=\"hm-poller\"") && frame().contains("id=\"hm-poller-op\""));
        // taken but not read back is never "applied"
        let ok = Ok(write::Sent::Ok);
        assert!(matches!(outcome(ok, false, "x"), write::Outcome::Accepted));
        assert!(matches!(
            outcome(Ok(write::Sent::Ok), true, "x"),
            write::Outcome::Applied(_)
        ));
        assert!(matches!(
            outcome(Ok(write::Sent::NoReply), false, "x"),
            write::Outcome::Unknown
        ));
    }
}
