//! Text helpers: the firmware's UCS-2 hex strings, its `YY,MM,DD,HH,MM,SS,±TZ` dates, and the
//! device clock.

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// True for a non-empty run of hex digits whose length is a multiple of four (one UTF-16 unit
/// per four digits), i.e. how the firmware hands out numbers and texts.
pub(crate) fn is_ucs2_hex(s: &str) -> bool {
    let t = s.trim().as_bytes();
    !t.is_empty() && t.len().is_multiple_of(4) && t.iter().all(u8::is_ascii_hexdigit)
}

/// UCS-2 hex back to text. Each four-digit group is one UTF-16 unit; groups that are not hex
/// or not a character on their own (lone surrogates) are dropped, a short tail is ignored.
pub(crate) fn decode_ucs2_hex(hex: &str) -> String {
    let (quads, _tail) = hex.trim().as_bytes().as_chunks::<4>();
    quads
        .iter()
        .filter_map(|quad| std::str::from_utf8(quad).ok())
        .filter_map(|quad| u16::from_str_radix(quad, 16).ok())
        .filter_map(|unit| char::from_u32(u32::from(unit)))
        .collect()
}

/// Text as UTF-16BE hex, upper case (what the send call wants with `encode_type` UNICODE).
pub(crate) fn encode_ucs2_hex(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 4);
    for unit in text.encode_utf16() {
        out.push_str(&format!("{unit:04X}"));
    }
    out
}

/// A device date as people read it, e.g. `Sep 25, 2026 1:20 PM UTC+8`; anything that does not
/// parse is returned unchanged.
pub(crate) fn humanize_zte_date(raw: &str) -> String {
    let fields: Vec<&str> = raw.split([',', ';']).map(str::trim).collect();
    let pretty = (fields.len() >= 6)
        .then(|| {
            let num = |i: usize| fields[i].parse::<u32>().ok();
            let (year, month, day, hour, minute) = (num(0)?, num(1)?, num(2)?, num(3)?, num(4)?);
            let name = MONTHS.get((month as usize).checked_sub(1)?)?;
            let half = if hour < 12 { "AM" } else { "PM" };
            let clock_hour = match hour {
                0 => 12,
                1..=12 => hour,
                pm => pm - 12,
            };
            let zone = match fields.get(6) {
                Some(z) if !z.is_empty() => format!(" UTC{z}"),
                _ => String::new(),
            };
            Some(format!(
                "{name} {day}, 20{year:02} {clock_hour}:{minute:02} {half}{zone}"
            ))
        })
        .flatten();
    pretty.unwrap_or_else(|| raw.to_string())
}

/// Now as a device SMS date, `YY;MM;DD;HH;MM;SS;±Z`. The digits come from `localtime_r` (the
/// clock already holds local time); `zone_hours` is the SNTP zone the host reports, because
/// `tm_gmtoff` reads 0 on this firmware.
pub(crate) fn format_sms_time(zone_hours: i64) -> String {
    // SAFETY: `time(NULL)` only returns; `localtime_r` fills the zeroed `tm` we own.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut out: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut out);
        out
    };
    let plus = if zone_hours < 0 { "" } else { "+" };
    let day = [tm.tm_year % 100, tm.tm_mon + 1, tm.tm_mday];
    let time = [tm.tm_hour, tm.tm_min, tm.tm_sec];
    let mut s = String::new();
    for p in day.into_iter().chain(time) {
        s.push_str(&format!("{p:02};"));
    }
    s.push_str(&format!("{plus}{zone_hours}"));
    s
}

/// At most `max` bytes of `s` (cut on a character edge), with `...` when something was cut.
pub(crate) fn preview(s: &str, max: usize) -> String {
    match s.len() > max {
        true => format!("{}...", &s[..s.floor_char_boundary(max)]),
        false => s.to_owned(),
    }
}

/// Device clock, unix seconds (local time labelled UTC).
pub(crate) fn now_ts() -> i64 {
    // SAFETY: a null argument just returns the time.
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    t as i64
}
