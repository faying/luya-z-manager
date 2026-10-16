use super::*;
use crate::envelope::{Mem, decode, encode};
use serde_json::json;

const BOOT: &str = "b-1";

fn tm(mono_ms: u64) -> Tm {
    Tm {
        boot: BOOT.into(),
        mono_ms,
        wall: 1_800_000_000 + (mono_ms / 1000) as i64,
        wall_ok: true,
    }
}

fn ev(name: &'static str, revision: u64, stale: bool, data: Value) -> BlockEvent {
    BlockEvent {
        seq: revision,
        name,
        revision,
        observed_at: 1_800_000_000 + revision,
        stale,
        data,
    }
}

/// The log mechanics without contracts: every value passes as is.
pub(crate) fn open_door(_: &str, v: &Value, _: &Ctx) -> Result<Checked, Refused> {
    Ok(Checked {
        value: v.clone(),
        exact: true,
    })
}

fn open_log(m: Mem, seg_max: u64, keep: u64) -> (EvLog, Replay) {
    EvLog::open(
        Box::new(m),
        seg_max,
        keep,
        BOOT,
        b"test-key".to_vec(),
        open_door,
    )
    .unwrap()
}

fn mem_log(seg_max: u64, keep: u64) -> EvLog {
    open_log(Mem::default(), seg_max, keep).0
}

/// Copy of what the log stored (to reopen it, or to damage it).
fn media_of(log: &EvLog) -> Mem {
    let mut m = Mem::default();
    for f in log.store.segments().unwrap() {
        m.0.insert(f, log.store.segment_bytes(f).unwrap());
    }
    m
}

fn replay_of(log: &EvLog) -> Replay {
    let (g, d) = log.read_all().unwrap();
    replay(&g, d)
}

#[test]
fn value_roundtrip() {
    let v = json!({
        "a": 1, "neg": -5, "big": u64::MAX, "f": 18.5, "whole_f": 18.0,
        "s": "18.0", "n": null, "t": true, "e": {}, "l": [], "nest": [{"a": 2, "b": [1, "x"]}]
    });
    let mut names: Vec<String> = Vec::new();
    let c = pack_value(&v, &mut names);
    let back = unpack_value(&c, &names).unwrap();
    assert_eq!(back, v);
    assert_eq!(
        serde_json::to_string(&back).unwrap(),
        serde_json::to_string(&v).unwrap()
    );
    // the key "a" is stored once
    assert_eq!(names.iter().filter(|n| *n == "a").count(), 1);
    assert!(unpack_value(&c, &names[..1]).is_none());
}

#[test]
fn replay_gives_back_the_state() {
    let mut log = mem_log(MEM_SEG_MAX, MEM_KEEP);
    log.on_block(&ev("sim", 1, false, json!({"state": "ready"})), &tm(1000));
    log.on_block(
        &ev("signal", 1, false, json!({"bars": 3, "nr_snr": "18.0"})),
        &tm(2000),
    );
    log.on_block(
        &ev("signal", 2, false, json!({"bars": 4, "nr_snr": "19.0"})),
        &tm(3000),
    );
    log.on_block(&ev("wlan", 1, true, Value::Null), &tm(4000));
    let r = replay_of(&log);
    assert_eq!(&r.state, log.state());
    assert_eq!(r.state["signal"].revision, 2);
    assert_eq!(
        (r.unknown, r.newer, r.undecodable, r.dropped_bytes),
        (0, 0, 0, 0)
    );
    assert_eq!(r.last.unwrap().mono_ms, 4000);
    let s = &log.stats;
    assert_eq!(s.blocks["signal"].records, 2);
    assert_eq!(s.checkpoints, 1);
}

#[test]
fn every_segment_starts_with_a_checkpoint_and_survives_pruning() {
    let mut log = mem_log(2048, 6144);
    for i in 1..=300u64 {
        let name = ["sim", "signal", "sms", "clients"][(i % 4) as usize];
        log.on_block(
            &ev(
                name,
                i,
                false,
                json!({"n": i, "pad": "x".repeat((i % 50) as usize)}),
            ),
            &tm(i * 1000),
        );
    }
    let (groups, dropped) = log.read_all().unwrap();
    assert_eq!(dropped, 0);
    assert!(groups.len() >= 2, "rotated");
    // oldest segments were pruned
    assert_ne!(groups[0][0].seq, 1);
    for g in &groups {
        let first = g.iter().find(|r| r.kind != KIND_NAMES).unwrap();
        assert_eq!(first.kind, KIND_CHECKPOINT, "segment at seq {}", g[0].seq);
    }
    // each segment alone rebuilds the state as of its end
    let r = replay(&groups[groups.len() - 1..], 0);
    assert_eq!(&r.state, log.state());
    assert_eq!(&replay(&groups, 0).state, log.state());
}

#[test]
fn live_is_written_at_most_once_a_gap_but_stale_flips_at_once() {
    let mut log = mem_log(MEM_SEG_MAX, MEM_KEEP);
    for s in 0..130u64 {
        log.on_block(
            &ev("live", s + 1, false, json!({"uptime": s})),
            &tm(s * 1000),
        );
    }
    let b = &log.stats.blocks["live"];
    assert_eq!(b.records, 3, "0 s, 60 s, 120 s");
    assert_eq!(b.held, 127);
    // the log holds what it wrote; 129 waits for the gap (or a checkpoint)
    assert_eq!(log.state()["live"].data, json!({"uptime": 120}));
    assert_eq!(log.held["live"].data, json!({"uptime": 129}));
    // a stale flip inside the gap is written
    log.on_block(&ev("live", 200, true, json!({"uptime": 129})), &tm(130_500));
    assert_eq!(log.stats.blocks["live"].records, 4);
    assert!(replay_of(&log).state["live"].stale);
}

#[test]
fn a_held_value_is_written_once_its_gap_is_over() {
    let mut log = mem_log(MEM_SEG_MAX, MEM_KEEP);
    log.on_block(&ev("live", 1, false, json!({"uptime": 0})), &tm(0));
    log.on_block(&ev("live", 2, false, json!({"uptime": 5})), &tm(5_000));
    log.round_end(&tm(5_000));
    assert_eq!(log.stats.blocks["live"].records, 1, "still inside the gap");
    // the block stops changing; the round end after the gap writes the held value
    log.round_end(&tm(60_000));
    assert_eq!(log.stats.blocks["live"].records, 2);
    assert_eq!(replay_of(&log).state["live"].data, json!({"uptime": 5}));
    log.round_end(&tm(200_000));
    assert_eq!(
        log.stats.blocks["live"].records, 2,
        "nothing held: nothing written"
    );
}

#[test]
fn held_changes_skip_the_contract() {
    fn counting_door(_: &str, v: &Value, _: &Ctx) -> Result<Checked, Refused> {
        DOOR_CALLS.with(|c| c.set(c.get() + 1));
        Ok(Checked {
            value: v.clone(),
            exact: true,
        })
    }
    thread_local!(static DOOR_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) });
    let (mut log, _) = EvLog::open(
        Box::new(Mem::default()),
        MEM_SEG_MAX,
        MEM_KEEP,
        BOOT,
        b"k".to_vec(),
        counting_door,
    )
    .unwrap();
    for s in 0..30u64 {
        log.on_block(
            &ev("live", s + 1, false, json!({"uptime": s})),
            &tm(s * 1000),
        );
    }
    assert_eq!(DOOR_CALLS.with(|c| c.get()), 1, "only the written change");
}

#[test]
fn hub_events_reach_the_log() {
    use crate::block::{Hub, phase1_blocks};
    use std::sync::{Arc, Mutex};
    let log = Arc::new(Mutex::new(mem_log(MEM_SEG_MAX, MEM_KEEP)));
    let hub = Hub::new(phase1_blocks(), Box::new(LogSink(log.clone())));
    let now = tokio::time::Instant::now();
    hub.record("sim", Ok(json!({"state": "ready", "iccid": "8986"})), now);
    hub.record("sim", Ok(json!({"state": "ready", "iccid": "8986"})), now);
    hub.record("sms", Ok(json!({"count": 1})), now);
    let g = log.lock().unwrap();
    assert_eq!(g.state()["sim"].data["state"], "ready");
    assert_eq!(
        g.state()["sim"].revision,
        1,
        "same data: no new revision, no record"
    );
    assert_eq!(g.stats.blocks["sim"].records, 1);
    assert!(g.state().contains_key("sms"));
    assert_eq!(&replay_of(&g).state, g.state());
}

#[test]
fn damaged_tail_reopens_into_a_new_segment_with_a_checkpoint() {
    let mut log = mem_log(MEM_SEG_MAX, MEM_KEEP);
    log.on_block(&ev("sim", 1, false, json!({"state": "ready"})), &tm(1000));
    log.on_block(&ev("sms", 1, false, json!({"count": 2})), &tm(2000));
    log.store.flush(false).unwrap();
    let mut m = media_of(&log);
    let (_, bytes) = m.0.iter_mut().next_back().unwrap();
    let n = bytes.len();
    bytes[n - 3] ^= 0xff; // the sms record is damaged
    let (mut log2, r) = open_log(m, MEM_SEG_MAX, MEM_KEEP);
    assert!(r.dropped_bytes > 0);
    assert!(r.state.contains_key("sim"));
    assert!(!r.state.contains_key("sms"));
    log2.on_block(&ev("sms", 2, false, json!({"count": 3})), &tm(9000));
    log2.store.flush(false).unwrap();
    let segs = log2.store.segments().unwrap();
    assert_eq!(segs.len(), 2, "never appends after a damaged tail");
    let (groups, _) = log2.read_all().unwrap();
    let last = groups.last().unwrap();
    assert_eq!(
        last.iter().find(|r| r.kind != KIND_NAMES).unwrap().kind,
        KIND_CHECKPOINT
    );
    let r = replay(&groups[1..], 0);
    assert_eq!(
        r.state["sim"].data,
        json!({"state": "ready"}),
        "carried by the checkpoint"
    );
    assert_eq!(r.state["sms"].data, json!({"count": 3}));
}

// ---- reading a newer log (D5): built by hand, frozen as goldens below ----

/// What a schema-2 build might write: a checkpoint and a block of schema 2 (a new
/// reason, an extra field), a record kind this build does not know, all after a
/// schema-1 segment start.
fn future_segment() -> Vec<u8> {
    use ciborium::Value as C;
    let names: Vec<String> = ["b-9", "sim", "state", "signal", "bars", "x"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let int = |i: u64| C::Integer(i.into());
    let m = C::Array(vec![int(0), int(5000), int(1_800_000_005), C::Bool(true)]);
    let rec = |fields: Vec<(&str, C)>| {
        C::Map(
            fields
                .into_iter()
                .map(|(k, v)| (C::Text(k.into()), v))
                .collect(),
        )
    };
    let cbor_v = |v: &C| {
        let mut b = Vec::new();
        ciborium::into_writer(v, &mut b).unwrap();
        b
    };
    let mut out = envelope::MAGIC.to_vec();
    let mut seq = 1;
    let mut put = |schema: u16, kind: u16, payload: Vec<u8>, out: &mut Vec<u8>| {
        encode(seq, schema, kind, &payload, out);
        seq += 1;
    };
    put(
        1,
        KIND_NAMES,
        cbor_v(&C::Array(vec![
            int(0),
            C::Array(names.iter().map(|n| C::Text(n.clone())).collect()),
        ])),
        &mut out,
    );
    // schema 1 checkpoint: sim ready
    let sim = C::Map(vec![(int(2), C::Text("ready".into()))]);
    put(
        1,
        KIND_CHECKPOINT,
        cbor_v(&rec(vec![
            ("m", m.clone()),
            ("r", C::Text("start".into())),
            (
                "s",
                C::Array(vec![C::Array(vec![
                    int(1),
                    int(1),
                    int(10),
                    C::Bool(false),
                    sim,
                ])]),
            ),
        ])),
        &mut out,
    );
    // schema 1 block with an extra field and an unknown reason elsewhere: applied
    let sig = C::Map(vec![(int(4), int(3))]);
    put(
        1,
        KIND_BLOCK,
        cbor_v(&rec(vec![
            ("m", m.clone()),
            ("b", C::Array(vec![int(3), int(1), int(11), C::Bool(false)])),
            ("d", sig),
            ("added_later", int(7)),
        ])),
        &mut out,
    );
    // schema 2 checkpoint with a reason this build does not know: passed over
    put(
        2,
        KIND_CHECKPOINT,
        cbor_v(&rec(vec![
            ("m", m.clone()),
            ("r", C::Text("compact".into())),
            ("s", C::Array(vec![])),
        ])),
        &mut out,
    );
    // schema 2 block: skipped, counted as newer
    put(
        2,
        KIND_BLOCK,
        cbor_v(&rec(vec![
            ("m", m.clone()),
            ("b", C::Array(vec![int(1), int(2), int(12), C::Bool(false)])),
            ("d", C::Map(vec![(int(5), int(1))])),
        ])),
        &mut out,
    );
    // a kind from the future: skipped, counted as unknown
    put(3, 999, vec![0xa0], &mut out);
    out
}

/// The schema-1 log this build writes, fixed inputs.
fn v1_segment() -> Vec<u8> {
    let mut log = mem_log(MEM_SEG_MAX, MEM_KEEP);
    let t = |ms| Tm {
        boot: "b-golden".into(),
        mono_ms: ms,
        wall: 1_800_000_000,
        wall_ok: true,
    };
    log.on_block(
        &ev("sim", 1, false, json!({"state": "ready", "slot": 1})),
        &t(1000),
    );
    log.on_block(
        &ev(
            "signal",
            1,
            false,
            json!({"bars": 4, "nr_snr": "18.0", "rsrp": -90.5}),
        ),
        &t(2000),
    );
    log.on_block(
        &ev("live", 1, false, json!({"system": {"uptime": 123}})),
        &t(3000),
    );
    log.on_block(&ev("wlan", 1, true, Value::Null), &t(4000));
    log.store.flush(false).unwrap();
    let segs = log.store.segments().unwrap();
    assert_eq!(segs.len(), 1);
    log.store.segment_bytes(segs[0]).unwrap()
}

/// Goldens are kept as hex text (no binary files in the repositories), 32 bytes a line.
fn golden(name: &str, made: &[u8]) -> Vec<u8> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/evlog")
        .join(format!("{name}.hex"));
    if std::env::var_os("LUYAZ_UPDATE_GOLDEN").is_some() {
        let hex: Vec<String> = made
            .chunks(32)
            .map(|c| c.iter().map(|b| format!("{b:02x}")).collect())
            .collect();
        std::fs::write(&p, hex.join("\n") + "\n").unwrap();
    }
    let text = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e} (LUYAZ_UPDATE_GOLDEN=1 writes it)", p.display()));
    let digits: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
    digits
        .chunks(2)
        .map(|d| u8::from_str_radix(std::str::from_utf8(d).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn golden_v1_is_what_this_build_writes_and_reads() {
    let made = v1_segment();
    let g = golden("v1.u60l", &made);
    assert_eq!(
        made, g,
        "the schema-1 format changed: bump SCHEMA or keep it the same"
    );
    let d = decode(&g[envelope::MAGIC.len()..]);
    let r = replay(&[d.records], d.dropped_tail);
    assert_eq!(r.state.len(), 4);
    assert_eq!(
        r.state["signal"].data,
        json!({"bars": 4, "nr_snr": "18.0", "rsrp": -90.5})
    );
    assert!(r.state["wlan"].stale);
    assert_eq!(r.last.as_ref().unwrap().boot, "b-golden");
    assert_eq!((r.unknown, r.newer, r.undecodable), (0, 0, 0));
}

#[test]
fn golden_future_is_read_by_this_build() {
    let g = golden("future-v2.u60l", &future_segment());
    assert_eq!(g, future_segment());
    let d = decode(&g[envelope::MAGIC.len()..]);
    assert_eq!(d.dropped_tail, 0, "a newer log is not damage");
    let r = replay(&[d.records], 0);
    assert_eq!(r.from, Some(2), "the schema-1 checkpoint");
    assert_eq!(r.state["sim"].data, json!({"state": "ready"}));
    assert_eq!(
        r.state["signal"].data,
        json!({"bars": 3}),
        "extra field ignored"
    );
    assert_eq!((r.newer, r.unknown, r.undecodable), (2, 1, 0));
}

#[test]
fn only_a_newer_checkpoint_left_means_rebuild() {
    let g = future_segment();
    let d = decode(&g[envelope::MAGIC.len()..]);
    // pruning took the schema-1 start: only the schema-2 records and the names remain
    let rest: Vec<Raw> = d
        .records
        .into_iter()
        .filter(|r| r.kind == KIND_NAMES || r.schema > SCHEMA)
        .collect();
    let r = replay(&[rest], 0);
    assert_eq!(r.from, None);
    assert!(r.state.is_empty(), "start empty, ubus fills it");
    assert_eq!((r.newer, r.unknown), (2, 1));
}

#[test]
fn newer_checkpoint_falls_back_to_an_earlier_segment() {
    // segment 1: schema-1 checkpoint + block; segment 2: a newer checkpoint, then a
    // schema-1 block (an older build again after a second rollback)
    let mut log = mem_log(MEM_SEG_MAX, MEM_KEEP);
    log.on_block(&ev("sim", 1, false, json!({"state": "ready"})), &tm(1000));
    log.on_block(&ev("sms", 1, false, json!({"count": 1})), &tm(2000));
    let (mut groups, _) = log.read_all().unwrap();
    let names = envelope::segment_names(&groups[0]);
    let mut seg2 = Vec::new();
    let mut nb = Vec::new();
    ciborium::into_writer(&(0u32, &names), &mut nb).unwrap();
    encode(10, 1, KIND_NAMES, &nb, &mut seg2);
    encode(11, 2, KIND_CHECKPOINT, b"\xa0", &mut seg2);
    let mut n2 = names.clone();
    let b = BlockC {
        m: pack_tm(&tm(3000), &mut n2),
        b: (n2.id("sms"), 2, 0, false),
        d: pack_value(&json!({"count": 2}), &mut n2),
    };
    assert_eq!(n2, names, "no new names needed");
    encode(12, 1, KIND_BLOCK, &cbor(&b), &mut seg2);
    groups.push(decode(&seg2).records);
    let r = replay(&groups, 0);
    assert_eq!(r.from, Some(groups[0][1].seq));
    assert_eq!(r.newer, 1);
    assert_eq!(r.state["sms"].data, json!({"count": 2}));
    assert_eq!(r.state["sim"].data, json!({"state": "ready"}));
}

#[test]
fn unknown_reason_and_extra_fields_decode() {
    let v: Reason = ciborium::from_reader(&cbor(&"compact")[..]).unwrap();
    assert_eq!(v, Reason::Unknown);
    let v: Reason = ciborium::from_reader(&cbor(&"segment")[..]).unwrap();
    assert_eq!(v, Reason::Segment);
}

/// Record sizes for a real `/v2/state` (LUYAZ_V2STATE=path, not in the repo: the
/// device's values are private). `cargo test device_sizes -- --ignored --nocapture`
#[test]
#[ignore]
fn device_sizes() {
    let p = std::env::var("LUYAZ_V2STATE").expect("LUYAZ_V2STATE");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    let mut log = mem_log(MEM_SEG_MAX, MEM_KEEP);
    let blocks = v["blocks"].as_object().unwrap();
    for (name, b) in blocks {
        let name: &'static str = Box::leak(name.clone().into_boxed_str());
        let e = ev(
            name,
            b["revision"].as_u64().unwrap(),
            false,
            b["data"].clone(),
        );
        log.on_block(&e, &tm(1000));
        let e = ev(
            name,
            b["revision"].as_u64().unwrap() + 1,
            false,
            b["data"].clone(),
        );
        log.on_block(&e, &tm(100_000));
    }
    for (name, s) in &log.stats.blocks {
        println!("{name:16} {:6} B/record", s.bytes / s.records.max(1));
    }
    println!(
        "checkpoint-ish names {} total {}",
        log.stats.name_bytes,
        log.bytes()
    );
}

// ---- changed fields only (KIND_DELTA) ----

fn big(state: &str, extra: Value) -> Value {
    json!({
        "state": state, "slot": 1, "spn": "carrier", "pin_status": "ready",
        "modem_state": "online", "dual_sim": 0, "nest": {"a": 1, "b": "x", "c": [1, 2]},
        "extra": extra
    })
}

#[test]
fn a_change_is_written_as_its_fields_and_replays() {
    let mut log = mem_log(MEM_SEG_MAX, MEM_KEEP);
    log.on_block(&ev("sim", 1, false, big("ready", json!(1))), &tm(1000));
    log.on_block(&ev("sim", 2, false, big("ready", json!(2))), &tm(2000));
    // a key gone and a key set to null are different things
    let mut v3 = big("ready", Value::Null);
    v3["nest"].as_object_mut().unwrap().remove("b");
    log.on_block(&ev("sim", 3, false, v3.clone()), &tm(3000));
    let s = &log.stats.blocks["sim"];
    assert_eq!((s.records, s.deltas), (3, 2));
    let r = replay_of(&log);
    assert_eq!(r.state["sim"].data, v3);
    assert_eq!(r.state["sim"].revision, 3);
    assert_eq!(r.delta_missed, 0);
}

#[test]
fn the_first_record_after_open_is_full() {
    let mut log = mem_log(MEM_SEG_MAX, MEM_KEEP);
    log.on_block(&ev("sim", 1, false, big("ready", json!(1))), &tm(1000));
    log.store.flush(false).unwrap();
    let (mut again, r) = open_log(media_of(&log), MEM_SEG_MAX, MEM_KEEP);
    assert_eq!(r.state["sim"].revision, 1);
    // a new process counts revisions from 1 again: no delta against the old record
    again.on_block(&ev("sim", 2, false, big("ready", json!(2))), &tm(5000));
    assert_eq!(again.stats.blocks["sim"].deltas, 0);
    again.on_block(&ev("sim", 3, false, big("ready", json!(3))), &tm(6000));
    assert_eq!(again.stats.blocks["sim"].deltas, 1);
    assert_eq!(replay_of(&again).state["sim"].data, big("ready", json!(3)));
}

#[test]
fn no_delta_next_to_an_untyped_value_or_when_not_smaller() {
    let mut log = mem_log(MEM_SEG_MAX, MEM_KEEP);
    log.on_block(&ev("sim", 1, false, untyped()), &tm(1000));
    log.on_block(&ev("sim", 2, false, big("ready", json!(1))), &tm(2000));
    log.on_block(&ev("sim", 3, false, untyped()), &tm(3000));
    // a small block that changes completely: the full record is not bigger
    log.on_block(&ev("nfc", 1, false, json!({"switch": 1})), &tm(4000));
    log.on_block(&ev("nfc", 2, false, json!({"switch": 0})), &tm(5000));
    assert_eq!(log.stats.blocks["sim"].deltas, 0);
    let r = replay_of(&log);
    assert_eq!(r.state["sim"].data, untyped());
    assert_eq!(r.state["nfc"].data, json!({"switch": 0}));
}

/// Full, delta, then a delta whose base is not the state's revision (a record lost in
/// between): that block leaves the state (ubus fills it again), the others stay.
fn delta_segment() -> Vec<u8> {
    let mut log = mem_log(MEM_SEG_MAX, MEM_KEEP);
    let t = |ms| Tm {
        boot: "b-golden".into(),
        mono_ms: ms,
        wall: 1_800_000_000,
        wall_ok: true,
    };
    log.on_block(&ev("sim", 1, false, big("ready", json!(1))), &t(1000));
    log.on_block(&ev("sim", 2, false, big("ready", json!(2))), &t(2000));
    log.on_block(&ev("signal", 1, false, big("x", json!(1))), &t(3000));
    let m = pack_tm(&t(4000), &mut log.store);
    let b = (log.store.intern("signal"), 7, 0, false);
    let p = vec![(
        vec![log.store.intern("extra")],
        pack_value(&json!(9), &mut log.store),
    )];
    let d = cbor(&DeltaC {
        m,
        b,
        base: 5,
        p,
        g: vec![],
    });
    log.store.push(SCHEMA, KIND_DELTA, &d);
    log.store.flush(false).unwrap();
    let segs = log.store.segments().unwrap();
    log.store.segment_bytes(segs[0]).unwrap()
}

#[test]
fn golden_v1_delta_is_read_by_this_build() {
    let made = delta_segment();
    let g = golden("v1-delta.u60l", &made);
    assert_eq!(made, g, "the delta format changed: bump SCHEMA or keep it");
    let d = decode(&g[envelope::MAGIC.len()..]);
    let r = replay(&[d.records], d.dropped_tail);
    assert_eq!(r.state["sim"].data, big("ready", json!(2)));
    assert_eq!(r.state["sim"].revision, 2);
    assert!(!r.state.contains_key("signal"));
    assert_eq!((r.delta_missed, r.undecodable, r.unknown), (1, 0, 0));
}

// ---- on disk ----

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("u60d-evlog-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn key_file_is_made_once_private_and_never_replaced() {
    use std::os::unix::fs::PermissionsExt;
    let d = temp_dir("key");
    let p = d.join("evlog.key");
    let k = load_or_create_key(&p).unwrap();
    assert_eq!(k.len(), 32);
    assert_eq!(
        std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&d).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(load_or_create_key(&p).unwrap(), k);
    std::fs::write(&p, b"short").unwrap();
    assert!(load_or_create_key(&p).is_err());
    assert_eq!(std::fs::read(&p).unwrap(), b"short");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn on_disk_round_trip_and_memory_fallback() {
    let d = temp_dir("disk");
    let (dir, key) = (d.join("evlog"), d.join("evlog.key"));
    {
        let (mut log, r) = EvLog::on_disk(&dir, &key, BOOT).unwrap();
        assert!(r.state.is_empty());
        let sim = crate::contract::samples()
            .into_iter()
            .find(|(n, _)| *n == "sim")
            .unwrap()
            .1;
        log.on_block(&ev("sim", 1, false, sim), &tm(1000));
        log.close().unwrap();
    }
    let (_, r) = EvLog::on_disk(&dir, &key, BOOT).unwrap();
    assert_eq!(r.state["sim"].revision, 1);
    // a broken key: memory mode, the file stays
    std::fs::write(&key, b"short").unwrap();
    let log = EvLog::on_disk_or_memory(&dir, &key, BOOT);
    assert!(log.store.segments().unwrap().is_empty());
    assert_eq!(std::fs::read(&key).unwrap(), b"short");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn segments_older_than_a_day_go() {
    let mut log = mem_log(600, 1 << 30);
    let t = |s: i64| Tm {
        boot: BOOT.into(),
        mono_ms: s as u64 * 1000,
        wall: 1_800_000_000 + s,
        wall_ok: true,
    };
    // a record an hour for two days, small segments: many segments
    for h in 0..48i64 {
        log.on_block(
            &ev("sim", h as u64 + 1, false, big("ready", json!(h))),
            &t(h * 3600),
        );
        log.store.flush(false).unwrap();
    }
    let segs = log.store.segments().unwrap();
    let r = replay_of(&log);
    assert_eq!(r.state["sim"].revision, 48);
    // nothing left that ended more than a day before the last record
    let (groups, _) = log.read_all().unwrap();
    let oldest = groups
        .iter()
        .flatten()
        .filter(|r| r.kind == KIND_CHECKPOINT)
        .filter_map(|r| {
            let names = envelope::segment_names(&groups[0]);
            ciborium::from_reader::<CheckpointHead, _>(&r.payload[..])
                .ok()
                .and_then(|h| unpack_tm(h.m, &names))
        })
        .map(|t| t.wall)
        .min()
        .unwrap();
    assert!(segs.len() < 48, "{} segments", segs.len());
    assert!(
        oldest >= 1_800_000_000 + 47 * 3600 - KEEP_SECS - 2 * 3600,
        "oldest checkpoint {}h before the last",
        (1_800_000_000 + 47 * 3600 - oldest) / 3600
    );
}

#[test]
fn stale_flips_are_fsynced_at_most_hourly_cap() {
    let mut log = mem_log(MEM_SEG_MAX, MEM_KEEP);
    log.on_block(&ev("wlan", 1, false, json!({"a": 1})), &tm(0));
    for i in 0..100u64 {
        log.on_block(
            &ev("wlan", i + 2, i % 2 == 0, json!({"a": 1})),
            &tm(1000 + i * 1000),
        );
    }
    assert_eq!(log.stats.fsyncs, FSYNC_PER_HOUR as u64);
    assert_eq!(log.stats.fsyncs_capped, 100 - FSYNC_PER_HOUR as u64);
    // an hour later the cap is free again
    // (the last flip above went to not stale)
    log.on_block(&ev("wlan", 200, true, json!({"a": 1})), &tm(3_700_000));
    assert_eq!(log.stats.fsyncs, FSYNC_PER_HOUR as u64 + 1);
}
