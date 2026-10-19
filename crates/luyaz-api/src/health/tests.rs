use super::*;

#[test]
fn parses_doctor_tsv_and_skips_junk() {
    let c = parse(
        "ok\tfota\tZTE 自动升级\t已关闭\nbad\tscreen\t触屏界面\t没在运行\nnonsense\nwarn\tx\ty\tdetail\twith tab\n",
    );
    assert_eq!(c.len(), 3);
    assert_eq!(c[1].level, "bad");
    assert_eq!(c[2].detail, "detail\twith tab");
    assert_eq!(counts(&c), (1, 1));
}

#[test]
fn the_manifest_row_is_an_ordinary_row() {
    // doctor.sh --tsv ends with the device manifest's verdict (touch-ui docs/SHIP.md)
    let tsv = "ok\tstandby\t待机\t正常\n\
               warn\tmanifest\t清单\t不一致的是 luyaz-core（清单 6bbf6ea6，实际 af3c8ad8）\n";
    let c = parse(tsv);
    assert_eq!(c.len(), 2);
    assert_eq!(c[1].id, "manifest");
    assert_eq!(c[1].label, "清单");
    assert_eq!(
        c[1].detail,
        "不一致的是 luyaz-core（清单 6bbf6ea6，实际 af3c8ad8）"
    );
    assert_eq!(counts(&c), (0, 1));
    let ok = parse("ok\tmanifest\t清单\t观察中（还剩 42 分钟）：核心刚上机；一致（12 项）\n");
    assert_eq!(counts(&ok), (0, 0));
}

// Built from the R2 spec (docs/designs/ui-english.md): marker line, then
// `level id label detail label_en detail_en`.
const TSV2: &str = "#tsv2\n\
    ok\tfota\tZTE 自动升级\t已关闭\tZTE auto-update\tOff\n\
    bad\tscreen\t触屏界面\t没在运行\tScreen UI\tNot running\n\
    nonsense\n\
    warn\tx\ty\tdetail\twith tab\tY\tDetail\n\
    ok\tshort\ta\tb\tc\n";

#[test]
fn parses_doctor_tsv2_with_english() {
    assert!(has_tsv2_marker(TSV2));
    let c = parse_tsv2(TSV2);
    assert_eq!(c.len(), 3, "5-column and junk rows are skipped");
    assert_eq!(
        (c[0].label.as_str(), c[0].detail.as_str()),
        ("ZTE 自动升级", "已关闭")
    );
    assert_eq!(
        (c[0].label_en.as_str(), c[0].detail_en.as_str()),
        ("ZTE auto-update", "Off")
    );
    assert_eq!(c[2].detail, "detail\twith tab");
    assert_eq!(
        (c[2].label_en.as_str(), c[2].detail_en.as_str()),
        ("Y", "Detail")
    );
    assert_eq!(counts(&c), (1, 1));
    for x in &c {
        assert!(!has_cjk(&x.label_en) && !has_cjk(&x.detail_en), "{x:?}");
    }
    let j = check_json(&c[1]);
    assert_eq!(j["label_en"], "Screen UI");
    assert_eq!(j["label"], "触屏界面");
}

/// A sample of real `<core> doctor --tsv2` output (the core's doctor test data; skipped
/// when it is not there).
#[test]
fn parses_touch_ui_tsv2_sample() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../luyaz-core/src/doctor/testdata/tsv2-sample.tsv"
    );
    let Ok(text) = fs::read_to_string(path) else {
        eprintln!("skipped: no {path}");
        return;
    };
    assert!(has_tsv2_marker(&text));
    let rows = text
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .count();
    let c = parse_tsv2(&text);
    assert_eq!(c.len(), rows, "every row of the sample parses");
    for x in &c {
        assert!(!x.label_en.is_empty() && !x.detail_en.is_empty(), "{x:?}");
        assert!(!has_cjk(&x.label_en) && !has_cjk(&x.detail_en), "{x:?}");
    }
    let (bad, warn) = counts(&c);
    assert_eq!(
        bad + warn + c.iter().filter(|x| x.level == "ok").count(),
        rows
    );
}

#[test]
fn marker_must_be_the_exact_first_line() {
    assert!(has_tsv2_marker("#tsv2\r\nok\ta\tb\tc\td\te\n"));
    assert!(!has_tsv2_marker("ok\tfota\tZTE 自动升级\t已关闭\n#tsv2\n"));
    assert!(!has_tsv2_marker("#tsv2x\n"));
    assert!(!has_tsv2_marker(" #tsv2\n"));
    assert!(!has_tsv2_marker(""));
}

#[test]
fn old_doctor_rows_have_no_english_and_send_null() {
    let c = parse("ok\tfota\tZTE 自动升级\t已关闭\n");
    let j = check_json(&c[0]);
    assert!(j["label_en"].is_null() && j["detail_en"].is_null());
    assert_eq!(j["detail"], "已关闭");
}

/// The touch screen reads /api/health into 32 KB (touch-ui alerts.c
/// AL_RESP_MAX). Doctor has ~38 report rows; 40 long ones in both
/// languages must leave a quarter for the crash-log list after them.
#[test]
fn forty_long_rows_fit_the_screen_buffer() {
    let mut tsv = String::from("#tsv2\n");
    for i in 0..40 {
        tsv.push_str(&format!(
            "warn\tid{i}\t{}\t{}\t{}\t{}\n",
            "标".repeat(10),
            "详".repeat(60),
            "L".repeat(24),
            "D".repeat(120)
        ));
    }
    let checks: Vec<Value> = parse_tsv2(&tsv).iter().map(check_json).collect();
    let size = serde_json::to_string(&checks).unwrap().len();
    eprintln!("health checks: {size} bytes");
    assert_eq!(checks.len(), 40);
    assert!(size < 24 * 1024, "{size} bytes");
}

#[test]
fn names_cannot_escape_the_crash_dir() {
    assert!(safe_name("luyaz-core"));
    assert!(safe_name("20260923-150424-up2325.log"));
    assert!(!safe_name(".."));
    assert!(!safe_name("../etc"));
    assert!(!safe_name("a/b"));
    assert!(!safe_name(".hidden"));
    assert!(!safe_name(""));
}

#[test]
fn the_snapshot_file_round_trips() {
    let s = Snapshot {
        checks: parse_tsv2(TSV2),
        at_device: 1_760_000_000,
        ran: true,
        error: None,
    };
    let back =
        Snapshot::from_json(&serde_json::from_str(&s.to_json().to_string()).unwrap()).unwrap();
    assert_eq!(back, s);
    let old = Snapshot {
        checks: parse("ok\tfota\tZTE 自动升级\t已关闭\n"),
        ..s.clone()
    };
    assert_eq!(
        Snapshot::from_json(&old.to_json()).unwrap(),
        old,
        "null English stays empty"
    );
    let failed = Snapshot {
        error: Some("doctor timed out".into()),
        ..s
    };
    assert_eq!(Snapshot::from_json(&failed.to_json()).unwrap(), failed);
    assert_eq!(public_summary(&failed)["checked"], false);
}

#[test]
fn a_missing_snapshot_reads_as_never_ran() {
    let s = Snapshot::read(Path::new("/nonexistent/health.json"));
    assert!(!s.ran);
    assert_eq!(
        public_summary(&s),
        json!({"checked": false, "bad": 0, "warn": 0})
    );
    let r = reply(&s, Vec::new());
    assert!(r["checked_at"].is_null() && r["checks"] == json!([]));
}
