//! O2 (decision-execution-loop.md §1.2, §4.7): what bound multi-target HTTPS attempts prove.
//! Pure rules only; the bound transport lives in the core (`o2.rs`).
//!
//! One set of attempts answers two questions:
//! - the application layer: verified TLS and an HTTP status from at least two organisations;
//! - the clock: at least two organisations' `Date` headers agree with the device clock; the
//!   others (a CDN can send a cached, older `Date`: Apple 76 s on the device 10-20) are only
//!   recorded as outliers. A
//!   certificate verifies with a clock minutes off (validity is months long), so the `Date`
//!   arrives over an authenticated connection and can judge the clock without a circle.
//!
//! The vendor's own sync flag never makes the clock trusted; it only records a disagreement.

use super::Check;
use serde::Serialize;

/// How far the device clock and a verified `Date` may differ.
pub const CLOCK_TOLERANCE_S: i64 = 60;
/// How many different organisations must agree.
pub const MIN_ORGS: usize = 2;

/// How one bound attempt ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attempt {
    /// Could not bind to the cellular interface/source: says nothing about the path.
    NoBind,
    /// No usable answer (bind failed, no server, or only non-global addresses such as a
    /// fake-ip 198.18/15): says nothing about the path.
    NoName,
    /// The operator DNS on the bound path said no (error rcode, empty, timed out): the DNS layer.
    DnsFailed,
    /// TCP connect or the handshake timed out / was refused on the bound path.
    Unreachable,
    /// The certificate did not verify (with an untrusted clock this proves nothing).
    TlsRejected,
    /// Verified TLS and an HTTP status line; `date` = the `Date` header as real UTC seconds.
    Verified { date: Option<i64> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    /// Who runs the target; two targets of one organisation count once.
    pub org: &'static str,
    pub attempt: Attempt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockTrust {
    Trusted,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Verdict {
    /// Application-level reachability over the bound path.
    pub app: Check,
    pub clock: ClockTrust,
    /// Largest |device − Date| among the agreeing organisations, or the smallest when none agree.
    pub skew_s: Option<i64>,
    /// Organisations whose `Date` is off by more than the tolerance; for the record.
    pub outliers: Vec<&'static str>,
    /// The vendor said synced and HTTPS disagrees, or the other way round; for the record.
    pub vendor_disagrees: bool,
    pub why: &'static str,
}

/// `device_utc`: the device clock already converted to real UTC (`clock::utc_offset`), None =
/// unreadable. `vendor_synced`: the SNTP/NITZ sync flag, None = unread.
pub fn judge(samples: &[Sample], device_utc: Option<i64>, vendor_synced: Option<bool>) -> Verdict {
    let mut orgs: Vec<&str> = Vec::new();
    let mut dates: Vec<(&'static str, i64)> = Vec::new();
    for s in samples {
        if let Attempt::Verified { date } = s.attempt {
            if !orgs.contains(&s.org) {
                orgs.push(s.org);
            }
            if let Some(d) = date
                && !dates.iter().any(|(o, _)| *o == s.org)
            {
                dates.push((s.org, d));
            }
        }
    }
    let app = if orgs.len() >= MIN_ORGS {
        Check::Pass
    } else if !samples.is_empty()
        && samples
            .iter()
            .all(|s| matches!(s.attempt, Attempt::Unreachable))
    {
        // every target, every organisation, on the bound path: down
        Check::Fail
    } else {
        // includes a rejected certificate: without a trusted clock that can't tell a bad path
        // from a bad clock
        Check::Unknown
    };

    let mut outliers = Vec::new();
    let (clock, skew_s, why) = match device_utc {
        None => (ClockTrust::Unknown, None, "device_clock_unreadable"),
        Some(_) if dates.len() < MIN_ORGS => (ClockTrust::Unknown, None, "too_few_dated_orgs"),
        Some(now) => {
            let skew = |d: i64| (now - d).abs();
            let agree: Vec<i64> = dates
                .iter()
                .filter(|(_, d)| skew(*d) <= CLOCK_TOLERANCE_S)
                .map(|(_, d)| skew(*d))
                .collect();
            outliers = dates
                .iter()
                .filter(|(_, d)| skew(*d) > CLOCK_TOLERANCE_S)
                .map(|(o, _)| *o)
                .collect();
            if agree.len() >= MIN_ORGS {
                (
                    ClockTrust::Trusted,
                    agree.iter().max().copied(),
                    "dates_agree",
                )
            } else {
                let min = dates.iter().map(|(_, d)| skew(*d)).min();
                // two or more remote clocks agreeing with each other but not with ours
                let others_agree = dates.iter().any(|(o, a)| {
                    dates
                        .iter()
                        .any(|(p, b)| o != p && (a - b).abs() <= CLOCK_TOLERANCE_S)
                });
                let why = if others_agree {
                    "device_clock_off"
                } else {
                    "dates_disagree"
                };
                (ClockTrust::Unknown, min, why)
            }
        }
    };
    let vendor_disagrees = matches!(
        (vendor_synced, clock, why),
        (Some(false), ClockTrust::Trusted, _)
            | (Some(true), ClockTrust::Unknown, "device_clock_off")
    );
    Verdict {
        app,
        clock,
        skew_s,
        outliers,
        vendor_disagrees,
        why,
    }
}

/// The DNS layer from the same attempts: Pass when any name resolved over the bound path, Fail
/// when every one was refused by the operator DNS, otherwise unknown.
pub fn dns_layer(samples: &[Sample]) -> Check {
    if samples.iter().any(|s| {
        matches!(
            s.attempt,
            Attempt::Verified { .. } | Attempt::Unreachable | Attempt::TlsRejected
        )
    }) {
        Check::Pass
    } else if !samples.is_empty() && samples.iter().all(|s| s.attempt == Attempt::DnsFailed) {
        Check::Fail
    } else {
        Check::Unknown
    }
}

/// `fault_recheck` for `data.no_internet` from one run: the `dns` and `exit` (application over
/// the cellular exit) layers. Valid only with a trusted clock and every epoch known and the same
/// as when the proposal was made (another card, connection or radio state is another fault).
pub fn recheck(
    samples: &[Sample],
    v: &Verdict,
    then: &super::Epochs,
    now: &super::Epochs,
) -> super::Recheck {
    let known = |e: &super::Epochs| {
        [&e.sim, &e.conn, &e.radio]
            .iter()
            .all(|x| x.as_deref().is_some_and(|s| !s.is_empty()))
    };
    super::Recheck {
        layers: vec![("dns".into(), dns_layer(samples)), ("exit".into(), v.app)],
        valid: v.clock == ClockTrust::Trusted && known(then) && then == now,
    }
}

/// RFC 9110 IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) → UTC seconds. Other forms → None.
pub fn parse_http_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let rest = s.get(5..)?;
    if s.as_bytes().get(3) != Some(&b',') || !rest.ends_with(" GMT") {
        return None;
    }
    let mut it = rest.trim_end_matches(" GMT").split(' ');
    let day: i64 = it.next()?.parse().ok()?;
    let mon = match it.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = it.next()?.parse().ok()?;
    let mut hms = it.next()?.split(':').map(|x| x.parse::<i64>().ok());
    let (h, m, sec) = (hms.next()??, hms.next()??, hms.next()??);
    if it.next().is_some() || !(1..=31).contains(&day) || h > 23 || m > 59 || sec > 60 {
        return None;
    }
    // days from civil (Howard Hinnant)
    let y = if mon <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mon + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3_600 + m * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_792_000_000;

    fn ok(org: &'static str, date: i64) -> Sample {
        Sample {
            org,
            attempt: Attempt::Verified { date: Some(date) },
        }
    }
    fn s(org: &'static str, attempt: Attempt) -> Sample {
        Sample { org, attempt }
    }

    #[test]
    fn two_organisations_agreeing_make_both_pass_and_trusted() {
        let v = judge(
            &[ok("apple", T + 3), ok("tencent", T - 2)],
            Some(T),
            Some(true),
        );
        assert_eq!(
            (v.app, v.clock, v.skew_s),
            (Check::Pass, ClockTrust::Trusted, Some(3))
        );
        assert!(!v.vendor_disagrees);
    }

    #[test]
    fn one_organisation_twice_is_not_two() {
        let v = judge(&[ok("apple", T), ok("apple", T)], Some(T), None);
        assert_eq!((v.app, v.clock), (Check::Unknown, ClockTrust::Unknown));
        assert_eq!(v.why, "too_few_dated_orgs");
    }

    #[test]
    fn a_clock_minutes_off_is_unknown_and_the_vendor_flag_is_recorded() {
        let v = judge(&[ok("a", T), ok("b", T + 1)], Some(T + 300), Some(true));
        assert_eq!(
            (v.app, v.clock, v.why),
            (Check::Pass, ClockTrust::Unknown, "device_clock_off")
        );
        assert!(v.vendor_disagrees);
        let v = judge(&[ok("a", T - 200), ok("b", T + 61)], Some(T), None);
        assert_eq!((v.clock, v.why), (ClockTrust::Unknown, "dates_disagree"));
    }

    #[test]
    fn a_cached_cdn_date_is_an_outlier_not_a_veto() {
        // as on the device 10-20: Apple's edge 76 s behind, Tencent and Baidu on time
        let v = judge(
            &[ok("apple", T - 76), ok("tencent", T), ok("baidu", T + 1)],
            Some(T),
            Some(true),
        );
        assert_eq!(
            (v.clock, v.skew_s, v.why),
            (ClockTrust::Trusted, Some(1), "dates_agree")
        );
        assert_eq!(v.outliers, ["apple"]);
        // one on time, one cached: not two that agree
        let v = judge(&[ok("apple", T - 76), ok("tencent", T)], Some(T), None);
        assert_eq!((v.clock, v.why), (ClockTrust::Unknown, "dates_disagree"));
    }

    #[test]
    fn the_vendor_flag_never_makes_trusted() {
        let v = judge(&[ok("a", T)], Some(T), Some(true));
        assert_eq!(v.clock, ClockTrust::Unknown);
        let v = judge(&[ok("a", T), ok("b", T)], Some(T), Some(false));
        assert_eq!(v.clock, ClockTrust::Trusted);
        assert!(v.vendor_disagrees);
        assert_eq!(
            judge(&[ok("a", T), ok("b", T)], None, Some(true)).clock,
            ClockTrust::Unknown
        );
    }

    #[test]
    fn only_every_target_unreachable_is_a_failure() {
        let down = [s("a", Attempt::Unreachable), s("b", Attempt::Unreachable)];
        assert_eq!(judge(&down, Some(T), None).app, Check::Fail);
        for other in [Attempt::NoBind, Attempt::NoName, Attempt::TlsRejected] {
            let v = judge(
                &[s("a", Attempt::Unreachable), s("b", other)],
                Some(T),
                None,
            );
            assert_eq!(v.app, Check::Unknown);
        }
        assert_eq!(judge(&[], Some(T), None).app, Check::Unknown);
    }

    #[test]
    fn verified_without_a_date_counts_for_the_app_not_the_clock() {
        let v = judge(
            &[s("a", Attempt::Verified { date: None }), ok("b", T)],
            Some(T),
            None,
        );
        assert_eq!((v.app, v.clock), (Check::Pass, ClockTrust::Unknown));
    }

    fn ep(c: &str) -> crate::execution::Epochs {
        crate::execution::Epochs {
            sim: Some("s".into()),
            conn: Some(c.into()),
            radio: Some("r".into()),
        }
    }

    #[test]
    fn a_recheck_is_valid_only_with_a_trusted_clock_and_the_same_epochs() {
        let good = [ok("a", T), ok("b", T)];
        let v = judge(&good, Some(T), None);
        let r = recheck(&good, &v, &ep("c1"), &ep("c1"));
        assert!(r.valid);
        assert_eq!(
            r.layers,
            [("dns".into(), Check::Pass), ("exit".into(), Check::Pass)]
        );
        assert!(
            !recheck(&good, &v, &ep("c1"), &ep("c2")).valid,
            "redialled since"
        );
        let none = crate::execution::Epochs::default();
        assert!(!recheck(&good, &v, &none, &none).valid, "unknown epochs");
        let off = judge(&good, Some(T + 600), None);
        assert!(
            !recheck(&good, &off, &ep("c1"), &ep("c1")).valid,
            "clock not trusted"
        );
    }

    #[test]
    fn the_dns_layer_comes_from_the_same_attempts() {
        let d = |a: Vec<Attempt>| dns_layer(&a.into_iter().map(|a| s("x", a)).collect::<Vec<_>>());
        assert_eq!(d(vec![Attempt::DnsFailed, Attempt::DnsFailed]), Check::Fail);
        assert_eq!(
            d(vec![Attempt::DnsFailed, Attempt::Unreachable]),
            Check::Pass
        );
        assert_eq!(d(vec![Attempt::DnsFailed, Attempt::NoName]), Check::Unknown);
        assert_eq!(d(vec![]), Check::Unknown);
        // names resolve but nothing connects: DNS fine, the exit is down
        let down = [s("a", Attempt::Unreachable), s("b", Attempt::Unreachable)];
        let v = judge(&down, Some(T), None);
        let r = recheck(&down, &v, &ep("c"), &ep("c"));
        assert_eq!(
            r.layers,
            [("dns".into(), Check::Pass), ("exit".into(), Check::Fail)]
        );
        assert!(!r.valid, "no dates, so no trusted clock");
    }

    #[test]
    fn http_dates() {
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(
            parse_http_date("Tue, 20 Oct 2026 00:40:58 GMT"),
            Some(1_792_456_858)
        );
        for bad in [
            "Sunday, 06-Nov-94 08:49:37 GMT",
            "Sun Nov  6 08:49:37 1994",
            "Sun, 06 Nov 1994 08:49:37 UTC",
            "Sun, 32 Nov 1994 08:49:37 GMT",
            "",
        ] {
            assert_eq!(parse_http_date(bad), None, "{bad}");
        }
    }
}
