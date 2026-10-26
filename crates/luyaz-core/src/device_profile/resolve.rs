use std::collections::BTreeSet;

use luyaz_api::{clock::BootTime, firmware::*};

use super::identity::{fingerprint, model_adapter};

pub(crate) const RULE_REVISION: u32 = 2;

/// Reviewed operation bindings, not user-provided probes or arbitrary executable paths.
struct Spec {
    id: &'static str,
    facet: Facet,
    probe: &'static str,
    target: &'static str,
    verifier: Option<&'static str>,
    contexts: &'static [ContextKey],
    dependencies: &'static [&'static str],
    /// None of the initial recorded generic ERROR replies establishes unsupported.
    unsupported_codes: &'static [&'static str],
}

impl Spec {
    const fn new(
        id: &'static str,
        facet: Facet,
        probe: &'static str,
        target: &'static str,
        contexts: &'static [ContextKey],
    ) -> Self {
        Self {
            id,
            facet,
            probe,
            target,
            contexts,
            verifier: None,
            dependencies: &[],
            unsupported_codes: &[],
        }
    }
}

use ContextKey::*;
const SPECS: &[Spec] = &[
    Spec::new(
        "usb.mode.setter.surface",
        Facet::Surface,
        "ubus.usb_methods.v1",
        "zwrt_bsp.usb:set",
        &[Ubus],
    ),
    Spec::new(
        "usb.typec.setter.surface",
        Facet::Surface,
        "ubus.typec_methods.v1",
        "zwrt_bsp.typec:set",
        &[Ubus],
    ),
    Spec::new(
        "usb.powerbank.setter.surface",
        Facet::Surface,
        "ubus.powerbank_methods.v1",
        "zwrt_bsp.powerbank:set",
        &[Ubus],
    ),
    Spec {
        dependencies: &["usb.mode.setter.surface"],
        verifier: Some("usb.mode.readback.v1"),
        ..Spec::new(
            "usb.mode.set",
            Facet::Write,
            "usb.mode.set.v1",
            "zwrt_bsp.usb:set",
            &[Ubus, Usb],
        )
    },
    Spec {
        dependencies: &["usb.typec.setter.surface"],
        verifier: Some("usb.share.readback.v1"),
        ..Spec::new(
            "usb.attach.share",
            Facet::Write,
            "usb.attach.share.v1",
            "zwrt_bsp.typec:set",
            &[Ubus, Usb],
        )
    },
    Spec {
        dependencies: &["usb.powerbank.setter.surface"],
        verifier: Some("usb.fast_charge.readback.v1"),
        ..Spec::new(
            "usb.attach.fast_charge",
            Facet::Write,
            "usb.attach.fast_charge.v1",
            "zwrt_bsp.powerbank:set",
            &[Ubus, Usb],
        )
    },
    Spec::new(
        "usb.status.read",
        Facet::Read,
        "usb.cached_read.v1",
        "zwrt_bsp.usb:list",
        &[Ubus, Usb],
    ),
    Spec::new(
        "qos.eps.log.read",
        Facet::Read,
        "qos.cached.v1",
        "qos:eps",
        &[Sim, Connection, Radio],
    ),
    Spec::new(
        "qos.nr_pdu.log.read",
        Facet::Read,
        "qos.cached.v1",
        "qos:nr_pdu",
        &[Sim, Connection, Radio],
    ),
    Spec::new(
        "at.transport.read",
        Facet::Read,
        "at.read.explicit.v1",
        "AT",
        &[Radio],
    ),
    Spec::new(
        "at.pdp_contexts.read",
        Facet::Read,
        "at.cgcontrdp.v1",
        "AT+CGCONTRDP",
        &[Sim, Connection, Radio],
    ),
    Spec::new(
        "at.eps_qos.read",
        Facet::Read,
        "at.cgeqosrdp.v1",
        "AT+CGEQOSRDP",
        &[Sim, Connection, Radio],
    ),
    // §12.6: B31 kept `connect_status` connected through a disconnect; no evidence = Unknown,
    // so consumers corroborate it (`luyaz_api::firmware::wwaniface_connected`).
    Spec::new(
        WWANIFACE_TRUSTWORTHY,
        Facet::Read,
        "ubus.wwaniface.v1",
        "zwrt_data:get_wwaniface",
        &[Connection],
    ),
    Spec {
        verifier: Some("modem.recovery.readback.v1"),
        ..Spec::new(
            "modem.recover_from_lpm",
            Facet::Workflow,
            "modem.recovery.v1",
            "modem:lpm_recovery",
            &[Radio],
        )
    },
];

pub(crate) fn capability_ids() -> impl Iterator<Item = &'static str> {
    SPECS.iter().map(|spec| spec.id)
}

pub(crate) struct ResolveInput<'a> {
    pub profile: &'a DeviceProfile,
    pub scope: &'a Scope,
    /// Supplied by the caller; the resolver never reads a clock or the wall time.
    pub now: Option<&'a BootTime>,
    pub evidence: &'a [Evidence],
    /// Current conditions, not inferred from the capability's historical support.
    pub availability: Availability,
}

fn base(input: &ResolveInput<'_>, id: &str) -> CapabilityAssessment {
    CapabilityAssessment {
        capability_id: id.into(),
        status: Support::Unknown,
        reason: AssessmentReason::NoEvidence,
        evidence_ids: vec![],
        contradicting_evidence_ids: vec![],
        availability: super::policy::availability(id, input.scope.context.radio_mode.as_deref())
            .unwrap_or_else(|| input.availability.clone()),
        scope: input.scope.clone(),
        freshness: Freshness {
            fresh: false,
            max_age_seconds: MAX_AGE_SECONDS,
            age_seconds: None,
        },
        validation: None,
        last_attempt_evidence_id: None,
        selected_dialect: None,
    }
}

/// Whether a ubus object add/remove can change a capability bound to the ubus epoch: the
/// object of any `[Ubus]` spec target (`object:method`), or a dotted parent/child of one.
pub(crate) fn ubus_object_relevant(path: &str) -> bool {
    let nested = |a: &str, b: &str| a.strip_prefix(b).is_some_and(|r| r.starts_with('.'));
    SPECS
        .iter()
        .filter(|s| s.contexts.contains(&Ubus))
        .filter_map(|s| s.target.split_once(':').map(|(object, _)| object))
        .any(|object| object == path || nested(object, path) || nested(path, object))
}

fn same_instance(a: &Scope, b: &Scope) -> bool {
    a.device_id == b.device_id
        && a.fingerprint == b.fingerprint
        && a.collector_id == b.collector_id
        && a.generation == b.generation
        && a.rule_revision == b.rule_revision
        && a.adapter_revision == b.adapter_revision
        && a.component_revision == b.component_revision
        && a.cohort == b.cohort
}

fn context_value(context: &Context, key: ContextKey) -> Option<&str> {
    match key {
        Ubus => context.ubus_epoch.as_deref(),
        Usb => context.usb_epoch.as_deref(),
        Radio => context.radio_epoch.as_deref(),
        Sim => context.sim_epoch.as_deref(),
        Connection => context.connection_epoch.as_deref(),
    }
    .filter(|s| !s.trim().is_empty())
}

fn contexts_match(a: &Context, b: &Context, keys: &[ContextKey]) -> bool {
    keys.iter().all(|&key| {
        let value = context_value(a, key);
        value.is_some()
            && value == context_value(b, key)
            && (key != Radio || (a.radio_mode.is_some() && a.radio_mode == b.radio_mode))
    })
}

fn note_unknown(out: &mut CapabilityAssessment, reason: AssessmentReason) {
    // Input order must not change the explanation when several unusable observations remain.
    let priority = |reason| match reason {
        AssessmentReason::InvalidScope => 6,
        AssessmentReason::ContextChanged => 5,
        AssessmentReason::StaleEvidence => 4,
        AssessmentReason::InvalidResult | AssessmentReason::UnverifiedEffect => 3,
        AssessmentReason::WeakEvidence => 2,
        _ => 0,
    };
    if priority(reason) > priority(out.reason) {
        out.reason = reason;
    }
}

fn judgement(spec: &Spec, evidence: &Evidence) -> Option<(Support, AssessmentReason)> {
    let result = evidence.result.as_ref()?;
    if evidence.facet != spec.facet
        || evidence.operation.probe_id != spec.probe
        || evidence.operation.target != spec.target
        || !result.complete
        || result.truncated
        || result.exit_code.is_some_and(|code| code != 0)
    {
        return None;
    }
    if result.outcome == ProbeOutcome::ExplicitUnsupported
        && result
            .vendor_code
            .as_deref()
            .is_some_and(|code| spec.unsupported_codes.contains(&code))
    {
        return Some((Support::Unsupported, AssessmentReason::ExplicitUnsupported));
    }
    let expected = match spec.facet {
        Facet::Surface => {
            if result.exit_code != Some(0) || evidence.verification != Verification::Captured {
                return None;
            }
            let (object, method) = spec.target.split_once(':')?;
            let value = result.value.as_ref()?;
            if value["object"].as_str() != Some(object) || value["method"].as_str() != Some(method)
            {
                return None;
            }
            if result.outcome == ProbeOutcome::Absent && result.proof == Some(Proof::MethodAbsent) {
                return Some((Support::Unsupported, AssessmentReason::MethodAbsent));
            }
            Proof::MethodPresent
        }
        Facet::Read => {
            if evidence.verification != Verification::Captured {
                return None;
            }
            Proof::ReadValidated
        }
        Facet::Write | Facet::Workflow => {
            if evidence.verification != Verification::WriteVerified
                || evidence.operation.verifier_id.as_deref() != spec.verifier
                || spec.verifier.is_none()
            {
                return None;
            }
            if spec.facet == Facet::Write {
                Proof::WriteVerified
            } else {
                Proof::WorkflowVerified
            }
        }
    };
    (result.outcome == ProbeOutcome::Success && result.proof == Some(expected))
        .then_some((Support::Supported, AssessmentReason::Verified))
}

/// Offline judgement only. Never promotes historical/replayed evidence or a method ACK.
pub(crate) fn resolve(input: &ResolveInput<'_>, capability_id: &str) -> CapabilityAssessment {
    resolve_inner(input, capability_id, 0)
}

fn resolve_inner(
    input: &ResolveInput<'_>,
    capability_id: &str,
    depth: usize,
) -> CapabilityAssessment {
    let mut out = base(input, capability_id);
    let Some(spec) = SPECS.iter().find(|spec| spec.id == capability_id) else {
        out.reason = AssessmentReason::UnknownCapability;
        return out;
    };
    if depth > SPECS.len() {
        out.reason = AssessmentReason::InvalidEvidence;
        return out;
    }
    let profile = input.profile;
    if profile.identity_status != IdentityStatus::Complete {
        out.reason = if profile.identity_status == IdentityStatus::Conflict {
            AssessmentReason::IdentityConflict
        } else {
            AssessmentReason::IdentityPartial
        };
        return out;
    }
    let scope = input.scope;
    let adapter = profile.model.as_deref().and_then(model_adapter);
    if scope.device_id.trim().is_empty()
        || scope.device_id != profile.device_id
        || scope.collector_id.trim().is_empty()
        || scope.component_revision.trim().is_empty()
        || scope.rule_revision != RULE_REVISION
        || scope.adapter_revision != profile.adapter_revision
        || adapter.as_ref().is_none_or(|a| {
            Some(a.id.as_str()) != profile.adapter_id.as_deref()
                || a.revision != profile.adapter_revision
        })
        || scope.cohort != profile.provenance.cohort
        || scope.fingerprint.is_none()
        || scope.fingerprint != profile.fingerprint
        || fingerprint(profile) != scope.fingerprint
    {
        out.reason = AssessmentReason::InvalidScope;
        return out;
    }
    let mut ids = BTreeSet::new();
    if input
        .evidence
        .iter()
        .any(|e| e.id.trim().is_empty() || !ids.insert(e.id.as_str()))
    {
        out.reason = AssessmentReason::InvalidEvidence;
        return out;
    }
    let mut positive = vec![];
    let mut negative = vec![];
    let mut decisive_age = 0;
    let mut latest = None;
    for e in input
        .evidence
        .iter()
        .filter(|e| e.capability_id == capability_id)
    {
        out.evidence_ids.push(e.id.clone());
        if e.origin != EvidenceOrigin::Live
            || e.source_kind != SourceKind::DeviceCapture
            || !matches!(
                e.verification,
                Verification::Captured | Verification::WriteVerified
            )
        {
            note_unknown(&mut out, AssessmentReason::WeakEvidence);
            continue;
        }
        let Some(observed_scope) = &e.scope else {
            note_unknown(&mut out, AssessmentReason::InvalidScope);
            continue;
        };
        if !same_instance(scope, observed_scope)
            || !contexts_match(&scope.context, &observed_scope.context, spec.contexts)
        {
            note_unknown(&mut out, AssessmentReason::ContextChanged);
            continue;
        }
        let age = input
            .now
            .zip(e.time.observed_at.as_ref())
            .and_then(|(now, at)| now.elapsed_since(at));
        let Some(age) = age else {
            note_unknown(&mut out, AssessmentReason::StaleEvidence);
            continue;
        };
        // Choose the latest attempt by boot time, not input order or wall date.
        if latest.is_none_or(|previous| age < previous)
            || (latest == Some(age)
                && out
                    .last_attempt_evidence_id
                    .as_deref()
                    .is_none_or(|id| e.id.as_str() < id))
        {
            latest = Some(age);
            out.last_attempt_evidence_id = Some(e.id.clone());
        }
        if age > MAX_AGE_SECONDS {
            note_unknown(&mut out, AssessmentReason::StaleEvidence);
            continue;
        }
        let Some((status, reason)) = judgement(spec, e) else {
            let reason = if matches!(spec.facet, Facet::Write | Facet::Workflow) {
                AssessmentReason::UnverifiedEffect
            } else {
                AssessmentReason::InvalidResult
            };
            note_unknown(&mut out, reason);
            continue;
        };
        decisive_age = decisive_age.max(age);
        if status == Support::Supported {
            positive.push(e);
        } else {
            negative.push((e, reason));
        }
    }
    if !positive.is_empty() || !negative.is_empty() {
        out.freshness.fresh = true;
        out.freshness.age_seconds = Some(decisive_age);
        if !positive.is_empty() && !negative.is_empty() {
            out.reason = AssessmentReason::ConflictingEvidence;
            out.contradicting_evidence_ids = negative.iter().map(|(e, _)| e.id.clone()).collect();
        } else if let Some((e, reason)) = negative.first() {
            out.status = Support::Unsupported;
            out.reason = *reason;
            out.validation = Some(e.verification);
        } else {
            out.status = Support::Supported;
            out.reason = AssessmentReason::Verified;
            out.validation = Some(positive[0].verification);
        }
    }
    // A necessary method's absence can disprove this exact write path, never all USB.
    for dependency in spec.dependencies {
        let dep = resolve_inner(input, dependency, depth + 1);
        out.evidence_ids.extend(dep.evidence_ids.clone());
        out.contradicting_evidence_ids
            .extend(dep.contradicting_evidence_ids.clone());
        if dep.status == Support::Unsupported {
            out.freshness = dep.freshness;
            if !positive.is_empty() {
                out.status = Support::Unknown;
                out.reason = AssessmentReason::ConflictingEvidence;
                out.contradicting_evidence_ids.extend(
                    input
                        .evidence
                        .iter()
                        .filter(|e| e.capability_id == *dependency)
                        .map(|e| e.id.clone()),
                );
                out.validation = None;
            } else {
                out.status = Support::Unsupported;
                out.reason = AssessmentReason::DependencyUnsupported;
                out.validation = dep.validation;
            }
        } else if dep.status == Support::Unknown && out.status != Support::Unsupported {
            // Retain a conflict from this facet; do not hide it behind a missing dependency.
            if dep.reason == AssessmentReason::ConflictingEvidence {
                out.reason = AssessmentReason::ConflictingEvidence;
            } else if out.reason != AssessmentReason::ConflictingEvidence {
                out.reason = AssessmentReason::DependencyUnknown;
            }
            out.status = Support::Unknown;
            out.validation = None;
            out.freshness.fresh = false;
        } else if out.status == Support::Supported {
            out.freshness.age_seconds = out
                .freshness
                .age_seconds
                .zip(dep.freshness.age_seconds)
                .map(|(a, b)| a.max(b));
        }
    }
    out.evidence_ids.sort();
    out.evidence_ids.dedup();
    out.contradicting_evidence_ids.sort();
    out.contradicting_evidence_ids.dedup();
    if out.status == Support::Supported {
        out.selected_dialect = super::policy::verified_dialect(capability_id).map(str::to_owned);
    }
    out
}
