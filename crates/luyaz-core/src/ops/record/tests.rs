//! 流水账：单一写者、并发追加时滚动、密码不落盘、skipped 合并（write-op-layer.md 测试计划「流水账」）。

use super::*;
use crate::ops::Source;

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("core-journal-{:016x}", rand::random::<u64>()));
    fs::create_dir_all(&d).unwrap();
    d
}

fn raw(dir: &Path) -> Vec<u8> {
    let mut all = Vec::new();
    for f in [JOURNAL, JOURNAL_OLD, OWNERS] {
        all.extend(fs::read(dir.join(f)).unwrap_or_default());
    }
    all
}

fn file_lines(dir: &Path, name: &str) -> Vec<String> {
    fs::read_to_string(dir.join(name))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn concurrent_appends_while_rolling_stay_whole_lines_and_bounded() {
    let dir = temp_dir();
    let max = 4_096;
    let r = Record::open_with(Some(dir.clone()), max);
    let threads: Vec<_> = (0..8)
        .map(|n| {
            let r = r.clone();
            std::thread::spawn(move || {
                for i in 0..60 {
                    r.append(json!({"source":"web","action":format!("t{n}"),"seq":i,"pad":"x".repeat(40)}));
                    if i % 7 == 0 {
                        std::thread::yield_now();
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    // 通道满时丢掉的记数，下一行之前补一行 dropped
    r.flush().await;
    r.append(json!({"source":"web","action":"last"}));
    r.flush().await;
    let mut total = 0;
    for name in [JOURNAL, JOURNAL_OLD] {
        let lines = file_lines(&dir, name);
        for l in &lines {
            let v: Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("{name}: {e}: {l}"));
            assert!(v["t"].is_string() && v["ts"].is_u64(), "{l}");
        }
        total += fs::metadata(dir.join(name)).map(|m| m.len()).unwrap_or(0);
    }
    assert!(total <= 2 * max + MAX_LINE as u64, "{total}");
    // 滚动过（旧的一份存在），最后一行在最新那份
    assert!(dir.join(JOURNAL_OLD).exists());
    assert_eq!(r.list(1)[0]["action"], "last");
    // 通道满时丢的会补一行 dropped，所以条数可能少于 481；但不会多
    let listed = r.list(10_000);
    assert!(listed.len() <= 481);
}

#[tokio::test]
async fn secrets_never_reach_the_disk() {
    let dir = temp_dir();
    let r = Record::open_with(Some(dir.clone()), 1 << 20);
    let secret = "Hunter2-ZQX-7731";
    for (action, params) in [
        (
            "wifi.configure",
            json!({"section":"main_5g","ssid":"home","key":secret}),
        ),
        (
            "wifi.interface.configure",
            json!({"ssid":"guest","key":secret}),
        ),
        (
            "apn.add",
            json!({"name":"x","apn":"internet","username":secret,"password":secret}),
        ),
        (
            "apn.modify",
            json!({"profile_id":"2","nested":{"auth":{"password":secret}}}),
        ),
        (
            "sms.send_raw",
            json!({"number":"+8613800000000","text":secret}),
        ),
    ] {
        r.append(
            json!({"source":"web","action":action,"params":redact(action, &params),"result":"ok"}),
        );
    }
    // journal.append 的内容也过一遍
    r.append(redact("esim.download", &json!({"source":"web","action":"esim.download","activation_code":"LPA:1$x","confirm_code":secret,"pin":"1234"})));
    // eSIM 任务结果里的完整 SIM 身份只留后 4 位
    r.append(redact(
        "esim.switch",
        &json!({"source":"web","action":"esim.switch","result":{"iccid":"89860098765432105678","eid":"89049032000001000000123456789012","profile":{"imsi":460001234567890u64,"msisdn":"+8613912345678"}}}),
    ));
    r.flush().await;
    let all = raw(&dir);
    let text = String::from_utf8_lossy(&all);
    assert!(!text.contains(secret), "{text}");
    for full in [
        "8986009876543210",
        "890490320000010000001234",
        "46000123456",
        "13912345678",
    ] {
        assert!(!text.contains(full), "{full} in {text}");
    }
    assert!(text.contains("\"iccid\":\"5678\"") && text.contains("\"imsi\":\"7890\""));
    assert!(!text.contains("8613800000000"));
    assert!(!text.contains("\"1234\""));
    assert!(!text.contains("LPA:1$x"));
    assert!(text.contains("\"ssid\":\"home\""));
}

#[test]
fn redaction_rules() {
    let p = redact(
        "wifi.configure",
        &json!({"ssid":"home","key":"k","encryption":"psk2","hidden":0}),
    );
    assert_eq!(p["key"], REDACTED);
    assert_eq!(p["ssid"], "home");
    // 加密方式的值不是秘密，但字段名里有 psk 的才遮；encryption 不遮
    assert_eq!(p["encryption"], "psk2");
    let p = redact(
        "apn.add",
        &json!({"apn":"internet","username":"u","password":"p","auth_mode":"chap"}),
    );
    assert_eq!(
        (p["username"].as_str(), p["password"].as_str()),
        (Some(REDACTED), Some(REDACTED))
    );
    assert_eq!(p["apn"], "internet");
    assert_eq!(redact("sms.delete", &json!({"ids":"1,2"})), Value::Null);
    // SIM PIN / 网络锁码（vendor.call 原样带的原厂字段名）
    let p = redact(
        "vendor.call",
        &json!({"object":"zwrt_zte_mdm.api","method":"sim_change_pin","args":{"pin_num":"1234","new_pin_num":"5678","pin_num_m":"1111","nck":"99887766","pin_mode":"1","pin_encode_flag":"0"}}),
    );
    for k in ["pin_num", "new_pin_num", "pin_num_m", "nck"] {
        assert_eq!(p["args"][k], REDACTED, "{k}");
    }
    assert_eq!(
        (
            p["args"]["pin_mode"].as_str(),
            p["args"]["pin_encode_flag"].as_str()
        ),
        (Some("1"), Some("0"))
    );
    assert_eq!(p["method"], "sim_change_pin");
    // 原样发的短信：号码、内容都不记
    assert_eq!(
        redact(
            "vendor.call",
            &json!({"object":"zwrt_wms","method":"zte_libwms_send_sms","args":{"number":"+8613800000000","message_body":"x"}})
        ),
        Value::Null
    );
    // 空值不写成「已改」
    assert_eq!(
        redact("apn.add", &json!({"password":null}))["password"],
        Value::Null
    );
}

#[tokio::test]
async fn hundred_skips_add_two_lines() {
    let dir = temp_dir();
    let r = Record::open_with(Some(dir.clone()), 1 << 20);
    for _ in 0..100 {
        r.skip("scenario", "network.mode", "user_hold");
    }
    // 保持期结束：情景这次真写了
    r.append(json!({"source":"scenario","item":"network.mode","action":"network.set_mode","result":"confirmed"}));
    r.flush().await;
    let lines = file_lines(&dir, JOURNAL);
    assert_eq!(lines.len(), 3, "{lines:?}");
    let start: Value = serde_json::from_str(&lines[0]).unwrap();
    let end: Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(
        (start["skip"].as_str(), start["count"].as_u64()),
        (Some("start"), Some(1))
    );
    assert_eq!(
        (end["skip"].as_str(), end["count"].as_u64()),
        (Some("end"), Some(100))
    );
    assert_eq!(end["reason"], "user_hold");
}

#[tokio::test]
async fn skip_reason_change_ends_the_run_and_others_do_not() {
    let dir = temp_dir();
    let r = Record::open_with(Some(dir.clone()), 1 << 20);
    r.skip("scheduler", "network.mode", "busy");
    r.skip("scheduler", "network.mode", "busy");
    // 别的来源、别的项不打断
    r.append(json!({"source":"web","item":"network.mode","result":"confirmed"}));
    r.skip("scenario", "network.mode", "user_hold");
    r.skip("scheduler", "network.mode", "busy");
    r.skip("scheduler", "network.mode", "user_hold");
    r.flush().await;
    let got: Vec<(String, String, String, u64)> = file_lines(&dir, JOURNAL)
        .iter()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v["result"] == "skipped")
        .map(|v| {
            (
                v["source"].as_str().unwrap().into(),
                v["reason"].as_str().unwrap().into(),
                v["skip"].as_str().unwrap().into(),
                v["count"].as_u64().unwrap(),
            )
        })
        .collect();
    let s = |a: &str, b: &str, c: &str, n| (a.to_owned(), b.to_owned(), c.to_owned(), n);
    assert_eq!(
        got,
        [
            s("scheduler", "busy", "start", 1),
            s("scenario", "user_hold", "start", 1),
            s("scheduler", "busy", "end", 3),
            s("scheduler", "user_hold", "start", 1),
        ]
    );
}

#[tokio::test]
async fn owners_survive_a_restart() {
    let dir = temp_dir();
    let r = Record::open_with(Some(dir.clone()), 1 << 20);
    r.set_owner(
        "network.mode",
        Source::Legacy,
        false,
        &json!("Only_LTE"),
        None,
    );
    r.set_owner(
        "network.mode",
        Source::Screen,
        true,
        &json!("WL_AND_5G"),
        Some("op9"),
    );
    r.flush().await;
    let again = Record::open_with(Some(dir.clone()), 1 << 20);
    let o = again.owners();
    assert_eq!(o["network.mode"]["source"], "screen");
    assert_eq!(o["network.mode"]["user"], true);
    assert_eq!(o["network.mode"]["undo"], true);
    assert_eq!(o["network.mode"]["value"], "WL_AND_5G");
    assert!(o["network.mode"]["ts"].as_u64().unwrap() > 1_700_000_000);
    r.set_owner(
        "network.mode",
        Source::Guard,
        false,
        &json!("TCHGWL_5G"),
        None,
    );
    assert_eq!(r.owners()["network.mode"]["user"], false);
}

#[tokio::test]
async fn long_lines_are_cut() {
    let dir = temp_dir();
    let r = Record::open_with(Some(dir.clone()), 1 << 20);
    r.append(json!({"source":"web","action":"x","detail":"y".repeat(5_000)}));
    let many: Vec<String> = (0..50)
        .map(|i| format!("{i}-{}", "z".repeat(200)))
        .collect();
    r.append(json!({"source":"web","action":"many","list":many,"result":"ok"}));
    r.flush().await;
    for l in file_lines(&dir, JOURNAL) {
        assert!(l.len() <= MAX_LINE, "{}", l.len());
    }
    let v = r.list(1);
    assert_eq!(
        (v[0]["action"].as_str(), v[0]["truncated"].as_bool()),
        (Some("many"), Some(true))
    );
}

#[test]
fn no_dir_records_nothing() {
    let r = Record::open(None);
    r.append(json!({"source":"web","action":"x"}));
    r.skip("scenario", "network.mode", "user_hold");
    assert!(r.list(10).is_empty());
    assert_eq!(r.owners(), json!({}));
}

#[test]
fn civil_time() {
    assert_eq!(civil(0), "1970-01-01 00:00:00");
    assert_eq!(civil(951_782_400), "2000-02-29 00:00:00");
    assert_eq!(civil(1_790_000_000), "2026-09-21 14:13:20");
}

#[test]
fn txn_line_has_only_the_iccid_tail() {
    let t = Txn::new(
        crate::ops::txn::NewTxn {
            op_id: "op1".into(),
            action: "network.set_mode".into(),
            item: "network.mode".into(),
            source: Source::Screen,
            approved_by: Source::Screen,
            undo: false,
            target: "Only_LTE".into(),
            old: "WL_AND_5G".into(),
            rollback_to: "TCHGWL_5G".into(),
            sim: crate::ops::txn::SimId {
                iccid: "89860012345678901234".into(),
                slot: 1,
            },
            conn: None,
            confirm: crate::ops::txn::Confirm::Registered,
            rollback_enabled: false,
            deadline_ms: 1,
            boot_id: "b".into(),
            receipt: false,
        },
        1,
    );
    let l = txn_line(&t);
    assert_eq!(l["sim"], "1234/1");
    assert!(!l.to_string().contains("8986001234"));
    assert_eq!(
        (l["old"].as_str(), l["rollback_to"].as_str()),
        (Some("WL_AND_5G"), Some("TCHGWL_5G"))
    );
    // 没确认通的不记用时
    assert!(l["took_ms"].is_null());
}
