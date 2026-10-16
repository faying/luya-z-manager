//! Small helpers the data modules share: the clocks C reads, and C's fixed-size string fields
//! (snprintf into char[N] keeps at most N - 1 bytes).
// SPDX-License-Identifier: MIT

use crate::json;

/// CLOCK_MONOTONIC in ms, as the modules' now_ms() (long).
pub fn mono_ms() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: plain clock read into a local
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec * 1000 + ts.tv_nsec / 1_000_000
}

/// The same as data.c's mono_ms(): 32 bits, wrapping.
pub fn mono_ms32() -> u32 {
    mono_ms() as u32
}

/// time(NULL)
pub fn wall() -> i64 {
    // SAFETY: time(NULL)
    unsafe { libc::time(std::ptr::null_mut()) }
}

/// CLOCK_BOOTTIME in s (ui_boot_s), CLOCK_MONOTONIC if that fails.
pub fn boot_s() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: plain clock reads into a local
    unsafe {
        if libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) != 0 {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
        }
    }
    ts.tv_sec
}

/// Bytes as text (C hands them on as they are; a cut character becomes U+FFFD here).
pub fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// snprintf(buf, cap, "%s", s): at most cap - 1 bytes.
pub fn trunc(s: &str, cap: usize) -> String {
    if s.len() < cap {
        return s.to_string();
    }
    text(&s.as_bytes()[..cap.saturating_sub(1)])
}

/// printf output (cf!) into a char[cap].
pub fn bytes_cap(v: &[u8], cap: usize) -> String {
    text(&v[..v.len().min(cap.saturating_sub(1))])
}

/// json_get into `out` when the key is there (C's json_get leaves `out` alone otherwise).
pub fn jset(obj: &[u8], key: &str, cap: usize, out: &mut String) -> bool {
    match json::get(obj, key, cap) {
        Some(v) => {
            *out = text(&v);
            true
        }
        None => false,
    }
}

/// json_get into a char[cap], "" when the key is missing.
pub fn jget(obj: &[u8], key: &str, cap: usize) -> String {
    json::get(obj, key, cap).map(|v| text(&v)).unwrap_or_default()
}

/// json_get into a char[cap], "" when missing or null (the modules' json_str / jstr).
pub fn jstr(obj: &[u8], key: &str, cap: usize) -> String {
    match json::get(obj, key, cap) {
        Some(v) if v != b"null" => text(&v),
        _ => String::new(),
    }
}

/// The value is the word true.
pub fn jtrue(obj: &[u8], key: &str) -> bool {
    json::get(obj, key, 8).is_some_and(|v| v == b"true")
}

/// The modules' change fingerprint (FNV-1a over a string).
pub fn fnv(mut h: u32, s: &[u8]) -> u32 {
    for &c in s {
        h ^= c as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

pub const FNV0: u32 = 2166136261;

/// Find `needle` in `hay` from `from` (strstr).
pub fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > hay.len() || needle.is_empty() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// strchr from `from`.
pub fn find_byte(hay: &[u8], c: u8, from: usize) -> Option<usize> {
    hay.get(from..)?.iter().position(|&b| b == c).map(|p| p + from)
}

/// The end ('}' index) of the object starting at `p` ('{'), skipping braces in strings; None
/// when it does not close (the modules' match_brace / obj_end).
pub fn obj_end(s: &[u8], p: usize) -> Option<usize> {
    let (mut depth, mut instr, mut esc) = (0i32, false, false);
    for (i, &c) in s.iter().enumerate().skip(p) {
        if c == 0 {
            break;
        }
        if instr {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                instr = false;
            }
        } else if c == b'"' {
            instr = true;
        } else if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// The objects of an array text one by one, as the modules cut them out: from each '{' to its
/// matching '}' (they do not stop at the array's ']').
pub fn objects(s: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut p = 0;
    while let Some(o) = find_byte(s, b'{', p) {
        let Some(e) = obj_end(s, o) else { break };
        out.push(&s[o..=e]);
        p = e + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_cut_like_c() {
        assert_eq!(trunc("abcdef", 4), "abc");
        assert_eq!(trunc("ab", 4), "ab");
        assert_eq!(jstr(br#"{"a":null,"b":"x"}"#, "a", 8), "");
        assert_eq!(jstr(br#"{"a":null,"b":"x"}"#, "b", 8), "x");
        assert_eq!(jget(br#"{"a":null}"#, "a", 8), "null");
    }

    #[test]
    fn objects_in_an_array() {
        let a = br#"[{"a":"}"},{"b":{"c":1}}]"#;
        let o = objects(a);
        assert_eq!(o, vec![&br#"{"a":"}"}"#[..], &br#"{"b":{"c":1}}"#[..]]);
    }
}
