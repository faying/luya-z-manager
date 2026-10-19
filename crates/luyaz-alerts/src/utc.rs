//! Trusted-gateway UTC attestation, not a network clock client. Only an authenticated root
//! provisioning channel may write ANCHOR_PATH. This validates bounds/replay/model, not NTS
//! packets: the gateway is responsible for verifying the independent source and retaining proof.
use crate::clock::{BootTime, CLOCK_YEAR_MIN, ClockEvidence, ClockModel, ClockSource, SyncState};
use serde::{Deserialize, Serialize};

pub const ANCHOR_PATH: &str = "/tmp/luyaz/utc-anchor";
pub const MAX_LIFETIME_S: u64 = 900;
pub const MAX_UNCERTAINTY_S: u64 = 5;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceSource {
    ChronyNts,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UtcAnchor {
    pub version: u8,
    pub source: ReferenceSource,
    pub source_id: String,
    /// SHA256 of retained source/SSH acquisition evidence on the trusted gateway.
    pub evidence_sha256: String,
    pub observed_at: BootTime,
    pub raw_wall: i64,
    pub utc_estimate: i64,
    /// Includes source bound, whole acquisition interval and second quantisation.
    pub uncertainty_s: u64,
    pub valid_for_s: u64,
    /// Conservative device elapsed-clock drift assumption; not a measured oscillator spec.
    pub drift_ppm: u64,
    /// Independently read display configuration; not an assumption about epoch semantics.
    pub display_zone: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Projection {
    pub utc_estimate: i64,
    pub uncertainty_s: u64,
    pub clock_model: ClockModel,
}

impl UtcAnchor {
    pub fn project(&self, now: &BootTime, raw_wall: i64) -> Option<Projection> {
        if self.version != 1
            || self.source_id.is_empty()
            || self.source_id.len() > 128
            || self.evidence_sha256.len() != 64
            || !self.evidence_sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || !(1..=MAX_LIFETIME_S).contains(&self.valid_for_s)
            || !(1..=MAX_UNCERTAINTY_S).contains(&self.uncertainty_s)
            || !(100..=1000).contains(&self.drift_ppm)
            || !(-43200..=50400).contains(&self.display_zone)
            || self.utc_estimate < CLOCK_YEAR_MIN
        {
            return None;
        }
        let age = now.elapsed_since(&self.observed_at)?;
        if age > self.valid_for_s {
            return None;
        }
        let drift = age.checked_mul(self.drift_ppm)?.div_ceil(1_000_000);
        let uncertainty = self.uncertainty_s.checked_add(drift)?;
        if uncertainty > MAX_UNCERTAINTY_S {
            return None;
        }
        let expected_wall = self.raw_wall.checked_add(i64::try_from(age).ok()?)?;
        // Reject an epoch step; do not renew/rebase the independent anchor.
        if expected_wall.abs_diff(raw_wall) > 2 {
            return None;
        }
        let delta = self.raw_wall.checked_sub(self.utc_estimate)?;
        let near_utc = delta.unsigned_abs() <= self.uncertainty_s;
        let near_local = delta.abs_diff(self.display_zone) <= self.uncertainty_s;
        let clock_model = match (near_utc, near_local) {
            (true, false) | (true, true) if self.display_zone == 0 || !near_local => {
                ClockModel::Utc
            }
            (false, true) => ClockModel::LocalWallAsUtc,
            _ => return None, // wrong or ambiguous epoch, never a fixed global -8h
        };
        Some(Projection {
            utc_estimate: self.utc_estimate.checked_add(i64::try_from(age).ok()?)?,
            uncertainty_s: uncertainty,
            clock_model,
        })
    }

    pub fn apply(&self, evidence: &mut ClockEvidence) -> bool {
        if !evidence
            .status(Some(&evidence.observed_at), evidence.raw_wall)
            .wall_usable()
        {
            return false;
        }
        let Some(p) = self.project(&evidence.observed_at, evidence.raw_wall) else {
            return false;
        };
        evidence.clock_model = p.clock_model;
        evidence.display_zone = Some(self.display_zone);
        evidence.utc_estimate = Some(p.utc_estimate);
        evidence.uncertainty_s = Some(p.uncertainty_s);
        evidence.source = ClockSource::TrustedGateway;
        evidence.sync_state = SyncState::Verified;
        evidence.utc_reference = Some(self.clone());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::EvidenceStatus;
    fn at(seconds: u64) -> BootTime {
        BootTime {
            boot_id: "a".into(),
            seconds,
        }
    }
    pub fn anchor() -> UtcAnchor {
        UtcAnchor {
            version: 1,
            source: ReferenceSource::ChronyNts,
            source_id: "nts.example".into(),
            evidence_sha256: "a".repeat(64),
            observed_at: at(0),
            raw_wall: 1_790_028_800,
            utc_estimate: 1_790_000_000,
            uncertainty_s: 2,
            valid_for_s: 900,
            drift_ppm: 1000,
            display_zone: 28800,
        }
    }
    #[test]
    fn source_ttl_never_renews_and_wall_model_is_per_anchor() {
        let a = anchor();
        let mut e =
            ClockEvidence::observation(a.raw_wall + 60, at(60), Some(60), ClockSource::Year);
        assert!(a.apply(&mut e));
        assert_eq!(e.clock_model, ClockModel::LocalWallAsUtc);
        assert_eq!(
            e.status(Some(&at(60)), a.raw_wall + 60),
            EvidenceStatus::Verified
        );
        assert_eq!(
            e.status(Some(&at(180)), a.raw_wall + 180),
            EvidenceStatus::Verified
        );
        let mut latest =
            ClockEvidence::observation(a.raw_wall + 900, at(900), None, ClockSource::Continuity);
        assert!(a.apply(&mut latest));
        assert_eq!(
            latest.status(Some(&at(901)), a.raw_wall + 901),
            EvidenceStatus::Unknown
        );
        assert!(
            a.project(
                &BootTime {
                    boot_id: "b".into(),
                    seconds: 60
                },
                a.raw_wall + 60
            )
            .is_none()
        );
        assert!(a.project(&at(60), a.raw_wall + 60 + 28800).is_none());
        let mut utc = a.clone();
        utc.raw_wall = utc.utc_estimate;
        assert_eq!(
            utc.project(&at(0), utc.raw_wall).unwrap().clock_model,
            ClockModel::Utc
        );
        utc.display_zone = 1;
        assert!(utc.project(&at(0), utc.raw_wall).is_none(), "ambiguous");
    }
    #[test]
    fn forged_verified_bad_or_unknown_source_and_unbounded_error_rejected() {
        let a = anchor();
        let mut e = ClockEvidence::observation(a.raw_wall, at(0), None, ClockSource::Year);
        e.sync_state = SyncState::Verified;
        e.clock_model = ClockModel::Utc;
        e.utc_estimate = Some(a.utc_estimate);
        e.uncertainty_s = Some(1);
        assert_eq!(e.status(Some(&at(0)), a.raw_wall), EvidenceStatus::Unknown);
        for field in 0..7 {
            let mut bad = a.clone();
            match field {
                0 => bad.uncertainty_s = 0,
                1 => bad.uncertainty_s = 6,
                2 => bad.valid_for_s = 901,
                3 => bad.evidence_sha256 = "bad".into(),
                4 => bad.drift_ppm = 0,
                5 => bad.utc_estimate = 0,
                _ => bad.observed_at = at(1),
            };
            assert!(bad.project(&at(0), bad.raw_wall).is_none());
        }
        let text = serde_json::to_string(&a).unwrap();
        assert!(
            serde_json::from_str::<UtcAnchor>(&text.replace("chrony_nts", "http_date")).is_err()
        );
        assert!(
            serde_json::from_str::<UtcAnchor>(
                &text.replace("\"uncertainty_s\":2", "\"uncertainty_s\":-1")
            )
            .is_err()
        );
        assert!(!a.apply(&mut ClockEvidence::observation(
            a.raw_wall,
            at(0),
            None,
            ClockSource::Revoked
        )));
    }
}
