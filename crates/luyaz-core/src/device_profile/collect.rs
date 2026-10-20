//! Bounded in-memory observations. The only inputs are results of work already requested
//! elsewhere. No tasks, device reads, report importer or command execution lives here.
use std::{collections::BTreeMap, sync::Mutex};

use luyaz_api::{clock::BootTime, firmware::*};
use rand::RngCore;
use serde_json::{Value, json};

use super::{
    identity::{self, IdentityInput},
    listing::{self, ListingInput},
    resolve::{self, ResolveInput},
};

struct IdentityRead {
    data: Value,
    evidence: Evidence,
}

struct Inner {
    device_id: String,
    collector_id: String,
    generation: u64,
    serial: u64,
    identity: BTreeMap<&'static str, IdentityRead>,
    // At most a positive proof, negative proof and latest unsuccessful attempt per capability.
    evidence: BTreeMap<String, Vec<Evidence>>,
    context: Context,
    radio_at: Option<BootTime>,
    // Observed departures from ONLINE (or failed mode reads). Unrelated invalidations do
    // not count, so an in-flight AT command is only stopped by a real radio change.
    radio_changes: u64,
    // The rtnetlink producer holds a loss-free subscription (see `netlink`).
    conn_live: bool,
    // The ubus listener confirmed its subscription with a probe (`listen::Notice::Ready`)
    // and has not had a gap since.
    sim_live: bool,
}

pub(crate) struct Collector(Mutex<Inner>);

fn opaque_id() -> String {
    let mut bytes = [0; 16];
    if rand::rngs::OsRng.try_fill_bytes(&mut bytes).is_err() {
        // Empty IDs fail closed in the resolver; never substitute device personal data.
        return String::new();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Only copy reviewed non-personal fields. Never retain an entire vendor/UCI reply.
fn text(v: &Value, key: &str) -> Option<String> {
    let s = v.get(key)?.as_str()?.trim();
    (!s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)).then(|| s.to_owned())
}

fn select(v: &Value, keys: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for key in keys {
        if let Some(value) = text(v, key) {
            out.insert((*key).into(), value.into());
        }
    }
    Value::Object(out)
}

/// Reviewed external-version grammars only. BD/internal versions remain separate raw
/// namespaces. Never extract just Bxx, guess a region or equate FLY with CN_ZTE.
fn external_version(value: &str, model: Option<&str>) -> Option<(String, String)> {
    let (line, region, rest) = if let Some(rest) = value.strip_prefix("CN_ZTE_") {
        ("CN_ZTE", "CN", rest)
    } else {
        ("FLY", "CN", value.strip_prefix("FLY_CN_")?)
    };
    let model = model?;
    let rest = rest.strip_prefix(model)?.strip_prefix('V')?;
    let (version, build) = rest.split_once('B')?;
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()))
        || build.is_empty()
        || !build.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((line.into(), region.into()))
}

impl Inner {
    fn profile(&self) -> DeviceProfile {
        let data = |key| self.identity.get(key).map(|r| &r.data);
        let value = |key, field| data(key).and_then(|v| text(v, field));
        let mut firmware = FirmwareProfile {
            raw_versions: vec![],
            product_line: None,
            region: None,
            version: None,
            build: None,
            modem_build: None,
            active_slot: None,
            completeness: IdentityStatus::Partial,
        };
        for (source, read) in &self.identity {
            for (field, namespace) in [
                ("wa_inner_version", "zte.internal"),
                ("integrate_version", "zte.external"),
                ("SoftwareVersion", "tr069.software"),
                ("release_description", "openwrt.description"),
                ("release_revision", "openwrt.revision"),
            ] {
                if let Some(value) = text(&read.data, field) {
                    firmware.raw_versions.push(RawVersion {
                        namespace: namespace.into(),
                        source: format!("{source}:{field}"),
                        value,
                        evidence_id: read.evidence.id.clone(),
                        full: false,
                    });
                }
            }
        }
        // Build the model using the exact same reviewed precedence/conflict rules as I1.
        let input = |firmware| IdentityInput {
            device_id: self.device_id.clone(),
            vendor: value("common", "manufacturer"),
            model_name: value("common", "model_name"),
            hardware_version: value("common", "hardware_version"),
            hardware_revision: value("common", "hardware_version"),
            board_name: value("board", "board_name"),
            identity_evidence_ids: self
                .identity
                .values()
                .map(|r| r.evidence.id.clone())
                .collect(),
            firmware,
            provenance: Provenance {
                cohort: Cohort::Unknown,
                previous_firmware: None,
                evidence_ids: vec![],
            },
            components: vec![Component {
                name: "core".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                commit: option_env!("LUYAZ_COMMIT")
                    .filter(|c| !c.is_empty())
                    .map(str::to_owned),
                hash: None,
            }],
        };
        let model = identity::build_profile(input(firmware.clone())).model;
        // Only common.integrate_version is a reviewed external firmware identity source.
        // TR069 and internal labels are preserved but cannot silently fill a missing identity.
        if let Some(version) = value("common", "integrate_version")
            && let Some((line, region)) = external_version(&version, model.as_deref())
        {
            firmware.product_line = Some(line);
            firmware.region = Some(region);
            firmware.version = Some(version.clone());
            for raw in &mut firmware.raw_versions {
                raw.full = raw.namespace == "zte.external" && raw.value == version;
            }
        }
        identity::build_profile(input(firmware))
    }

    fn scope(&self) -> Scope {
        let profile = self.profile();
        Scope {
            device_id: self.device_id.clone(),
            fingerprint: profile.fingerprint,
            collector_id: self.collector_id.clone(),
            generation: self.generation,
            rule_revision: resolve::RULE_REVISION,
            adapter_revision: profile.adapter_revision,
            component_revision: format!(
                "core:{}:{}:passive-v1",
                env!("CARGO_PKG_VERSION"),
                option_env!("LUYAZ_COMMIT").unwrap_or("unmarked")
            ),
            cohort: Cohort::Unknown,
            context: self.context.clone(),
        }
    }

    fn evidence(
        &mut self,
        scope: Scope,
        at: Option<BootTime>,
        capability: &str,
        operation: Operation,
        result: ProbeResult,
        source: &str,
    ) -> Evidence {
        self.serial += 1;
        Evidence {
            id: format!("{}:{}", self.collector_id, self.serial),
            capability_id: capability.into(),
            facet: if capability.ends_with(".surface") {
                Facet::Surface
            } else {
                Facet::Read
            },
            origin: EvidenceOrigin::Live,
            source_kind: SourceKind::DeviceCapture,
            verification: Verification::Captured,
            scope: Some(scope),
            observed_on: None,
            recorded_on: String::new(),
            time: EvidenceTime {
                observed_at: at,
                utc: None,
                utc_quality: UtcQuality::Unknown,
            },
            operation,
            result: Some(result),
            source_ref: source.into(),
            locator: None,
            artifact: Artifact {
                sha256: None,
                parser_revision: Some("passive-v1".into()),
                redacted: true,
            },
            summary: "passive observation of an existing read".into(),
            limitations: vec![
                "No installation provenance or unobserved context epochs inferred".into(),
            ],
        }
    }

    /// After a revocation, a still-live subscription gives a *new* opaque epoch: nothing
    /// bound to the old one matches again, and no event can have been missed meanwhile.
    fn remint_connection(&mut self) {
        if self.conn_live && self.context.connection_epoch.is_none() {
            self.context.connection_epoch = Some(opaque_id()).filter(|id| !id.is_empty());
        }
    }

    fn remint_sim(&mut self) {
        if self.sim_live && self.context.sim_epoch.is_none() {
            self.context.sim_epoch = Some(opaque_id()).filter(|id| !id.is_empty());
        }
    }

    /// Radio epoch (R18-P1-01, firmware-capabilities §12.8): derived, there is no mode-change
    /// event. It exists only while both the rtnetlink and the ubus subscriptions are confirmed,
    /// and every connection, card or power-state event and every observed mode change gives a
    /// new value: a modem reset or LPM drops the data call and resets the card.
    fn bump_radio(&mut self) {
        self.context.radio_epoch = None;
        if self.conn_live && self.sim_live {
            self.context.radio_epoch = Some(opaque_id()).filter(|id| !id.is_empty());
        }
    }

    /// After a mode change cleared all three: new values for whatever is still live.
    fn remint(&mut self) {
        self.remint_connection();
        self.remint_sim();
        self.bump_radio();
    }

    fn note_radio(&mut self, mode: Option<&str>) {
        if mode.map(str::trim) != Some("ONLINE") {
            self.radio_changes = self.radio_changes.saturating_add(1);
        }
    }

    fn retain(&mut self, mut e: Evidence) {
        if e.scope.as_ref() != Some(&self.scope()) {
            // Preserve the original scope for explanation, but a late observation cannot
            // replace a newer proof collected in the current identity/context.
            if let Some(result) = &mut e.result {
                result.proof = None;
            }
            e.limitations
                .push("scope changed while the existing read was in flight".into());
        }
        let entries = self.evidence.entry(e.capability_id.clone()).or_default();
        let proof = e.result.as_ref().and_then(|r| r.proof);
        // Keep opposing proofs so that a fresh contradiction remains unknown. A new
        // observation only replaces the previous observation of its own proof category.
        entries.retain(|old| old.result.as_ref().and_then(|r| r.proof) != proof);
        entries.push(e);
    }
}

#[cfg(test)]
#[path = "collect_tests.rs"]
mod tests;

fn result(value: Option<Value>, proof: Option<Proof>) -> ProbeResult {
    ProbeResult {
        outcome: if value.is_some() {
            ProbeOutcome::Success
        } else {
            ProbeOutcome::Unavailable
        },
        exit_code: None,
        vendor_code: None,
        value,
        proof,
        complete: true,
        truncated: false,
        duration_ms: 0,
    }
}

fn operation(probe: &str, target: &str) -> Operation {
    Operation {
        probe_id: probe.into(),
        target: target.into(),
        verifier_id: None,
        args: None,
    }
}

impl Collector {
    pub(crate) fn new() -> Self {
        // Warm the boot identity once at startup; report reads use only the cached ID and
        // clock_gettime. IDs are deliberately ephemeral until a persistent identity owner exists.
        let _ = BootTime::read();
        Self(Mutex::new(Inner {
            device_id: opaque_id(),
            collector_id: opaque_id(),
            generation: 0,
            serial: 0,
            identity: BTreeMap::new(),
            evidence: BTreeMap::new(),
            context: Context::default(),
            radio_at: None,
            radio_changes: 0,
            conn_live: false,
            sim_live: false,
        }))
    }

    pub(crate) fn token(&self) -> Scope {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).scope()
    }

    /// The decision loop's epochs and device id, from one `scope()` (decision-execution-loop
    /// §4.9). O5 as the owner closed it: the radio epoch counts only while the last real mode
    /// read said ONLINE within `seconds`; otherwise it is None (unknown, never executable).
    /// Memory only: safe under the engine lock.
    pub(crate) fn loop_epochs(
        &self,
        now: Option<&BootTime>,
        seconds: u64,
    ) -> (luyaz_api::execution::Epochs, String) {
        let g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let scope = g.scope();
        let mut e = luyaz_api::execution::Epochs::of(&scope.context);
        if g.context.radio_mode.as_deref() != Some("ONLINE")
            || !super::context::within(g.radio_at.as_ref(), now, seconds)
        {
            e.radio = None;
        }
        (e, scope.device_id)
    }

    /// Called only after an actual transport attempt, never on a TTL cache hit or Skipped.
    /// The scope is captured before awaiting the read; completion cannot rebind an old result.
    pub(crate) fn ubus(
        &self,
        token: Scope,
        at: Option<BootTime>,
        object: &str,
        method: &str,
        value: Option<&Value>,
    ) {
        let identity_source = match (object, method) {
            ("zwrt_zte_mdm.api", "get_zwrt_common_info") => Some("common"),
            ("system", "board") => Some("board"),
            _ => None,
        };
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(source) = identity_source {
            let v = value.cloned().unwrap_or(Value::Null);
            let data = if source == "common" {
                select(
                    &v,
                    &[
                        "manufacturer",
                        "model_name",
                        "hardware_version",
                        "wa_inner_version",
                        "integrate_version",
                    ],
                )
            } else {
                json!({"board_name": text(&v, "board_name"),
                    "release_description": text(&v["release"], "description"),
                    "release_revision": text(&v["release"], "revision")})
            };
            // Do not accept an identity result that started before another identity update.
            if token.generation != g.generation
                || token.collector_id != g.collector_id
                || token.device_id != g.device_id
            {
                return;
            }
            let changed = g.identity.get(source).is_none_or(|old| old.data != data);
            let e = g.evidence(
                token,
                at,
                "device.identity",
                operation("identity.passive.v1", source),
                result(value.map(|_| data.clone()), None),
                source,
            );
            g.identity
                .insert(source, IdentityRead { data, evidence: e });
            if changed {
                g.generation = g.generation.saturating_add(1);
            }
        } else if (object, method) == ("zwrt_bsp.usb", "list") {
            let mode = value
                .and_then(|v| text(v, "mode"))
                .filter(|s| matches!(s.as_str(), "user" | "debug"));
            let validated = mode.map(|mode| json!({"mode": mode}));
            let proof = validated.as_ref().map(|_| Proof::ReadValidated);
            let e = g.evidence(
                token,
                at,
                "usb.status.read",
                operation("usb.cached_read.v1", "zwrt_bsp.usb:list"),
                result(validated, proof),
                "zwrt_bsp.usb:list",
            );
            g.retain(e);
        }
    }

    /// Existing UCI collection only. Raw versions are namespace-preserving; no personal data.
    pub(crate) fn uci(
        &self,
        token: Scope,
        package: &str,
        values: &BTreeMap<String, String>,
        at: Option<BootTime>,
    ) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if token.generation != g.generation
            || token.collector_id != g.collector_id
            || token.device_id != g.device_id
        {
            return;
        }
        let (source, fields): (&str, &[(&str, &str)]) = match package {
            "zwrt_common_info" => (
                "uci.common",
                &[
                    ("common_config.wa_inner_version", "wa_inner_version"),
                    ("common_config.integrate_version", "integrate_version"),
                ],
            ),
            "zwrt_tr069" => (
                "uci.tr069",
                &[("DeviceInfo.SoftwareVersion", "SoftwareVersion")],
            ),
            // UCI radio mode is an observed condition, not a SIM/connection epoch.
            "zte_nwinfo" => {
                let mode = values
                    .get("zte_nwinfo.sys_info.operate_mode")
                    .map(String::as_str);
                g.note_radio(mode);
                if super::context::radio(&mut g.context, mode) {
                    g.remint();
                    g.generation = g.generation.saturating_add(1);
                }
                g.radio_at = at;
                return;
            }
            _ => return,
        };
        let data: Value = fields
            .iter()
            .filter_map(|(path, field)| {
                let value = values.get(&format!("{package}.{path}"))?;
                let v = json!({*field: value});
                text(&v, field).map(|s| ((*field).into(), Value::String(s)))
            })
            .collect::<serde_json::Map<_, _>>()
            .into();
        let changed = g.identity.get(source).is_none_or(|old| old.data != data);
        let e = g.evidence(
            token,
            at,
            "device.identity",
            operation("identity.passive.v1", source),
            result(Some(data.clone()), None),
            source,
        );
        g.identity
            .insert(source, IdentityRead { data, evidence: e });
        if changed {
            g.generation = g.generation.saturating_add(1);
        }
    }

    /// Every observed event is an invalidation even when payload/state repeats (ABA).
    /// Called before event debounce or queueing and on listener/transport gaps.
    pub(crate) fn invalidate_context(&self, loss: super::context::Loss) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        super::context::invalidate(&mut g.context, loss);
        if matches!(loss, super::context::Loss::Ubus) {
            g.radio_at = None;
            // A listener gap can hide a SIM event; only the next probe re-arms it.
            g.sim_live = false;
            g.remint_connection();
            g.bump_radio();
        }
        g.generation = g.generation.saturating_add(1);
    }

    /// rtnetlink producer: `true` once a link dump completed on a subscribed socket,
    /// `false` on overrun/read error/close. Either way the old epoch is gone.
    pub(crate) fn connection_live(&self, live: bool) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.conn_live = live;
        crate::api::band_flap::live(live);
        g.context.connection_epoch = None;
        g.remint_connection();
        g.bump_radio();
        g.generation = g.generation.saturating_add(1);
    }

    /// ubus listener confirmed its subscription (probe seen) after a gap.
    pub(crate) fn sim_listening(&self) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.sim_live = true;
        crate::api::band_flap::sim_changed();
        g.context.sim_epoch = None;
        g.remint_sim();
        g.bump_radio();
        g.generation = g.generation.saturating_add(1);
    }

    /// Any `zwrt_card_state_event` (hot remove, ready, repeats): a new SIM epoch.
    pub(crate) fn sim_event(&self) {
        crate::api::band_flap::sim_changed();
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.context.sim_epoch = None;
        g.remint_sim();
        g.bump_radio();
        g.generation = g.generation.saturating_add(1);
    }

    /// Any rmnet link/address/route event, even a repeat (same-IP redial is ABA).
    pub(crate) fn connection_event(&self) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.context.connection_epoch = None;
        g.remint_connection();
        g.bump_radio();
        g.generation = g.generation.saturating_add(1);
    }

    /// `zwrt_device_power_state_event` (sleep/wake): the radio may have changed state.
    pub(crate) fn radio_event(&self) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.bump_radio();
        g.generation = g.generation.saturating_add(1);
    }

    /// Only an actual UCI result, not a cache hit. Failure clears ONLINE immediately.
    pub(crate) fn radio_read(&self, token: Scope, value: Option<&str>, at: Option<BootTime>) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // A non-ONLINE read stops in-flight AT even when the scope moved meanwhile.
        g.note_radio(value);
        if token != g.scope() {
            return;
        }
        if super::context::radio(&mut g.context, value) {
            g.remint();
            g.generation = g.generation.saturating_add(1);
        }
        g.radio_at = at;
    }

    pub(crate) fn at_ready(&self, token: &Scope, now: Option<&BootTime>) -> bool {
        let g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        *token == g.scope()
            && g.context.radio_mode.as_deref() == Some("ONLINE")
            && super::context::fresh(g.radio_at.as_ref(), now)
    }

    /// Marker for an admitted AT attempt; see [`Inner::note_radio`].
    pub(crate) fn radio_mark(&self) -> u64 {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .radio_changes
    }

    pub(crate) fn radio_unchanged(&self, mark: u64) -> bool {
        self.radio_mark() == mark
    }

    pub(crate) fn at_reply_current(&self, token: &Scope, now: Option<&BootTime>) -> bool {
        let g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        *token == g.scope()
            && g.context.radio_mode.as_deref() == Some("ONLINE")
            && super::context::within(g.radio_at.as_ref(), now, MAX_AGE_SECONDS)
    }

    /// Reuse an explicitly requested AT response. No raw APN, address, SIM data or port.
    pub(crate) fn at_read(
        &self,
        token: Scope,
        at: Option<BootTime>,
        command: &str,
        reply: Option<&str>,
    ) {
        let Some(binding) = super::at::binding(command) else {
            return;
        };
        let value = reply.and_then(|r| super::at::validate(command, r));
        let proof = value.as_ref().map(|_| Proof::ReadValidated);
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let e = g.evidence(
            token,
            at,
            binding.capability,
            operation(binding.dialect, binding.target),
            result(value, proof),
            "at:explicit-read",
        );
        g.retain(e);
    }

    /// Reuse the already requested complete listing, including original failure metadata.
    pub(crate) fn usb_methods(&self, token: Scope, at: Option<BootTime>, input: ListingInput<'_>) {
        self.methods(token, at, "zwrt_bsp.usb", input);
    }

    /// Fixed, reviewed USB objects only; the caller reuses a completed listing.
    pub(crate) fn methods(
        &self,
        token: Scope,
        at: Option<BootTime>,
        object: &str,
        input: ListingInput<'_>,
    ) {
        let (capability, probe) = match object {
            "zwrt_bsp.usb" => ("usb.mode.setter.surface", "ubus.usb_methods.v1"),
            "zwrt_bsp.typec" => ("usb.typec.setter.surface", "ubus.typec_methods.v1"),
            "zwrt_bsp.powerbank" => ("usb.powerbank.setter.surface", "ubus.powerbank_methods.v1"),
            _ => return,
        };
        let result = listing::method_result(input, object, "set");
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let e = g.evidence(
            token,
            at,
            capability,
            operation(probe, &format!("{object}:set")),
            result,
            &format!("ubus:-v:list:{object}"),
        );
        g.retain(e);
    }

    pub(crate) fn qos(&self, token: Scope, parsed: &crate::qos::Parsed, query: crate::qos::Query) {
        let Some((capability, value)) = crate::qos::validated_read(parsed, query) else {
            return;
        };
        let at = parsed.observed_at.clone();
        let target = match capability {
            "qos.eps.log.read" => "qos:eps",
            "qos.nr_pdu.log.read" => "qos:nr_pdu",
            _ => return,
        };
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let value = Some(value);
        let proof = value.as_ref().map(|_| Proof::ReadValidated);
        let e = g.evidence(
            token,
            at,
            capability,
            operation("qos.cached.v1", target),
            result(value, proof),
            "qos:existing-log-selection",
        );
        g.retain(e);
    }

    /// A real, authorized write followed by direct matching readback. An ACK or failed
    /// readback remains an attempt; absent context epochs still prevent live support.
    pub(crate) fn usb_write(
        &self,
        token: Scope,
        at: Option<BootTime>,
        object: &str,
        value: Option<Value>,
    ) {
        let (id, probe, verifier) = match object {
            "zwrt_bsp.usb" => ("usb.mode.set", "usb.mode.set.v1", "usb.mode.readback.v1"),
            "zwrt_bsp.typec" => (
                "usb.attach.share",
                "usb.attach.share.v1",
                "usb.share.readback.v1",
            ),
            "zwrt_bsp.powerbank" => (
                "usb.attach.fast_charge",
                "usb.attach.fast_charge.v1",
                "usb.fast_charge.readback.v1",
            ),
            _ => return,
        };
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let verified = value.is_some();
        let mut op = operation(probe, &format!("{object}:set"));
        op.verifier_id = Some(verifier.into());
        let mut e = g.evidence(
            token,
            at,
            id,
            op,
            result(value, verified.then_some(Proof::WriteVerified)),
            "usb:authorized-write-readback",
        );
        e.facet = Facet::Write;
        e.summary = "authorized USB write with bounded direct effect verification".into();
        if !verified {
            e.limitations.push("No changed, matching effect was verified; ACK/no-op/error alone cannot prove write support".into());
        }
        if verified {
            e.verification = Verification::WriteVerified;
        }
        g.retain(e);
    }

    /// Snapshot only: caller supplies BOOTTIME. No refresh, disk reads, commands or HTTP calls.
    pub(crate) fn report(&self, now: Option<&BootTime>) -> CapabilityReport {
        let g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let profile = g.profile();
        let mut scope = g.scope();
        if !super::context::fresh(g.radio_at.as_ref(), now) {
            scope.context.radio_mode = None;
            scope.context.radio_epoch = None;
        }
        let evidence: Vec<_> = g
            .identity
            .values()
            .map(|r| r.evidence.clone())
            .chain(g.evidence.values().flatten().cloned())
            .collect();
        let input = ResolveInput {
            profile: &profile,
            scope: &scope,
            now,
            evidence: &evidence,
            availability: Availability {
                state: AvailabilityState::Unknown,
                reason: "passive_report_only".into(),
            },
        };
        CapabilityReport {
            context_coverage: super::context::coverage(),
            schema_version: SCHEMA_VERSION,
            assessments: resolve::capability_ids()
                .map(|id| resolve::resolve(&input, id))
                .collect(),
            profile,
            evidence,
        }
    }
}
