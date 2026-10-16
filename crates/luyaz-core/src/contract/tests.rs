use super::*;
use crate::evlog::EvLog;
use crate::sens::To;
use std::collections::BTreeSet;

#[test]
fn every_published_block_has_a_contract() {
    let hub: BTreeSet<&str> = crate::block::phase1_blocks()
        .iter()
        .map(|b| b.name)
        .collect();
    let have: BTreeSet<&str> = BLOCKS.iter().copied().collect();
    assert_eq!(
        hub, have,
        "a block without a contract is never logged in plain text"
    );
}

#[test]
fn samples_are_exact() {
    for (name, v) in samples() {
        assert_eq!(shape_error(name, &v), None, "{name}");
    }
}

/// Every string leaf of the samples says its level; scan what each audience gets.
#[test]
fn secrets_never_reach_the_log_or_a_view() {
    let key = b"k".to_vec();
    for (name, v) in samples() {
        let json = |to: To| {
            let c = redact(name, &v, &Ctx { to, key: &key }).unwrap();
            assert!(c.exact, "{name}");
            serde_json::to_string(&c.value).unwrap()
        };
        let log = json(To::Log);
        assert!(!log.contains("S:secret:"), "{name} log: {log}");
        let owner = json(To::Owner);
        assert!(!owner.contains("S:secret:"), "{name} owner: {owner}");
        let mcp = json(To::Mcp);
        assert!(!mcp.contains("S:secret:"), "{name} mcp: {mcp}");
        // personal data: in full on the device, for the owner and for authenticated MCP
        if v.to_string().contains("S:personal:") {
            assert!(log.contains("S:personal:"), "{name}");
            assert!(owner.contains("S:personal:"), "{name}");
            assert!(mcp.contains("S:personal:"), "{name}");
        }
    }
}

/// The same scan over the bytes the event log really holds.
#[test]
fn the_event_log_bytes_hold_no_secret() {
    let mut log = EvLog::memory("b-scan");
    let samples = samples();
    for (i, (name, v)) in samples.iter().enumerate() {
        let ev = crate::block::BlockEvent {
            seq: i as u64,
            name,
            revision: 1,
            observed_at: 1,
            stale: false,
            data: v.clone(),
        };
        log.on_block(&ev, &crate::evlog::Tm::now("b-scan"));
    }
    let (groups, _) = log.read_all().unwrap();
    let bytes: Vec<u8> = groups
        .iter()
        .flatten()
        .flat_map(|r| r.payload.clone())
        .collect();
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    assert!(!has(b"S:secret:"));
    assert!(has(b"S:personal:"), "the scan sees the payloads");
    for (name, _) in &samples {
        let s = &log.stats.blocks[*name];
        assert_eq!((s.untyped, s.inexact), (0, 0), "{name}");
    }
    // the SMS text is there as a reference: a change shows, the text does not
    let text = &log.state()["sms_list"].data["list"][0]["text"];
    assert!(text.as_str().unwrap().starts_with("ref:"), "{text}");
}

#[test]
fn a_block_that_misses_its_contract_is_not_logged_plainly() {
    let mut log = EvLog::memory("b-x");
    let ev = crate::block::BlockEvent {
        seq: 1,
        name: "sms_list",
        revision: 1,
        observed_at: 1,
        stale: false,
        data: serde_json::json!({"list": [{"text": "code 991122", "num": 5}]}),
    };
    log.on_block(&ev, &crate::evlog::Tm::now("b-x"));
    assert_eq!(log.state()["sms_list"].data, crate::evlog::untyped());
    assert_eq!(log.stats.blocks["sms_list"].untyped, 1);
    let (groups, _) = log.read_all().unwrap();
    let bytes: Vec<u8> = groups
        .iter()
        .flatten()
        .flat_map(|r| r.payload.clone())
        .collect();
    assert!(!bytes.windows(6).any(|w| w == b"991122"));
    // a field the contract does not name is dropped, counted
    let ev = crate::block::BlockEvent {
        data: serde_json::json!({"count": 1, "max_id": 2, "unread": 0, "new_field": "x"}),
        name: "sms",
        ..ev
    };
    log.on_block(&ev, &crate::evlog::Tm::now("b-x"));
    assert_eq!(log.stats.blocks["sms"].inexact, 1);
    assert_eq!(
        log.state()["sms"].data,
        serde_json::json!({"count": 1, "max_id": 2, "unread": 0})
    );
}

/// A real `/v2/state` against the contracts (LUYAZ_V2STATE=path, kept out of the repo).
/// `cargo test device_state -- --ignored --nocapture`
#[test]
#[ignore]
fn device_state_fits() {
    let p = std::env::var("LUYAZ_V2STATE").expect("LUYAZ_V2STATE");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    let mut bad = Vec::new();
    for (name, b) in v["blocks"].as_object().unwrap() {
        if !BLOCKS.contains(&name.as_str()) {
            println!("{name}: no contract yet");
            continue;
        }
        if let Some(e) = shape_error(name, &b["data"]) {
            // field names only: the values are the owner's
            bad.push(format!(
                "{name}: {}",
                e.chars().take(120).collect::<String>()
            ));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

/// A charger mode other than enable/disable comes out as nulls (as datad writes it) and the
/// contract takes it; before the projection built the type, the contract said bool/String
/// and would have refused the block.
#[test]
fn unknown_direct_supply_mode_passes() {
    let v = crate::project::snapshot::power_object(&serde_json::json!({
        "direct_power_supply_mode": "auto"
    }))
    .unwrap();
    assert_eq!(
        v,
        serde_json::json!({"direct_supply": {"supported": true, "enabled": null, "mode": null}})
    );
    let c = redact(
        "charger",
        &v,
        &Ctx {
            to: To::Log,
            key: b"k",
        },
    )
    .unwrap();
    assert!(c.exact);
}

/// No SIM or an unknown country: the roaming fields are null (as datad writes them) and
/// the contract takes them; it used to say i64/bool and would have refused the block.
#[test]
fn unknown_roaming_countries_pass() {
    assert_eq!(
        crate::project::snapshot::roaming_fields(0, ""),
        (None, None, None)
    );
    let mut v = samples()
        .into_iter()
        .find(|(n, _)| *n == "signal")
        .unwrap()
        .1;
    for k in ["serving_mcc", "home_mcc", "true_roaming"] {
        v[k] = serde_json::Value::Null;
    }
    let c = redact(
        "signal",
        &v,
        &Ctx {
            to: To::Log,
            key: b"k",
        },
    )
    .unwrap();
    assert!(c.exact);
}

/// The contracts as JSON Schema for the web pages and MCP (`contract-schema.json` in this
/// crate; `LUYAZ_UPDATE_GOLDEN=1` rewrites it). Every field says its level in
/// `x-u60-level`, so a consumer can mask without its own list.
#[test]
fn contract_schema_file_is_current() {
    let mut blocks = serde_json::Map::new();
    for (name, schema) in schemas() {
        blocks.insert(name.to_string(), serde_json::to_value(&schema).unwrap());
    }
    let doc = serde_json::json!({
        "$comment": "Generated from crates/luyaz-core/src/contract.rs by the test contract_schema_file_is_current (LUYAZ_UPDATE_GOLDEN=1). Levels: normal, personal (identifiers; owner and authenticated MCP see them), secret (never in plain text outside the live state), nested.",
        "blocks": blocks,
    });
    let text = serde_json::to_string_pretty(&doc).unwrap() + "\n";
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("contract-schema.json");
    if std::env::var_os("LUYAZ_UPDATE_GOLDEN").is_some() {
        std::fs::write(&p, &text).unwrap();
    }
    let on_disk = std::fs::read_to_string(&p).unwrap_or_default();
    assert!(
        on_disk == text,
        "contract-schema.json is out of date: LUYAZ_UPDATE_GOLDEN=1 cargo test -p luyaz-core contract_schema"
    );
    // every property of every object says its level
    fn walk(v: &serde_json::Value, missing: &mut Vec<String>, levels: &mut BTreeSet<String>) {
        if let Some(props) = v.get("properties").and_then(|p| p.as_object()) {
            for (k, f) in props {
                match f.get("x-u60-level").and_then(|l| l.as_str()) {
                    Some(l) => {
                        levels.insert(format!("{k}={l}"));
                    }
                    None => missing.push(k.clone()),
                }
            }
        }
        match v {
            serde_json::Value::Object(m) => m.values().for_each(|x| walk(x, missing, levels)),
            serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, missing, levels)),
            _ => {}
        }
    }
    let (mut missing, mut levels) = (Vec::new(), BTreeSet::new());
    walk(&doc, &mut missing, &mut levels);
    assert!(missing.is_empty(), "no level: {missing:?}");
    assert!(levels.contains("text=secret"));
    assert!(levels.contains("imsi=personal"));
    assert!(levels.contains("list=nested"));
}
