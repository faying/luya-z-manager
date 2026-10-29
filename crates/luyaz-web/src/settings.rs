//! /settings (设置): settings of this browser only, so nothing is
//! read from the device and nothing is written to it (tier 1, no confirm).
//!
//! | old control | now |
//! |---|---|
//! | Appearance (system / light / dark) | three buttons; a Datastar expression writes localStorage `u60.theme` (auto = key removed) and sets `<html data-theme>` (base.html applies it on load) |
//! | Language | two submit buttons of a form: `POST /lang` (cookie), back to this page |
//! | Log out; Clear stored token | one button: `POST /logout` (the old two did the same thing) |
//! | Agent URL, Save, Test connectivity, Clear stored agent URL | dropped: same origin, no address to set |
//! | Polling interval, Save preferences | dropped: nothing ever read the value |
//!
//! One device section (tailnet-poller.md §5): the core's allow list of `power.tailnet_poller`
//! (`GET /api/poller`), each entry with "make permanent" and "delete" (`POST /settings/poller`,
//! a form: `/control` `poller.allow {forever}` / `poller.unallow`, then back here with the result).

use askama::Template;
use axum::Router;
use axum::extract::{Form, FromRequest, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::Caller;
use crate::i18n::Lang;
use crate::pages::Shared;

const PATH: &str = "/settings";

pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/settings/poller", post(poller_set))
}

/// One allow-list entry as the table shows it.
struct Allow {
    peer: String,
    port: u64,
    label: String,
    /// "MM-DD HH:mm" (device clock), None = for good.
    until: Option<String>,
}

fn allow_rows(v: &Value) -> Vec<Allow> {
    v["allow"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .map(|a| Allow {
            peer: a["peer"].as_str().unwrap_or("").into(),
            port: a["port"].as_u64().unwrap_or(0),
            label: a["label"].as_str().unwrap_or("").into(),
            until: a["until"].as_i64().map(crate::alerts_page::mdhm),
        })
        .collect()
}

#[derive(Template)]
#[template(path = "settings.html")]
struct Page {
    shell: crate::shell::Shell,
    /// None = the core did not answer.
    allow: Option<Vec<Allow>>,
    /// The last poller write's result (`?pm=`).
    note: Option<(&'static str, String)>,
    /// A plugin with settings is installed: its title (None before the core has read it).
    plugin: Option<Option<String>>,
}

#[derive(Deserialize, Default)]
struct PageQuery {
    #[serde(default)]
    pm: String,
}

fn note_of(l: Lang, pm: &str) -> Option<(&'static str, String)> {
    match pm {
        "forever" => Some(("ok", l.pick("Now allowed for good", "已改成永久").into())),
        "removed" => Some(("ok", l.pick("Removed", "已删除").into())),
        "" => None,
        // the write's own words (write::Outcome::text: "没有生效：…", "设备已接受，还没读回" …)
        e => Some(("bad", e.to_string())),
    }
}

async fn page(
    State(app): State<Shared>,
    Query(q): Query<PageQuery>,
    req: axum::extract::Request,
) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let who = Caller::of(&app, &req);
    let (allow, plugin) = tokio::join!(
        crate::agent::get(&app, &who, crate::home::poller::STATUS),
        crate::plugin_settings::entry(&app, std::time::Duration::from_millis(3500)),
    );
    let allow = allow.ok().map(|v| allow_rows(&v));
    let shell = app
        .hub
        .read(|st| crate::shell::Shell::new(&headers, PATH).live(st, &ctx, None));
    let html = Page {
        shell,
        allow,
        note: note_of(ctx.lang, &q.pm),
        plugin,
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(
        &headers,
        "text/html; charset=utf-8",
        html.into_bytes(),
        "no-store",
    )
}

#[derive(Deserialize)]
struct PollerForm {
    op: String,
    peer: String,
    port: u16,
    #[serde(default)]
    label: String,
}

/// POST /settings/poller: `op=forever` (the entry for good) or `op=remove`; back to the page.
async fn poller_set(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> Response {
    let l = crate::topbar::Ctx::of_request(&req, PATH).lang;
    let who = Caller::of(&app, &req);
    let Ok(Form(f)) = Form::<PollerForm>::from_request(req, &app).await else {
        return Redirect::to(&format!("{PATH}?pm=bad+form")).into_response();
    };
    let (action, params, done) = match f.op.as_str() {
        "forever" => (
            "poller.allow",
            json!({"peer": f.peer, "port": f.port, "label": f.label, "forever": true}),
            "forever",
        ),
        _ => (
            "poller.unallow",
            json!({"peer": f.peer, "port": f.port}),
            "removed",
        ),
    };
    let sent = crate::home::poller::send(&app, l, action, params).await;
    // read back: the entry for good, or gone
    let v = crate::agent::get(&app, &who, crate::home::poller::STATUS).await.ok();
    let held = v.as_ref().is_some_and(|v| {
        let e = crate::home::poller::allowed_in(v, &f.peer, f.port.into());
        match done {
            "forever" => e.is_some_and(|e| e["until"].is_null()),
            _ => e.is_none(),
        }
    });
    let pm = match crate::home::poller::outcome(sent, held, done) {
        crate::write::Outcome::Applied(_) => done.to_string(),
        o => o.text(l),
    };
    let enc: String = pm
        .bytes()
        .map(|b| match b {
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    Redirect::to(&format!("{PATH}?pm={enc}#st-pl-t")).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Shell;
    use axum::http::HeaderMap;

    fn render(lang: &str) -> String {
        let mut h = HeaderMap::new();
        h.insert("cookie", format!("u60_lang={lang}").parse().unwrap());
        Page {
            shell: Shell::new(&h, PATH),
            allow: Some(Vec::new()),
            note: None,
            plugin: None,
        }
        .render()
        .unwrap()
    }

    #[test]
    fn allow_list_rows_with_their_two_buttons() {
        let v = serde_json::json!({"allow": [
            {"peer": "192.168.50.88", "port": 80, "label": "家里", "until": 1_790_000_000},
            {"peer": "100.101.1.2", "port": 9090, "label": "", "until": null},
        ]});
        let mut h = HeaderMap::new();
        h.insert("cookie", "u60_lang=zh".parse().unwrap());
        let html = Page {
            shell: Shell::new(&h, PATH),
            allow: Some(allow_rows(&v)),
            note: note_of(Lang::Zh, "forever"),
            plugin: Some(Some("<i>x</i>".into())),
        }
        .render()
        .unwrap();
        assert!(html.contains("192.168.50.88") && html.contains(":80") && html.contains("家里"));
        assert!(html.contains("到 ") && html.contains("永久"));
        assert!(html.contains(r#"action="/settings/poller""#));
        assert!(html.contains(r#"name="op" value="forever""#) && html.contains(r#"name="op" value="remove""#));
        assert!(html.contains("已改成永久"));
        // the plugin entry: its title as text, linking to the plugin page
        assert!(html.contains(r#"href="/settings/plugin""#) && html.contains("&#60;i&#62;x&#60;/i&#62;"));
        // an empty list and an unreadable one say so
        let empty = Page { shell: Shell::new(&h, PATH), allow: Some(Vec::new()), note: None, plugin: None }.render().unwrap();
        assert!(empty.contains("还没有放行的设备"));
        let none = Page { shell: Shell::new(&h, PATH), allow: None, note: None, plugin: None }.render().unwrap();
        assert!(none.contains("读不到"));
        assert!(!none.contains("/settings/plugin"), "no plugin, no entry");
    }

    #[test]
    fn theme_and_language_and_logout() {
        let h = render("en");
        assert!(h.contains("localStorage.setItem('u60.theme','dark')"));
        assert!(h.contains("localStorage.removeItem('u60.theme')"));
        assert!(h.contains(r#"action="/logout""#));
        assert!(h.contains(r#"name="back" value="/settings""#));
        // the old agent-address controls are gone
        assert!(!h.contains("Agent URL") && !h.contains("Polling"));
    }

    #[test]
    fn current_language_is_the_checked_one() {
        let zh = render("zh");
        assert!(zh.contains(r#"value="zh" role="radio" class="nd-seg__item" aria-checked="true""#));
        let en = render("en");
        assert!(en.contains(r#"value="en" role="radio" class="nd-seg__item" aria-checked="true""#));
    }
}
