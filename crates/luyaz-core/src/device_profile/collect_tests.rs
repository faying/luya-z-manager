//! All identities, clocks and context epochs here are synthetic, not new device evidence.
use super::*;

fn at(seconds: u64) -> BootTime {
    BootTime {
        boot_id: "synthetic-boot".into(),
        seconds,
    }
}
fn common(version: &str) -> Value {
    json!({"manufacturer":"ZTE", "model_name":"MU5250", "hardware_version":"MU5250_HW1.0",
        "integrate_version":version, "wa_inner_version":"BD_CN_ZTE_MU5250V1.0.0B31",
        "imei":"DO_NOT_EXPORT_IMEI", "imsi":"DO_NOT_EXPORT_IMSI"})
}
fn identity(c: &Collector, version: &str) {
    c.ubus(
        c.token(),
        Some(at(1)),
        "zwrt_zte_mdm.api",
        "get_zwrt_common_info",
        Some(&common(version)),
    );
}
fn epochs(c: &Collector) {
    c.0.lock().unwrap().radio_at = Some(at(1));
    c.0.lock().unwrap().context = Context {
        ubus_epoch: Some("synthetic-ubus".into()),
        usb_epoch: Some("synthetic-usb".into()),
        radio_mode: Some("ONLINE".into()),
        radio_epoch: Some("synthetic-radio".into()),
        sim_epoch: None,
        connection_epoch: None,
    };
}
fn listing(c: &Collector, token: Scope, seconds: u64, text: &str, complete: bool) {
    c.usb_methods(
        token,
        Some(at(seconds)),
        ListingInput {
            text,
            exit_code: Some(0),
            complete,
            truncated: false,
            duration_ms: 2,
        },
    );
}
const ABSENT: &str = "'zwrt_bsp.usb' @1234\n\t\"list\":{}\n";
const PRESENT: &str = "'zwrt_bsp.usb' @1234\n\t\"list\":{}\n\t\"set\":{\"mode\":\"String\"}\n";
fn assessment<'a>(r: &'a CapabilityReport, id: &str) -> &'a CapabilityAssessment {
    r.assessments
        .iter()
        .find(|a| a.capability_id == id)
        .unwrap()
}

#[test]
fn empty_report_is_complete_schema_and_unique_per_collector() {
    let c = Collector::new();
    let report = c.report(Some(&at(1)));
    assert_eq!(report.assessments.len(), 13);
    assert!(
        report
            .assessments
            .iter()
            .all(|a| a.status == Support::Unknown)
    );
    assert_eq!(report.profile.identity_status, IdentityStatus::Partial);
    assert!(report.evidence.is_empty());
    report.validate().unwrap();
    assert_eq!(
        CapabilityReport::decode(&serde_json::to_string(&report).unwrap()).unwrap(),
        report
    );
    let other = Collector::new();
    assert_ne!(c.token().collector_id, other.token().collector_id);
    assert_ne!(c.token().device_id, other.token().device_id);
}

#[test]
fn decoder_preserves_namespaces_and_never_exports_personal_fields() {
    let c = Collector::new();
    identity(&c, "CN_ZTE_MU5250V1.0.0B31");
    let r = c.report(Some(&at(2)));
    assert_eq!(r.profile.identity_status, IdentityStatus::Complete);
    assert_eq!(r.profile.firmware.product_line.as_deref(), Some("CN_ZTE"));
    assert_eq!(r.profile.firmware.region.as_deref(), Some("CN"));
    assert_eq!(r.profile.firmware.raw_versions.len(), 2);
    assert!(
        r.profile
            .firmware
            .raw_versions
            .iter()
            .any(|v| v.namespace == "zte.external" && v.full)
    );
    assert!(
        r.profile
            .firmware
            .raw_versions
            .iter()
            .any(|v| v.namespace == "zte.internal" && !v.full)
    );
    assert_eq!(r.profile.provenance.cohort, Cohort::Unknown);
    assert_eq!(r.profile.firmware.active_slot, None);
    let encoded = serde_json::to_string(&r).unwrap();
    assert!(!encoded.contains("DO_NOT_EXPORT"));
    r.validate().unwrap();
}

#[test]
fn suffix_and_unknown_or_mismatched_external_grammar_remain_partial() {
    for label in [
        "B31",
        "BD_CN_ZTE_MU5250V1.0.0B31",
        "CN_ZTE_MU5252V1.0.0B31",
        "CN_ZTE_MU5250V1.0B31",
        "CN_ZTE_MU5250V1.0.0B31-extra",
        "UNKNOWN_MU5250V1.0.0B31",
    ] {
        let c = Collector::new();
        identity(&c, label);
        assert_eq!(
            c.report(Some(&at(2))).profile.identity_status,
            IdentityStatus::Partial,
            "{label}"
        );
    }
    let c = Collector::new();
    identity(&c, "FLY_CN_MU5250V1.0.0B11");
    let r = c.report(Some(&at(2)));
    assert_eq!(r.profile.firmware.product_line.as_deref(), Some("FLY"));
    assert_eq!(r.profile.identity_status, IdentityStatus::Complete);
}

#[test]
fn same_namespace_uci_disagreement_blocks_identity_without_overwriting_raw() {
    let c = Collector::new();
    identity(&c, "CN_ZTE_MU5250V1.0.0B31");
    c.uci(
        c.token(),
        "zwrt_common_info",
        &BTreeMap::from([
            (
                "zwrt_common_info.common_config.integrate_version".into(),
                "CN_ZTE_MU5250V1.0.0B27".into(),
            ),
            ("zwrt_common_info.secret".into(), "DO_NOT_EXPORT".into()),
        ]),
        Some(at(2)),
    );
    let r = c.report(Some(&at(3)));
    assert_eq!(r.profile.identity_status, IdentityStatus::Conflict);
    assert!(r.profile.fingerprint.is_none());
    assert_eq!(r.profile.firmware.raw_versions.len(), 3);
    r.validate().unwrap();
    assert!(!serde_json::to_string(&r).unwrap().contains("DO_NOT_EXPORT"));
}

#[test]
fn observation_does_not_fabricate_context_and_surface_never_proves_write() {
    let c = Collector::new();
    identity(&c, "CN_ZTE_MU5250V1.0.0B31");
    listing(&c, c.token(), 2, PRESENT, true);
    let r = c.report(Some(&at(3)));
    assert_eq!(r.assessments[0].scope.context.ubus_epoch, None);
    assert_eq!(
        assessment(&r, "usb.mode.setter.surface").status,
        Support::Unknown
    );
    // With synthetic authoritative epochs, only the method surface can become supported.
    epochs(&c);
    listing(&c, c.token(), 3, PRESENT, true);
    let r = c.report(Some(&at(4)));
    assert_eq!(
        assessment(&r, "usb.mode.setter.surface").status,
        Support::Supported
    );
    assert_eq!(assessment(&r, "usb.mode.set").status, Support::Unknown);
    assert_eq!(
        assessment(&r, "modem.recover_from_lpm").availability.state,
        AvailabilityState::Blocked
    );
}

#[test]
fn identity_changes_reject_late_results_and_repeated_reads_do_not_bump_generation() {
    let c = Collector::new();
    identity(&c, "CN_ZTE_MU5250V1.0.0B31");
    epochs(&c);
    let old = c.token();
    identity(&c, "CN_ZTE_MU5250V1.0.0B31");
    assert_eq!(old.generation, c.token().generation);
    identity(&c, "CN_ZTE_MU5250V1.0.0B27");
    assert_ne!(old.generation, c.token().generation);
    listing(&c, old.clone(), 4, PRESENT, true);
    let r = c.report(Some(&at(5)));
    assert_eq!(
        assessment(&r, "usb.mode.setter.surface").status,
        Support::Unknown
    );
    assert_eq!(
        r.evidence
            .iter()
            .find(|e| e.capability_id.ends_with(".surface"))
            .unwrap()
            .scope
            .as_ref(),
        Some(&old)
    );
    // A stale identity result cannot undo a newer firmware observation.
    c.ubus(
        old,
        Some(at(5)),
        "zwrt_zte_mdm.api",
        "get_zwrt_common_info",
        Some(&common("CN_ZTE_MU5250V1.0.0B31")),
    );
    assert_eq!(
        c.report(Some(&at(6))).profile.firmware.version.as_deref(),
        Some("CN_ZTE_MU5250V1.0.0B27")
    );
}

#[test]
fn fresh_conflicts_survive_and_transient_attempts_do_not_delete_proof() {
    let c = Collector::new();
    identity(&c, "CN_ZTE_MU5250V1.0.0B31");
    epochs(&c);
    listing(&c, c.token(), 2, ABSENT, true);
    let r = c.report(Some(&at(3)));
    assert_eq!(
        assessment(&r, "usb.mode.setter.surface").status,
        Support::Unsupported
    );
    assert_eq!(assessment(&r, "usb.mode.set").status, Support::Unsupported);
    listing(&c, c.token(), 3, PRESENT, false);
    assert_eq!(
        assessment(&c.report(Some(&at(4))), "usb.mode.setter.surface").status,
        Support::Unsupported
    );
    listing(&c, c.token(), 4, PRESENT, true);
    let r = c.report(Some(&at(5)));
    assert_eq!(
        assessment(&r, "usb.mode.setter.surface").reason,
        AssessmentReason::ConflictingEvidence
    );
    assert_eq!(
        assessment(&r, "usb.mode.setter.surface").status,
        Support::Unknown
    );
}

#[test]
fn reports_recompute_expiration_without_mutating_or_renewing_evidence() {
    let c = Collector::new();
    identity(&c, "CN_ZTE_MU5250V1.0.0B31");
    epochs(&c);
    listing(&c, c.token(), 2, PRESENT, true);
    let fresh = c.report(Some(&at(62)));
    assert_eq!(
        assessment(&fresh, "usb.mode.setter.surface").status,
        Support::Supported
    );
    let stale = c.report(Some(&at(63)));
    assert_eq!(
        assessment(&stale, "usb.mode.setter.surface").reason,
        AssessmentReason::StaleEvidence
    );
    assert_eq!(fresh.evidence, stale.evidence);
    assert_eq!(
        assessment(&c.report(None), "usb.mode.setter.surface").status,
        Support::Unknown
    );
    let reboot = BootTime {
        boot_id: "new-boot".into(),
        seconds: 3,
    };
    assert_eq!(
        assessment(&c.report(Some(&reboot)), "usb.mode.setter.surface").status,
        Support::Unknown
    );
}

#[test]
fn evidence_is_bounded_while_identity_references_remain_valid() {
    let c = Collector::new();
    identity(&c, "CN_ZTE_MU5250V1.0.0B31");
    epochs(&c);
    for i in 2..302 {
        let token = c.token();
        listing(
            &c,
            token.clone(),
            i,
            if i % 2 == 0 { PRESENT } else { ABSENT },
            true,
        );
        listing(&c, token, i, "bad listing", false);
    }
    let r = c.report(Some(&at(302)));
    assert_eq!(r.evidence.len(), 4); // identity + present + absent + failed attempt
    r.validate().unwrap();
}

#[test]
fn usb_decoder_requires_known_mode_and_keeps_only_selected_fields() {
    let c = Collector::new();
    identity(&c, "CN_ZTE_MU5250V1.0.0B31");
    epochs(&c);
    c.ubus(
        c.token(),
        Some(at(2)),
        "zwrt_bsp.usb",
        "list",
        Some(&json!({"mode":"user", "secret":"DO_NOT_EXPORT"})),
    );
    let r = c.report(Some(&at(3)));
    assert_eq!(assessment(&r, "usb.status.read").status, Support::Supported);
    assert!(!serde_json::to_string(&r).unwrap().contains("DO_NOT_EXPORT"));
    for data in [json!({}), json!({"mode":true}), json!({"mode":"unknown"})] {
        let c = Collector::new();
        identity(&c, "CN_ZTE_MU5250V1.0.0B31");
        epochs(&c);
        c.ubus(c.token(), Some(at(2)), "zwrt_bsp.usb", "list", Some(&data));
        assert_eq!(
            assessment(&c.report(Some(&at(3))), "usb.status.read").status,
            Support::Unknown
        );
    }
}

#[test]
fn late_result_cannot_evict_a_newer_proof_from_the_current_scope() {
    let c = Collector::new();
    identity(&c, "CN_ZTE_MU5250V1.0.0B31");
    epochs(&c);
    let old = c.token();
    identity(&c, "CN_ZTE_MU5250V1.0.0B27");
    listing(&c, c.token(), 3, PRESENT, true);
    listing(&c, old, 4, PRESENT, true);
    let r = c.report(Some(&at(5)));
    assert_eq!(
        assessment(&r, "usb.mode.setter.surface").status,
        Support::Supported
    );
    assert_eq!(r.evidence.len(), 3);
    r.validate().unwrap();
}

#[test]
fn usb_policy_conflicts_and_unsupported_cannot_be_overridden_by_a_human() {
    use crate::device_profile::usb;
    let c = Collector::new();
    identity(&c, "CN_ZTE_MU5250V1.0.0B31");
    epochs(&c);
    listing(&c, c.token(), 2, ABSENT, true);
    let report = c.report(Some(&at(3)));
    let policy = report.usb_write_policy("usb.mode.set").unwrap();
    assert_eq!(policy.assessment.status, Support::Unsupported);
    assert!(usb::check(&policy, true).is_err());
    assert!(usb::check(&policy, false).is_err());
    // Opposing fresh method proofs are a conflict, never a manual fallback.
    listing(&c, c.token(), 3, PRESENT, true);
    let policy = c
        .report(Some(&at(4)))
        .usb_write_policy("usb.mode.set")
        .unwrap();
    assert_eq!(
        policy.assessment.reason,
        AssessmentReason::ConflictingEvidence
    );
    assert!(usb::check(&policy, true).is_err());
    // Expiry permits a new explicit preflight, but cannot authorize automation.
    let policy = c
        .report(Some(&at(100)))
        .usb_write_policy("usb.mode.set")
        .unwrap();
    assert!(usb::check(&policy, false).is_err());
    assert!(usb::check(&policy, true).is_ok());
}

#[test]
fn usb_policy_identity_conflict_and_write_ack_fail_closed() {
    use crate::device_profile::usb;
    let c = Collector::new();
    identity(&c, "CN_ZTE_MU5250V1.0.0B31");
    epochs(&c);
    listing(&c, c.token(), 2, PRESENT, true);
    c.usb_write(c.token(), Some(at(3)), "zwrt_bsp.usb", None);
    let report = c.report(Some(&at(4)));
    assert_eq!(
        report
            .usb_write_policy("usb.mode.set")
            .unwrap()
            .assessment
            .status,
        Support::Unknown
    );
    let mut common = common("CN_ZTE_MU5250V1.0.0B31");
    common["hardware_version"] = json!("MC7523");
    c.ubus(
        c.token(),
        Some(at(5)),
        "zwrt_zte_mdm.api",
        "get_zwrt_common_info",
        Some(&common),
    );
    let policy = c
        .report(Some(&at(6)))
        .usb_write_policy("usb.mode.set")
        .unwrap();
    assert!(usb::check(&policy, true).is_err());
}

#[test]
fn context_events_revoke_inflight_reads_even_after_aba() {
    let c = Collector::new();
    identity(&c, "CN_ZTE_MU5250V1.0.0B31");
    epochs(&c);
    let old = c.token();
    for _ in 0..2 {
        c.invalidate_context(crate::device_profile::context::Loss::Usb);
    }
    assert_ne!(old.generation, c.token().generation);
    assert_eq!(c.token().context.usb_epoch, None);
    c.usb_write(
        old,
        Some(at(3)),
        "zwrt_bsp.usb",
        Some(json!({"mode":"debug"})),
    );
    let r = c.report(Some(&at(4)));
    assert!(
        r.evidence.iter().all(
            |e| e.capability_id != "usb.mode.set" || e.result.as_ref().unwrap().proof.is_none()
        )
    );
    assert_eq!(
        r.usb_write_policy("usb.mode.set")
            .unwrap()
            .assessment
            .status,
        Support::Unknown
    );
}

#[test]
fn radio_read_freshness_failure_and_late_completion() {
    let c = Collector::new();
    c.radio_read(c.token(), Some("ONLINE"), Some(at(10)));
    let token = c.token();
    assert!(c.at_ready(&token, Some(&at(15))));
    assert!(!c.at_ready(&token, Some(&at(16))));
    assert_eq!(
        c.report(Some(&at(16))).assessments[0]
            .scope
            .context
            .radio_mode,
        None
    );
    c.invalidate_context(crate::device_profile::context::Loss::Ubus);
    c.radio_read(token, Some("ONLINE"), Some(at(20)));
    assert_eq!(
        c.token().context.radio_mode,
        None,
        "late read cannot restore ONLINE"
    );
    c.radio_read(c.token(), Some("ONLINE"), Some(at(21)));
    c.radio_read(c.token(), None, Some(at(22)));
    assert!(!c.at_ready(&c.token(), Some(&at(22))));
    assert_eq!(c.token().context.radio_epoch, None);
}

#[test]
fn at_dialect_proof_requires_epochs_and_redacts_payload() {
    let c = Collector::new();
    identity(&c, "CN_ZTE_MU5250V1.0.0B31");
    c.radio_read(c.token(), Some("ONLINE"), Some(at(2)));
    let command = "AT+CGCONTRDP";
    let response = "+CGCONTRDP: 1,5,\"secret.apn\",\"10.1.2.3\"\r\nOK\r\n";
    c.at_read(c.token(), Some(at(3)), command, Some(response));
    let report = c.report(Some(&at(4)));
    let a = report
        .assessments
        .iter()
        .find(|a| a.capability_id == "at.pdp_contexts.read")
        .unwrap();
    assert_eq!(a.status, Support::Unknown);
    assert_eq!(a.reason, AssessmentReason::ContextChanged);
    assert!(!serde_json::to_string(&report).unwrap().contains("secret"));
    assert!(!serde_json::to_string(&report).unwrap().contains("10.1.2.3"));
    // Only synthetic owner data, never installed by production observations.
    {
        let mut g = c.0.lock().unwrap();
        g.context.radio_epoch = Some("synthetic-radio".into());
        g.context.sim_epoch = Some("synthetic-sim".into());
        g.context.connection_epoch = Some("synthetic-connection".into());
    }
    c.at_read(c.token(), Some(at(4)), command, Some(response));
    let report = c.report(Some(&at(5)));
    let a = report
        .assessments
        .iter()
        .find(|a| a.capability_id == "at.pdp_contexts.read")
        .unwrap();
    assert_eq!(a.status, Support::Supported);
    assert_eq!(a.selected_dialect.as_deref(), Some("at.cgcontrdp.v1"));
    c.at_read(c.token(), Some(at(5)), "AT+CGEQOSRDP=1", Some("ERROR\r\n"));
    let report = c.report(Some(&at(5)));
    let a = report
        .assessments
        .iter()
        .find(|a| a.capability_id == "at.eps_qos.read")
        .unwrap();
    assert_eq!(a.status, Support::Unknown);
}

#[test]
fn loop_epochs_count_the_radio_only_after_a_recent_online_read() {
    let c = Collector::new();
    epochs(&c);
    let (e, id) = c.loop_epochs(Some(&at(20)), 30);
    assert_eq!(e.radio.as_deref(), Some("synthetic-radio"));
    assert!(!id.is_empty());
    // too old for the window
    assert_eq!(c.loop_epochs(Some(&at(40)), 30).0.radio, None);
    // other boot
    let other = BootTime {
        boot_id: "other".into(),
        seconds: 2,
    };
    assert_eq!(c.loop_epochs(Some(&other), 30).0.radio, None);
    // not ONLINE
    c.0.lock().unwrap().context.radio_mode = Some("LOW_POWER".into());
    assert_eq!(c.loop_epochs(Some(&at(2)), 30).0.radio, None);
}

#[test]
fn a_fresh_online_read_keeps_the_radio_epoch_the_proposal_bound() {
    // the approval reads the mode again before the engine (api/approve.rs): the same ONLINE
    // must give the same epoch, or every approval would be refused as a radio change
    let c = Collector::new();
    epochs(&c);
    let first = c.loop_epochs(Some(&at(2)), 30).0.radio;
    assert_eq!(first.as_deref(), Some("synthetic-radio"));
    c.radio_read(c.token(), Some("ONLINE"), Some(at(200)));
    assert_eq!(c.loop_epochs(Some(&at(201)), 30).0.radio, first);
    // a mode change between is another epoch
    c.radio_read(c.token(), Some("LOW_POWER"), Some(at(210)));
    c.radio_read(c.token(), Some("ONLINE"), Some(at(220)));
    assert_ne!(c.loop_epochs(Some(&at(221)), 30).0.radio, first);
}
