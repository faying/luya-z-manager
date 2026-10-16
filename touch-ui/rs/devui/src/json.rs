//! The small JSON reader the C UI uses (src/json.c): top-level key lookup on raw text, values
//! as text (strings unescaped), arrays walked one element at a time. Byte for byte the same
//! results as C, including where a value is cut at the buffer size (`cap` = C's outlen).
// SPDX-License-Identifier: MIT

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

/// Past the closing quote of the string starting at i, or None.
fn skip_string(s: &[u8], mut i: usize) -> Option<usize> {
    if at(s, i) != b'"' {
        return None;
    }
    let mut esc = false;
    i += 1;
    while at(s, i) != 0 {
        let c = s[i];
        if esc {
            esc = false;
        } else if c == b'\\' {
            esc = true;
        } else if c == b'"' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

fn hex4(s: &[u8], i: usize) -> Option<u32> {
    let mut v = 0u32;
    for k in 0..4 {
        let c = at(s, i + k);
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => 10 + c - b'a',
            b'A'..=b'F' => 10 + c - b'A',
            _ => return None,
        };
        v = (v << 4) | d as u32;
    }
    Some(v)
}

fn push_cp(out: &mut Vec<u8>, cap: usize, mut cp: u32) {
    let pos = out.len();
    if cp <= 0x7f {
        if pos + 1 < cap {
            out.push(cp as u8);
        }
    } else if cp <= 0x7ff {
        if pos + 2 < cap {
            out.extend([0xc0 | (cp >> 6) as u8, 0x80 | (cp & 0x3f) as u8]);
        }
    } else if cp <= 0xffff {
        if (0xd800..=0xdfff).contains(&cp) {
            cp = 0xfffd;
        }
        if pos + 3 < cap {
            out.extend([0xe0 | (cp >> 12) as u8, 0x80 | ((cp >> 6) & 0x3f) as u8, 0x80 | (cp & 0x3f) as u8]);
        }
    } else if cp <= 0x10ffff && pos + 4 < cap {
        out.extend([0xf0 | (cp >> 18) as u8, 0x80 | ((cp >> 12) & 0x3f) as u8, 0x80 | ((cp >> 6) & 0x3f) as u8, 0x80 | (cp & 0x3f) as u8]);
    }
}

/// Just past the ':' of a top-level "key":, or None.
fn find_key(s: &[u8], key: &[u8]) -> Option<usize> {
    let (mut obj, mut arr, mut expect_key) = (0, 0, false);
    let mut i = 0;
    while at(s, i) != 0 {
        let c = s[i];
        match c {
            b'"' => {
                let ks = i + 1;
                let kp = skip_string(s, i)?;
                let ke = kp - 1;
                if kp > ks && expect_key && obj == 1 && &s[ks..ke] == key {
                    let mut q = kp;
                    while is_ws(at(s, q)) {
                        q += 1;
                    }
                    if at(s, q) == b':' {
                        return Some(q + 1);
                    }
                }
                i = kp;
                if obj == 1 && expect_key {
                    expect_key = false;
                }
                continue;
            }
            b'{' => {
                obj += 1;
                expect_key = obj == 1;
            }
            b'}' => {
                if obj > 0 {
                    obj -= 1;
                }
                expect_key = obj == 1;
            }
            b'[' => {
                arr += 1;
                expect_key = false;
            }
            b']' => {
                if arr > 0 {
                    arr -= 1;
                }
            }
            b',' => {
                if obj == 1 && arr == 0 {
                    expect_key = true;
                }
            }
            b':' => expect_key = false,
            _ => {}
        }
        i += 1;
    }
    None
}

/// The value of a top-level key as text (at most cap - 1 bytes), or None when absent.
pub fn get(json: &[u8], key: &str, cap: usize) -> Option<Vec<u8>> {
    if cap == 0 {
        return None;
    }
    let mut v = find_key(json, key.as_bytes())?;
    while is_ws(at(json, v)) {
        v += 1;
    }
    let mut out = Vec::new();
    let s = json;
    if at(s, v) == b'"' {
        v += 1;
        while at(s, v) != 0 && s[v] != b'"' && out.len() < cap - 1 {
            if s[v] == b'\\' && at(s, v + 1) != 0 {
                v += 1;
                let e = s[v];
                match e {
                    b'"' | b'\\' | b'/' => out.push(e),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'u' => {
                        v += 1;
                        let Some(mut cp) = hex4(s, v) else { break };
                        let mut q = v + 4;
                        if (0xd800..=0xdbff).contains(&cp)
                            && at(s, q) == b'\\'
                            && at(s, q + 1) == b'u'
                            && let Some(lo) = hex4(s, q + 2)
                            && (0xdc00..=0xdfff).contains(&lo)
                        {
                            cp = 0x10000 + ((cp - 0xd800) << 10) + (lo - 0xdc00);
                            q += 6;
                        }
                        push_cp(&mut out, cap, cp);
                        v = q;
                        continue;
                    }
                    _ => out.push(e),
                }
                v += 1;
                continue;
            }
            out.push(s[v]);
            v += 1;
        }
    } else if at(s, v) == b'{' || at(s, v) == b'[' {
        let open = s[v];
        let close = if open == b'{' { b'}' } else { b']' };
        let (mut depth, mut in_str) = (0i32, false);
        while at(s, v) != 0 && out.len() < cap - 1 {
            let c = s[v];
            if in_str {
                if c == b'\\' && at(s, v + 1) != 0 {
                    out.push(s[v]);
                    v += 1;
                    if out.len() < cap - 1 {
                        out.push(s[v]);
                        v += 1;
                    }
                    continue;
                }
                if c == b'"' {
                    in_str = false;
                }
            } else if c == b'"' {
                in_str = true;
            } else if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
            }
            out.push(c);
            v += 1;
            if !in_str && depth == 0 {
                break;
            }
        }
    } else {
        while at(s, v) != 0 && !matches!(s[v], b',' | b'}' | b']') && !is_ws(s[v]) && out.len() < cap - 1 {
            out.push(s[v]);
            v += 1;
        }
    }
    Some(out)
}

/// get() as a String (lossy: C hands the bytes on as they are).
pub fn get_str(json: &[u8], key: &str, cap: usize) -> Option<String> {
    get(json, key, cap).map(|v| String::from_utf8_lossy(&v).into_owned())
}

/// strtol of the value, or def.
pub fn get_int(json: &[u8], key: &str, def: i64) -> i64 {
    let Some(v) = get(json, key, 64) else { return def };
    strtol(&v).unwrap_or(def)
}

/// C strtol(s, &end, 10): leading blanks, one sign, digits (saturating); None when no digit.
pub fn strtol(s: &[u8]) -> Option<i64> {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    let neg = match s.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let start = i;
    let mut v: i64 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        let d = (s[i] - b'0') as i64;
        v = if neg { v.saturating_mul(10).saturating_sub(d) } else { v.saturating_mul(10).saturating_add(d) };
        i += 1;
    }
    (i > start).then_some(v)
}

/// C atoi / atol: strtol, 0 when there is no number.
pub fn atol(s: &[u8]) -> i64 {
    strtol(s).unwrap_or(0)
}

/// C atof / strtod: the longest leading decimal number (blanks, sign, digits, '.', exponent),
/// 0 when there is none.
pub fn atof(s: &[u8]) -> f64 {
    let t = std::str::from_utf8(s).unwrap_or("");
    let t = t.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let b = t.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'-' | b'+')) {
        i += 1;
    }
    let ds = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut digits = i > ds;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let fs = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        digits |= i > fs;
    }
    if !digits {
        return 0.0;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'-' | b'+')) {
            j += 1;
        }
        let es = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > es {
            i = j;
        }
    }
    t[..i].parse().unwrap_or(0.0)
}

/// Walk an array: from p (at its '[' or past an element), the next element as text and the
/// position after it, or None at the end.
pub fn arr_next(s: &[u8], mut p: usize, cap: usize) -> Option<(Vec<u8>, usize)> {
    if cap == 0 {
        return None;
    }
    if at(s, p) == b'[' {
        p += 1;
    }
    while at(s, p) != 0 && (is_ws(s[p]) || s[p] == b',') {
        p += 1;
    }
    if matches!(at(s, p), 0 | b']' | b'}') {
        return None;
    }
    let start = p;
    let mut out = Vec::new();
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    while at(s, p) != 0 {
        let c = s[p];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c == b'{' || c == b'[' {
            depth += 1;
        } else if c == b'}' || c == b']' {
            if depth == 0 {
                break; // the array's own ']'
            }
            depth -= 1;
        } else if c == b',' && depth == 0 {
            break;
        }
        if out.len() < cap - 1 {
            out.push(c);
        }
        if !in_str && depth == 0 && (c == b'}' || c == b']') {
            p += 1;
            break;
        }
        p += 1;
    }
    while out.last().is_some_and(|&c| is_ws(c)) {
        out.pop();
    }
    (p > start).then_some((out, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_as_text() {
        let j: &[u8] = b"{\"a\":\"x\\\"y\",\"n\": 42 ,\"o\":{\"a\":1,\"s\":\"}\"},\"l\":[1,{\"b\":2}],\"u\":\"\\u4e2d\\ud83d\\ude00\"}";
        assert_eq!(get(j, "a", 64).unwrap(), b"x\"y");
        assert_eq!(get_int(j, "n", -1), 42);
        assert_eq!(get(j, "o", 64).unwrap(), br#"{"a":1,"s":"}"}"#);
        assert_eq!(get(j, "l", 64).unwrap(), br#"[1,{"b":2}]"#);
        assert_eq!(get(j, "u", 64).unwrap(), "中😀".as_bytes());
        // only the top level: "b" is inside an array
        assert!(get(j, "b", 64).is_none());
        // cut at cap - 1 bytes
        assert_eq!(get(j, "o", 5).unwrap(), b"{\"a\"");
        assert_eq!(get_int(j, "zz", 7), 7);
    }

    #[test]
    fn numbers_as_c_reads_them() {
        assert_eq!(strtol(b" -12x"), Some(-12));
        assert_eq!(strtol(b"--5"), None);
        assert_eq!(strtol(b"12.9"), Some(12));
        assert_eq!(atol(b"null"), 0);
        assert_eq!(atof(b"1000.5"), 1000.5);
        assert_eq!(atof(b" 2e3x"), 2000.0);
        assert_eq!(atof(b"null"), 0.0);
        assert_eq!(atof(b"-.5"), -0.5);
    }

    #[test]
    fn arrays_one_by_one() {
        let a = br#"[{"x":1}, "s,]", 3 ]"#;
        let (e, p) = arr_next(a, 0, 64).unwrap();
        assert_eq!(e, br#"{"x":1}"#);
        let (e, p) = arr_next(a, p, 64).unwrap();
        assert_eq!(e, br#""s,]""#);
        let (e, p) = arr_next(a, p, 64).unwrap();
        assert_eq!(e, b"3");
        assert!(arr_next(a, p, 64).is_none());
    }
}
