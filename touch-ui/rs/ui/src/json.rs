//! Values out of JSON text without building a tree. Every screen reads core's replies through
//! here, often a dozen keys from the same 1 MB snapshot, so lookups scan the bytes in place.
//!
//! The rules the callers lean on:
//! - `get` looks at the top-level object only; a key inside a nested object or array is not seen.
//! - Values come back as text: strings unescaped (`\uXXXX` to UTF-8, surrogate pairs joined,
//!   a lone surrogate as U+FFFD), objects and arrays verbatim, anything else up to the next
//!   delimiter or blank.
//! - `cap` is the size of the field the value lands in, so at most cap - 1 bytes come back. Text
//!   is cut where cap falls, even inside a character; only a `\uXXXX` that would not fit whole
//!   is left out.
//! - A NUL byte ends the text, and so does running off the end; unfinished text gives what was
//!   read so far.
//!
//! Numbers read the way the C library reads them (`strtol`, `atof`): leading blanks, a sign,
//! as many digits as there are, junk after them ignored.

fn blank(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

/// The byte at i; 0 past the end, so the end and a NUL read alike.
fn peek(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

fn skip_blanks(s: &[u8], mut i: usize) -> usize {
    while blank(peek(s, i)) {
        i += 1;
    }
    i
}

/// Whether a byte-by-byte walk is inside a string literal. `outside(c)` feeds the next byte and
/// says whether it is structure (true) or string content, quotes included (false).
#[derive(Default)]
pub(crate) struct Quotes {
    inside: bool,
    escaped: bool,
}

impl Quotes {
    pub(crate) fn outside(&mut self, c: u8) -> bool {
        if !self.inside {
            self.inside = c == b'"';
            return !self.inside;
        }
        if self.escaped {
            self.escaped = false;
        } else if c == b'\\' {
            self.escaped = true;
        } else if c == b'"' {
            self.inside = false;
        }
        false
    }
}

/// i at an opening quote: the index just past its closing quote. None when it never closes.
fn string_end(s: &[u8], i: usize) -> Option<usize> {
    let mut q = Quotes { inside: true, escaped: false };
    for j in i + 1.. {
        match peek(s, j) {
            0 => return None,
            c if !q.outside(c) && !q.inside => return Some(j + 1),
            _ => {}
        }
    }
    None
}

/// How deep a walk is, and whether the next string in the top-level object names a key. It is
/// lenient on purpose: brackets are counted, not matched.
#[derive(Default)]
struct Nesting {
    objects: u32,
    arrays: u32,
    key_next: bool,
}

impl Nesting {
    fn top(&self) -> bool {
        self.objects == 1
    }

    fn structure(&mut self, c: u8) {
        match c {
            b'{' => {
                self.objects += 1;
                self.key_next = self.top();
            }
            b'}' => {
                self.objects = self.objects.saturating_sub(1);
                self.key_next = self.top();
            }
            b'[' => {
                self.arrays += 1;
                self.key_next = false;
            }
            b']' => self.arrays = self.arrays.saturating_sub(1),
            b',' if self.top() && self.arrays == 0 => self.key_next = true,
            b':' => self.key_next = false,
            _ => {}
        }
    }
}

/// Where the value of top-level `key` starts (just past its ':'), or None.
fn value_start(s: &[u8], key: &[u8]) -> Option<usize> {
    let mut n = Nesting::default();
    let mut i = 0;
    loop {
        let c = peek(s, i);
        if c == 0 {
            return None;
        }
        if c != b'"' {
            n.structure(c);
            i += 1;
            continue;
        }
        let end = string_end(s, i)?;
        if n.key_next && n.top() {
            if &s[i + 1..end - 1] == key {
                let colon = skip_blanks(s, end);
                if peek(s, colon) == b':' {
                    return Some(colon + 1);
                }
            }
            n.key_next = false;
        }
        i = end;
    }
}

/// Four hex digits at i.
fn hex4(s: &[u8], i: usize) -> Option<u32> {
    (i..i + 4).try_fold(0u32, |v, k| Some(v << 4 | (peek(s, k) as char).to_digit(16)?))
}

/// Appends code point `cp` as UTF-8 when all of it fits below `cap`.
fn put_char(out: &mut Vec<u8>, cap: usize, cp: u32) {
    let ch = match cp {
        0xd800..=0xdfff => '\u{fffd}',
        _ => match char::from_u32(cp) {
            Some(ch) => ch,
            None => return,
        },
    };
    let mut buf = [0u8; 4];
    let enc = ch.encode_utf8(&mut buf).as_bytes();
    if out.len() + enc.len() < cap {
        out.extend_from_slice(enc);
    }
}

/// A string value, i just past its opening quote: unescaped, at most cap - 1 bytes.
fn read_string(s: &[u8], mut i: usize, cap: usize, out: &mut Vec<u8>) {
    while out.len() + 1 < cap {
        let c = peek(s, i);
        if c == 0 || c == b'"' {
            return;
        }
        let e = peek(s, i + 1);
        if c != b'\\' || e == 0 {
            out.push(c);
            i += 1;
            continue;
        }
        i += 2;
        let plain = match e {
            b'b' => 8,
            b'f' => 12,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'u' => {
                let Some(hi) = hex4(s, i) else { return };
                i += 4;
                let mut cp = hi;
                if (0xd800..0xdc00).contains(&hi)
                    && peek(s, i) == b'\\'
                    && peek(s, i + 1) == b'u'
                    && let Some(lo) = hex4(s, i + 2).filter(|lo| (0xdc00..0xe000).contains(lo))
                {
                    cp = 0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00);
                    i += 6;
                }
                put_char(out, cap, cp);
                continue;
            }
            other => other, // \" \\ \/ and anything unknown: the character itself
        };
        out.push(plain);
    }
}

/// An object or array value as written, from its opening bracket to the matching close (only
/// brackets of its own kind count), at most cap - 1 bytes.
fn read_container(s: &[u8], mut i: usize, cap: usize, out: &mut Vec<u8>) {
    let open = peek(s, i);
    let close = if open == b'{' { b'}' } else { b']' };
    let mut q = Quotes::default();
    let mut depth = 0i32;
    while out.len() + 1 < cap {
        let c = peek(s, i);
        if c == 0 {
            return;
        }
        out.push(c);
        i += 1;
        if !q.outside(c) {
            continue;
        }
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
        }
        if depth == 0 {
            return;
        }
    }
}

/// Anything else (a number, true, null…): up to the next , } ] or blank.
fn read_bare(s: &[u8], mut i: usize, cap: usize, out: &mut Vec<u8>) {
    while out.len() + 1 < cap {
        let c = peek(s, i);
        if c == 0 || matches!(c, b',' | b'}' | b']') || blank(c) {
            return;
        }
        out.push(c);
        i += 1;
    }
}

/// The value of a top-level key as text (at most cap - 1 bytes), or None when absent.
pub fn get(json: &[u8], key: &str, cap: usize) -> Option<Vec<u8>> {
    if cap == 0 {
        return None;
    }
    let at = skip_blanks(json, value_start(json, key.as_bytes())?);
    let mut out = Vec::new();
    match peek(json, at) {
        b'"' => read_string(json, at + 1, cap, &mut out),
        b'{' | b'[' => read_container(json, at, cap, &mut out),
        _ => read_bare(json, at, cap, &mut out),
    }
    Some(out)
}

/// get() as a String, invalid UTF-8 replaced.
pub fn get_str(json: &[u8], key: &str, cap: usize) -> Option<String> {
    get(json, key, cap).map(|v| String::from_utf8_lossy(&v).into_owned())
}

/// The value read as a whole number (strtol), or def when absent or not a number.
pub fn get_int(json: &[u8], key: &str, def: i64) -> i64 {
    get(json, key, 64).and_then(|v| strtol(&v)).unwrap_or(def)
}

/// C's isspace.
fn c_space(c: &u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// strtol(s, _, 10): blanks, one sign, decimal digits, clamped to i64. None without a digit.
pub fn strtol(s: &[u8]) -> Option<i64> {
    let t = &s[s.iter().take_while(|c| c_space(c)).count()..];
    let (negative, t) = match t.first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let digits = t.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    // accumulate toward the sign so i64::MIN is reachable; saturate past either end
    Some(t[..digits].iter().fold(0i64, |n, &d| {
        let d = i64::from(d - b'0');
        let n = n.saturating_mul(10);
        if negative { n.saturating_sub(d) } else { n.saturating_add(d) }
    }))
}

/// atoi / atol: strtol, 0 without a number.
pub fn atol(s: &[u8]) -> i64 {
    strtol(s).unwrap_or(0)
}

/// Length of the run of ASCII digits at the start of b.
fn digit_run(b: &[u8]) -> usize {
    b.iter().take_while(|c| c.is_ascii_digit()).count()
}

/// atof / strtod: the longest decimal number at the start (blanks, sign, digits, a point, an
/// exponent), 0 without one. Text that is not UTF-8 reads as 0.
pub fn atof(s: &[u8]) -> f64 {
    let Ok(t) = std::str::from_utf8(s) else { return 0.0 };
    let t = t.trim_start_matches(|c: char| c.is_ascii() && c_space(&(c as u8)));
    let b = t.as_bytes();
    let mut n = usize::from(matches!(b.first(), Some(b'-' | b'+')));
    let whole = digit_run(&b[n..]);
    n += whole;
    let mut frac = 0;
    if b.get(n) == Some(&b'.') {
        frac = digit_run(&b[n + 1..]);
        n += 1 + frac;
    }
    if whole + frac == 0 {
        return 0.0;
    }
    if matches!(b.get(n), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(b.get(n + 1), Some(b'-' | b'+')));
        let exp = digit_run(&b[(n + 1 + sign).min(b.len())..]);
        if exp > 0 {
            n += 1 + sign + exp;
        }
    }
    t[..n].parse().unwrap_or(0.0)
}

/// Steps through an array. From p (at the array's '[', or where the previous call stopped):
/// the next element as text (at most cap - 1 bytes, trailing blanks dropped) and where to go on
/// from, or None after the last one.
pub fn arr_next(s: &[u8], mut p: usize, cap: usize) -> Option<(Vec<u8>, usize)> {
    if cap == 0 {
        return None;
    }
    if peek(s, p) == b'[' {
        p += 1;
    }
    while blank(peek(s, p)) || peek(s, p) == b',' {
        p += 1;
    }
    if matches!(peek(s, p), 0 | b']' | b'}') {
        return None;
    }
    let start = p;
    let mut out = Vec::new();
    let mut q = Quotes::default();
    let mut depth = 0u32;
    loop {
        let c = peek(s, p);
        if c == 0 {
            break;
        }
        let structure = q.outside(c);
        let closer = matches!(c, b'}' | b']');
        if structure {
            // the array's own ']' or the ',' after this element ends it, unread
            if (closer && depth == 0) || (c == b',' && depth == 0) {
                break;
            }
            match c {
                b'{' | b'[' => depth += 1,
                b'}' | b']' => depth -= 1,
                _ => {}
            }
        }
        if out.len() + 1 < cap {
            out.push(c);
        }
        p += 1;
        if structure && closer && depth == 0 {
            break; // a container element ends with its own closer
        }
    }
    let kept = out.iter().rposition(|&c| !blank(c)).map_or(0, |i| i + 1);
    out.truncate(kept);
    (p > start).then_some((out, p))
}

/// Each `{…}` in s outside strings, in order, scanning on after each one's closing brace;
/// stops at one that does not close. Objects nested in another array element count too.
pub fn objects(s: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut from = 0;
    std::iter::from_fn(move || {
        let mut q = Quotes::default();
        let start = (from..).take_while(|&i| peek(s, i) != 0).find(|&i| q.outside(s[i]) && s[i] == b'{')?;
        let mut q = Quotes::default();
        let mut depth = 0u32;
        for i in start.. {
            let c = peek(s, i);
            if c == 0 {
                return None;
            }
            if !q.outside(c) {
                continue;
            }
            match c {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        from = i + 1;
                        return Some(&s[start..=i]);
                    }
                }
                _ => {}
            }
        }
        None
    })
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
    fn caps_and_odd_escapes() {
        let j = "{\"s\":\"ab\\u4e2d\",\"r\":\"ab中\",\"e\":\"\\q\\ud800z\",\"h\":\"x\\u12g4y\"}".as_bytes();
        // an escaped 中 needs 3 bytes: with room for 2 it is left out; written raw it is cut
        assert_eq!(get(j, "s", 5).unwrap(), b"ab");
        assert_eq!(get(j, "s", 6).unwrap(), "ab中".as_bytes());
        assert_eq!(get(j, "r", 5).unwrap(), b"ab\xe4\xb8");
        // an unknown escape is the character itself; a lone surrogate is U+FFFD
        assert_eq!(get(j, "e", 64).unwrap(), "q\u{fffd}z".as_bytes());
        // bad hex ends the string there
        assert_eq!(get(j, "h", 64).unwrap(), b"x");
        assert_eq!(get(j, "s", 1).unwrap(), b"");
        assert!(get(j, "s", 0).is_none());
        // NUL ends the text
        assert_eq!(get(b"{\"a\":\"x\0y\"}", "a", 64).unwrap(), b"x");
    }

    #[test]
    fn numbers_as_c_reads_them() {
        assert_eq!(strtol(b" -12x"), Some(-12));
        assert_eq!(strtol(b"--5"), None);
        assert_eq!(strtol(b"12.9"), Some(12));
        assert_eq!(strtol(b"-9223372036854775809"), Some(i64::MIN));
        assert_eq!(atol(b"null"), 0);
        assert_eq!(atof(b"1000.5"), 1000.5);
        assert_eq!(atof(b" 2e3x"), 2000.0);
        assert_eq!(atof(b"1e"), 1.0);
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
        // cut elements still step past their whole text
        let (e, p) = arr_next(br#"["abcdef",1]"#, 0, 4).unwrap();
        assert_eq!((&e[..], p), (&b"\"ab"[..], 9));
    }

    #[test]
    fn objects_in_a_list() {
        let l = br#"[{"a":"}"},"{",[1,{"b":2}],{"c":{"d":1}},{"open":1"#;
        let got: Vec<&[u8]> = objects(l).collect();
        assert_eq!(got, [&br#"{"a":"}"}"#[..], br#"{"b":2}"#, br#"{"c":{"d":1}}"#]);
    }
}
