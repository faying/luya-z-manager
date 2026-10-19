use std::collections::BTreeMap;

use luyaz_api::firmware::*;
use sha2::{Digest, Sha256};

pub(crate) struct IdentityInput {
    pub device_id: String,
    pub vendor: Option<String>,
    pub model_name: Option<String>,
    pub hardware_version: Option<String>,
    pub hardware_revision: Option<String>,
    pub board_name: Option<String>,
    pub identity_evidence_ids: Vec<String>,
    pub firmware: FirmwareProfile,
    pub provenance: Provenance,
    pub components: Vec<Component>,
}

fn clean(value: Option<String>) -> Option<String> {
    value.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

fn known_model(value: &str) -> Option<String> {
    let normalized = normalize_profile(value);
    matches!(
        normalized.as_str(),
        "mu5250" | "mu5252" | "mc7523" | "mc8532b"
    )
    .then(|| normalized.to_ascii_uppercase())
}

fn firmware_status(firmware: &FirmwareProfile) -> IdentityStatus {
    let mut versions = BTreeMap::new();
    for raw in &firmware.raw_versions {
        if raw.namespace.trim().is_empty() || raw.value.trim().is_empty() {
            continue;
        }
        if versions
            .insert(raw.namespace.trim(), raw.value.trim())
            .is_some_and(|before| before != raw.value.trim())
        {
            return IdentityStatus::Conflict;
        }
    }
    if firmware.completeness == IdentityStatus::Conflict {
        return IdentityStatus::Conflict;
    }
    let full_version = firmware.version.as_deref().is_some_and(|version| {
        firmware.raw_versions.iter().any(|raw| {
            raw.full
                && raw.value.trim() == version
                && !raw.namespace.trim().is_empty()
                && !raw.source.trim().is_empty()
                && !raw.evidence_id.trim().is_empty()
        })
    });
    if full_version
        && firmware
            .product_line
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
        && firmware
            .region
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
    {
        IdentityStatus::Complete
    } else {
        IdentityStatus::Partial
    }
}

/// Reuses the existing normalization without changing the production snapshot projection.
pub(crate) fn build_profile(input: IdentityInput) -> DeviceProfile {
    let model_name = clean(input.model_name);
    let hardware = clean(input.hardware_version);
    let named = model_name.as_deref().and_then(known_model);
    let fallback = hardware.as_deref().and_then(known_model);
    let conflict = named.is_some() && fallback.is_some() && named != fallback;
    // As in snapshot: a present but unrecognized model_name is not silently replaced.
    let model = if model_name.is_some() {
        named
    } else {
        fallback
    };
    let adapter = (!conflict)
        .then(|| model.as_deref().and_then(model_adapter))
        .flatten();
    let mut firmware = input.firmware;
    for value in [
        &mut firmware.product_line,
        &mut firmware.region,
        &mut firmware.version,
        &mut firmware.build,
        &mut firmware.modem_build,
        &mut firmware.active_slot,
    ] {
        *value = clean(value.take());
    }
    firmware.completeness = firmware_status(&firmware);
    let mut profile = DeviceProfile {
        device_id: input.device_id,
        vendor: clean(input.vendor),
        model,
        hardware_revision: clean(input.hardware_revision),
        board_name: clean(input.board_name),
        identity_evidence_ids: input.identity_evidence_ids,
        firmware,
        provenance: input.provenance,
        components: input.components,
        adapter_id: adapter.as_ref().map(|a| a.id.clone()),
        adapter_revision: adapter.as_ref().map_or(0, |a| a.revision),
        fingerprint: None,
        identity_status: IdentityStatus::Partial,
    };
    if conflict || profile.firmware.completeness == IdentityStatus::Conflict {
        profile.identity_status = IdentityStatus::Conflict;
    } else if !profile.device_id.trim().is_empty()
        && profile.vendor.is_some()
        && profile.model.is_some()
        && profile.firmware.completeness == IdentityStatus::Complete
    {
        profile.identity_status = IdentityStatus::Complete;
        profile.fingerprint = fingerprint(&profile);
    }
    profile
}

/// SHA-256 of a JSON object with lexically sorted keys and explicit nulls, not version suffixes.
pub(crate) fn fingerprint(profile: &DeviceProfile) -> Option<String> {
    if profile.identity_status != IdentityStatus::Complete
        || profile.firmware.completeness != IdentityStatus::Complete
        || firmware_status(&profile.firmware) != IdentityStatus::Complete
        || profile.model.as_deref().and_then(known_model).as_deref() != profile.model.as_deref()
        || profile.model.is_none()
    {
        return None;
    }
    let f = &profile.firmware;
    let fields = BTreeMap::from([
        ("vendor", profile.vendor.as_deref()),
        ("model", profile.model.as_deref()),
        ("hardware_revision", profile.hardware_revision.as_deref()),
        ("board_name", profile.board_name.as_deref()),
        ("product_line", f.product_line.as_deref()),
        ("region", f.region.as_deref()),
        ("version", f.version.as_deref()),
        ("build", f.build.as_deref()),
        ("modem_build", f.modem_build.as_deref()),
        ("active_slot", f.active_slot.as_deref()),
    ]);
    for key in ["vendor", "model", "product_line", "region", "version"] {
        if fields[key].is_none_or(|v| v.trim().is_empty()) {
            return None;
        }
    }
    let encoded = serde_json::to_vec(&fields).ok()?;
    Some(format!("{:x}", Sha256::digest(encoded)))
}

/// Existing model source policies only; no firmware-specific dialect is inferred from Bxx.
pub(crate) fn model_adapter(model: &str) -> Option<ModelAdapter> {
    use CpuTemperatureSource::*;
    use NetworkSource::*;
    use WifiSectionSource::*;
    let (network_source, cpu_temperature, wifi_section) = match model {
        "MU5250" => (UbusOnly, CpussOnly, Main2g),
        "MU5252" => (UbusWithUciFallback, CpussOnly, FirstEnabledThenPresent),
        "MC7523" => (UbusOnly, NamedFieldsThenSysfs, FirstEnabledThenPresent),
        "MC8532B" => (
            UbusWithUciFallback,
            NamedFieldsThenSysfs,
            FirstEnabledThenPresent,
        ),
        _ => return None,
    };
    Some(ModelAdapter {
        id: format!("model.{}.v1", model.to_ascii_lowercase()),
        revision: 1,
        model: model.into(),
        firmware_fingerprint: None,
        network_source,
        cpu_temperature,
        wifi_section,
        command_dialects: BTreeMap::new(),
    })
}

pub(crate) fn normalize_profile(v: &str) -> String {
    let mut out = String::new();
    let mut sep = true;
    for c in v.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            sep = false
        } else if !sep && matches!(c, '-' | '_' | ' ' | '/' | '.') {
            out.push('_');
            sep = true
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "unknown".into()
    } else {
        out
    }
}
