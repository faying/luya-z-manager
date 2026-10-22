//! Helpers every data module leans on: clock readings, byte-capped strings (a field of `cap`
//! bytes holds at most cap - 1), JSON field shortcuts, a change hash, and byte searches.

use crate::json;

fn clock(id: libc::clockid_t) -> Option<libc::timespec> {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: the kernel fills a timespec we own
    (unsafe { libc::clock_gettime(id, &mut ts) } == 0).then_some(ts)
}

/// Milliseconds on the monotonic clock.
pub fn mono_ms() -> i64 {
    clock(libc::CLOCK_MONOTONIC).map_or(0, |t| t.tv_sec * 1000 + t.tv_nsec / 1_000_000)
}

/// [`mono_ms`] cut to 32 bits; callers compare with wrapping arithmetic.
pub fn mono_ms32() -> u32 {
    mono_ms() as u32
}

/// Seconds since the epoch.
pub fn wall() -> i64 {
    // SAFETY: a null pointer asks time() only for its return value
    unsafe { libc::time(std::ptr::null_mut()) }
}

/// Seconds since boot, sleep included; the monotonic clock where boottime is missing.
pub fn boot_s() -> i64 {
    clock(libc::CLOCK_BOOTTIME).or_else(|| clock(libc::CLOCK_MONOTONIC)).map_or(0, |t| t.tv_sec)
}

/// Lossy UTF-8: a character split by a byte cap turns into U+FFFD.
pub fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The first `cap - 1` bytes of `b` at most, as text.
pub fn bytes_cap(b: &[u8], cap: usize) -> String {
    let keep = cap.saturating_sub(1).min(b.len());
    text(&b[..keep])
}

/// `s` fitted into a field of `cap` bytes.
pub fn trunc(s: &str, cap: usize) -> String {
    match s.len() < cap {
        true => s.to_owned(),
        false => bytes_cap(s.as_bytes(), cap),
    }
}

/// Field `key` of `obj` (at most `cap - 1` bytes) into `out`; `out` stays as it was when the key
/// is absent. Tells whether it was found.
pub fn jset(obj: &[u8], key: &str, cap: usize, out: &mut String) -> bool {
    let Some(v) = json::get(obj, key, cap) else { return false };
    *out = text(&v);
    true
}

/// Field `key` as text, empty when absent (a JSON null comes back as the word null).
pub fn jget(obj: &[u8], key: &str, cap: usize) -> String {
    let mut s = String::new();
    jset(obj, key, cap, &mut s);
    s
}

/// Field `key` as text, empty when absent or null.
pub fn jstr(obj: &[u8], key: &str, cap: usize) -> String {
    json::get(obj, key, cap).filter(|v| v.as_slice() != b"null").map(|v| text(&v)).unwrap_or_default()
}

/// Field `key` is literally `true`.
pub fn jtrue(obj: &[u8], key: &str) -> bool {
    matches!(json::get(obj, key, 8).as_deref(), Some(b"true"))
}

/// The seed for [`fnv`].
pub const FNV0: u32 = 0x811c_9dc5;

/// Fold `bytes` into the 32-bit FNV-1a hash `h`: modules hash what they show to notice changes.
pub fn fnv(h: u32, bytes: &[u8]) -> u32 {
    bytes.iter().fold(h, |acc, &b| (acc ^ u32::from(b)).wrapping_mul(0x0100_0193))
}

/// Where `needle` first occurs in `hay` at or after `from`. An empty needle matches nothing.
pub fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let rest = hay.get(from..)?;
    if needle.is_empty() || needle.len() > rest.len() {
        return None;
    }
    (0..=rest.len() - needle.len()).find(|&i| rest[i..].starts_with(needle)).map(|i| from + i)
}

/// Where byte `c` first occurs in `hay` at or after `from`.
pub fn find_byte(hay: &[u8], c: u8, from: usize) -> Option<usize> {
    let rest = hay.get(from..)?;
    rest.iter().position(|&b| b == c).map(|i| from + i)
}

/// Index of the `}` that brings the brace count started at `start` back to zero, ignoring braces
/// inside JSON strings. Scanning stops at a NUL byte; None when nothing closes.
pub fn obj_end(s: &[u8], start: usize) -> Option<usize> {
    let mut open = 0i32;
    let mut quoted = false;
    let mut escaped = false;
    for (i, &c) in s.iter().enumerate().skip(start).take_while(|&(_, &c)| c != 0) {
        match (quoted, c) {
            (true, _) if escaped => escaped = false,
            (true, b'\\') => escaped = true,
            (true, b'"') => quoted = false,
            (true, _) => {}
            (false, b'"') => quoted = true,
            (false, b'{') => open += 1,
            (false, b'}') => {
                open -= 1;
                if open == 0 {
                    return Some(i);
                }
            }
            (false, _) => {}
        }
    }
    None
}

/// Every balanced `{…}` in `s`, left to right, each search resuming after the last one closed.
/// Brackets are not tracked, so objects after a `]` are included too.
pub fn objects(s: &[u8]) -> Vec<&[u8]> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(open) = find_byte(s, b'{', at) {
        match obj_end(s, open) {
            Some(close) => {
                found.push(&s[open..=close]);
                at = close + 1;
            }
            None => break,
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capped_fields() {
        assert_eq!(trunc("abcdef", 4), "abc");
        assert_eq!(trunc("abc", 4), "abc");
        assert_eq!(trunc("abcd", 4), "abc");
        assert_eq!(trunc("x", 0), "");
        assert_eq!(bytes_cap(b"hello", 3), "he");
        assert_eq!(bytes_cap(b"hi", 9), "hi");
        assert_eq!(bytes_cap(b"hi", 0), "");
    }

    #[test]
    fn json_shortcuts() {
        let o = br#"{"a":null,"b":"x","t":true,"f":false}"#;
        assert_eq!(jstr(o, "a", 8), "");
        assert_eq!(jstr(o, "b", 8), "x");
        assert_eq!(jget(o, "a", 8), "null");
        assert_eq!(jget(o, "zz", 8), "");
        assert!(jtrue(o, "t") && !jtrue(o, "f") && !jtrue(o, "zz"));
        let mut s = "keep".to_string();
        assert!(!jset(o, "zz", 8, &mut s) && s == "keep");
        assert!(jset(o, "b", 8, &mut s) && s == "x");
    }

    #[test]
    fn hash_and_search() {
        assert_eq!(fnv(FNV0, b""), 2166136261);
        assert_eq!(fnv(FNV0, b"a"), 0xe40c292c);
        assert_eq!(find(b"abcabc", b"bc", 0), Some(1));
        assert_eq!(find(b"abcabc", b"bc", 2), Some(4));
        assert_eq!(find(b"abc", b"", 0), None);
        assert_eq!(find(b"abc", b"c", 3), None);
        assert_eq!(find(b"abc", b"c", 4), None);
        assert_eq!(find_byte(b"abc", b'c', 3), None);
        assert_eq!(find_byte(b"abc", b'c', 9), None);
    }

    #[test]
    fn objects_in_an_array() {
        let a = br#"[{"a":"}"},{"b":{"c":1}}]"#;
        assert_eq!(objects(a), vec![&br#"{"a":"}"}"#[..], &br#"{"b":{"c":1}}"#[..]]);
        assert_eq!(obj_end(br#"{"x\"}":1}"#, 0), Some(9));
        assert_eq!(obj_end(b"{{}\0}", 0), None);
        assert!(objects(b"{ unclosed").is_empty());
    }
}
