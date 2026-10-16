//! Two languages, picked by the `u60_lang` cookie (same key the old web keeps in localStorage);
//! Chinese when unset. Strings live next to their use as `l.pick(en, zh)`.

use axum::http::{HeaderMap, header};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Zh,
}

impl Lang {
    pub fn from_headers(h: &HeaderMap) -> Lang {
        match cookie(h, "u60_lang").as_deref() {
            Some("en") => Lang::En,
            _ => Lang::Zh,
        }
    }
    pub fn pick(self, en: &'static str, zh: &'static str) -> &'static str {
        match self {
            Lang::En => en,
            Lang::Zh => zh,
        }
    }
    /// `<html lang>`.
    pub fn tag(self) -> &'static str {
        self.pick("en", "zh-CN")
    }
    pub fn code(self) -> &'static str {
        self.pick("en", "zh")
    }
    pub fn other(self) -> Lang {
        match self {
            Lang::En => Lang::Zh,
            Lang::Zh => Lang::En,
        }
    }
}

/// One cookie's value from the request.
pub fn cookie(h: &HeaderMap, name: &str) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_cookie() {
        let mut h = HeaderMap::new();
        assert_eq!(Lang::from_headers(&h), Lang::Zh);
        h.insert(header::COOKIE, "a=1; u60_lang=en; b=2".parse().unwrap());
        assert_eq!(Lang::from_headers(&h), Lang::En);
        assert_eq!(cookie(&h, "b").as_deref(), Some("2"));
    }
}
