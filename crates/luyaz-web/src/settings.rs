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

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;

use crate::pages::Shared;

const PATH: &str = "/settings";

pub fn routes() -> Router<Shared> {
    Router::new().route(PATH, get(page))
}

#[derive(Template)]
#[template(path = "settings.html")]
struct Page {
    shell: crate::shell::Shell,
    /// A plugin with settings is installed: its title (None before the core has read it).
    plugin: Option<Option<String>>,
}

async fn page(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let plugin = crate::plugin_settings::entry(&app, std::time::Duration::from_millis(3500)).await;
    let shell = app
        .hub
        .read(|st| crate::shell::Shell::new(&headers, PATH).live(st, &ctx, None));
    let html = Page {
        shell,
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
            plugin: None,
        }
        .render()
        .unwrap()
    }

    #[test]
    fn plugin_entry_is_escaped_and_optional() {
        let mut h = HeaderMap::new();
        h.insert("cookie", "u60_lang=zh".parse().unwrap());
        let html = Page {
            shell: Shell::new(&h, PATH),
            plugin: Some(Some("<i>x</i>".into())),
        }
        .render()
        .unwrap();
        // the plugin entry: its title as text, linking to the plugin page
        assert!(html.contains(r#"href="/settings/plugin""#) && html.contains("&#60;i&#62;x&#60;/i&#62;"));
        let none = Page { shell: Shell::new(&h, PATH), plugin: None }.render().unwrap();
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
