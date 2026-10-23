//! LAN clients (luci-rpc hints + DHCP leases + iw stations) and the bearer table (AT) moved from the agent.
//!
//! u60-web.md 8d-1b: the agent's reads luyaz-web still used, served here at the agent's path with
//! the agent's reply shape (`{ok, data}`), so luyaz-web only changes where it asks. On demand,
//! off the collection tick (spawn_blocking / a timeout), never cached across requests unless said.

use std::{future::Future, time::Duration};

use axum::{extract::State, response::Response};
use serde_json::{Map, Value, json};
use tokio::time::{Instant, timeout};

use super::{Compat, reply};

/// One ubus read (the collector has its own limit; this is the backstop).
const UBUS_WAIT: Duration = Duration::from_secs(8);
/// `iw` runs through `sh` with no deadline of its own.
const STATIONS_WAIT: Duration = Duration::from_secs(5);
/// The whole bearer table: luyaz-web gives up at 30 s, and `at::send` alone may take 25 s.
const QOS_BUDGET: Duration = Duration::from_secs(20);
const UCI_WAIT: Duration = Duration::from_secs(2);

/// GET /api/network/clients
pub async fn clients(State(c): State<Compat>) -> Response {
    let ubus = |m: &'static str, a: Value| {
        let f = (c.src.ubus)("luci-rpc", m, a);
        async move {
            timeout(UBUS_WAIT, f)
                .await
                .unwrap_or_else(|_| Err(format!("luci-rpc {m}: timed out")))
        }
    };
    let stations = c.src.stations.clone();
    let (hints, leases, wifi) = tokio::join!(
        ubus("getHostHints", json!({})),
        ubus("getDHCPLeases", json!({"family": 4})),
        timeout(
            STATIONS_WAIT,
            tokio::task::spawn_blocking(move || stations())
        ),
    );
    let wifi = match wifi {
        Ok(Ok(list)) => list,
        _ => Vec::new(),
    };
    reply(
        200,
        json!({"ok": true, "data": clients_data(hints, leases, &wifi)}),
    )
}

/// The agent's `network_clients` body: `hosts` is null when the hints fail and `dhcp_leases` is
/// left out (not `[]`) when the leases fail; luyaz-web takes the missing array as the failure.
pub(super) fn clients_data(
    hints: Result<Value, String>,
    leases: Result<Value, String>,
    stations: &[luyaz_api::netinfo::Station],
) -> Value {
    let mut d = Map::new();
    d.insert("hosts".into(), hints.unwrap_or(Value::Null));
    if let Some(l) = leases.ok().and_then(|v| v.get("dhcp_leases").cloned()) {
        d.insert("dhcp_leases".into(), l);
    }
    let wifi = stations
        .iter()
        .map(luyaz_api::netinfo::station_wifi_json)
        .collect();
    d.insert("wifi".into(), Value::Array(wifi));
    Value::Object(d)
}

/// One bearer table read at a time: a second caller is told so instead of queueing on the AT port.
static QOS_RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// GET /api/network/qos. Only with the radio ONLINE: in low power an AT call on B31 blocked for
/// over 30 s (10-05), and an AT command left stuck makes `crate::at` refuse every later one on
/// that port, the register guard's COPS=0 included, until it returns.
pub async fn qos(State(c): State<Compat>) -> Response {
    let Ok(_one) = QOS_RUNNING.try_lock() else {
        return reply(
            503,
            json!({"ok": false, "error": "a QoS read is already running"}),
        );
    };
    let (_, m) = super::sys_reads::modem_reply(&c).await;
    if m["data"]["operate_mode"] != "ONLINE" {
        return reply(
            503,
            json!({"ok": false, "error": "CGCONTRDP failed: the radio is not online"}),
        );
    }
    let (status, body) = qos_reply(at_read, uci_read, QOS_BUDGET).await;
    reply(status, body)
}

/// The two reads `qos_reply` sends, mapped onto `crate::at`'s fixed list (cid 1..=24; anything
/// else is refused, never sent).
async fn at_read(text: String) -> Result<String, String> {
    let cmd = crate::device_profile::at::qos_command(&text)
        .ok_or_else(|| format!("{text}: not in u60d's AT command list"))?;
    crate::at::send(cmd).await
}

/// `uci get` through the CLI like the agent: `zwrt_data_tmp` is runtime state.
async fn uci_read(key: String) -> Option<String> {
    let out = crate::command::run(&crate::state::uci_bin(), ["-q", "get", &key], UCI_WAIT)
        .await
        .ok()?;
    Some(String::from_utf8_lossy(&out).trim().to_string())
}

/// The agent's `network_qos` with the AT and uci reads passed in. Every AT command shares
/// `budget`; once it is spent the remaining contexts keep null QoS instead of failing the reply.
pub(super) async fn qos_reply<A, AF, U, UF>(at: A, uci: U, budget: Duration) -> (u16, Value)
where
    A: Fn(String) -> AF,
    AF: Future<Output = Result<String, String>>,
    U: Fn(String) -> UF,
    UF: Future<Output = Option<String>>,
{
    let deadline = Instant::now() + budget;
    let left = || deadline.saturating_duration_since(Instant::now());
    let cgcontrdp = match timeout(left(), at("AT+CGCONTRDP".into())).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            return (
                503,
                json!({"ok": false, "error": format!("CGCONTRDP failed: {e}")}),
            );
        }
        Err(_) => {
            return (
                503,
                json!({"ok": false, "error": "CGCONTRDP failed: timed out"}),
            );
        }
    };

    // a dual-stack context is listed once per family: ask the modem once per cid
    let mut seen: Vec<(u32, Option<u32>, Qos)> = Vec::new();
    let mut contexts = Vec::new();
    for (cid, bearer_id, apn) in parse_cgcontrdp(&cgcontrdp) {
        let (qci_device, q) = match seen.iter().find(|(c, ..)| *c == cid) {
            Some((_, d, q)) => (*d, *q),
            None => {
                let d = uci(format!("zwrt_data_tmp.wwaniface{cid}.qci"))
                    .await
                    .and_then(|s| s.trim().parse().ok());
                // an ERROR answer or no `+CGEQOSRDP:` line is the default bearer
                let q = match left() {
                    w if w.is_zero() => Qos::default(),
                    w => match timeout(w, at(format!("AT+CGEQOSRDP={cid}"))).await {
                        Ok(Ok(resp)) => parse_cgeqosrdp(&resp),
                        _ => Qos::default(),
                    },
                };
                seen.push((cid, d, q));
                (d, q)
            }
        };
        contexts.push(json!({
            "cid": cid,
            "bearer_id": bearer_id,
            "apn": apn,
            "qci": q.qci,
            "qci_device": qci_device,
            "qci_inferred": infer_5qi(&apn),
            "dl_gbr_kbps": q.dl_gbr,
            "ul_gbr_kbps": q.ul_gbr,
            "dl_mbr_kbps": q.dl_mbr,
            "ul_mbr_kbps": q.ul_mbr,
        }));
    }
    (
        200,
        json!({
            "ok": true,
            "data": {
                "contexts": contexts,
                "raw_cgcontrdp": cgcontrdp.trim(),
                "note": "QCI comes from the modem's qci_device (the number the stock About screen shows); a dedicated bearer can add a 5QI measured over AT; with neither, it is guessed from the APN.",
            }
        }),
    )
}

// Existing response display decoders now live with the reviewed command dialects.
pub(super) use crate::device_profile::at::{Qos, parse_cgcontrdp, parse_cgeqosrdp};

/// IMS-bearing APN → 5 (signalling), anything else → 9 (best effort). Labelled as inferred.
pub(super) fn infer_5qi(apn: &str) -> u8 {
    if apn.to_ascii_lowercase().contains("ims") {
        5
    } else {
        9
    }
}
