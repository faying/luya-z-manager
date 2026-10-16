use super::*;

fn now(net: &str, sel: &str, band: &str) -> Now {
    Now {
        net: (!net.is_empty()).then(|| net.to_string()),
        net_select: (!sel.is_empty()).then(|| sel.to_string()),
        band: (!band.is_empty()).then(|| band.to_string()),
    }
}

fn boot_ev(boot: &str, code: i64) -> Value {
    json!({"k": "boot", "boot_id": format!("{boot}-0000-0000-0000-000000000000"), "code": code})
}

/// `secs` of 60 s checks in `boot` from boot second `from`.
fn run(p: &mut Places, boot: &str, from: i64, secs: i64, n: &Now) -> i64 {
    let mut t = from;
    while t <= from + secs {
        p.step(boot, t, Some(1_800_000_000 + t), n, &[], true);
        t += 60;
    }
    t
}

#[test]
fn buckets_and_bands() {
    assert_eq!(bucket(Some("WL_AND_5G")), "auto");
    assert_eq!(bucket(Some("TCHGWL_5G")), "auto");
    assert_eq!(bucket(Some("NETWORK_auto")), "auto");
    assert_eq!(bucket(Some("WCDMA_AND_LTE")), SAFE);
    assert_eq!(bucket(None), "unknown");
    assert_eq!(bucket(Some("")), "unknown");
    assert_eq!(band(Some("n78")).as_deref(), Some("n78"));
    assert_eq!(band(Some("B3")).as_deref(), Some("B3"));
    assert_eq!(band(Some("-")), None);
    assert_eq!(
        band(Some("n78 pci=12")),
        None,
        "anything but a band name stays out"
    );
}

#[test]
fn exposure_drops_and_a_crash_charged_to_the_boot_before() {
    let mut p = Places::default();
    let jp = now("440-20", "WL_AND_5G", "n77");
    run(&mut p, "aaaaaaaa", 100, 3600, &jp);
    assert_eq!(p.places["440-20"].prefs["auto"].secs, 3600);
    assert!(p.save_due(3700), "the key changed: on flash at once");
    p.step(
        "aaaaaaaa",
        3760,
        None,
        &jp,
        &[json!({"k": "link", "state": "down"})],
        false,
    );
    assert_eq!(p.places["440-20"].prefs["auto"].drops, 1);
    // the device goes down; the new boot sees its network first, the reason code ~60 s in
    let b = "bbbbbbbb";
    let jp3 = now("440-20", "WCDMA_AND_LTE", "B1");
    p.step(b, 20, None, &jp3, &[], true);
    p.step(b, 80, None, &jp3, &[boot_ev(b, 1132)], true);
    let pl = &p.places["440-20"];
    assert_eq!(pl.prefs["auto"].crashes, 1, "the boot before was on auto");
    assert_eq!(pl.bands["n77"], 1);
    assert_eq!(pl.prefs[SAFE].crashes, 0);
    assert_eq!(pl.prefs[SAFE].secs, 60);
    // the same facts again (a restarted core): counted once
    p.step(b, 140, None, &jp3, &[boot_ev(b, 1132)], true);
    assert_eq!(p.places["440-20"].prefs["auto"].crashes, 1);
    // another boot's facts never count for this one
    p.step(b, 200, None, &jp3, &[boot_ev("cccccccc", 1134)], false);
    assert_eq!(p.places["440-20"].prefs["auto"].crashes, 1);
    // 1182: a raw count, no crash
    p.step("dddddddd", 30, None, &jp3, &[], true);
    p.step(
        "dddddddd",
        90,
        None,
        &jp3,
        &[boot_ev("dddddddd", 1182)],
        true,
    );
    assert_eq!(p.places["440-20"].unclear, 1);
    assert_eq!(p.places["440-20"].prefs[SAFE].crashes, 0);
    // a hand-pressed reboot: nothing
    p.step("eeeeeeee", 30, None, &jp3, &[], true);
    p.step(
        "eeeeeeee",
        90,
        None,
        &jp3,
        &[boot_ev("eeeeeeee", 1150)],
        true,
    );
    assert_eq!(
        p.places["440-20"]
            .prefs
            .values()
            .map(|s| s.crashes)
            .sum::<u32>(),
        1
    );
}

#[test]
fn the_key_survives_a_restart_and_exposure_waits_ten_minutes() {
    let dir = std::env::temp_dir().join(format!("u60d-places-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut p = Places::default();
    let cn = now("460-11", "WL_AND_5G", "n78");
    p.step("aaaaaaaa", 100, None, &cn, &[], true);
    p.save(&dir, 100).unwrap();
    p.step("aaaaaaaa", 160, None, &cn, &[], true);
    assert!(!p.save_due(160), "exposure only: not yet");
    p.step("aaaaaaaa", 700, None, &cn, &[], true);
    assert!(p.save_due(700), "ten minutes on");
    p.save(&dir, 700).unwrap();
    // the device dies; a new process in a new boot reads the file
    let mut q = Places::load(&dir);
    q.step("bbbbbbbb", 20, None, &cn, &[], true);
    q.step(
        "bbbbbbbb",
        80,
        None,
        &cn,
        &[boot_ev("bbbbbbbb", 1134)],
        true,
    );
    assert_eq!(q.places["460-11"].prefs["auto"].crashes, 1);
    assert_eq!(
        q.places["460-11"].prefs["auto"].secs,
        600 + 60,
        "and the new boot's first minute"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn place(prefs: &[(&str, i64, u32)]) -> Places {
    let mut p = Places::default();
    for &(k, secs, crashes) in prefs {
        let s = p.stat("440-20", k);
        s.secs = secs;
        s.crashes = crashes;
    }
    p
}

#[test]
fn advice_follows_d12() {
    let jp_auto = now("440-20", "WL_AND_5G", "");
    // enough on both sides: suggest
    let p = place(&[("auto", 3 * 3600, 3), (SAFE, 4 * 3600, 0)]);
    let a = p.advice(&jp_auto);
    assert_eq!(a["verdict"], "suggest", "{a}");
    assert_eq!(
        a["text"],
        "这里上次用「自动」崩了 3 次（3.0 小时）；4G + 3G 用了 4.0 小时没崩，要不要切 4G + 3G？"
    );
    // already on 4G + 3G: no suggestion
    assert_eq!(
        p.advice(&now("440-20", "WCDMA_AND_LTE", ""))["verdict"],
        "insufficient"
    );
    // each threshold one short: not enough evidence, counts shown
    for prefs in [
        [("auto", 3 * 3600, 1), (SAFE, 4 * 3600, 0)],
        [("auto", 2 * 3600 - 1, 3), (SAFE, 4 * 3600, 0)],
        [("auto", 3 * 3600, 3), (SAFE, 3 * 3600 - 1, 0)],
        [("auto", 3 * 3600, 3), (SAFE, 4 * 3600, 1)],
    ] {
        let a = place(&prefs).advice(&jp_auto);
        assert_eq!(a["verdict"], "insufficient", "{prefs:?}");
        assert!(
            a["text"]
                .as_str()
                .unwrap()
                .starts_with("证据不足：这里崩过 "),
            "{a}"
        );
    }
    // unknown preference never takes part
    let p = place(&[("unknown", 9 * 3600, 9), (SAFE, 9 * 3600, 0)]);
    assert_eq!(p.advice(&now("440-20", "", ""))["verdict"], "insufficient");
    // no crash here; somewhere unknown
    assert_eq!(
        place(&[(SAFE, 3600, 0)]).advice(&jp_auto)["verdict"],
        "quiet"
    );
    assert_eq!(
        Places::default().advice(&now("", "", ""))["verdict"],
        "unknown"
    );
    // the block matches its contract
    let a = p.advice(&jp_auto);
    let _: crate::contract::Advice = serde_json::from_value(a).unwrap();
    let _: crate::contract::Advice = serde_json::from_value(advice_now()).unwrap();
}

/// A ledger shaped on the 9-26 audit: Japan on auto with n77 crashes, then 4G + 3G; the
/// preference only on the first boot line and a `net_change`, carried across boots.
fn ledger(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let w = |name: &str, lines: &[Value]| {
        let t: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(dir.join(name), t).unwrap();
    };
    let hour = |from: i64, to: i64, net: &str| json!({"k": "hour", "from": from, "to": to, "net": net, "tt": 1_790_000_000 + to});
    w(
        "boot-000001-11111111-001.jsonl",
        &[
            json!({"k": "boot", "boot": "11111111-x", "code": 1150, "net_select": "WL_AND_5G"}),
            json!({"k": "net_change", "net_select": "WL_AND_5G", "net": "440-20", "home": "460-11"}),
            hour(0, 3600, "440-20"),
            json!({"k": "ssr", "band": "-", "nrband": "n77"}),
            hour(3600, 5400, "440-20"),
        ],
    );
    // crashed: charged to auto/n77
    w(
        "boot-000002-22222222-001.jsonl",
        &[
            json!({"k": "boot", "boot": "22222222-x", "code": 1132, "net_select": null}),
            hour(0, 1800, "440-20"),
            json!({"k": "link", "state": "down"}),
        ],
    );
    // crashed again (pref carried: still auto); then switched to 4G + 3G
    w(
        "boot-000003-33333333-001.jsonl",
        &[
            json!({"k": "boot", "boot": "33333333-x", "code": 1134, "net_select": null}),
            json!({"k": "net_change", "net_select": "WCDMA_AND_LTE", "net": "440-20", "home": "460-11"}),
            hour(0, 3600, "440-20"),
        ],
    );
    w(
        "boot-000003-33333333-002.jsonl",
        &[hour(3600, 7200, "440-20"), hour(7200, 10800, "440-20")],
    );
    // a hand reboot, still 4G + 3G; back home later
    w(
        "boot-000004-44444444-001.jsonl",
        &[
            json!({"k": "boot", "boot": "44444444-x", "code": 1150, "net_select": null}),
            hour(0, 3600, "440-20"),
            json!({"k": "net_change", "net_select": "WL_AND_5G", "net": "460-11", "home": "460-11"}),
            hour(3600, 7200, "460-11"),
        ],
    );
}

#[test]
fn backfill_from_the_guard_ledger() {
    let dir = std::env::temp_dir().join(format!("u60d-places-ledger-{}", std::process::id()));
    ledger(&dir);
    let mut p = Places::default();
    p.counted.push("44444444".into());
    p.backfill(&dir);
    assert!(p.backfilled);
    let jp = &p.places["440-20"];
    assert_eq!(
        jp.prefs["auto"],
        Stat {
            secs: 5400 + 1800,
            crashes: 2,
            drops: 1
        }
    );
    assert_eq!(
        jp.prefs[SAFE],
        Stat {
            secs: 10800 + 3600,
            crashes: 0,
            drops: 0
        }
    );
    assert_eq!(
        jp.bands["n77"], 1,
        "the band of the first crash's boot; the second had no ssr"
    );
    assert_eq!(p.places["460-11"].prefs["auto"].secs, 3600);
    assert_eq!(p.counted, ["11111111", "22222222", "33333333", "44444444"]);
    // in Japan on auto: 2 crashes over 2 h, 4G + 3G 4 h clean → the first suggestion
    assert_eq!(
        p.advice(&now("440-20", "WL_AND_5G", ""))["verdict"],
        "suggest"
    );
    // the live fold picks up in the last ledger boot: no crash counted twice
    p.step(
        "44444444",
        8000,
        None,
        &now("460-11", "WL_AND_5G", "n78"),
        &[boot_ev("44444444", 1134)],
        true,
    );
    assert_eq!(p.places["460-11"].prefs["auto"].crashes, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// D6: no cell id, no PCI — in the saved state or the block.
#[test]
fn no_cell_identity_anywhere() {
    let dir = std::env::temp_dir().join(format!("u60d-places-scan-{}", std::process::id()));
    ledger(&dir);
    let mut p = Places::default();
    p.backfill(&dir);
    p.step(
        "55555555",
        100,
        None,
        &now("440-20", "WL_AND_5G", "n77"),
        &[],
        true,
    );
    let a = p.advice(&now("440-20", "WL_AND_5G", "n77"));
    for text in [serde_json::to_string(&p).unwrap(), a.to_string()] {
        let low = text.to_lowercase();
        for word in [
            "cell", "pci", "cid", "tac", "enb", "gnb", "earfcn", "arfcn", "imsi", "iccid",
        ] {
            assert!(!low.contains(&format!("\"{word}")), "{word} in {text}");
        }
    }
    for pl in p.places.values() {
        assert!(pl.bands.keys().all(|b| band(Some(b)).is_some()));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
