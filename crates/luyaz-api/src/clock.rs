// ─────────────────────────────────────────────────────────────────────────────
// clock (the pure part, moved from zte-agent `src/clock.rs`, Phase 4 step 8a; reading uci and
// caching stay with each caller: see [`utc_offset`]) — what the device's clock actually means.
//
// ZTE's SNTP client (zwrt_zte_sntp) sets the system clock to *local wall time*
// and leaves the system time zone at UTC. So `date` prints "14:34 UTC" at
// 14:34 in Beijing, and every epoch the device produces (time(), file mtimes,
// dnsmasq leases, our own logs) is ahead of real UTC by the configured zone —
// measured 2026-09-23: 28 801 s ahead with time_from_utc='8.00'.
//
// We do not "fix" that: the vendor daemons (SMS store, traffic day/month
// boundaries) are built around it. Instead:
//  * on the device, local-time formatting (localtime_r under TZ=UTC) already
//    gives the right wall-clock digits — keep doing that;
//  * anything that crosses to a browser or a phone carries `utc_offset`, the
//    number of seconds the device clock runs ahead of UTC, so the other side
//    can line device times up with its own clock.
// ─────────────────────────────────────────────────────────────────────────────

// Shared with the independent sentinel without bringing login dependencies into it.
pub use luyaz_alerts::clock::*;

/// "8.00" / "5.50" / "-3.50" → seconds. Half- and quarter-hour zones exist.
pub fn parse_hours(s: &str) -> Option<i64> {
    let h: f64 = s.trim().parse().ok()?;
    if !(-14.0..=14.0).contains(&h) {
        return None;
    }
    Some((h * 3600.0).round() as i64)
}

/// The zone the system clock itself is in (0 on this firmware: TZ=UTC).
pub fn system_gmtoff() -> i64 {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm.tm_gmtoff as i64
    }
}

/// `sntp_zone`: ZTE's configured zone in seconds; `_gmtoff`: the system TZ's.
/// On ZTE firmware, `zwrt_zte_sntp` writes local wall time (`UTC + sntp_zone`) into the system
/// clock (`CLOCK_REALTIME`) even if a process or `/etc/TZ` sets a non-zero `tm_gmtoff`. Never
/// infer from `gmtoff != 0` that the system clock is plain UTC.
pub fn offset_from(sntp_zone: Option<i64>, _gmtoff: i64) -> i64 {
    sntp_zone.unwrap_or(0)
}

/// Local display/SMS zone in seconds ahead of real UTC: ZTE's configured `sntp_zone` when
/// present, otherwise `gmtoff` (never `sntp_zone + gmtoff`, which would double-add `+8h` when
/// `TZ` is non-zero while `zwrt_zte_sntp` still writes local wall time).
pub fn zone_from(sntp_zone: Option<i64>, gmtoff: i64) -> i64 {
    sntp_zone.unwrap_or(gmtoff)
}

/// Decompose device `secs` into `(hour, minute, weekday 0 = Monday)`.
/// When `sntp_zone` is non-zero (`LocalWallAsUtc`), `secs` already holds local wall time in the
/// UTC epoch, so `gmtime_r(&secs)` extracts the wall digits without letting a non-zero
/// `tm_gmtoff` shift them a second time. When no SNTP zone is configured (`None` or `Some(0)`),
/// `secs + gmtoff` gives local wall time.
pub fn local_hm_dow(secs: i64, sntp_zone: Option<i64>, gmtoff: i64) -> Option<(u8, u8, u8)> {
    let wall = if sntp_zone.is_some_and(|z| z != 0) {
        secs
    } else {
        secs.checked_add(gmtoff)?
    };
    // libc marks musl's time_t deprecated (its width changed in musl 1.2); 64-bit on this target
    #[allow(deprecated)]
    let t = wall as libc::time_t;
    // SAFETY: gmtime_r only writes the tm we pass it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::gmtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    let dow = ((tm.tm_wday + 6) % 7) as u8;
    Some((tm.tm_hour as u8, tm.tm_min as u8, dow))
}

/// Seconds the device clock runs ahead of real UTC, from ZTE's two uci settings
/// (`zwrt_zte_sntp.settings.time_from_utc`, then `.timezone`).
pub fn utc_offset(time_from_utc: Option<&str>, timezone: Option<&str>) -> i64 {
    let zone = time_from_utc
        .and_then(parse_hours)
        .or_else(|| timezone.and_then(parse_hours));
    offset_from(zone, system_gmtoff())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_requires_the_same_boot_and_never_uses_wall_time() {
        let a = BootTime {
            boot_id: "a".into(),
            seconds: 0,
        };
        let later = BootTime {
            seconds: 60,
            ..a.clone()
        };
        assert_eq!(later.elapsed_since(&a), Some(60));
        assert_eq!(a.elapsed_since(&later), None);
        assert_eq!(
            later.elapsed_since(&BootTime {
                boot_id: "b".into(),
                seconds: 0
            }),
            None
        );
        assert_eq!(
            a.elapsed_since(&BootTime {
                boot_id: String::new(),
                seconds: 0
            }),
            None
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn boot_clock_matches_linux_uptime() {
        let start = BootTime::read().expect("Linux boot clock");
        let uptime: f64 = std::fs::read_to_string("/proc/uptime")
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let end = BootTime::read().unwrap();
        assert_eq!(start.boot_id, end.boot_id);
        assert!(uptime >= start.seconds as f64 && uptime < (end.seconds + 1) as f64);
    }

    #[test]
    fn hours_parse() {
        assert_eq!(parse_hours("8.00"), Some(28800));
        assert_eq!(parse_hours("8"), Some(28800));
        assert_eq!(parse_hours("5.50"), Some(19800));
        assert_eq!(parse_hours("-3.50"), Some(-12600));
        assert_eq!(parse_hours(""), None);
        assert_eq!(parse_hours("99"), None);
    }

    #[test]
    fn offset_and_local_digits_do_not_trust_nonzero_tz_over_vendor_wall_clock() {
        assert_eq!(offset_from(Some(28800), 0), 28800);
        // Even if TZ is non-zero, zwrt_zte_sntp still writes local wall time to CLOCK_REALTIME.
        assert_eq!(offset_from(Some(28800), 28800), 28800);
        assert_eq!(offset_from(Some(28800), -18000), 28800);
        assert_eq!(offset_from(None, 0), 0);
        assert_eq!(offset_from(None, 28800), 0);

        assert_eq!(zone_from(Some(28800), 0), 28800);
        assert_eq!(zone_from(Some(28800), 28800), 28800);
        assert_eq!(zone_from(None, 28800), 28800);
        assert_eq!(zone_from(None, 0), 0);

        // 2026-10-12 03:00:00 stored as UTC epoch (Monday).
        let mon_3 = 1_791_774_000;
        assert_eq!(local_hm_dow(mon_3, Some(28800), 0), Some((3, 0, 0)));
        assert_eq!(
            local_hm_dow(mon_3, Some(28800), 28800),
            Some((3, 0, 0)),
            "non-zero TZ must not shift ZTE local-wall-as-UTC a second time to 11:00"
        );
        assert_eq!(local_hm_dow(mon_3, Some(28800), -18000), Some((3, 0, 0)));
        assert_eq!(local_hm_dow(mon_3, None, 0), Some((3, 0, 0)));
        assert_eq!(local_hm_dow(mon_3 - 28800, None, 28800), Some((3, 0, 0)));
    }
}
