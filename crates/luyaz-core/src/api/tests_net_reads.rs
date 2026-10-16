//! Tests for net_reads.rs (8d-1b).

use super::net_reads::*;
use super::*;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

/// As the modem answers with echo on: internet dual stack (one line per family), then IMS.
/// Documentation addresses.
const CGCONTRDP: &str = "AT+CGCONTRDP\r\r\n\
+CGCONTRDP: 1,5,\"cmnet.mnc000.mcc460.gprs\",\"198.51.100.23.255.255.255.0\",\"198.51.100.1\",\"192.0.2.53\",\"192.0.2.54\",\"0.0.0.0\",\"0.0.0.0\",0\r\n\
+CGCONTRDP: 1,5,\"cmnet.mnc000.mcc460.gprs\",\"32.1.13.184.0.0.0.0.0.0.0.0.0.0.0.1.255.255.255.255.255.255.255.255.0.0.0.0.0.0.0.0\",\"\",\"32.1.13.184.0.0.0.0.0.0.0.0.0.0.0.83\",\"\",\"\",\"\",0\r\n\
+CGCONTRDP: 2,6,\"IMS\",\"32.1.13.184.0.0.0.0.0.0.0.0.0.0.0.2.255.255.255.255.255.255.255.255.0.0.0.0.0.0.0.0\"\r\n\
\r\nOK\r\n";

#[test]
fn cgcontrdp_records_in_order() {
    assert_eq!(
        parse_cgcontrdp(CGCONTRDP),
        vec![
            (1, 5, "cmnet.mnc000.mcc460.gprs".to_string()),
            (1, 5, "cmnet.mnc000.mcc460.gprs".to_string()),
            (2, 6, "IMS".to_string()),
        ]
    );
    // unquoted APN, junk and a bare OK
    assert_eq!(
        parse_cgcontrdp("+CGCONTRDP: 3,7,internet,10.0.0.1\n+CGCONTRDP: x,1,\"a\"\nOK\n"),
        vec![(3, 7, "internet".to_string())]
    );
    assert!(parse_cgcontrdp("OK\r\n").is_empty());
}

#[test]
fn cgeqosrdp_default_measured_and_short() {
    // default bearer: no line
    assert_eq!(
        parse_cgeqosrdp("AT+CGEQOSRDP=1\r\r\nOK\r\n"),
        Default::default()
    );
    let q = parse_cgeqosrdp("AT+CGEQOSRDP=3\r\n+CGEQOSRDP: 3,1,128,128,256,256\r\nOK\r\n");
    assert_eq!(
        (q.qci, q.dl_gbr, q.ul_gbr, q.dl_mbr, q.ul_mbr),
        (Some(1), Some(128), Some(128), Some(256), Some(256))
    );
    let q = parse_cgeqosrdp("+CGEQOSRDP: 1,9\r\nOK\r\n");
    assert_eq!((q.qci, q.dl_gbr, q.ul_mbr), (Some(9), None, None));
}

#[test]
fn apn_heuristic() {
    assert_eq!(infer_5qi("IMS"), 5);
    assert_eq!(infer_5qi("ims.mnc000.mcc460.gprs"), 5);
    assert_eq!(infer_5qi("cmnet"), 9);
    assert_eq!(infer_5qi(""), 9);
}

type Sent = Arc<Mutex<Vec<String>>>;

fn fake_at(
    sent: Sent,
    delay: Duration,
) -> impl Fn(String) -> std::pin::Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
    move |cmd: String| {
        sent.lock().unwrap().push(cmd.clone());
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(match cmd.as_str() {
                "AT+CGCONTRDP" => CGCONTRDP.to_string(),
                "AT+CGEQOSRDP=2" => "+CGEQOSRDP: 2,5\r\nOK\r\n".to_string(),
                _ => "OK\r\n".to_string(),
            })
        })
    }
}

fn fake_uci(key: String) -> std::future::Ready<Option<String>> {
    std::future::ready((key == "zwrt_data_tmp.wwaniface1.qci").then(|| "6\n".to_string()))
}

#[tokio::test]
async fn qos_assembles_the_agents_table() {
    let sent: Sent = Default::default();
    let (st, body) = qos_reply(
        fake_at(sent.clone(), Duration::ZERO),
        fake_uci,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(st, 200, "{body}");
    assert_eq!(body["ok"], true);
    let d = &body["data"];
    let ctx = d["contexts"].as_array().unwrap();
    // the agent's rows, one per record; the modem is asked once per cid
    assert_eq!(ctx.len(), 3);
    assert_eq!(
        *sent.lock().unwrap(),
        ["AT+CGCONTRDP", "AT+CGEQOSRDP=1", "AT+CGEQOSRDP=2"]
    );
    assert_eq!(
        ctx[0],
        json!({"cid": 1, "bearer_id": 5, "apn": "cmnet.mnc000.mcc460.gprs", "qci": null, "qci_device": 6,
            "qci_inferred": 9, "dl_gbr_kbps": null, "ul_gbr_kbps": null, "dl_mbr_kbps": null, "ul_mbr_kbps": null})
    );
    assert_eq!(ctx[1], ctx[0]);
    assert_eq!(
        (
            ctx[2]["qci"].clone(),
            ctx[2]["qci_device"].clone(),
            ctx[2]["qci_inferred"].clone()
        ),
        (json!(5), Value::Null, json!(5))
    );
    assert!(
        d["raw_cgcontrdp"]
            .as_str()
            .unwrap()
            .starts_with("AT+CGCONTRDP")
    );
    assert!(d["raw_cgcontrdp"].as_str().unwrap().ends_with("OK"));
    assert!(d["note"].as_str().unwrap().contains("qci_device"));
}

#[tokio::test]
async fn qos_fails_like_the_agent_without_cgcontrdp() {
    let at = |_c: String| std::future::ready(Err::<String, _>("AT port busy".to_string()));
    let (st, body) = qos_reply(at, fake_uci, Duration::from_secs(5)).await;
    assert_eq!(st, 503);
    assert_eq!(
        body,
        json!({"ok": false, "error": "CGCONTRDP failed: AT port busy"})
    );
}

#[tokio::test(start_paused = true)]
async fn qos_slow_modem_keeps_to_the_budget() {
    // each answer takes 4 s, the budget is 6 s: CGCONTRDP fits, the first probe is cut, the rest skipped
    let sent: Sent = Default::default();
    let t = tokio::time::Instant::now();
    let (st, body) = qos_reply(
        fake_at(sent.clone(), Duration::from_secs(4)),
        fake_uci,
        Duration::from_secs(6),
    )
    .await;
    assert_eq!(st, 200, "{body}");
    assert!(t.elapsed() <= Duration::from_secs(6));
    assert_eq!(*sent.lock().unwrap(), ["AT+CGCONTRDP", "AT+CGEQOSRDP=1"]);
    let ctx = body["data"]["contexts"].as_array().unwrap();
    assert_eq!(ctx.len(), 3);
    assert_eq!(ctx[2]["qci"], Value::Null);
    assert_eq!(ctx[0]["qci_device"], 6);
    // slower than the whole budget: the agent's 503
    let (st, body) = qos_reply(
        fake_at(Default::default(), Duration::from_secs(9)),
        fake_uci,
        Duration::from_secs(6),
    )
    .await;
    assert_eq!(
        (st, body["error"].as_str().unwrap()),
        (503, "CGCONTRDP failed: timed out")
    );
}

/// luci-rpc as the device answers it (made-up MACs, documentation addresses).
fn luci(o: &'static str, m: &'static str, a: Value) -> Result<Value, String> {
    match (o, m) {
        ("luci-rpc", "getHostHints") => Ok(json!({
            "02:00:00:00:00:01": {"name": "phone", "ipaddrs": ["192.0.2.100"], "ip6addrs": []},
            "02:00:00:00:00:02": {"ipaddrs": ["192.0.2.101"]}})),
        ("luci-rpc", "getDHCPLeases") if a == json!({"family": 4}) => Ok(json!({"dhcp_leases": [
            {"expires": 43000, "hostname": "phone", "macaddr": "02:00:00:00:00:01", "ipaddr": "192.0.2.100"}]})),
        _ => Err(format!("{o} {m}: not faked")),
    }
}

fn station() -> luyaz_api::netinfo::Station {
    luyaz_api::netinfo::Station {
        mac: "02:00:00:00:00:01".into(),
        iface: "wlan2".into(),
        signal: Some(-60),
        band: "5 GHz",
        channel: Some(149),
        width_mhz: Some(80),
        wifi_gen: Some(6),
        link_down_mbps: Some(1201),
        link_up_mbps: Some(864),
        connected_secs: 3600,
        ..Default::default()
    }
}

#[test]
fn clients_data_keeps_the_agents_failure_shape() {
    let d = clients_data(
        luci("luci-rpc", "getHostHints", json!({})),
        luci("luci-rpc", "getDHCPLeases", json!({"family": 4})),
        &[station()],
    );
    assert_eq!(d["hosts"]["02:00:00:00:00:01"]["name"], "phone");
    assert_eq!(d["dhcp_leases"][0]["macaddr"], "02:00:00:00:00:01");
    assert_eq!(
        d["wifi"],
        json!([{"mac": "02:00:00:00:00:01", "iface": "wlan2", "band": "5 GHz", "channel": 149, "width_mhz": 80,
            "wifi_gen": 6, "link_down_mbps": 1201, "link_up_mbps": 864, "signal": -60, "signal_tier": "good",
            "connected_secs": 3600}])
    );
    // failures: hosts null, dhcp_leases left out, wifi still an array
    let d = clients_data(Err("x".into()), Err("y".into()), &[]);
    assert_eq!(d, json!({"hosts": null, "wifi": []}));
    let d = clients_data(Ok(json!({})), Ok(json!({"other": 1})), &[]);
    assert!(d.get("dhcp_leases").is_none());
}

#[tokio::test]
async fn clients_route_reads_luci_and_stations() {
    let src = Sources {
        ubus: Arc::new(|o, m, a| Box::pin(async move { luci(o, m, a) })),
        stations: Arc::new(|| vec![station()]),
        ..Sources::none()
    };
    let root = std::env::temp_dir().join(format!("u60d-netreads-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let c = Compat::new(root.to_str().unwrap(), src);
    let resp = clients(axum::extract::State(c)).await;
    assert_eq!(resp.status(), 200);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["dhcp_leases"][0]["hostname"], "phone");
    assert_eq!(body["data"]["wifi"][0]["band"], "5 GHz");
    assert_eq!(
        body["data"]["hosts"]["02:00:00:00:00:02"]["ipaddrs"][0],
        "192.0.2.101"
    );
}

#[tokio::test]
async fn qos_touches_no_at_unless_the_radio_is_online() {
    let root = std::env::temp_dir().join(format!("u60d-netreads-lpm-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for mode in [Some("LOW_POWER"), None] {
        let src = Sources {
            ubus: Arc::new(move |o, m, _| {
                let r = match (o, m, mode) {
                    ("uci", "get", Some(v)) => Ok(json!({"value": v})),
                    _ => Err("no ubus".to_string()),
                };
                Box::pin(async move { r })
            }),
            ..Sources::none()
        };
        let c = Compat::new(root.to_str().unwrap(), src);
        let resp = qos(axum::extract::State(c)).await;
        assert_eq!(resp.status(), 503, "{mode:?}");
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "CGCONTRDP failed: the radio is not online", "{mode:?}");
    }
}
