//! Dual-slot regression contract (dual-slot-removable.md R6/D8 ①): the main device's `/v2` `sim`
//! block stays byte for byte what it was before the card table. Fixtures are the main device's
//! real `get_sim_info` and `uci show zwrt_zte_mdm.sim_info` (11-16, core 1.98.9 / 161deb0,
//! read-only; ICCID/IMSI/MSISDN/MAC replaced by fakes of the same length and prefix); the golden
//! is that device's `/v2/state` `blocks.sim.data` with the same replacements, not this code's
//! output. `epoch`/`phase`/`say*` are stamped later by `state::sim_identity_stamp`; the test
//! stamps them through the same `state::sim_stamp` for a fresh identity tracker (epoch 0, online), as on the device.
//! Do not re-bless the golden to make a change pass: a diff here is a regression on the main
//! device.

use super::*;

fn fixture(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// A round where every read failed except `get_sim_info` and the uci packages.
fn inputs(sim: Value, uci_sets: Vec<BTreeMap<String, String>>) -> Inputs {
    let no = || Err::<Value, String>("not read in this test".into());
    Inputs {
        common: no(),
        board: no(),
        info: no(),
        net: no(),
        traffic: no(),
        accounting: no(),
        limit: no(),
        clear_day: no(),
        sim: Ok(sim),
        sim_read: true,
        imei: no(),
        lan_clients: no(),
        wifi_clients: no(),
        router_status: no(),
        thermal: no(),
        hightemp: no(),
        qci_now: no(),
        usb: no(),
        battery: no(),
        charger: no(),
        nfc: no(),
        apn: Err("not read in this test".into()),
        router: [no(), no(), no(), no(), no(), no(), no(), no()],
        guest_left: None,
        sms_capacity: no(),
        capacity_read: false,
        sms_nv: no(),
        nv_read: false,
        sms_sim: no(),
        sim_list_read: false,
        now_epoch: 1_794_823_071,
        standby_signal: None,
        sms_list: None,
        lan_if: no(),
        wan4_if: no(),
        wan6_if: no(),
        lan_config: no(),
        cellular: no(),
        uci_sets,
        qos_logs: Default::default(),
        leases: Default::default(),
        rails: Default::default(),
        cpu_sys: 0,
        zones: Value::Null,
        cpu_usage_tenths: -1,
        runtime: Default::default(),
    }
}

fn sim_block(sim: Value, mdm_uci: &str) -> Value {
    let mut uci_sets = vec![BTreeMap::new(); PACKAGES.len()];
    uci_sets[0] = crate::state::uci_show_map(mdm_uci.as_bytes());
    let p = project(inputs(sim, uci_sets), 2000);
    let mut v = p
        .blocks
        .iter()
        .find(|(n, _)| *n == "sim")
        .and_then(|(_, d)| d.as_ref().ok().cloned())
        .expect("a sim block");
    // what state::sim_identity_stamp adds, for a fresh tracker
    let t = crate::sim_identity::SimIdentity::default();
    crate::state::sim_stamp(
        &mut v,
        t.epoch(),
        t.phase().as_str(),
        crate::sim_say::for_phase(t.phase()),
    );
    v
}

#[test]
fn the_main_device_sim_block_is_unchanged() {
    let sim: Value = serde_json::from_str(&fixture("main-get-sim-info.json")).unwrap();
    let v = sim_block(sim, &fixture("main-uci-sim-info.txt"));
    assert!(v.get("dual").is_none(), "single-slot: no dual key");
    assert!(
        v.get("cards").is_none(),
        "the card table is its own block, not in sim"
    );
    assert_eq!(
        serde_json::to_string(&v).unwrap() + "\n",
        fixture("main-v2-sim.golden.json")
    );
}

/// The msisdn fallback still reads uci when `get_sim_info` has none.
#[test]
fn the_msisdn_falls_back_to_uci_as_before() {
    let mut sim: Value = serde_json::from_str(&fixture("main-get-sim-info.json")).unwrap();
    sim["msisdn"] = json!("");
    let v = sim_block(sim, &fixture("main-uci-sim-info.txt"));
    assert_eq!(v["msisdn"], "8613000000000");
}
