//! Phase 5 step 5c: the guard's alert SMS and LAN IPv6 duties once the guard hands them over.

use super::tests_ops::{Dev, Shared, control_server};
use super::*;
use std::fs;
use std::sync::Mutex;
use std::time::Duration;

async fn wait(what: &str, f: impl Fn() -> bool) {
    for _ in 0..1000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never got there: {what}");
}

fn actions(d: &Shared, action: &str) -> Vec<Value> {
    d.lock()
        .unwrap()
        .log
        .iter()
        .filter(|v| v["action"] == action)
        .cloned()
        .collect()
}

fn put(root: &std::path::Path, p: &str, text: &str) {
    let f = root.join(p.trim_start_matches('/'));
    fs::create_dir_all(f.parent().unwrap()).unwrap();
    fs::write(f, text).unwrap();
}

#[tokio::test]
async fn u60d_sends_alert_sms_and_keeps_lan_v6_off_once_the_guard_hands_over() {
    let root = std::env::temp_dir().join(format!("u60d-guardduties-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    put(&root, "/proc/uptime", "5000.00 1.00\n");
    put(&root, "/proc/sys/kernel/random/boot_id", "boot-a\n");
    put(
        &root,
        "/tmp/luyaz/clock-ok",
        &luyaz_api::clock::ClockEvidence::observation(
            luyaz_api::alerts::now(),
            luyaz_api::clock::BootTime {
                boot_id: "boot-a".into(),
                seconds: 5000,
            },
            Some(5000),
            luyaz_api::clock::ClockSource::Year,
        )
        .encode(),
    );
    put(&root, "/data/luyaz/ui/ui.conf", "lang=en\n");
    put(&root, "/data/luyaz/scripts/lan-ipv6-off", "");
    put(&root, "/proc/sys/net/ipv6/conf/br-lan/disable_ipv6", "0\n");
    let a = luyaz_api::alerts::Alerts::at(&root.join("data/alerts"));
    a.write_whole("sms-to", "+8613800000000\n").unwrap();
    a.add("web-crash", "x", luyaz_api::alerts::now(), 4000)
        .unwrap();

    let d: Shared = Arc::new(Mutex::new(Dev::default()));
    let url = control_server(d.clone()).await;
    let src = Sources {
        // the sent box holds the copy the firmware kept (phone memory)
        ubus: Arc::new(move |o, m, a| {
            let r = match (o, m) {
                ("zwrt_wms", "zte_libwms_get_sms_data") if a["mem_store"] == json!(1) => {
                    Ok(json!({"messages": [{"id": "41", "number": "+8613800000000"}]}))
                }
                ("zwrt_wms", "zte_libwms_get_sms_data") => Ok(json!({"messages": []})),
                _ => Err("not faked".to_string()),
            };
            Box::pin(async move { r })
        }),
        control: Arc::new(control::Client::new(url, None)),
        ..Sources::none()
    };
    let c = Compat::new(root.to_str().unwrap(), src)
        .with_writes(true)
        .with_clock(1000);
    tokio::spawn(crate::engines::guard_duties::run(c.clone()));

    // still the guard's: nothing sent, LAN IPv6 left alone
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(actions(&d, "vendor.call").is_empty());
    let v6 =
        || fs::read_to_string(root.join("proc/sys/net/ipv6/conf/br-lan/disable_ipv6")).unwrap();
    assert_eq!(v6(), "0\n");

    // handed over: one SMS as source guard, in English with the code, the copy deleted
    put(
        &root,
        crate::engines::guard_duties::FILE,
        &luyaz_api::handover::text(&["sms", "lanv6", "core"]),
    );
    wait("sent", || a.read_u64("sms-done") == 1).await;
    let sent = actions(&d, "vendor.call");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["source"], "guard");
    assert_eq!(sent[0]["params"]["method"], "zte_libwms_send_sms");
    let body = sent[0]["params"]["args"]["message_body"].as_str().unwrap();
    let text: String = (0..body.len() / 4)
        .map(|i| char::from_u32(u32::from_str_radix(&body[i * 4..i * 4 + 4], 16).unwrap()).unwrap())
        .collect();
    assert!(
        text.starts_with("[Luya-Z] Admin pages crashed;") && text.ends_with(")#1"),
        "{text}"
    );
    wait("copy deleted", || !actions(&d, "sms.delete").is_empty()).await;
    assert_eq!(actions(&d, "sms.delete")[0]["params"]["ids"], "41");
    assert!(
        fs::read_to_string(a.path("sms-log"))
            .unwrap()
            .contains("\t1\tweb-crash\tsent")
    );
    wait("v6 off", || v6() == "1\n").await;

    // taken back: a new alert stays for the guard
    put(&root, crate::engines::guard_duties::FILE, "");
    tokio::time::sleep(Duration::from_millis(50)).await;
    a.add("ui-crash", "x", luyaz_api::alerts::now(), 4001)
        .unwrap();
    put(&root, "/proc/sys/net/ipv6/conf/br-lan/disable_ipv6", "0\n");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(a.read_u64("sms-done"), 1);
    assert_eq!(actions(&d, "vendor.call").len(), 1);
    assert_eq!(v6(), "0\n");
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn no_writes_no_duties() {
    let root = std::env::temp_dir().join(format!("u60d-guardduties-nw-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    put(&root, crate::engines::guard_duties::FILE, "sms\nlanv6\n");
    let c = Compat::new(root.to_str().unwrap(), Sources::none());
    assert!(!crate::engines::guard_duties::owns(&c, "sms"));
    let c = c.with_writes(true);
    assert!(
        crate::engines::guard_duties::owns(&c, "sms")
            && crate::engines::guard_duties::owns(&c, "lanv6")
    );
    assert!(!crate::engines::guard_duties::owns(&c, "core-x"));
    let _ = fs::remove_dir_all(&root);
}

/// The duties start on their own, with no listener of theirs (M6).
#[tokio::test]
async fn duties_run_without_the_listener() {
    let root = std::env::temp_dir().join(format!("u60d-guardduties-nl-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    put(&root, "/proc/sys/net/ipv6/conf/br-lan/disable_ipv6", "0\n");
    put(&root, "/data/luyaz/scripts/lan-ipv6-off", "");
    put(
        &root,
        crate::engines::guard_duties::FILE,
        &luyaz_api::handover::text(&["lanv6"]),
    );
    let c = Compat::new(root.to_str().unwrap(), Sources::none())
        .with_writes(true)
        .with_clock(1000);
    crate::api::start(c);
    let v6 =
        || fs::read_to_string(root.join("proc/sys/net/ipv6/conf/br-lan/disable_ipv6")).unwrap();
    wait("v6 off", || v6() == "1\n").await;
    let _ = fs::remove_dir_all(&root);
}
