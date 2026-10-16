//! Operator logos (web/src/lib/operatorLogo.ts): PLMN → slug, the file read from `--logo-dir` at
//! request time (they are trademarks: never embedded, never in a public tree), and a first-letter
//! badge when the PLMN is unknown or the file is not there. File existence is decided here, when
//! the leaf is rendered, so the page never shows a broken image.

use std::path::Path;

use axum::extract::{Path as UrlPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::pages::Shared;

/// "MCC-MNC" (3+3 digits) → slug.
fn slug_of(plmn: &str) -> Option<&'static str> {
    Some(match plmn {
        "460-000" | "460-002" | "460-004" | "460-007" | "460-008" => "china-mobile",
        "460-001" | "460-006" | "460-009" | "454-007" => "china-unicom",
        "460-003" | "460-005" | "460-011" | "454-031" | "455-007" => "china-telecom",
        "460-015" => "china-broadnet",
        "454-000" | "454-002" | "454-010" | "454-018" | "454-016" | "454-019" | "454-020" => "csl",
        "454-003" | "454-004" | "455-005" => "three-hk",
        "454-006" | "454-015" => "smartone",
        "455-001" | "455-004" => "ctm",
        "454-012" | "454-013" => "cmhk",
        "466-092" => "chunghwa",
        "466-001" => "fetnet",
        "466-097" => "taiwan-mobile",
        "440-010" => "docomo",
        "440-020" => "softbank",
        "440-011" => "rakuten",
        "440-050" | "440-051" | "440-052" | "440-053" | "440-054" => "au",
        "450-005" => "skt",
        "450-008" => "kt",
        "450-006" => "lguplus",
        "525-001" => "singtel",
        "525-003" => "m1",
        "525-005" => "starhub",
        "310-260" => "t-mobile-us",
        "310-410" => "att",
        "311-480" => "verizon",
        _ => return None,
    })
}

/// Every slug the table can give (the only names `/oplogo/<slug>` answers for).
const SLUGS: [&str; 25] = [
    "att", "au", "china-broadnet", "china-mobile", "china-telecom", "china-unicom", "chunghwa", "cmhk", "csl",
    "ctm", "docomo", "fetnet", "kt", "lguplus", "m1", "rakuten", "singtel", "skt", "smartone", "softbank",
    "starhub", "t-mobile-us", "taiwan-mobile", "three-hk", "verizon",
];

/// "460","1" / "460","01" → "460-001"; None if not digits.
pub fn normalize(mcc: &str, mnc: &str) -> Option<String> {
    let (c, n) = (mcc.trim(), mnc.trim());
    (c.len() == 3
        && c.bytes().all(|b| b.is_ascii_digit())
        && (1..=3).contains(&n.len())
        && n.bytes().all(|b| b.is_ascii_digit()))
    .then(|| format!("{c}-{n:0>3}"))
}

pub fn slug(mcc: &str, mnc: &str) -> Option<&'static str> {
    slug_of(&normalize(mcc, mnc)?)
}

/// First character (a whole CJK character, or an upper-cased letter); "?" for nothing.
pub fn badge(name: &str) -> String {
    name.trim().chars().next().map_or("?".into(), |c| c.to_uppercase().collect())
}

/// The file for a slug in the table: `<slug>.svg`, else `<slug>.png`.
fn file(dir: &Path, slug: &str) -> Option<(std::path::PathBuf, &'static str)> {
    if !SLUGS.contains(&slug) {
        return None;
    }
    [("svg", "image/svg+xml"), ("png", "image/png")]
        .into_iter()
        .map(|(e, t)| (dir.join(format!("{slug}.{e}")), t))
        .find(|(p, _)| p.is_file())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The mark in front of an operator name: a white chip with the image (wide marks up to 4x the
/// height), else a round badge with the first character.
pub fn html(dir: &Path, mcc: &str, mnc: &str, name: &str, size: u32) -> String {
    if let Some(s) = slug(mcc, mnc).filter(|s| file(dir, s).is_some()) {
        return format!(
            "<span class=\"op-logo\" style=\"height:{size}px;max-width:{}px\"><img src=\"/oplogo/{s}\" alt=\"\" loading=\"lazy\" height=\"{}\"></span>",
            size * 4,
            size - 2
        );
    }
    format!(
        "<span class=\"op-badge\" aria-hidden=\"true\" style=\"width:{size}px;height:{size}px;font-size:{}px\">{}</span>",
        size * 55 / 100,
        esc(&badge(name))
    )
}

pub async fn serve(State(app): State<Shared>, UrlPath(slug): UrlPath<String>) -> Response {
    let Some((path, ctype)) = file(&app.logo_dir, &slug) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match tokio::fs::read(path).await {
        Ok(b) => (
            [
                (header::CONTENT_TYPE, ctype),
                (header::CACHE_CONTROL, "private, max-age=31536000, immutable"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            b,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plmn_pads_and_maps() {
        assert_eq!(normalize("460", "1").as_deref(), Some("460-001"));
        assert_eq!(normalize("460", "01"), normalize("460", "001"));
        assert!(normalize("46", "1").is_none() && normalize("460", "").is_none() && normalize("46a", "1").is_none());
        assert_eq!(slug("460", "1"), Some("china-unicom"));
        assert_eq!(slug("460", "0"), Some("china-mobile"));
        assert_eq!(slug("466", "92"), Some("chunghwa"));
        assert_eq!(slug("999", "99"), None);
    }

    #[test]
    fn every_slug_of_the_table_is_served_by_name() {
        for m in 0..1000 {
            for c in ["460", "454", "455", "466", "440", "450", "525", "310", "311"] {
                if let Some(s) = slug(c, &m.to_string()) {
                    assert!(SLUGS.contains(&s), "{s}");
                }
            }
        }
    }

    #[test]
    fn badge_and_missing_file() {
        assert_eq!(badge(" 中华电信"), "中");
        assert_eq!(badge("chunghwa"), "C");
        assert_eq!(badge(""), "?");
        let d = Path::new("/nonexistent-logo-dir");
        assert!(html(d, "466", "92", "Chunghwa", 20).contains("op-badge"));
        assert!(html(d, "999", "1", "<x>", 20).contains("&lt;"));
        assert!(file(d, "../etc/passwd").is_none());
    }

    #[test]
    fn file_found_as_svg_or_png() {
        let d = std::env::temp_dir().join(format!("oplogo-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("chunghwa.svg"), "<svg/>").unwrap();
        std::fs::write(d.join("kt.png"), "x").unwrap();
        assert!(html(&d, "466", "92", "x", 20).contains("src=\"/oplogo/chunghwa\""));
        assert_eq!(file(&d, "kt").unwrap().1, "image/png");
        assert!(file(&d, "csl").is_none() && file(&d, "unknown").is_none());
        std::fs::remove_dir_all(&d).ok();
    }
}
