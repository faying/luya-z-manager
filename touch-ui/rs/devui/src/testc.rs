//! Test helper: the C tests' string constants, read from their headers (the inputs stay one
//! copy, shared with the C tests).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

/// The C source without its comments (string literals kept as they are).
pub fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let (mut i, mut out) = (0, Vec::with_capacity(b.len()));
    while i < b.len() {
        if b[i] == b'"' {
            out.push(b[i]);
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    out.push(b[i]);
                    i += 1;
                }
                out.push(b[i]);
                i += 1;
            }
            out.push(b'"');
            i += 1;
        } else if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            i += 2;
            while i + 1 < b.len() && !b[i..].starts_with(b"*/") {
                i += 1;
            }
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// The C string literals from `from` up to the next ';', joined, escapes decoded.
pub fn literal_at(src: &str, from: usize) -> String {
    let b = src.as_bytes();
    let (mut i, mut out) = (from, Vec::new());
    while i < b.len() && b[i] != b';' {
        if b[i] == b'"' {
            i += 1;
            while b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                    out.push(match b[i] {
                        b'n' => b'\n',
                        b't' => b'\t',
                        c => c,
                    });
                } else {
                    out.push(b[i]);
                }
                i += 1;
            }
        }
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

/// `static const char NAME[] = "…" "…";` from a C header's text.
pub fn c_string(src: &str, name: &str) -> String {
    let src = &strip_comments(src);
    let at = src.find(&format!("{name}[] =")).unwrap_or_else(|| panic!("{name} not found"));
    literal_at(src, at)
}
