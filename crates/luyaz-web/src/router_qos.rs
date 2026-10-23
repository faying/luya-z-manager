//! /router/qos (QoS): which vendor 带宽分配 mode the device is in and
//! whether app recognition (xdpi) is on. Read-only since 2026-09-25 (owner's call,
//! `docs/designs/touch-menu-tabs.md`「不做」): every mode but Nomal (极速) sorts traffic by app,
//! which needs xdpi, and xdpi is off on purpose (about 25 % CPU).
//!
//! | Old page | Old source | Now |
//! |---|---|---|
//! | Mode (状态句 + 行) | agent `/api/router/qos` `smart_qos_mode` | u60d `router.qos.mode` (`""` = unread) |
//! | xdpi on / off | agent `xdpi_support` ("0"/"1") | u60d `router.qos.xdpi` (null = unread) |
//! | "Not available on this firmware" (old agent 503) | agent | u60d `router.qos` null (the read failed) |
//!
//! No agent call: everything is the `router` block on the one upstream connection, so it is live
//! (u60d re-reads every 60 s and right after any write).

use std::time::Duration;

use askama::Template;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

use crate::agent::Polled;
use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, split_leaves};

const PATH: &str = "/router/qos";

/// This page's routes (the page, its stream, its retry).
pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route("/router/qos/events", get(events))
        .route("/router/qos/refresh", post(refresh))
}

/// The vendor's mode names (`zwrt_smart_mng.smart_qos.mode`; "Nomal" is the firmware's spelling).
pub fn mode_name(m: &str, l: Lang) -> Option<&'static str> {
    Some(match m {
        "Nomal_mode" => l.pick("Fastest (no per-app sorting)", "极速（不区分应用）"),
        "Ai_mode" => l.pick("AI", "AI 智能"),
        "Live_mode" => l.pick("Live streaming", "直播"),
        "Chat_mode" => l.pick("Chat", "聊天"),
        "Game_mode" => l.pick("Gaming", "游戏"),
        "Video_mode" => l.pick("Video", "视频"),
        _ => return None,
    })
}

/// A row's value.
#[derive(Debug, PartialEq)]
pub enum Val {
    /// Nothing read yet (skeleton).
    Loading,
    Empty,
    /// Our words.
    Text(&'static str),
    /// The firmware's own text (an unknown mode name): not translated.
    Raw(String),
}

pub struct View {
    pub l: Lang,
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub meta: Option<&'static str>,
    pub retry: bool,
    pub mode: Val,
    pub xdpi: Val,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "warn" | "stale" => "▲",
            "bad" => "■",
            _ => "●",
        }
    }
}

pub fn view(st: &St, l: Lang) -> View {
    let b = st.blocks.get("router");
    let waiting = b.is_none() && !matches!(st.link, Link::Live | Link::Down);
    let down = st.link == Link::Down || (b.is_none() && st.link == Link::Live);
    let stale = b.is_some_and(|b| b.stale) || (b.is_some() && st.link != Link::Live);
    let empty = |w: bool| if w { Val::Loading } else { Val::Empty };
    let qos = b.map(|b| &b.data["qos"]).filter(|q| q.is_object());

    let Some(q) = qos else {
        let (tone, state, reason) = if waiting {
            ("neutral", l.pick("Reading QoS…", "正在读取 QoS…"), None)
        } else if b.is_none() || down {
            (
                "bad",
                l.pick("Can't read QoS", "读不到 QoS"),
                Some(l.pick(
                    "The device's data service isn't answering.",
                    "设备的数据服务没有回应。",
                )),
            )
        } else {
            (
                "warn",
                l.pick("Bandwidth mode unknown", "读不到带宽分配模式"),
                Some(l.pick(
                    "The firmware didn't answer this read; the device tries again within a minute.",
                    "固件没有回答这次读取；设备一分钟内会再读一次。",
                )),
            )
        };
        return View {
            l,
            tone,
            state: state.into(),
            reason: reason.map(Into::into),
            meta: None,
            retry: down,
            mode: empty(waiting),
            xdpi: empty(waiting),
        };
    };

    let m = q["mode"].as_str().unwrap_or_default();
    let mode = match (m, mode_name(m, l)) {
        ("", _) => Val::Empty,
        (_, Some(n)) => Val::Text(n),
        (m, None) => Val::Raw(m.to_owned()),
    };
    let xdpi = q["xdpi"].as_bool();
    let state = match &mode {
        Val::Text(n) => match l {
            Lang::En => format!("Bandwidth mode: {n}"),
            Lang::Zh => format!("带宽分配：{n}"),
        },
        Val::Raw(n) => match l {
            Lang::En => format!("Bandwidth mode: {n}"),
            Lang::Zh => format!("带宽分配：{n}"),
        },
        _ => l
            .pick("Bandwidth mode unknown", "读不到带宽分配模式")
            .into(),
    };
    let why = if xdpi == Some(true) {
        l.pick(
            "App recognition (xdpi) is on right now, although it is normally kept off to save about a quarter of the CPU. This page only shows the mode and doesn't change it.",
            "应用识别（xdpi）现在是开着的（平时为省约四分之一的 CPU 关着）。这里只显示模式，不改。",
        )
    } else {
        l.pick(
            "The other vendor modes (AI, live, chat, gaming, video) decide by recognising apps, which needs xdpi. xdpi is off to save about a quarter of the CPU, so this page only shows the mode and doesn't change it.",
            "原厂的其他模式（AI、直播、聊天、游戏、视频）要先识别是哪个应用（xdpi），为省约四分之一的 CPU 已经关掉，所以这里只显示、不改。",
        )
    };
    View {
        l,
        tone: if stale {
            "stale"
        } else if m.is_empty() {
            "warn"
        } else {
            "ok"
        },
        state,
        reason: Some(why.into()),
        meta: stale.then(|| {
            l.pick(
                "Not updating · showing the last reading",
                "没有在更新 · 显示的是上一次的读数",
            )
        }),
        retry: down,
        mode,
        xdpi: match xdpi {
            Some(true) => Val::Text(l.pick("On", "已开")),
            Some(false) => Val::Text(l.pick("Off", "已关")),
            None => Val::Empty,
        },
    }
}

// ---- templates and the stream ----

#[derive(Template)]
#[template(path = "router_qos_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

pub struct Poll {
    l: Lang,
}

impl Poll {
    fn leaves(&self, app: &Shared) -> Vec<(String, String)> {
        app.hub.read(|st| {
            let v = view(st, self.l);
            split_leaves(&Leaves { v: &v }.render().unwrap())
        })
    }
}

impl Polled for Poll {
    /// Nothing to poll: the page follows the `router` block.
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        (self.leaves(app), Duration::from_secs(24 * 3600))
    }
    fn watches(&self, block: &str) -> bool {
        block == "router"
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(self.leaves(app))
    }
}

#[derive(Template)]
#[template(path = "router_qos.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll { l: ctx.lang };
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(
            st,
            &ctx,
            Some(format!("{PATH}/events?g={}", st.generation)),
        )
    });
    let html = Page {
        shell,
        l: ctx.lang,
        leaves: p.leaves(&app),
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

pub async fn events(
    State(app): State<Shared>,
    Query(q): Query<StreamQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll { l: ctx.lang };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// Retry: reconnect to u60d now if it is down.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    app.hub.retry();
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::Hub;
    use crate::sse::Frame;
    use serde_json::{Value, json};

    fn hub(router: Option<Value>, stale: bool) -> std::sync::Arc<Hub> {
        let hub = Hub::new();
        let mut blocks = serde_json::Map::new();
        if let Some(r) = router {
            blocks.insert(
                "router".into(),
                json!({"revision":1,"observed_at":1,"stale":stale,"data":r}),
            );
        }
        hub.apply(&Frame {
            event: "snapshot".into(),
            data: json!({"epoch":"e","blocks":blocks}).to_string(),
        });
        hub.set_link_for_test(Link::Live);
        hub
    }

    fn v(router: Option<Value>, stale: bool, l: Lang) -> View {
        hub(router, stale).read(|st| view(st, l))
    }

    #[test]
    fn mode_and_xdpi_like_the_old_page() {
        let r = json!({"qos":{"mode":"Nomal_mode","xdpi":false},"upnp":null});
        let x = v(Some(r), false, Lang::Zh);
        assert_eq!(
            (x.tone, x.state.as_str()),
            ("ok", "带宽分配：极速（不区分应用）")
        );
        assert_eq!(x.mode, Val::Text("极速（不区分应用）"));
        assert_eq!(x.xdpi, Val::Text("已关"));
        assert!(x.reason.unwrap().contains("xdpi") && !x.retry && x.meta.is_none());
        let x = v(
            Some(json!({"qos":{"mode":"Game_mode","xdpi":true}})),
            false,
            Lang::En,
        );
        assert_eq!(x.state, "Bandwidth mode: Gaming");
        assert_eq!(x.xdpi, Val::Text("On"));
        assert!(
            x.reason
                .unwrap()
                .starts_with("App recognition (xdpi) is on")
        );
    }

    #[test]
    fn unread_parts_say_dash_not_off() {
        let x = v(
            Some(json!({"qos":{"mode":"","xdpi":null}})),
            false,
            Lang::En,
        );
        assert_eq!(
            (x.tone, x.state.as_str()),
            ("warn", "Bandwidth mode unknown")
        );
        assert_eq!((x.mode, x.xdpi), (Val::Empty, Val::Empty));
        // the vendor read failed: router.qos null
        let x = v(Some(json!({"qos":null})), false, Lang::Zh);
        assert_eq!((x.tone, x.state.as_str()), ("warn", "读不到带宽分配模式"));
        assert!(!x.retry && x.xdpi == Val::Empty);
        // a mode this page doesn't know: the firmware's word, as is
        let x = v(
            Some(json!({"qos":{"mode":"New_mode","xdpi":false}})),
            false,
            Lang::En,
        );
        assert_eq!(x.mode, Val::Raw("New_mode".into()));
        assert!(
            Leaves { v: &x }
                .render()
                .unwrap()
                .contains("data-data>New_mode<")
        );
    }

    #[test]
    fn stale_missing_and_waiting() {
        let x = v(
            Some(json!({"qos":{"mode":"Nomal_mode","xdpi":false}})),
            true,
            Lang::En,
        );
        assert_eq!(x.tone, "stale");
        assert_eq!(x.meta, Some("Not updating · showing the last reading"));
        assert!(Leaves { v: &x }.render().unwrap().contains("nd-stale"));
        // connected, no router block: u60d doesn't have it
        let x = v(None, false, Lang::Zh);
        assert_eq!((x.tone, x.state.as_str()), ("bad", "读不到 QoS"));
        assert!(x.retry);
        assert!(
            Leaves { v: &x }
                .render()
                .unwrap()
                .contains("/router/qos/refresh")
        );
        // still connecting: skeletons
        let hub = Hub::new();
        let x = hub.read(|st| view(st, Lang::En));
        assert_eq!((x.tone, x.mode), ("neutral", Val::Loading));
    }

    #[test]
    fn the_english_page_has_no_chinese() {
        for (r, s) in [
            (
                Some(json!({"qos":{"mode":"Nomal_mode","xdpi":false}})),
                false,
            ),
            (Some(json!({"qos":{"mode":"Ai_mode","xdpi":true}})), true),
            (Some(json!({"qos":null})), false),
            (None, false),
        ] {
            let html = Leaves {
                v: &v(r, s, Lang::En),
            }
            .render()
            .unwrap();
            assert!(
                !html.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)
                    || ('\u{3000}'..='\u{303f}').contains(&c)
                    || ('\u{ff00}'..='\u{ffef}').contains(&c)),
                "{html}"
            );
        }
    }
}
