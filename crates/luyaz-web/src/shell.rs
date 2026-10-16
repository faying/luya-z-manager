//! The page frame every page extends (DESIGN.md §5 导航): <640 top bar + floating 4-tab bar,
//! 640–1023 icon rail, ≥1024 sidebar with the groups; ⌘K search over the page table; language
//! switch. Also the three hub pages (/charts, /functions, /system), which are just the table.

use askama::Template;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use crate::i18n::Lang;
use crate::routes::{Anchor, ROUTES, anchor_for, family_for, route_for};

/// `<svg><use …></svg>` for a sprite icon (`assets` builds the sprite from `icons.rs`).
pub fn icon(name: &str, fill: bool, size: u32, class: &str) -> String {
    let cls = if class.is_empty() {
        String::new()
    } else {
        format!(" class=\"{class}\"")
    };
    format!(
        "<svg{cls} width=\"{size}\" height=\"{size}\" aria-hidden=\"true\"><use href=\"{}#{name}{}\"/></svg>",
        crate::assets::url("icons.svg"),
        if fill { "-fill" } else { "" }
    )
}

pub struct NavItem {
    pub href: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
    pub current: bool,
}

impl NavItem {
    pub fn svg(&self, size: u32) -> String {
        icon(self.icon, self.current, size, "")
    }
    /// Sidebar rows: bold even when current (only anchors have fill symbols).
    pub fn side_svg(&self) -> String {
        icon(
            self.icon,
            self.current
                && crate::routes::Anchor::ALL
                    .iter()
                    .any(|a| a.icon() == self.icon),
            20,
            "",
        )
    }
}

pub struct NavGroup {
    pub title: &'static str,
    pub items: Vec<NavItem>,
}

pub struct NavSection {
    pub anchor: NavItem,
    pub groups: Vec<NavGroup>,
}

/// One ⌘K entry: what it shows and the folded text it matches.
pub struct SearchItem {
    pub href: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub icon: &'static str,
    pub keys: String,
}

/// What the frame needs about the current request.
pub struct Shell {
    pub lang: Lang,
    pub path: String,
    /// Top-bar status icons and the alert bar as rendered now (topbar.rs); empty until `live`.
    pub status: String,
    pub banner: String,
    /// The page's one push stream (`data-init`); a page with live parts of its own passes its
    /// own, which also carries the frame.
    pub stream: String,
}

impl Shell {
    pub fn new(headers: &HeaderMap, path: &str) -> Shell {
        Shell {
            lang: Lang::from_headers(headers),
            path: path.to_string(),
            status: String::new(),
            banner: String::new(),
            stream: String::new(),
        }
    }

    /// The frame's live parts for this request, read at the hub's state `st`; `stream` = None
    /// means the frame-only stream at that state's generation.
    pub fn live(
        mut self,
        st: &crate::hub::St,
        ctx: &crate::topbar::Ctx,
        stream: Option<String>,
    ) -> Shell {
        let (status, banner) = crate::topbar::initial(st, ctx);
        self.status = status;
        self.banner = banner;
        self.stream =
            stream.unwrap_or_else(|| format!("/shell/events?g={}&p={}", st.generation, self.path));
        self
    }
    pub fn t(&self, en: &'static str, zh: &'static str) -> &'static str {
        self.lang.pick(en, zh)
    }
    pub fn family(&self) -> &'static str {
        family_for(&self.path)
    }
    pub fn anchor(&self) -> Anchor {
        anchor_for(&self.path)
    }
    /// Page title (also `<title>`): the route's, or the hub's.
    pub fn title(&self) -> &'static str {
        if let Some(a) = Anchor::from_hub(&self.path) {
            return a.title(self.lang);
        }
        if self.path == "/" {
            return Anchor::Home.title(self.lang);
        }
        route_for(&self.path)
            .map(|r| r.title(self.lang))
            .unwrap_or("")
    }
    pub fn is_top(&self) -> bool {
        self.path == "/" || Anchor::from_hub(&self.path).is_some()
    }
    /// Back link on phones: the hub this page belongs to.
    pub fn back(&self) -> NavItem {
        let a = self.anchor();
        NavItem {
            href: a.href(),
            label: a.title(self.lang),
            icon: "CaretLeft",
            current: false,
        }
    }
    pub fn anchors(&self) -> Vec<NavItem> {
        let cur = self.anchor();
        Anchor::ALL
            .into_iter()
            .map(|a| NavItem {
                href: a.href(),
                label: a.title(self.lang),
                icon: a.icon(),
                current: a == cur,
            })
            .collect()
    }
    /// Sidebar: home, then each anchor with its groups.
    pub fn sections(&self) -> Vec<NavSection> {
        let cur = route_for(&self.path).map(|r| r.href);
        [Anchor::Charts, Anchor::Functions, Anchor::System]
            .into_iter()
            .map(|a| NavSection {
                anchor: NavItem {
                    href: a.href(),
                    label: a.title(self.lang),
                    icon: a.icon(),
                    current: self.path == a.href(),
                },
                groups: a
                    .groups()
                    .map(|g| NavGroup {
                        title: g.title(self.lang),
                        items: g
                            .routes()
                            .map(|r| NavItem {
                                href: r.href,
                                label: r.title(self.lang),
                                icon: r.icon,
                                current: cur == Some(r.href),
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect()
    }
    pub fn home(&self) -> NavItem {
        NavItem {
            href: "/",
            label: Anchor::Home.title(self.lang),
            icon: "House",
            current: self.path == "/",
        }
    }
    pub fn search_items(&self) -> Vec<SearchItem> {
        ROUTES
            .iter()
            .map(|r| SearchItem {
                href: r.href,
                label: r.title(self.lang),
                group: r.group.title(self.lang),
                icon: r.icon,
                keys: fold(&format!("{} {} {}", r.zh, r.en, r.aliases)),
            })
            .collect()
    }
    pub fn other_lang(&self) -> &'static str {
        match self.lang {
            Lang::En => "中文",
            Lang::Zh => "English",
        }
    }
    pub fn icon(&self, name: &str, size: u32) -> String {
        icon(name, false, size, "")
    }
}

/// Lowercase, no spaces: what ⌘K compares (the script folds the query the same way).
pub fn fold(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

// ---- hub pages ----

#[derive(Template)]
#[template(path = "hub.html")]
struct HubPage {
    shell: Shell,
    sections: Vec<NavGroup>,
}

pub async fn hub(State(app): State<crate::pages::Shared>, req: axum::extract::Request) -> Response {
    let path = req.uri().path().to_string();
    let Some(a) = Anchor::from_hub(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, &path);
    let shell = app
        .hub
        .read(|st| Shell::new(&headers, &path).live(st, &ctx, None));
    let sections = a
        .groups()
        .map(|g| NavGroup {
            title: g.title(shell.lang),
            items: g
                .routes()
                .map(|r| NavItem {
                    href: r.href,
                    label: r.title(shell.lang),
                    icon: r.icon,
                    current: false,
                })
                .collect(),
        })
        .collect();
    let html = HubPage { shell, sections }.render().unwrap();
    crate::assets::maybe_gzip(
        &headers,
        "text/html; charset=utf-8",
        html.into_bytes(),
        "no-store",
    )
}

// ---- language switch ----

#[derive(Deserialize)]
pub struct LangForm {
    lang: String,
    #[serde(default)]
    back: String,
}

/// POST /lang: set the cookie, back to the page it came from (same-site paths only).
pub async fn set_lang(Form(f): Form<LangForm>) -> Response {
    let lang = if f.lang == "en" { "en" } else { "zh" };
    let back = if f.back.starts_with('/') && !f.back.starts_with("//") {
        f.back
    } else {
        "/".into()
    };
    let mut r = Redirect::to(&back).into_response();
    r.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "u60_lang={lang}; Path=/; Max-Age=31536000; SameSite=Lax"
        ))
        .unwrap(),
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_for_a_page() {
        let s = Shell::new(&HeaderMap::new(), "/tools/speedtest");
        assert_eq!(s.title(), "测速");
        assert_eq!(s.family(), "charts");
        assert!(!s.is_top());
        assert_eq!(
            s.anchors()
                .iter()
                .filter(|a| a.current)
                .map(|a| a.href)
                .collect::<Vec<_>>(),
            ["/charts"]
        );
        let cur: Vec<_> = s
            .sections()
            .into_iter()
            .flat_map(|x| x.groups)
            .flat_map(|g| g.items)
            .filter(|i| i.current)
            .map(|i| i.href)
            .collect();
        assert_eq!(cur, ["/tools/speedtest"]);
        assert!(
            s.search_items().iter().any(|i| i.href == "/sms/compose"),
            "hidden pages are still searchable"
        );
        assert!(
            s.search_items()
                .iter()
                .find(|i| i.href == "/bandlock")
                .unwrap()
                .keys
                .contains("suopin")
        );
    }
}
