use luyaz_api::{clock::BootTime, firmware::*};
use serde_json::json;

use super::{
    identity::{IdentityInput, build_profile, model_adapter},
    listing::{ListingInput, method_result},
    resolve::{ResolveInput, resolve},
};

const RECORDED: &str =
    "'zwrt_bsp.powerbank' @00000001\n\t\"set\":{\"state\":\"Integer\"}\n\t\"get\":{\"property\":\"String\"}\n'zwrt_bsp.typec' @00000002\n\t\"list\":{}\n\t\"set\":{\"PR_Swap\":\"String\",\"DR_Swap\":\"String\"}\n'zwrt_bsp.usb' @00000003\n\t\"list\":{}\n";

fn identity() -> IdentityInput {
    IdentityInput {
        device_id: "test-device-a".into(),
        vendor: Some("ZTE".into()),
        model_name: Some("MU5250".into()),
        hardware_version: None,
        hardware_revision: Some("test-revision".into()),
        board_name: None,
        identity_evidence_ids: vec!["identity".into()],
        firmware: FirmwareProfile {
            raw_versions: vec![RawVersion {
                namespace: "vendor.external".into(),
                source: "test input".into(),
                value: "CN_ZTE_MU5250V1.0.0B31".into(),
                evidence_id: "identity".into(),
                full: true,
            }],
            product_line: Some("CN_ZTE".into()),
            region: Some("CN".into()),
            version: Some("CN_ZTE_MU5250V1.0.0B31".into()),
            build: None,
            modem_build: None,
            active_slot: Some("b".into()),
            completeness: IdentityStatus::Partial,
        },
        provenance: Provenance {
            cohort: Cohort::Upgraded,
            previous_firmware: None,
            evidence_ids: vec![],
        },
        components: vec![],
    }
}

fn profile() -> DeviceProfile {
    build_profile(identity())
}

fn scope(profile: &DeviceProfile) -> Scope {
    Scope {
        device_id: profile.device_id.clone(),
        fingerprint: profile.fingerprint.clone(),
        collector_id: "test-core-instance".into(),
        generation: 1,
        rule_revision: super::resolve::RULE_REVISION,
        adapter_revision: profile.adapter_revision,
        component_revision: "test-code-revision".into(),
        cohort: profile.provenance.cohort,
        context: Context {
            sim_epoch: Some("sim1".into()),
            connection_epoch: Some("connection1".into()),
            usb_epoch: Some("usb1".into()),
            ubus_epoch: Some("ubus1".into()),
            radio_mode: Some("ONLINE".into()),
            radio_epoch: Some("synthetic-radio".into()),
        },
    }
}

fn now(seconds: u64) -> BootTime {
    BootTime {
        boot_id: "test-boot".into(),
        seconds,
    }
}

fn listing(text: &str, object: &str) -> ProbeResult {
    method_result(
        ListingInput {
            text,
            exit_code: Some(0),
            complete: true,
            truncated: false,
            duration_ms: 1,
        },
        object,
        "set",
    )
}

fn surface(s: &Scope, object: &str, text: &str) -> Evidence {
    // Unit-test live metadata is synthetic, not a claim about the recorded device's identity.
    let (capability_id, probe_id) = match object {
        "zwrt_bsp.usb" => ("usb.mode.setter.surface", "ubus.usb_methods.v1"),
        "zwrt_bsp.typec" => ("usb.typec.setter.surface", "ubus.typec_methods.v1"),
        _ => ("usb.powerbank.setter.surface", "ubus.powerbank_methods.v1"),
    };
    Evidence {
        id: object.into(),
        capability_id: capability_id.into(),
        facet: Facet::Surface,
        origin: EvidenceOrigin::Live,
        source_kind: SourceKind::DeviceCapture,
        verification: Verification::Captured,
        scope: Some(s.clone()),
        observed_on: None,
        recorded_on: "test date, not used for freshness".into(),
        time: EvidenceTime {
            observed_at: Some(now(100)),
            utc: None,
            utc_quality: UtcQuality::Unknown,
        },
        operation: Operation {
            probe_id: probe_id.into(),
            target: format!("{object}:set"),
            verifier_id: None,
            args: None,
        },
        result: Some(listing(text, object)),
        source_ref: "synthetic unit-test observation".into(),
        locator: None,
        artifact: Artifact {
            sha256: None,
            parser_revision: Some("test-parser".into()),
            redacted: true,
        },
        summary: "Synthetic metadata; only the listing shape is recorded".into(),
        limitations: vec![],
    }
}

fn read(s: &Scope) -> Evidence {
    let mut e = surface(s, "zwrt_bsp.usb", RECORDED);
    e.id = "read".into();
    e.capability_id = "usb.status.read".into();
    e.facet = Facet::Read;
    e.operation.probe_id = "usb.cached_read.v1".into();
    e.operation.target = "zwrt_bsp.usb:list".into();
    e.result = Some(ProbeResult {
        outcome: ProbeOutcome::Success,
        exit_code: Some(0),
        vendor_code: None,
        value: Some(json!({"mode":"user"})),
        proof: Some(Proof::ReadValidated),
        complete: true,
        truncated: false,
        duration_ms: 1,
    });
    e
}

fn write(s: &Scope) -> Evidence {
    let mut e = read(s);
    e.id = "write".into();
    e.capability_id = "usb.attach.share".into();
    e.facet = Facet::Write;
    e.verification = Verification::WriteVerified;
    e.operation.probe_id = "usb.attach.share.v1".into();
    e.operation.target = "zwrt_bsp.typec:set".into();
    e.operation.verifier_id = Some("usb.share.readback.v1".into());
    e.result.as_mut().unwrap().proof = Some(Proof::WriteVerified);
    e
}

fn assess(
    profile: &DeviceProfile,
    scope: &Scope,
    ev: &[Evidence],
    id: &str,
) -> CapabilityAssessment {
    resolve(
        &ResolveInput {
            profile,
            scope,
            now: Some(&now(101)),
            evidence: ev,
            availability: Availability {
                state: AvailabilityState::Unknown,
                reason: "not sampled".into(),
            },
        },
        id,
    )
}

#[test]
fn recorded_objects_are_independent_and_absence_only_disproves_exact_write_path() {
    let p = profile();
    let s = scope(&p);
    let ev = [
        surface(&s, "zwrt_bsp.usb", RECORDED),
        surface(&s, "zwrt_bsp.typec", RECORDED),
        surface(&s, "zwrt_bsp.powerbank", RECORDED),
    ];
    for (id, expected) in [
        ("usb.mode.setter.surface", Support::Unsupported),
        ("usb.typec.setter.surface", Support::Supported),
        ("usb.powerbank.setter.surface", Support::Supported),
        ("usb.mode.set", Support::Unsupported),
        ("usb.attach.share", Support::Unknown),
        ("usb.attach.fast_charge", Support::Unknown),
        ("usb.adb", Support::Unknown),
    ] {
        assert_eq!(assess(&p, &s, &ev, id).status, expected, "{id}");
    }
}

#[test]
fn method_presence_read_and_ack_do_not_prove_write_effect() {
    let p = profile();
    let s = scope(&p);
    let method = surface(&s, "zwrt_bsp.typec", RECORDED);
    let mut e = write(&s);
    for proof in [None, Some(Proof::MethodPresent), Some(Proof::ReadValidated)] {
        e.result.as_mut().unwrap().proof = proof;
        assert_eq!(
            assess(&p, &s, &[method.clone(), e.clone()], "usb.attach.share").status,
            Support::Unknown
        );
    }
    e.result.as_mut().unwrap().proof = Some(Proof::WriteVerified);
    e.operation.verifier_id = Some("wrong-action-verifier".into());
    assert_eq!(
        assess(&p, &s, &[method.clone(), e.clone()], "usb.attach.share").status,
        Support::Unknown
    );
    e.operation.verifier_id = Some("usb.share.readback.v1".into());
    let a = assess(&p, &s, &[method, e], "usb.attach.share");
    assert_eq!(a.status, Support::Supported);
    assert_eq!(a.evidence_ids, ["write", "zwrt_bsp.typec"]);
    assert_eq!(a.availability.state, AvailabilityState::Unknown);
}

#[test]
fn read_is_its_own_facet_and_missing_validation_stays_unknown() {
    let p = profile();
    let s = scope(&p);
    let mut e = read(&s);
    assert_eq!(
        assess(&p, &s, std::slice::from_ref(&e), "usb.status.read").status,
        Support::Supported
    );
    e.result.as_mut().unwrap().proof = None;
    assert_eq!(
        assess(&p, &s, &[e], "usb.status.read").status,
        Support::Unknown
    );
}

#[test]
fn malformed_failed_and_truncated_listings_never_prove_absence() {
    let samples = [
        "Command failed: Not found",
        "\"set\":{}",
        "'zwrt_bsp.usb' @1\n",
        "'zwrt_bsp.usb' @1\n\t\"list\":{\n",
        "'zwrt_bsp.usb' @1\n\t\"set\":{},\"set\":{}\n",
        "'zwrt_bsp.usb' @1\n\t\"list\":{}\n\t\"list\":{}\n",
        "'zwrt_bsp.usb' @1\n\t\"list\":{}\n'zwrt_bsp.usb' @2\n\t\"set\":{}\n",
    ];
    for text in samples {
        assert_eq!(listing(text, "zwrt_bsp.usb").proof, None, "{text}");
    }
    for (exit_code, complete, truncated) in [
        (Some(1), true, false),
        (None, true, false),
        (Some(0), false, false),
        (Some(0), true, true),
    ] {
        let r = method_result(
            ListingInput {
                text: RECORDED,
                exit_code,
                complete,
                truncated,
                duration_ms: 0,
            },
            "zwrt_bsp.usb",
            "set",
        );
        assert_eq!(r.proof, None);
    }
    assert_eq!(listing(RECORDED, "not.registered").proof, None);
    let oversized = format!("{}{}", " ".repeat(256 * 1024), RECORDED);
    assert_eq!(listing(&oversized, "zwrt_bsp.usb").proof, None);
}

#[test]
fn historical_code_external_and_replay_cannot_become_live_support() {
    let p = profile();
    let s = scope(&p);
    for origin in [EvidenceOrigin::Historical, EvidenceOrigin::Replay] {
        let mut e = read(&s);
        e.origin = origin;
        assert_eq!(
            assess(&p, &s, &[e], "usb.status.read").status,
            Support::Unknown
        );
    }
    for source in [
        SourceKind::Code,
        SourceKind::DeviceReport,
        SourceKind::ExternalReport,
        SourceKind::Fixture,
        SourceKind::Inference,
    ] {
        let mut e = read(&s);
        e.source_kind = source;
        assert_eq!(
            assess(&p, &s, &[e], "usb.status.read").status,
            Support::Unknown
        );
    }
    for verification in [
        Verification::CodeReviewed,
        Verification::Documented,
        Verification::Replayed,
    ] {
        let mut e = read(&s);
        e.verification = verification;
        assert_eq!(
            assess(&p, &s, &[e], "usb.status.read").status,
            Support::Unknown
        );
    }
}

#[test]
fn expiry_missing_clock_backwards_time_and_other_boot_are_unknown() {
    let p = profile();
    let s = scope(&p);
    let e = read(&s);
    for time in [
        Some(now(161)),
        Some(now(99)),
        Some(BootTime {
            boot_id: "other".into(),
            seconds: 101,
        }),
        Some(BootTime {
            boot_id: "".into(),
            seconds: 101,
        }),
        None,
    ] {
        let a = resolve(
            &ResolveInput {
                profile: &p,
                scope: &s,
                now: time.as_ref(),
                evidence: std::slice::from_ref(&e),
                availability: Availability {
                    state: AvailabilityState::Unknown,
                    reason: "test".into(),
                },
            },
            "usb.status.read",
        );
        assert_eq!(a.status, Support::Unknown);
        assert!(!a.freshness.fresh);
    }
    let a = resolve(
        &ResolveInput {
            profile: &p,
            scope: &s,
            now: Some(&now(160)),
            evidence: &[e],
            availability: Availability {
                state: AvailabilityState::Unknown,
                reason: "test".into(),
            },
        },
        "usb.status.read",
    );
    assert_eq!(a.status, Support::Supported);
    assert_eq!(a.freshness.age_seconds, Some(60));
}

#[test]
fn wall_dates_never_change_freshness() {
    let p = profile();
    let s = scope(&p);
    let e = read(&s);
    let baseline = assess(&p, &s, std::slice::from_ref(&e), "usb.status.read");
    let mut jumped = e;
    jumped.recorded_on = "2099-12-31".into();
    jumped.observed_on = Some("1970-01-01".into());
    jumped.time.utc = Some("2099-12-31T00:00:00Z".into());
    jumped.time.utc_quality = UtcQuality::Trusted;
    assert_eq!(assess(&p, &s, &[jumped], "usb.status.read"), baseline);
}

#[test]
fn instance_identity_rules_adapter_component_and_usb_changes_invalidate_evidence() {
    let p = profile();
    let s = scope(&p);
    for change in 0..11 {
        let mut e = read(&s);
        let bound = e.scope.as_mut().unwrap();
        match change {
            0 => bound.device_id = "device-b".into(),
            1 => bound.fingerprint = Some("other firmware".into()),
            2 => bound.collector_id = "restarted-core".into(),
            3 => bound.generation += 1,
            4 => bound.rule_revision += 1,
            5 => bound.adapter_revision += 1,
            6 => bound.component_revision = "other code".into(),
            7 => bound.cohort = Cohort::Native,
            8 => bound.context.usb_epoch = Some("replugged".into()),
            9 => bound.context.ubus_epoch = Some("reconnected".into()),
            _ => bound.context.usb_epoch = None,
        }
        assert_eq!(
            assess(&p, &s, &[e], "usb.status.read").status,
            Support::Unknown,
            "{change}"
        );
    }
    let mut missing = s.clone();
    missing.context.usb_epoch = None;
    assert_eq!(
        assess(&p, &missing, &[read(&missing)], "usb.status.read").status,
        Support::Unknown
    );
}

#[test]
fn unrelated_epochs_do_not_invalidate_surface_but_qos_requires_connection_context() {
    let p = profile();
    let s = scope(&p);
    let mut current = s.clone();
    current.context.usb_epoch = Some("replugged".into());
    current.context.sim_epoch = Some("new-sim".into());
    assert_eq!(
        assess(
            &p,
            &current,
            &[surface(&s, "zwrt_bsp.typec", RECORDED)],
            "usb.typec.setter.surface"
        )
        .status,
        Support::Supported
    );
    let mut e = read(&s);
    e.capability_id = "qos.nr_pdu.log.read".into();
    e.operation.probe_id = "qos.cached.v1".into();
    e.operation.target = "qos:nr_pdu".into();
    assert_eq!(
        assess(&p, &s, std::slice::from_ref(&e), "qos.nr_pdu.log.read").status,
        Support::Supported
    );
    assert_eq!(
        assess(&p, &s, std::slice::from_ref(&e), "qos.nr_pdu.log.read")
            .selected_dialect
            .as_deref(),
        Some("qos.nr_pdu.log.v1")
    );
    for change in 0..3 {
        let mut current = s.clone();
        match change {
            0 => current.context.sim_epoch = Some("new".into()),
            1 => current.context.connection_epoch = Some("new".into()),
            _ => current.context.radio_mode = Some("LTE".into()),
        }
        assert_eq!(
            assess(
                &p,
                &current,
                std::slice::from_ref(&e),
                "qos.nr_pdu.log.read"
            )
            .status,
            Support::Unknown
        );
    }
}

#[test]
fn transient_failures_keep_support_and_availability_is_separate() {
    let p = profile();
    let s = scope(&p);
    let good = read(&s);
    for outcome in [
        ProbeOutcome::Busy,
        ProbeOutcome::Held,
        ProbeOutcome::Timeout,
        ProbeOutcome::Denied,
        ProbeOutcome::Unavailable,
        ProbeOutcome::Empty,
        ProbeOutcome::Skipped,
    ] {
        let mut failure = good.clone();
        failure.id = "latest-failure".into();
        failure.time.observed_at = Some(now(101));
        failure.result.as_mut().unwrap().outcome = outcome;
        let a = resolve(
            &ResolveInput {
                profile: &p,
                scope: &s,
                now: Some(&now(101)),
                evidence: &[good.clone(), failure],
                availability: Availability {
                    state: AvailabilityState::Blocked,
                    reason: "at_busy".into(),
                },
            },
            "usb.status.read",
        );
        assert_eq!(a.status, Support::Supported);
        assert_eq!(
            a.last_attempt_evidence_id.as_deref(),
            Some("latest-failure")
        );
        assert_eq!(a.availability.state, AvailabilityState::Blocked);
    }
}

#[test]
fn ordinary_error_even_labelled_explicit_is_not_unsupported() {
    let p = profile();
    let s = scope(&p);
    let mut e = read(&s);
    e.result.as_mut().unwrap().outcome = ProbeOutcome::ExplicitUnsupported;
    e.result.as_mut().unwrap().vendor_code = Some("ERROR".into());
    assert_eq!(
        assess(&p, &s, &[e], "usb.status.read").status,
        Support::Unknown
    );
}

#[test]
fn opposite_direct_evidence_is_preserved_in_either_order() {
    let p = profile();
    let s = scope(&p);
    let absent = surface(&s, "zwrt_bsp.usb", RECORDED);
    let mut present = surface(&s, "zwrt_bsp.usb", "'zwrt_bsp.usb' @1\n\t\"set\":{}\n");
    present.id = "present".into();
    let a = assess(
        &p,
        &s,
        &[absent.clone(), present.clone()],
        "usb.mode.setter.surface",
    );
    let b = assess(&p, &s, &[present, absent], "usb.mode.setter.surface");
    assert_eq!(a, b);
    assert_eq!(a.reason, AssessmentReason::ConflictingEvidence);
    assert_eq!(a.status, Support::Unknown);
    assert_eq!(a.contradicting_evidence_ids, ["zwrt_bsp.usb"]);
}

#[test]
fn effect_conflicting_with_missing_dependency_does_not_become_unsupported() {
    let p = profile();
    let s = scope(&p);
    let mut e = write(&s);
    e.capability_id = "usb.mode.set".into();
    e.operation.probe_id = "usb.mode.set.v1".into();
    e.operation.target = "zwrt_bsp.usb:set".into();
    e.operation.verifier_id = Some("usb.mode.readback.v1".into());
    let a = assess(
        &p,
        &s,
        &[e, surface(&s, "zwrt_bsp.usb", RECORDED)],
        "usb.mode.set",
    );
    assert_eq!(a.status, Support::Unknown);
    assert_eq!(a.reason, AssessmentReason::ConflictingEvidence);
    assert!(!a.contradicting_evidence_ids.is_empty());
}

#[test]
fn duplicate_evidence_ids_fail_closed() {
    let p = profile();
    let s = scope(&p);
    let e = read(&s);
    assert_eq!(
        assess(&p, &s, &[e.clone(), e], "usb.status.read").reason,
        AssessmentReason::InvalidEvidence
    );
}

#[test]
fn model_identity_conflicts_missing_full_version_and_unknown_models_stay_unknown() {
    let mut input = identity();
    input.hardware_version = Some("MU5252".into());
    let p = build_profile(input);
    assert_eq!(p.identity_status, IdentityStatus::Conflict);
    assert!(p.adapter_id.is_none());
    assert!(p.fingerprint.is_none());
    let mut input = identity();
    input.firmware.raw_versions[0].full = false;
    let partial = build_profile(input);
    assert_eq!(partial.identity_status, IdentityStatus::Partial);
    assert_eq!(
        assess(&partial, &scope(&partial), &[], "usb.status.read").reason,
        AssessmentReason::IdentityPartial
    );
    let mut input = identity();
    input.model_name = Some("unrecognized".into());
    input.hardware_version = Some("MU5250".into());
    let p = build_profile(input);
    assert!(p.adapter_id.is_none());
    assert!(p.fingerprint.is_none());
    let mut input = identity();
    input.model_name = None;
    input.hardware_version = Some("mu5252".into());
    assert_eq!(build_profile(input).model.as_deref(), Some("MU5252"));
}

#[test]
fn raw_versions_are_preserved_and_only_same_namespace_conflicts() {
    let mut input = identity();
    let mut raw = input.firmware.raw_versions[0].clone();
    raw.value = "BD_CNMU5250V1.0.0B31".into();
    raw.namespace = "vendor.internal".into();
    input.firmware.raw_versions.push(raw);
    let p = build_profile(input);
    assert_eq!(p.identity_status, IdentityStatus::Complete);
    assert_eq!(p.firmware.raw_versions.len(), 2);
    let mut input = identity();
    let mut raw = input.firmware.raw_versions[0].clone();
    raw.value = "different build".into();
    input.firmware.raw_versions.push(raw);
    assert_eq!(
        build_profile(input).identity_status,
        IdentityStatus::Conflict
    );
}

#[test]
fn firmware_slot_build_and_branch_are_part_of_fingerprint() {
    let baseline = profile().fingerprint.unwrap();
    for change in 0..5 {
        let mut input = identity();
        match change {
            0 => input.firmware.active_slot = Some("a".into()),
            1 => input.firmware.build = Some("other build".into()),
            2 => input.firmware.product_line = Some("FLY".into()),
            3 => input.firmware.modem_build = Some("other modem".into()),
            _ => input.firmware.region = None,
        }
        assert_ne!(
            build_profile(input).fingerprint.as_deref(),
            Some(baseline.as_str())
        );
    }
    let mut input = identity();
    input.provenance.cohort = Cohort::Native;
    // Provenance is independently checked in Scope; it does not change firmware identity.
    assert_eq!(
        build_profile(input).fingerprint.as_deref(),
        Some(baseline.as_str())
    );
}

#[test]
fn model_policy_matches_existing_source_choices_without_granting_support() {
    let expected = [
        (
            "MU5250",
            NetworkSource::UbusOnly,
            CpuTemperatureSource::CpussOnly,
            WifiSectionSource::Main2g,
        ),
        (
            "MU5252",
            NetworkSource::UbusWithUciFallback,
            CpuTemperatureSource::CpussOnly,
            WifiSectionSource::FirstEnabledThenPresent,
        ),
        (
            "MC7523",
            NetworkSource::UbusOnly,
            CpuTemperatureSource::NamedFieldsThenSysfs,
            WifiSectionSource::FirstEnabledThenPresent,
        ),
        (
            "MC8532B",
            NetworkSource::UbusWithUciFallback,
            CpuTemperatureSource::NamedFieldsThenSysfs,
            WifiSectionSource::FirstEnabledThenPresent,
        ),
    ];
    for (model, network, temperature, wifi) in expected {
        let a = model_adapter(model).unwrap();
        assert_eq!(
            (a.network_source, a.cpu_temperature, a.wifi_section),
            (network, temperature, wifi)
        );
        assert!(a.command_dialects.is_empty());
        assert!(a.firmware_fingerprint.is_none());
    }
    assert!(model_adapter("MU5250 B31").is_none());
}

#[test]
fn unsafe_lpm_and_offline_at_remain_blocked_even_if_caller_says_ready() {
    let p = profile();
    let mut s = scope(&p);
    s.context.radio_mode = Some("LOW_POWER".into());
    for id in ["modem.recover_from_lpm", "at.transport.read"] {
        let a = resolve(
            &ResolveInput {
                profile: &p,
                scope: &s,
                now: Some(&now(101)),
                evidence: &[],
                availability: Availability {
                    state: AvailabilityState::Ready,
                    reason: "test".into(),
                },
            },
            id,
        );
        assert_eq!(a.availability.state, AvailabilityState::Blocked);
    }
    // Expired or never-read radio mode is unknown, not a claim that the radio is offline.
    s.context.radio_mode = None;
    let a = resolve(
        &ResolveInput {
            profile: &p,
            scope: &s,
            now: Some(&now(101)),
            evidence: &[],
            availability: Availability {
                state: AvailabilityState::Ready,
                reason: "test".into(),
            },
        },
        "at.transport.read",
    );
    assert_eq!(
        (a.availability.state, a.availability.reason.as_str()),
        (AvailabilityState::Unknown, "radio_not_observed")
    );
}

#[test]
fn changed_operation_binding_never_reuses_another_methods_success() {
    let p = profile();
    let s = scope(&p);
    for change in 0..3 {
        let mut e = read(&s);
        match change {
            0 => e.operation.probe_id = "unknown".into(),
            1 => e.operation.target = "zwrt_bsp.typec:list".into(),
            _ => e.facet = Facet::Surface,
        }
        assert_eq!(
            assess(&p, &s, &[e], "usb.status.read").status,
            Support::Unknown
        );
    }
}

#[test]
fn listing_proof_is_bound_to_its_object_and_method() {
    let p = profile();
    let s = scope(&p);
    let mut e = surface(&s, "zwrt_bsp.typec", RECORDED);
    e.result = Some(listing(RECORDED, "zwrt_bsp.usb"));
    assert_eq!(
        assess(&p, &s, &[e], "usb.typec.setter.surface").status,
        Support::Unknown
    );
}

#[test]
fn complete_flags_do_not_rescue_unbound_firmware_or_wrong_adapter() {
    for change in 0..3 {
        let mut p = profile();
        let s = scope(&p);
        match change {
            0 => p.firmware.raw_versions[0].full = false,
            1 => {
                let mut raw = p.firmware.raw_versions[0].clone();
                raw.value = "conflicting version".into();
                p.firmware.raw_versions.push(raw);
            }
            _ => p.adapter_id = Some("different adapter".into()),
        }
        let a = assess(&p, &s, &[read(&s)], "usb.status.read");
        assert_eq!(a.status, Support::Unknown);
        assert_eq!(a.reason, AssessmentReason::InvalidScope);
    }
}

#[test]
fn unknown_explanations_do_not_depend_on_evidence_order() {
    let p = profile();
    let s = scope(&p);
    let mut weak = read(&s);
    weak.id = "weak".into();
    weak.origin = EvidenceOrigin::Replay;
    let mut stale = read(&s);
    stale.id = "stale".into();
    stale.time.observed_at = Some(now(0));
    assert_eq!(
        assess(&p, &s, &[weak.clone(), stale.clone()], "usb.status.read"),
        assess(&p, &s, &[stale, weak], "usb.status.read")
    );
}

#[test]
fn versioned_report_roundtrip_rejects_bad_schema_enums_duplicates_and_dangling_ids() {
    let p = profile();
    let s = scope(&p);
    let mut ev = vec![read(&s)];
    let mut identity = ev[0].clone();
    identity.id = "identity".into();
    identity.capability_id = "identity".into();
    identity.origin = EvidenceOrigin::Historical;
    identity.scope = None;
    ev.push(identity);
    let report = CapabilityReport {
        context_coverage: Default::default(),
        schema_version: SCHEMA_VERSION,
        profile: p.clone(),
        assessments: vec![assess(&p, &s, &ev, "usb.status.read")],
        evidence: ev,
    };
    let encoded = serde_json::to_value(&report).unwrap();
    assert_eq!(
        CapabilityReport::decode(&encoded.to_string()).unwrap(),
        report
    );
    for change in 0..7 {
        let mut bad = encoded.clone();
        match change {
            0 => bad["schema_version"] = json!(2),
            1 => bad["assessments"][0]["status"] = json!("probably"),
            2 => bad["assessments"][0]["evidence_ids"] = json!(["missing"]),
            3 => bad["evidence"]
                .as_array_mut()
                .unwrap()
                .push(encoded["evidence"][0].clone()),
            4 => bad["assessments"]
                .as_array_mut()
                .unwrap()
                .push(encoded["assessments"][0].clone()),
            5 => {
                bad.as_object_mut().unwrap().remove("schema_version");
            }
            _ => bad["assessments"][0]["evidence_ids"] = json!([]),
        }
        assert!(
            CapabilityReport::decode(&bad.to_string()).is_err(),
            "{change}"
        );
    }
    assert!(
        CapabilityReport::decode("{}")
        .is_err()
    );
}

#[test]
fn online_value_without_radio_epoch_cannot_authorize_at_or_qos() {
    let p = profile();
    let mut s = scope(&p);
    s.context.radio_epoch = None;
    let mut raw = serde_json::to_value(&s.context).unwrap();
    raw.as_object_mut().unwrap().remove("radio_epoch");
    let old: Context = serde_json::from_value(raw).unwrap();
    assert_eq!(old.radio_epoch, None);
    let evidence = vec![];
    for id in [
        "at.transport.read",
        "at.pdp_contexts.read",
        "at.eps_qos.read",
        "qos.eps.log.read",
    ] {
        let a = assess(&p, &s, &evidence, id);
        assert_eq!(a.status, Support::Unknown);
        assert_eq!(a.selected_dialect, None);
    }
}
