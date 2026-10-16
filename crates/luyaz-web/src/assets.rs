//! Static files, built into the binary. The stylesheet is the admin web's own token and component
//! CSS (newdesign.css, nd.css, shell.css: one definition of the tokens, DESIGN.md §5) plus the few
//! utilities the pages need (static/web.css). Gzipped once at start, served with a
//! content hash in the URL so browsers cache them for good.

use std::io::Write;
use std::sync::OnceLock;

use axum::body::Body;
use axum::extract::Path;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use flate2::Compression;
use flate2::write::GzEncoder;

const CSS_PARTS: [&str; 5] = [
    // Same layer order as Tailwind, so the reset stays under nd.css (`@layer components`).
    "@layer theme, base, components, utilities;",
    include_str!("../static/nd/newdesign.css"),
    include_str!("../static/nd/nd.css"),
    include_str!("../static/nd/shell.css"),
    include_str!("../static/web.css"),
];
/// The step 5 pages' own bits (not on the signal page's first load).
const PAGES_CSS: &str = include_str!("../static/pages.css");
/// /sms and /sms/compose only (not on every other page's first load).
const SMS_CSS: &str = include_str!("../static/sms.css");
const HOME_CSS: &str = include_str!("../static/home/home.css");
/// The 5b service pages (one file per page, one asset).
const SVC_CSS: [&str; 1] = [include_str!("../static/svc/scenario.css")];
/// Its own asset: the Tailscale page's first load had only 0.4 KiB left with the scenario CSS in.
const TS_CSS: &str = include_str!("../static/svc/tailscale.css");
/// The batch-4 Wi-Fi and LAN pages, one source file per page so they can be written in parallel.
const ROUTER_CSS: [&str; 7] = [
    include_str!("../static/router/qos.css"),
    include_str!("../static/router/vpn.css"),
    include_str!("../static/router/filter.css"),
    include_str!("../static/router/firewall.css"),
    include_str!("../static/router/lan.css"),
    include_str!("../static/router/wifi_guest.css"),
    include_str!("../static/router/wifi_main.css"),
];
/// The batch-5a pages (alerts, SMS forwarding, scheduler, eSIM, health, settings, config), one
/// source file per page so they can be written in parallel.
const SYS_CSS: [&str; 7] = [
    include_str!("../static/sys/alerts.css"),
    include_str!("../static/sys/sms_forward.css"),
    include_str!("../static/sys/scheduler.css"),
    include_str!("../static/sys/esim.css"),
    include_str!("../static/sys/health.css"),
    include_str!("../static/sys/settings.css"),
    include_str!("../static/sys/config.css"),
];
/// The other batch-5c pages (SIM, STC, STK, AT, processes, enable-ADB, DNS), one source
/// file per page so they can be written in parallel.
const DEV_CSS: [&str; 7] = [
    include_str!("../static/dev/sim.css"),
    include_str!("../static/dev/stc.css"),
    include_str!("../static/dev/stk.css"),
    include_str!("../static/dev/at.css"),
    include_str!("../static/dev/processes.css"),
    include_str!("../static/dev/enable_adb.css"),
    include_str!("../static/dev/dns.css"),
];
/// Its own asset: the device page's first load was 13 bytes over 40 KiB with the other 5c CSS in.
const DEVICE_CSS: &str = include_str!("../static/dev/device.css");
const DATASTAR: &[u8] = include_bytes!("../static/datastar-1.0.4.js");
const SHELL_JS: &[u8] = include_bytes!("../static/shell.js");

struct Asset {
    name: &'static str,
    mime: &'static str,
    raw: Vec<u8>,
    gz: Option<Vec<u8>>,
    hash: String,
}

fn gzip(b: &[u8]) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::best());
    e.write_all(b).unwrap();
    e.finish().unwrap()
}

/// Comments and indentation out (the sources are the Next.js web's, heavily commented); about
/// 1 KB of gzip on first load. Not a real minifier: no CSS string here contains `/*`.
fn minify_css(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        rest = rest[i..].find("*/").map_or("", |j| &rest[i + j + 2..]);
    }
    out.push_str(rest);
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// FNV-1a, enough to bust caches.
fn hash(b: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for x in b {
        h ^= *x as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", h as u32)
}

fn assets() -> &'static [Asset] {
    static A: OnceLock<Vec<Asset>> = OnceLock::new();
    A.get_or_init(|| {
        let css = minify_css(&CSS_PARTS.join("\n")).into_bytes();
        let mk = |name, mime, raw: Vec<u8>, compress: bool| Asset {
            name,
            mime,
            gz: compress.then(|| gzip(&raw)),
            hash: hash(&raw),
            raw,
        };
        vec![
            mk("app.css", "text/css; charset=utf-8", css, true),
            mk(
                "pages.css",
                "text/css; charset=utf-8",
                minify_css(PAGES_CSS).into_bytes(),
                true,
            ),
            mk(
                "sms.css",
                "text/css; charset=utf-8",
                minify_css(SMS_CSS).into_bytes(),
                true,
            ),
            mk(
                "router.css",
                "text/css; charset=utf-8",
                minify_css(&ROUTER_CSS.join("\n")).into_bytes(),
                true,
            ),
            mk(
                "home.css",
                "text/css; charset=utf-8",
                minify_css(HOME_CSS).into_bytes(),
                true,
            ),
            mk(
                "tailscale.css",
                "text/css; charset=utf-8",
                minify_css(TS_CSS).into_bytes(),
                true,
            ),
            mk(
                "svc.css",
                "text/css; charset=utf-8",
                minify_css(&SVC_CSS.join("\n")).into_bytes(),
                true,
            ),
            mk(
                "sys.css",
                "text/css; charset=utf-8",
                minify_css(&SYS_CSS.join("\n")).into_bytes(),
                true,
            ),
            mk(
                "dev.css",
                "text/css; charset=utf-8",
                minify_css(&DEV_CSS.join("\n")).into_bytes(),
                true,
            ),
            mk(
                "device.css",
                "text/css; charset=utf-8",
                minify_css(DEVICE_CSS).into_bytes(),
                true,
            ),
            mk(
                "datastar.js",
                "text/javascript; charset=utf-8",
                DATASTAR.to_vec(),
                true,
            ),
            mk("shell.js", "text/javascript; charset=utf-8", SHELL_JS.to_vec(), true),
            mk("icons.svg", "image/svg+xml", sprite().into_bytes(), true),
            // the old web's favicon.ico is 26 KB; this is the signal glyph, ink on transparent
            mk("favicon.svg", "image/svg+xml", format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 256 256\"><style>path{{fill:#17171c}}@media(prefers-color-scheme:dark){{path{{fill:#f3f3ef}}}}</style>{}</svg>", crate::icons::path("CellSignalFull", false)).into_bytes(), true),
        ]
    })
}

/// Every icon the pages use, as `<symbol id="Name">`; the four anchors (the current tab) and the
/// top-bar status icons that are filled when on (`topbar::FILLED`) also as `id="Name-fill"`. Page icons stay bold when current: the row's own highlight shows it, and
/// fill versions of all of them would double the sprite (first-load budget, u60-web.md §3 line 1).
fn sprite() -> String {
    let mut out = String::from("<svg xmlns=\"http://www.w3.org/2000/svg\">");
    for name in crate::icons::NAMES {
        let anchor = crate::routes::Anchor::ALL.iter().any(|a| a.icon() == *name)
            || crate::topbar::FILLED.contains(name);
        for fill in [false, true].into_iter().filter(|f| !f || anchor) {
            out += &format!(
                "<symbol id=\"{name}{}\" viewBox=\"0 0 256 256\" fill=\"currentColor\">{}</symbol>",
                if fill { "-fill" } else { "" },
                crate::icons::path(name, fill)
            );
        }
    }
    out + "</svg>"
}

/// "/a/app.css?v=1a2b3c4d" for templates.
pub fn url(name: &str) -> String {
    let a = assets()
        .iter()
        .find(|a| a.name == name)
        .expect("known asset");
    format!("/a/{}?v={}", a.name, a.hash)
}

fn accepts_gzip(h: &HeaderMap) -> bool {
    h.get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("gzip"))
}

pub fn maybe_gzip(
    h: &HeaderMap,
    mime: &'static str,
    body: Vec<u8>,
    cache: &'static str,
) -> Response {
    let mut r = if accepts_gzip(h) {
        let mut r = Response::new(Body::from(gzip(&body)));
        r.headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        r
    } else {
        Response::new(Body::from(body))
    };
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    r.headers_mut()
        .insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    r
}

pub async fn serve(Path(name): Path<String>, h: HeaderMap) -> Response {
    let Some(a) = assets().iter().find(|a| a.name == name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (body, gz) = match (&a.gz, accepts_gzip(&h)) {
        (Some(gz), true) => (gz.clone(), true),
        _ => (a.raw.clone(), false),
    };
    let mut r = Response::new(Body::from(body));
    let hd = r.headers_mut();
    hd.insert(header::CONTENT_TYPE, HeaderValue::from_static(a.mime));
    hd.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    hd.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    if gz {
        hd.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }
    r
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_sprite_has_every_top_bar_icon() {
        let s = super::sprite();
        let hub = crate::hub::Hub::new();
        for l in [crate::i18n::Lang::Zh] {
            for i in hub.read(|st| crate::topbar::icons(st, l)) {
                for fill in [false, true] {
                    let filled = fill && i.icon != "CellSignalFull";
                    let id = format!("id=\"{}{}\"", i.icon, if filled { "-fill" } else { "" });
                    assert!(s.contains(&id), "{id}");
                }
            }
        }
    }

    #[test]
    fn css_comments_out() {
        assert_eq!(
            super::minify_css("/* a */\n  .x { color: red; } /* b\n c */\n\n.y{}"),
            ".x { color: red; }\n.y{}"
        );
    }
}
