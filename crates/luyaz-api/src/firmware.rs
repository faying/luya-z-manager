//! Firmware capability contracts (R18-P1-01). Data and pure projections only: no discovery, I/O or permission tokens.
//! Historical compatibility rows and replay evidence must never authorize a live operation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::clock::BootTime;
use serde_json::Value;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_AGE_SECONDS: u64 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Facet {
    Surface,
    Read,
    Write,
    Workflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextKey {
    #[serde(rename = "ubus_epoch")]
    Ubus,
    #[serde(rename = "usb_epoch")]
    Usb,
    #[serde(rename = "radio_mode")]
    Radio,
    #[serde(rename = "sim_epoch")]
    Sim,
    #[serde(rename = "connection_epoch")]
    Connection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Ubus,
    At,
    Uci,
    File,
    Procedure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Validity {
    Boot,
    Context,
    PersistentClaim,
}

/// Describes an operation; it is not an executable probe definition or an authorization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySpec {
    pub id: String,
    pub facet: Facet,
    pub transport: Transport,
    pub dependencies: Vec<String>,
    pub probe_id: Option<String>,
    pub context_keys: Vec<ContextKey>,
    pub validity: Validity,
    pub verifier_id: Option<String>,
    pub policy_tags: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityStatus {
    Complete,
    Partial,
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cohort {
    Upgraded,
    Native,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawVersion {
    /// Semantic namespace, not merely the file from which the value was read.
    pub namespace: String,
    pub source: String,
    pub value: String,
    pub evidence_id: String,
    /// Set only by a reviewed identity decoder; a display suffix such as B31 is not full.
    pub full: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareProfile {
    pub raw_versions: Vec<RawVersion>,
    pub product_line: Option<String>,
    pub region: Option<String>,
    pub version: Option<String>,
    pub build: Option<String>,
    pub modem_build: Option<String>,
    pub active_slot: Option<String>,
    pub completeness: IdentityStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub cohort: Cohort,
    pub previous_firmware: Option<FirmwareProfile>,
    pub evidence_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    pub name: String,
    pub version: Option<String>,
    pub commit: Option<String>,
    pub hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceProfile {
    pub device_id: String,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub hardware_revision: Option<String>,
    pub board_name: Option<String>,
    pub identity_evidence_ids: Vec<String>,
    pub firmware: FirmwareProfile,
    pub provenance: Provenance,
    pub components: Vec<Component>,
    pub adapter_id: Option<String>,
    pub adapter_revision: u32,
    pub fingerprint: Option<String>,
    pub identity_status: IdentityStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkSource {
    UbusOnly,
    UbusWithUciFallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CpuTemperatureSource {
    CpussOnly,
    NamedFieldsThenSysfs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WifiSectionSource {
    #[serde(rename = "main_2g")]
    Main2g,
    FirstEnabledThenPresent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAdapter {
    pub id: String,
    pub revision: u32,
    /// Exact normalized model; firmware-specific dialects require a full fingerprint too.
    pub model: String,
    pub firmware_fingerprint: Option<String>,
    pub network_source: NetworkSource,
    pub cpu_temperature: CpuTemperatureSource,
    pub wifi_section: WifiSectionSource,
    pub command_dialects: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOrigin {
    Live,
    Historical,
    Replay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Code,
    DeviceCapture,
    DeviceReport,
    ExternalReport,
    Fixture,
    Inference,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    CodeReviewed,
    Documented,
    Captured,
    Replayed,
    WriteVerified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UtcQuality {
    Trusted,
    Estimated,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceTime {
    /// Existing CLOCK_BOOTTIME contract, in seconds; None for unbound historical records.
    pub observed_at: Option<BootTime>,
    pub utc: Option<String>,
    pub utc_quality: UtcQuality,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub sim_epoch: Option<String>,
    pub connection_epoch: Option<String>,
    pub usb_epoch: Option<String>,
    pub ubus_epoch: Option<String>,
    pub radio_mode: Option<String>,
    /// Requires continuous radio lifecycle coverage; an ONLINE string is not an epoch.
    #[serde(default)]
    pub radio_epoch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub device_id: String,
    pub fingerprint: Option<String>,
    /// Opaque Core instance ID. A per-process generation alone can collide after restart.
    pub collector_id: String,
    pub generation: u64,
    pub rule_revision: u32,
    pub adapter_revision: u32,
    pub component_revision: String,
    pub cohort: Cohort,
    pub context: Context,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeOutcome {
    Success,
    Absent,
    ExplicitUnsupported,
    Empty,
    Malformed,
    Timeout,
    Busy,
    Held,
    Denied,
    Unavailable,
    Skipped,
}

/// Produced by the operation's reviewed decoder/verifier, never from a method name or ACK.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Proof {
    MethodPresent,
    MethodAbsent,
    ReadValidated,
    WriteVerified,
    WorkflowVerified,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub outcome: ProbeOutcome,
    pub exit_code: Option<i32>,
    pub vendor_code: Option<String>,
    pub value: Option<Value>,
    pub proof: Option<Proof>,
    pub complete: bool,
    pub truncated: bool,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub probe_id: String,
    pub target: String,
    pub verifier_id: Option<String>,
    pub args: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub sha256: Option<String>,
    pub parser_revision: Option<String>,
    pub redacted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: String,
    pub capability_id: String,
    pub facet: Facet,
    pub origin: EvidenceOrigin,
    pub source_kind: SourceKind,
    pub verification: Verification,
    pub scope: Option<Scope>,
    pub observed_on: Option<String>,
    pub recorded_on: String,
    pub time: EvidenceTime,
    pub operation: Operation,
    pub result: Option<ProbeResult>,
    pub source_ref: String,
    pub locator: Option<String>,
    pub artifact: Artifact,
    pub summary: String,
    pub limitations: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityState {
    Ready,
    Blocked,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Availability {
    pub state: AvailabilityState,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Freshness {
    pub fresh: bool,
    pub max_age_seconds: u64,
    pub age_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentReason {
    Verified,
    MethodAbsent,
    ExplicitUnsupported,
    UnknownCapability,
    IdentityPartial,
    IdentityConflict,
    InvalidScope,
    NoEvidence,
    WeakEvidence,
    StaleEvidence,
    ContextChanged,
    InvalidResult,
    UnverifiedEffect,
    ConflictingEvidence,
    DependencyUnknown,
    DependencyUnsupported,
    InvalidEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityAssessment {
    pub capability_id: String,
    pub status: Support,
    pub reason: AssessmentReason,
    pub evidence_ids: Vec<String>,
    pub contradicting_evidence_ids: Vec<String>,
    pub availability: Availability,
    pub scope: Scope,
    pub freshness: Freshness,
    pub validation: Option<Verification>,
    pub last_attempt_evidence_id: Option<String>,
    pub selected_dialect: Option<String>,
}

/// Historical claim only, deliberately distinct from a live assessment or permission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatrixRow {
    pub id: String,
    pub profile_id: String,
    pub capability_id: String,
    pub facet: Facet,
    pub status: Support,
    pub validation: Verification,
    pub evidence_ids: Vec<String>,
    pub reason: String,
    pub conditions: Vec<String>,
    pub limitations: Vec<String>,
}

/// Versioned passive report, also returned by Core's GET /v2/device/capabilities.
/// Decoding an external copy never creates trusted live evidence.
/// Coverage of the producer, separate from a state's observed value. Missing coverage
/// cannot be filled by a report importer, firmware suffix or successful command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCoverage {
    pub source: String,
    pub authoritative: bool,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityReport {
    #[serde(default)]
    pub context_coverage: BTreeMap<String, ContextCoverage>,
    pub schema_version: u32,
    pub profile: DeviceProfile,
    pub assessments: Vec<CapabilityAssessment>,
    pub evidence: Vec<Evidence>,
}

/// Presentation of the Core USB write policy, never a permission token. The executor
/// resolves again at dispatch; a confirmation cannot extend an observation's lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsbWriteDisposition {
    Ready,
    HumanPreflight,
    Denied,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbWritePolicy {
    pub assessment: CapabilityAssessment,
    pub disposition: UsbWriteDisposition,
}

impl CapabilityReport {
    pub fn usb_write_policy(&self, id: &str) -> Option<UsbWritePolicy> {
        if !matches!(
            id,
            "usb.mode.set" | "usb.attach.share" | "usb.attach.fast_charge"
        ) {
            return None;
        }
        let a = self.assessments.iter().find(|a| a.capability_id == id)?;
        let denied = self.profile.identity_status == IdentityStatus::Conflict
            || a.status == Support::Unsupported
            || a.availability.state == AvailabilityState::Blocked
            || a.reason == AssessmentReason::ConflictingEvidence;
        let disposition = if denied {
            UsbWriteDisposition::Denied
        } else if a.status == Support::Supported
            && a.freshness.fresh
            && a.availability.state == AvailabilityState::Ready
        {
            UsbWriteDisposition::Ready
        } else {
            UsbWriteDisposition::HumanPreflight
        };
        Some(UsbWritePolicy {
            assessment: a.clone(),
            disposition,
        })
    }
}

impl CapabilityReport {
    /// Decode a report, rejecting unsupported versions, duplicate IDs and dangling references.
    /// This does not make the decoded evidence live or establish that its claims are true.
    pub fn decode(text: &str) -> Result<Self, String> {
        let report: Self = serde_json::from_str(text).map_err(|e| e.to_string())?;
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> Result<(), String> {
        use std::collections::BTreeSet;
        if self.schema_version != SCHEMA_VERSION {
            return Err("unsupported firmware capability schema".into());
        }
        let mut ids = BTreeSet::new();
        for e in &self.evidence {
            if e.id.trim().is_empty() || !ids.insert(e.id.as_str()) {
                return Err("empty or duplicate evidence ID".into());
            }
        }
        let mut capabilities = BTreeSet::new();
        for a in &self.assessments {
            if a.capability_id.trim().is_empty() || !capabilities.insert(&a.capability_id) {
                return Err("empty or duplicate assessment ID".into());
            }
            if a.status != Support::Unknown && a.evidence_ids.is_empty() {
                return Err("support assessment lacks evidence".into());
            }
            for id in a
                .evidence_ids
                .iter()
                .chain(&a.contradicting_evidence_ids)
                .chain(a.last_attempt_evidence_id.iter())
            {
                if !ids.contains(id.as_str()) {
                    return Err(format!("unknown evidence ID: {id}"));
                }
            }
        }
        for id in self
            .profile
            .identity_evidence_ids
            .iter()
            .chain(self.profile.provenance.evidence_ids.iter())
            .chain(
                self.profile
                    .firmware
                    .raw_versions
                    .iter()
                    .map(|v| &v.evidence_id),
            )
            .chain(
                self.profile
                    .provenance
                    .previous_firmware
                    .iter()
                    .flat_map(|f| f.raw_versions.iter().map(|v| &v.evidence_id)),
            )
        {
            if !ids.contains(id.as_str()) {
                return Err(format!("unknown identity evidence ID: {id}"));
            }
        }
        Ok(())
    }
}
