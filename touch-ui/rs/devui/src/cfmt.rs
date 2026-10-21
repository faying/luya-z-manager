//! printf for the format strings that come out of the English table: TR("%d 条未读") is a C
//! format at run time, so it cannot be a format!() literal. The subset the UI uses: flags `-0`,
//! width, `.precision`, length letters (ignored), and `d i u x s f c %`. Bytes in, bytes out:
//! `%.16s` cuts at a byte like C does (the C UI then shows a broken glyph; so does this).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

/// One printf argument.
#[derive(Clone, Copy, Debug)]
pub enum Arg<'a> {
    S(&'a [u8]),
    I(i64),
    F(f64),
}

impl<'a> From<&'a str> for Arg<'a> {
    fn from(s: &'a str) -> Self {
        Arg::S(s.as_bytes())
    }
}
impl<'a> From<&'a String> for Arg<'a> {
    fn from(s: &'a String) -> Self {
        Arg::S(s.as_bytes())
    }
}
impl<'a> From<&'a [u8]> for Arg<'a> {
    fn from(s: &'a [u8]) -> Self {
        Arg::S(s)
    }
}
impl<'a> From<&'a Vec<u8>> for Arg<'a> {
    fn from(s: &'a Vec<u8>) -> Self {
        Arg::S(s)
    }
}
macro_rules! int_arg {
    ($($t:ty),*) => {$(impl From<$t> for Arg<'_> {
        fn from(v: $t) -> Self { Arg::I(v as i64) }
    })*};
}
int_arg!(i32, i64, u32, u64, usize, isize, u8, u16, i16);
impl From<f64> for Arg<'_> {
    fn from(v: f64) -> Self {
        Arg::F(v)
    }
}

/// `cf!(fmt, a, b)`: printf into a byte string.
#[macro_export]
macro_rules! cf {
    ($fmt:expr $(, $a:expr)* $(,)?) => {
        $crate::cfmt::cfmt(AsRef::<[u8]>::as_ref($fmt), &[$($crate::cfmt::Arg::from($a)),*])
    };
}

fn pad(out: &mut Vec<u8>, body: &[u8], width: usize, left: bool, zero: bool) {
    let fill = width.saturating_sub(body.len());   // C counts bytes
    if left {
        out.extend_from_slice(body);
        out.extend(std::iter::repeat_n(b' ', fill));
    } else if zero && fill > 0 && (body.first() == Some(&b'-') || body.first() == Some(&b'+')) {
        out.push(body[0]);
        out.extend(std::iter::repeat_n(b'0', fill));
        out.extend_from_slice(&body[1..]);
    } else {
        out.extend(std::iter::repeat_n(if zero { b'0' } else { b' ' }, fill));
        out.extend_from_slice(body);
    }
}

/// printf(fmt, args) for the subset above; a missing argument prints nothing.
pub fn cfmt(fmt: &[u8], args: &[Arg]) -> Vec<u8> {
    let mut out = Vec::with_capacity(fmt.len() + 16);
    let mut args = args.iter();
    let mut i = 0;
    while i < fmt.len() {
        let c = fmt[i];
        i += 1;
        if c != b'%' {
            out.push(c);
            continue;
        }
        let (mut left, mut zero, mut plus) = (false, false, false);
        while i < fmt.len() && matches!(fmt[i], b'-' | b'0' | b'+' | b' ' | b'#') {
            match fmt[i] {
                b'-' => left = true,
                b'0' => zero = true,
                b'+' => plus = true,
                _ => {}
            }
            i += 1;
        }
        let mut width = 0usize;
        while i < fmt.len() && fmt[i].is_ascii_digit() {
            width = width * 10 + (fmt[i] - b'0') as usize;
            i += 1;
        }
        let mut prec: Option<usize> = None;
        if i < fmt.len() && fmt[i] == b'.' {
            i += 1;
            let mut p = 0usize;
            if i < fmt.len() && fmt[i] == b'*' {
                i += 1;
                if let Some(Arg::I(v)) = args.next() {
                    p = (*v).max(0) as usize;
                }
            }
            while i < fmt.len() && fmt[i].is_ascii_digit() {
                p = p * 10 + (fmt[i] - b'0') as usize;
                i += 1;
            }
            prec = Some(p);
        }
        while i < fmt.len() && matches!(fmt[i], b'l' | b'h' | b'z' | b'j' | b't' | b'L') {
            i += 1;
        }
        let Some(&conv) = fmt.get(i) else { break };
        i += 1;
        if conv == b'%' {
            out.push(b'%');
            continue;
        }
        let a = args.next().copied();
        let body: Vec<u8> = match (conv, a) {
            (b's', Some(Arg::S(s))) => s[..prec.map_or(s.len(), |p| p.min(s.len()))].to_vec(),
            (b'd' | b'i', Some(Arg::I(v))) => {
                if plus && v >= 0 { format!("+{v}") } else { v.to_string() }.into_bytes()
            }
            (b'u', Some(Arg::I(v))) => (v as u64 & if v < 0 { u32::MAX as u64 } else { u64::MAX }).to_string().into_bytes(),
            (b'x', Some(Arg::I(v))) => format!("{v:x}").into_bytes(),
            (b'X', Some(Arg::I(v))) => format!("{v:X}").into_bytes(),
            (b'c', Some(Arg::I(v))) => vec![v as u8],
            (b'f', Some(Arg::F(v))) => format!("{:.*}", prec.unwrap_or(6), v).into_bytes(),
            (b'f', Some(Arg::I(v))) => format!("{:.*}", prec.unwrap_or(6), v as f64).into_bytes(),
            _ => Vec::new(),
        };
        pad(&mut out, &body, width, left, zero && !left && conv != b's');
    }
    out
}

#[cfg(test)]
mod tests {
    fn s(v: Vec<u8>) -> String {
        String::from_utf8(v).unwrap()
    }

    #[test]
    fn the_formats_the_ui_uses() {
        assert_eq!(s(cf!("%02d:%02d", 9, 5)), "09:05");
        assert_eq!(s(cf!("%.16s · %d 台", "U60-Test", 6)), "U60-Test · 6 台");
        assert_eq!(s(cf!("%.4s********%s", "490154203237518", "518")), "4901********518");
        assert_eq!(s(cf!("%ld%%", 15i64)), "15%");
        assert_eq!(s(cf!("%.1fMB", 12.34)), "12.3MB");
        assert_eq!(s(cf!("%.0fK%s", 183.6, "b")), "184Kb");
        assert_eq!(s(cf!("%u", 7u32)), "7");
        assert_eq!(s(cf!("%5d|%-5d|", 42, 42)), "   42|42   |");
    }

    #[test]
    fn precision_cuts_bytes_like_c() {
        // "中" is 3 bytes: %.4s keeps it and one byte of the next
        assert_eq!(cf!("%.4s", "中国"), b"\xe4\xb8\xad\xe5".to_vec());
    }
}
