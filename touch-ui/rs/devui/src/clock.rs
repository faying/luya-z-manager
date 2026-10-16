//! Wall-clock fields without a time-zone database: the device clock is local time labelled
//! UTC (ZTE SNTP) and the process runs without TZ, so C's localtime() is a UTC breakdown.
// SPDX-License-Identifier: MIT

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tm {
    pub year: i32,
    /// 1–12
    pub mon: i32,
    pub mday: i32,
    pub hour: i32,
    pub min: i32,
    pub sec: i32,
    /// 0–365
    pub yday: i32,
}

/// localtime_r with TZ unset.
pub fn local(t: i64) -> Tm {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400) as i32;
    // civil from days (H. Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let mday = (doy - (153 * mp + 2) / 5 + 1) as i32;
    let mon = if mp < 10 { mp + 3 } else { mp - 9 } as i32;
    let year = (yoe + era * 400 + if mon <= 2 { 1 } else { 0 }) as i32;
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    const CUM: [i32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let yday = CUM[(mon - 1) as usize] + mday - 1 + if leap && mon > 2 { 1 } else { 0 };
    Tm { year, mon, mday, hour: secs / 3600, min: secs % 3600 / 60, sec: secs % 60, yday }
}

const MON: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// strftime for the conversions the UI uses: %H %M %S %m %d %b %Y %%.
pub fn strftime(fmt: &str, tm: &Tm) -> String {
    let mut out = String::new();
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('H') => out += &format!("{:02}", tm.hour),
            Some('M') => out += &format!("{:02}", tm.min),
            Some('S') => out += &format!("{:02}", tm.sec),
            Some('m') => out += &format!("{:02}", tm.mon),
            Some('d') => out += &format!("{:02}", tm.mday),
            Some('b') => out += MON[(tm.mon - 1) as usize],
            Some('Y') => out += &tm.year.to_string(),
            Some('%') => out.push('%'),
            Some(o) => {
                out.push('%');
                out.push(o);
            }
            None => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_breakdown() {
        // the render test's clock: 1790250112 = 2026-09-24 11:41:52
        let tm = local(1790250112);
        assert_eq!(tm, Tm { year: 2026, mon: 9, mday: 24, hour: 11, min: 41, sec: 52, yday: 266 });
        assert_eq!(strftime("%H:%M", &tm), "11:41");
        assert_eq!(strftime("%m-%d", &tm), "09-24");
        assert_eq!(strftime("%d %b", &tm), "24 Sep");
        assert_eq!(local(0), Tm { year: 1970, mon: 1, mday: 1, hour: 0, min: 0, sec: 0, yday: 0 });
        assert_eq!(local(951782400).yday, 59); // 2000-02-29
        assert_eq!(local(1704067199), Tm { year: 2023, mon: 12, mday: 31, hour: 23, min: 59, sec: 59, yday: 364 });
    }
}
