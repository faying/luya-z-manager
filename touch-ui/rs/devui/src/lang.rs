//! Screen language (the C UI's src/lang.c until Phase 10 X5): Chinese is the key; in English the
//! row of lang/en.inc whose first column is exactly that text (missing → the Chinese). Chosen once
//! per process, before the first TR().
//!
//!   tr("中文")             the common case
//!   trc!("场景", "中文")   same Chinese, different English: key 场景|中文
//!   trn("%d 台设备", n)    English one|other, picked by n == 1
//!   n_("中文")             marks a literal in a static table; tr() it where it is shown
//!   pick(zh, en)           text from core/agent with an *_en sibling
// SPDX-License-Identifier: MIT

#[cfg(not(test))]
use std::sync::atomic::{AtomicBool, Ordering};

mod table {
    include!(concat!(env!("OUT_DIR"), "/lang_en.rs"));
}

#[cfg(not(test))]
static EN: AtomicBool = AtomicBool::new(false);

/// 0 = zh (default), 1 = en. Set once at startup, before any tr().
#[cfg(not(test))]
pub fn set_en(en: bool) {
    EN.store(en, Ordering::Relaxed);
}

#[cfg(not(test))]
pub fn is_en() -> bool {
    EN.load(Ordering::Relaxed)
}

// tests run in parallel threads: each has its own language
#[cfg(test)]
thread_local! {
    static EN_T: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub fn set_en(en: bool) {
    EN_T.with(|c| c.set(en));
}

#[cfg(test)]
pub fn is_en() -> bool {
    EN_T.with(|c| c.get())
}

/// The value after "lang=": exactly "en" (a trailing newline allowed) → true. Same rule as
/// the old guard script's sms_lang.
pub fn parse(v: &str) -> bool {
    v == "en" || v == "en\n"
}

fn find(key: &str) -> Option<&'static (&'static str, &'static str, Option<&'static str>)> {
    let rows = table::ROWS;
    rows.binary_search_by(|r| r.0.as_bytes().cmp(key.as_bytes())).ok().map(|i| &rows[i])
}

pub fn tr(zh: &'static str) -> &'static str {
    if !is_en() {
        return zh;
    }
    find(zh).map_or(zh, |r| r.1)
}

/// TRC: `key` is "context|中文".
pub fn trc_key(key: &str, zh: &'static str) -> &'static str {
    if !is_en() {
        return zh;
    }
    find(key).map_or(zh, |r| r.1)
}

#[macro_export]
macro_rules! trc {
    ($c:literal, $s:literal) => {
        $crate::lang::trc_key(concat!($c, "|", $s), $s)
    };
}

pub fn trn(zh: &'static str, n: i64) -> &'static str {
    if !is_en() {
        return zh;
    }
    match find(zh) {
        None => zh,
        Some(r) => match r.2 {
            Some(other) if n != 1 => other,
            _ => r.1,
        },
    }
}

pub const fn n_(s: &'static str) -> &'static str {
    s
}

/// Backend text: the English sibling in English when it is there and not empty.
pub fn pick<'a>(zh: &'a str, en: &'a str) -> &'a str {
    if is_en() && !en.is_empty() { en } else { zh }
}

pub fn count() -> usize {
    table::ROWS.len()
}

/// Tests that switch the process-wide language hold this, so they don't race each other.
#[cfg(test)]
pub static TEST_LANG: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    // tests/lang_test.c's rules; one test so the process-wide language is not raced
    #[test]
    fn lookup_rules() {
        let _l = TEST_LANG.lock().unwrap_or_else(|e| e.into_inner());
        assert!(parse("en") && parse("en\n") && !parse("en ") && !parse("zh") && !parse(""));
        assert!(count() > 100);
        // sorted by bytes, no duplicate keys: binary search finds every row
        for w in table::ROWS.windows(2) {
            assert!(w[0].0.as_bytes() < w[1].0.as_bytes(), "{} / {}", w[0].0, w[1].0);
        }
        set_en(false);
        assert_eq!(tr("首页"), "首页");
        assert_eq!(trn("%d 台", 2), "%d 台");
        set_en(true);
        assert_eq!(tr("没有这一行的中文"), "没有这一行的中文");
        assert_eq!(trn("%d 台", 1), "%d device");
        assert_eq!(trn("%d 台", 2), "%d devices");
        assert_eq!(trc!("tailscale", "本机"), "This device");
        assert_eq!(pick("中", "en"), "en");
        assert_eq!(pick("中", ""), "中");
        set_en(false);
    }
}
