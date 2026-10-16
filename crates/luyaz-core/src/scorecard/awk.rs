//! The few awk behaviours the scorecard's numbers and words depend on, as busybox awk does them
//! (probed in the busybox container, 2026-10-10): field extraction by regex, string-to-number,
//! the `isn` test, and the comparison rules (a string from `substr`/`fld` against a number is
//! compared as text; the empty string sorts below everything).

/// `fld(l, name)` of doctor's REPORT_AWK: the value after the first `"name":`, a quoted string
/// (quotes dropped, no escapes) or the bare text up to `,` or `}`; "" when absent. Of the two
/// alternatives the longer match wins (POSIX leftmost-longest), as in the awk.
pub fn fld<'a>(l: &'a str, name: &str) -> &'a str {
    let key = format!("\"{name}\":");
    let Some(at) = l.find(&key) else { return "" };
    let rest = &l[at + key.len()..];
    let bare = rest.find([',', '}']).unwrap_or(rest.len());
    let quoted = rest
        .strip_prefix('"')
        .and_then(|r| r.find('"'))
        .map(|e| e + 2);
    let n = quoted.map_or(bare, |q| q.max(bare));
    let v = &rest[..n];
    if v.starts_with('"') {
        // substr(v, 2, length(v) - 2): the first and the last character go, whatever they are
        let mut c = v.char_indices();
        c.next();
        let start = c.next().map_or(v.len(), |(i, _)| i);
        let end = v.char_indices().last().map_or(0, |(i, _)| i);
        if end > start { &v[start..end] } else { "" }
    } else {
        v
    }
}

/// awk's string-to-number: the longest numeric prefix after blanks (decimal with an exponent, or
/// busybox's 0x hex); 0 when there is none.
pub fn num(s: &str) -> f64 {
    let t = s.trim_start_matches([' ', '\t', '\n']);
    let b = t.as_bytes();
    let mut i = 0;
    let neg = match b.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };
    if b.len() > i + 2 && b[i] == b'0' && (b[i + 1] | 0x20) == b'x' {
        let h: String = t[i + 2..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        if !h.is_empty() {
            let v = u64::from_str_radix(&h, 16).map_or(f64::MAX, |v| v as f64);
            return if neg { -v } else { v };
        }
    }
    let mut j = i;
    while j < b.len() && b[j].is_ascii_digit() {
        j += 1;
    }
    if j < b.len() && b[j] == b'.' {
        j += 1;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
    }
    if j == i || (j == i + 1 && b[i] == b'.') {
        return 0.0;
    }
    let mut e = j;
    if e < b.len() && (b[e] | 0x20) == b'e' {
        let mut k = e + 1;
        if k < b.len() && (b[k] == b'-' || b[k] == b'+') {
            k += 1;
        }
        let d = k;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        if k > d {
            e = k;
        }
    }
    t[..e].parse::<f64>().unwrap_or(0.0)
}

/// `isn(x)`: `^-?[0-9]+(\.[0-9]+)?$`.
pub fn isn(x: &str) -> bool {
    let x = x.strip_prefix('-').unwrap_or(x);
    let (a, b) = match x.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (x, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    digits(a) && b.is_none_or(digits)
}

/// Whether a value from getline/split/-v ("strnum") is taken as a number by busybox awk.
pub fn looks_numeric(s: &str) -> bool {
    let t = s.trim_matches([' ', '\t', '\n']);
    let b = t.strip_prefix(['-', '+']).unwrap_or(t);
    if b.len() > 2 && b[..2].eq_ignore_ascii_case("0x") {
        return b[2..].bytes().all(|c| c.is_ascii_hexdigit());
    }
    !b.is_empty()
        && b.bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'-' | b'+'))
        && b.parse::<f64>().is_ok()
}

/// A value that is a number or awk's empty string (`wall()` and friends return "" when unknown).
pub type Num = Option<f64>;

/// `a > b` with `a` possibly "": "" against a number is text, and "" sorts first.
pub fn gt(a: Num, b: f64) -> bool {
    a.is_some_and(|a| a > b)
}
/// `a < b` with `a` possibly "".
pub fn lt(a: Num, b: f64) -> bool {
    a.is_none_or(|a| a < b)
}
/// `a <= b` with `a` possibly "".
pub fn le(a: Num, b: f64) -> bool {
    a.is_none_or(|a| a <= b)
}

/// `int(x)`, and `%d` of a number: toward zero.
pub fn int(x: f64) -> i64 {
    x.trunc() as i64
}

/// `printf "%.Nf"`: Rust formats from the exact binary value, ties to even, as musl's printf.
pub fn fixed(x: f64, n: usize) -> String {
    format!("{x:.n$}")
}

/// busybox `mktime("YYYY MM DD HH MM SS")`, the device's zone being UTC: -1 unless six numbers.
pub fn mktime(s: &str) -> f64 {
    let f: Vec<&str> = s.split_whitespace().collect();
    if f.len() < 6 {
        return -1.0;
    }
    let mut v = [0i64; 6];
    for (i, x) in f.iter().take(6).enumerate() {
        let d = x.strip_prefix(['-', '+']).unwrap_or(x);
        let n: String = d.chars().take_while(char::is_ascii_digit).collect();
        if n.is_empty() {
            return -1.0;
        }
        let n: i64 = n.parse().unwrap_or(0);
        v[i] = if x.starts_with('-') { -n } else { n };
    }
    // normalise the month as mktime does
    let m0 = v[1] - 1;
    let y = v[0] + m0.div_euclid(12);
    let m = m0.rem_euclid(12) + 1;
    let days = days_from_civil(y, m, 1) + v[2] - 1;
    (days * 86_400 + v[3] * 3600 + v[4] * 60 + v[5]) as f64
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `strftime("%Y-%m-%d %H:%M", x)` in the device's zone (UTC: the device keeps local time as UTC).
pub fn ymd_hm(x: f64) -> String {
    let c = crate::ops::record::civil(x.max(0.0) as u64);
    c[..16].to_string()
}

/// `strftime("%m-%d %H:%M", x)`.
pub fn md_hm(x: f64) -> String {
    let c = crate::ops::record::civil(x.max(0.0) as u64);
    c[5..16].to_string()
}

/// The lines `getline` gives from a file's text (no empty line after a final newline).
pub fn lines(text: &str) -> impl Iterator<Item = &str> {
    let t = text.strip_suffix('\n').unwrap_or(text);
    t.split('\n').filter(move |_| !text.is_empty())
}

/// `split(l, a, " ")`: awk's default field splitting (blanks, leading/trailing dropped).
pub fn split_blank(l: &str) -> Vec<&str> {
    l.split([' ', '\t', '\n'])
        .filter(|s| !s.is_empty())
        .collect()
}
