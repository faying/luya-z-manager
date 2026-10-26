//! Clock evidence shared by the sentinel and core. This module never sets the clock or probes
//! the network. A plausible local wall clock is an estimate, not evidence of NTP or correct UTC.

use serde::{Deserialize, Serialize};

/// Elapsed seconds since this boot, including suspend. Never compare these with Unix time.
/// The boot identity makes persisted deadlines safe across core restarts and device reboots.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BootTime {
    pub boot_id: String,
    pub seconds: u64,
}

impl BootTime {
    /// Linux is the device platform. Unsupported/unreadable clocks remain unknown rather than
    /// falling back to wall time (which the vendor may step by eight hours).
    pub fn read() -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            use std::sync::OnceLock;
            static BOOT: OnceLock<Option<String>> = OnceLock::new();
            let boot_id = BOOT
                .get_or_init(|| {
                    let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
                    let id = id.trim();
                    (!id.is_empty()).then(|| id.to_owned())
                })
                .as_ref()?
                .clone();
            let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
            if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } != 0 || ts.tv_sec < 0 {
                return None;
            }
            Some(Self {
                boot_id,
                seconds: ts.tv_sec as u64,
            })
        }
        #[cfg(not(target_os = "linux"))]
        None
    }

    /// Parse the existing proc snapshot (including host-root fixtures); unreadable is not zero.
    pub fn from_uptime(boot_id: &str, uptime: &str) -> Option<Self> {
        let boot_id = boot_id.trim();
        if boot_id.is_empty() {
            return None;
        }
        let token = uptime.split_whitespace().next()?;
        let (whole, fraction) = token.split_once('.').unwrap_or((token, "0"));
        if whole.is_empty()
            || fraction.is_empty()
            || !whole
                .bytes()
                .chain(fraction.bytes())
                .all(|b| b.is_ascii_digit())
        {
            return None;
        }
        Some(Self {
            boot_id: boot_id.into(),
            seconds: whole.parse().ok()?,
        })
    }

    /// None means different/unknown boot identity or an invalid backwards elapsed clock.
    pub fn elapsed_since(&self, earlier: &Self) -> Option<u64> {
        if self.boot_id.is_empty() || self.boot_id != earlier.boot_id {
            return None;
        }
        self.seconds.checked_sub(earlier.seconds)
    }
}

/// Three existing 60-second worker rounds; expiry includes suspend and never wakes the device.
pub const EVIDENCE_MAX_AGE_S: u64 = 180;
/// Allow second rounding and a small slew, never an hour-scale step.
pub const WALL_STEP_TOLERANCE_S: u64 = 5;
pub const CLOCK_YEAR_MIN: i64 = 1_767_225_600;
pub const CLOCK_OK: &str = "/tmp/luyaz/clock-ok";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockModel {
    Utc,
    LocalWallAsUtc,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    Estimated,
    Verified,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockSource {
    Year,
    LastWall,
    Jump,
    Continuity,
    TrustedGateway,
    Revoked,
    #[default]
    #[serde(other)]
    None,
}

impl ClockSource {
    pub fn how(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Year => "year",
            Self::LastWall => "last_wall",
            Self::Jump => "jump",
            Self::Continuity => "continuity",
            Self::TrustedGateway => "trusted_gateway",
            Self::Revoked => "revoked",
        }
    }
}

/// The v2 record deliberately leaves UTC, its error bound, the firmware clock model and zone
/// unknown until an independent, firmware-appropriate source can establish them. Neither TZ
/// nor a plausible year establishes any of those facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockEvidence {
    pub version: u8,
    pub raw_wall: i64,
    pub observed_at: BootTime,
    pub monotonic: Option<u64>,
    pub clock_model: ClockModel,
    pub display_zone: Option<i64>,
    pub utc_estimate: Option<i64>,
    pub uncertainty_s: Option<u64>,
    pub source: ClockSource,
    pub sync_state: SyncState,
    /// Root-provisioned attestation; independent source expiry must never slide per round.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc_reference: Option<crate::utc::UtcAnchor>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
    Estimated,
    Verified,
    Stale,
    BootMismatch,
    WallChanged,
    Revoked,
    #[default]
    #[serde(other)]
    Unknown,
}

impl EvidenceStatus {
    /// Existing local-wall accounting/SMS policy may use an estimate. This never authorizes
    /// UTC-sensitive operations, certificate bypasses or any recovery action.
    pub fn wall_usable(self) -> bool {
        matches!(self, Self::Estimated | Self::Verified)
    }
}

impl ClockEvidence {
    pub fn observation(
        raw_wall: i64,
        observed_at: BootTime,
        monotonic: Option<u64>,
        source: ClockSource,
    ) -> Self {
        Self {
            version: 2,
            raw_wall,
            observed_at,
            monotonic,
            clock_model: ClockModel::Unknown,
            display_zone: None,
            utc_estimate: None,
            uncertainty_s: None,
            source,
            utc_reference: None,
            sync_state: if matches!(source, ClockSource::None | ClockSource::Revoked) {
                SyncState::Unknown
            } else {
                SyncState::Estimated
            },
        }
    }

    pub fn offset(&self) -> Option<i64> {
        self.raw_wall
            .checked_sub(i64::try_from(self.observed_at.seconds).ok()?)
    }

    pub fn status(&self, now: Option<&BootTime>, raw_wall: i64) -> EvidenceStatus {
        let Some(now) = now else {
            return EvidenceStatus::Unknown;
        };
        if self.version != 2 || self.observed_at.boot_id.is_empty() || now.boot_id.is_empty() {
            return EvidenceStatus::Unknown;
        }
        if now.boot_id != self.observed_at.boot_id {
            return EvidenceStatus::BootMismatch;
        }
        let Some(age) = now.elapsed_since(&self.observed_at) else {
            return EvidenceStatus::Stale;
        };
        if age > EVIDENCE_MAX_AGE_S {
            return EvidenceStatus::Stale;
        }
        let expected = i64::try_from(age)
            .ok()
            .and_then(|s| self.raw_wall.checked_add(s));
        if expected.is_none_or(|w| w.abs_diff(raw_wall) > WALL_STEP_TOLERANCE_S) {
            return EvidenceStatus::WallChanged;
        }
        if self.source == ClockSource::Revoked {
            return EvidenceStatus::Revoked;
        }
        if self.raw_wall < CLOCK_YEAR_MIN || self.source == ClockSource::None {
            return EvidenceStatus::Unknown;
        }
        match self.sync_state {
            SyncState::Unknown => EvidenceStatus::Unknown,
            SyncState::Estimated => EvidenceStatus::Estimated,
            SyncState::Verified
                if self.source == ClockSource::TrustedGateway
                    && self.utc_reference.as_ref().is_some_and(|anchor| {
                        anchor.project(now, raw_wall).is_some_and(|p| {
                            p.clock_model == self.clock_model
                                && self.display_zone == Some(anchor.display_zone)
                                && Some(p.utc_estimate)
                                    == self
                                        .utc_estimate
                                        .and_then(|u| u.checked_add(i64::try_from(age).ok()?))
                                && self.uncertainty_s
                                    == anchor
                                        .project(&self.observed_at, self.raw_wall)
                                        .map(|p| p.uncertainty_s)
                        })
                    }) =>
            {
                EvidenceStatus::Verified
            }
            SyncState::Verified => EvidenceStatus::Unknown,
        }
    }

    fn header(&self) -> String {
        let usable = self
            .status(Some(&self.observed_at), self.raw_wall)
            .wall_usable();
        format!(
            "{} {} {} {}",
            u8::from(usable),
            self.offset().unwrap_or(0),
            self.observed_at.seconds,
            self.source.how()
        )
    }

    /// One atomic file: old readers still see their original first line. New readers require
    /// the boot-bound second line and an exactly matching header; a bare legacy '1' is unknown.
    pub fn encode(&self) -> String {
        format!(
            "{}\n{}\n",
            self.header(),
            serde_json::to_string(self).expect("clock evidence")
        )
    }

    pub fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let header = lines.next()?;
        let e: Self = serde_json::from_str(lines.next()?).ok()?;
        if lines.next().is_some()
            || e.version != 2
            || e.observed_at.boot_id.is_empty()
            || e.offset().is_none()
            || header != e.header()
        {
            return None;
        }
        Some(e)
    }
}

/// A failed, legacy or malformed read has no usable evidence. Caller supplies its own clock
/// snapshot so tests and the core's host-root fixtures never fall back to the real host's boot.
pub fn evidence_status(text: &str, now: Option<&BootTime>, raw_wall: i64) -> EvidenceStatus {
    ClockEvidence::decode(text).map_or(EvidenceStatus::Unknown, |e| e.status(now, raw_wall))
}

/// Usable and stepped past the wall the device booted with: `last_wall` alone is only "not
/// earlier than before" and on this firmware can be hours off until SNTP steps it.
pub fn evidence_settled(text: &str, now: Option<&BootTime>, raw_wall: i64) -> bool {
    ClockEvidence::decode(text).is_some_and(|e| {
        e.source != ClockSource::LastWall && e.status(now, raw_wall).wall_usable()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const WALL: i64 = 1_790_000_000;
    fn at(boot: &str, seconds: u64) -> BootTime {
        BootTime {
            boot_id: boot.into(),
            seconds,
        }
    }
    fn estimate() -> ClockEvidence {
        ClockEvidence::observation(WALL, at("a", 0), Some(0), ClockSource::Year)
    }

    #[test]
    fn plausibility_never_claims_utc_or_sync() {
        let e = estimate();
        assert_eq!(e.status(Some(&at("a", 0)), WALL), EvidenceStatus::Estimated);
        assert_eq!(e.sync_state, SyncState::Estimated);
        assert_eq!(e.clock_model, ClockModel::Unknown);
        assert_eq!(
            (e.utc_estimate, e.uncertainty_s, e.display_zone),
            (None, None, None)
        );
        let mut invalid_verified = e.clone();
        invalid_verified.sync_state = SyncState::Verified;
        assert_eq!(
            invalid_verified.status(Some(&at("a", 0)), WALL),
            EvidenceStatus::Unknown
        );
    }

    #[test]
    fn freshness_counts_sleep_and_rejects_future_and_other_boots() {
        let e = estimate();
        assert_eq!(
            e.status(Some(&at("a", 180)), WALL + 180),
            EvidenceStatus::Estimated
        );
        assert_eq!(
            e.status(Some(&at("a", 181)), WALL + 181),
            EvidenceStatus::Stale
        );
        assert_eq!(
            e.status(Some(&at("b", 0)), WALL),
            EvidenceStatus::BootMismatch
        );
        assert_eq!(e.status(None, WALL), EvidenceStatus::Unknown);
        let future = ClockEvidence::observation(WALL, at("a", 100), Some(50), ClockSource::Year);
        assert_eq!(
            future.status(Some(&at("a", 99)), WALL),
            EvidenceStatus::Stale
        );
        // MONOTONIC did not advance in suspend; BOOTTIME did, so old evidence expires.
        assert_eq!(
            future.status(Some(&at("a", 400)), WALL + 300),
            EvidenceStatus::Stale
        );
    }

    #[test]
    fn reader_rejects_steps_before_the_next_worker_round() {
        let e = estimate();
        for step in [-28_800, 28_800, -6, 6] {
            assert_eq!(
                e.status(Some(&at("a", 60)), WALL + 60 + step),
                EvidenceStatus::WallChanged
            );
        }
        for step in [-5, 0, 5] {
            assert!(e.status(Some(&at("a", 60)), WALL + 60 + step).wall_usable());
        }
        assert_eq!(
            e.status(Some(&at("a", 60)), WALL),
            EvidenceStatus::WallChanged,
            "frozen wall clock"
        );
    }

    #[test]
    fn legacy_corrupt_and_inconsistent_records_are_unknown() {
        let e = estimate();
        let text = e.encode();
        assert_eq!(ClockEvidence::decode(&text), Some(e.clone()));
        let mut next_version = e.clone();
        next_version.version = 3;
        let mut empty_boot = e.clone();
        empty_boot.observed_at.boot_id.clear();
        for bad in [
            String::new(),
            "1 1790000000 0 year\n".into(),
            "1\n{}\n".into(),
            text.replacen("1 ", "0 ", 1),
            format!("{text}extra\n"),
            next_version.encode(),
            empty_boot.encode(),
        ] {
            assert_eq!(
                evidence_status(&bad, Some(&at("a", 0)), WALL),
                EvidenceStatus::Unknown,
                "{bad}"
            );
        }
    }

    #[test]
    fn unknown_and_revoked_are_not_usable_and_arithmetic_is_checked() {
        for source in [ClockSource::None, ClockSource::Revoked] {
            let e = ClockEvidence::observation(WALL, at("a", 1), None, source);
            assert_eq!(
                e.status(Some(&at("a", 1)), WALL),
                if source == ClockSource::Revoked {
                    EvidenceStatus::Revoked
                } else {
                    EvidenceStatus::Unknown
                }
            );
            assert!(e.encode().starts_with("0 "));
        }
        let e = ClockEvidence::observation(i64::MAX, at("a", 0), None, ClockSource::Year);
        assert_eq!(
            e.status(Some(&at("a", 1)), i64::MAX),
            EvidenceStatus::WallChanged
        );
        let e = ClockEvidence::observation(WALL, at("a", u64::MAX), None, ClockSource::Year);
        assert!(ClockEvidence::decode(&e.encode()).is_none());
        let e = ClockEvidence::observation(1_735_948_800, at("a", 0), None, ClockSource::Year);
        assert!(!e.status(Some(&at("a", 0)), e.raw_wall).wall_usable());
    }

    #[test]
    fn proc_snapshot_zero_is_valid_but_missing_is_not() {
        assert_eq!(
            BootTime::from_uptime(" a\n", "0.00 1.00\n"),
            Some(at("a", 0))
        );
        for uptime in [
            "",
            "oops",
            "-1.0 1",
            "1.nan 1",
            "1. 1",
            ".5 1",
            "18446744073709551616.0",
        ] {
            assert!(BootTime::from_uptime("a", uptime).is_none(), "{uptime}");
        }
        assert!(BootTime::from_uptime("", "1.0").is_none());
        assert_eq!(at("a", 60).elapsed_since(&at("a", 0)), Some(60));
        assert_eq!(at("a", 0).elapsed_since(&at("a", 60)), None);
        assert_eq!(at("b", 60).elapsed_since(&at("a", 0)), None);
    }
}
