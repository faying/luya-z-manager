//! `luyaz-web mock`: a stand-in for u60d's `/v2/events` and `/control` for local trials and tests.
//! Replays a recorded `/v2/events` stream (snapshot first, then the recorded blocks with their
//! original spacing, looping), keeps a hub of the latest blocks like u60d does (a new connection
//! gets a snapshot of now), and answers `cellular.set {roaming}` by changing
//! `interfaces.cellular.roam_enable` after a short delay, the way the readback arrives on the device.
//!
//! Test hooks: `GET /mock/stats` (open /v2/events connections, every `/control` op_id), `POST /mock/drop` (close them all),
//! `POST /mock/epoch` (new epoch + drop), `POST /mock/control?fail=true|noreadback=true` (next write),
//! `POST /mock/alerts?unread=N` (how many of the two fake alerts are unread; the frame reads them
//! from `/v2/api/alerts` and `/v2/api/public/status`, as from u60d's screen API).
//!
//! SMS: the recording's `sms` / `sms_list` blocks (real messages) are never served: the mock keeps
//! a made-up inbox, publishes `sms` / `sms_list` from it, and answers `sms.list_after`,
//! `sms.mark_read`, `sms.delete` (NV rows only, like the firmware: SIM rows stay), `sms.db_delete`
//! and `sms.send_raw` (recorded, nothing sent; a sent row appears). `POST /mock/sms?count=N`
//! replaces the inbox with N made-up rows (`count=0`: the default six); `GET /mock/stats` has
//! `sms_ops` (actions in order), `sms_sends` (the send params) and `sms_ids`.
//!
//! Change log (/changes): a made-up `journal.list` (the old web's mock rows, fixtures/ops.ts; the
//! network-mode row `web-20` is the one its "last changed" link opens) and `network.set_mode` as a
//! transaction: `op.active` (applying → verifying, 1 s) then `op.last` 2 s later, ended as
//! `POST /mock/txn?end=confirmed|rolled_back|live` says (live: never ends), with a row prepended
//! and the older network-mode rows turned 「之后又改过」. `POST /mock/journal?mode=full|empty|down|old`
//! (full also resets the rows); `GET /mock/stats` has `txns` (every network.set_mode body).
//!
//! USB (/usb): `usb.set {mode}` puts the mode into `qos.usb_mode` and `vendor.call` of
//! `zwrt_bsp.powerbank set {state}` (the only vendor call the mock knows; others are refused like
//! u60d's allow-list) into `powerbank.state`, each after a short delay (`noreadback`: accepted,
//! nothing changes). `POST /mock/usb?cc=0|1` plugs or unplugs (`typec.cc_attch_state`).
//! `GET /mock/stats` has `usb_ops` (every usb.set / vendor.call body).
//!
//! Band lock (/bandlock): `band.set_lte {bands}`, `band.set_nr {sa, nsa}` and `band.reset {}` (or
//! `{lte, sa, nsa}`, the undo) as transactions like network.set_mode (same `/mock/txn?end=`), items
//! `band.lte` / `band.nr` / `band.all`. Confirmed: the locks go into the `signal` block's
//! `lte_bands` / `sa_bands` / `nsa_bands` (kept over the recording's, which re-publishes them);
//! reset = the `*_supported_bands`. `POST /mock/bands?lte=&sa=&nsa=` sets the locks directly (empty =
//! back to the recording's). Any transaction write while `op.active` is set gets u60d's 409 busy.
//! Bodies go into `txns` (with `action`).
//!
//! Cell lock (/router/celllock): `cell.lock_nr {pci, arfcn, band}` / `cell.lock_lte {pci, earfcn}`
//! (or `{off: true}`, the undo) the same way, items `cell.nr` / `cell.lte`; confirmed, the lock goes
//! into the `signal` block's `nr_cell_lock` ("pci,arfcn,band") / `lte_cell_lock` ("pci,earfcn"); a
//! confirmed `band.reset` clears both. `POST /mock/cells?nr=&lte=` sets them raw.
//!
//! APN (/router/apn): a made-up `apn` block (auto mode; two manual profiles, two carrier ones with
//! the ids of the typed mock agent's `apn-candidates`). `apn.set_mode`, `apn.enable` / `apn.modify`
//! of what changes the connection (manual mode; the profile in use) run as transactions on item
//! `apn`; enable in auto mode, modify of another profile, `apn.add`, `apn.delete` (400 for the one
//! in use) and `apn.set_pdp_type` are plain writes (no `op` in the reply), applied after 1 s.
//! Bodies go into `apn_ops`. `POST /mock/apn` puts the made-up block back.
//!
//! Network (/router/mobile-network, /router/network-mode): a confirmed network.set_mode also puts
//! the mode into the `signal` block's `net_select`; `cellular.set {enabled}` turns mobile data on/off
//! (`interfaces.cellular.enable` / `connect_status`, body into `apn_ops`); `device.reboot` is counted
//! (`reboots`). `POST /mock/net` puts mode, data and roaming back to the recording's.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};
use tokio_stream::wrappers::BroadcastStream;

use crate::sse::Parser;

struct Mock {
    epoch: Mutex<String>,
    blocks: Mutex<BTreeMap<String, Value>>,
    seq: AtomicU64,
    roam: Mutex<Option<i64>>,
    next_write: Mutex<(bool, bool)>,
    tx: broadcast::Sender<String>,
    drop_gen: watch::Sender<u64>,
    open: AtomicUsize,
    total: AtomicU64,
    /// Bytes written to /v2/events since start (upstream side of the traffic table).
    sent: AtomicU64,
    /// Unread fake alerts (0..=2).
    unread: AtomicU64,
    /// `exec_age_ms` put in every heartbeat instead of the recording's (`u64::MAX` = the recording's).
    exec_age: AtomicU64,
    /// `op.notice_ack` writes received.
    notice_acks: AtomicU64,
    /// The `op_id` of every `/control` write, in order ("" when it had none).
    op_ids: Mutex<Vec<String>>,
    /// The made-up inbox (never the recording's).
    sms: Mutex<Vec<SmsRow>>,
    /// `sms.*` actions received, in order (reads too).
    sms_ops: Mutex<Vec<String>>,
    /// `sms.send_raw` params, in order.
    sms_sends: Mutex<Vec<Value>>,
    /// The change log, newest first, as u60d decorates it.
    journal: Mutex<Vec<Value>>,
    /// `full` / `empty` / `down` / `old` (no journal.list).
    journal_mode: Mutex<String>,
    /// How the next network.set_mode ends: `confirmed` / `rolled_back` / `live`.
    txn_end: Mutex<String>,
    /// Every network.set_mode body.
    txns: Mutex<Vec<Value>>,
    /// The network mode the made-up log ends at.
    mode: Mutex<String>,
    /// Every usb.set / vendor.call body.
    usb_ops: Mutex<Vec<Value>>,
    /// Band locks (lte, sa, nsa) over the recording's; None = the recording's.
    bands: Mutex<Option<(String, String, String)>>,
    /// Cell locks as the signal block's raw fields: (`nr_cell_lock`, `lte_cell_lock`).
    cells: Mutex<(String, String)>,
    /// The sim block's `dual` (global edition); None = single-slot (the recording's).
    dual: Mutex<Option<Value>>,
    /// Every apn.* body (with the password as sent: the e2e checks it went exactly once).
    apn_ops: Mutex<Vec<Value>>,
    /// `net_select` over the recording's after a confirmed network.set_mode (None = the recording's).
    net_select: Mutex<Option<String>>,
    /// Mobile data (`interfaces.cellular.enable` / `connect_status`) over the recording's.
    data_on: Mutex<Option<bool>>,
    /// `device.reboot` writes received.
    reboots: AtomicU64,
    /// Every batch-4 page write body (Wi-Fi, LAN, firewall, VPN, QoS, domain filter), in order.
    page_ops: Mutex<Vec<Value>>,
}

#[derive(Clone)]
struct SmsRow {
    id: u64,
    /// As the firmware gives it: plain, or UCS-2 hex (one row, to exercise the decode).
    number: String,
    /// UCS-2 hex.
    content: String,
    /// "YY,MM,DD,HH,MM,SS,+TZ"
    date: String,
    tag: &'static str,
    /// 1 = NV, 0 = SIM.
    store: u8,
}

fn hex(t: &str) -> String {
    t.encode_utf16().map(|c| format!("{c:04X}")).collect()
}

fn unhex(h: &str) -> String {
    let u: Vec<u16> = h
        .as_bytes()
        .chunks(4)
        .filter_map(|c| u16::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
        .collect();
    String::from_utf16_lossy(&u)
}

/// Six made-up messages: unread on both stores, read, sent, send failed, a sender name in hex.
fn default_inbox() -> Vec<SmsRow> {
    let r = |id, number: &str, text: &str, date: &str, tag, store| SmsRow {
        id,
        number: number.into(),
        content: hex(text),
        date: date.into(),
        tag,
        store,
    };
    vec![
        r(31, "10086", "您的验证码是 246810，5 分钟内有效。", "26,10,12,09,30,05,+32", "1", 1),
        r(30, "+886900000104", "Hello from the SIM card.\nSecond line.", "26,10,11,21,02,44,+32", "1", 0),
        r(29, "95588", "账户变动提醒：测试数据，不是真的短信。", "26,10,10,08,15,00,+32", "0", 1),
        r(28, "+886900000104", "On my way", "26,10,09,18,40,12,+32", "2", 1),
        r(27, &hex("CMHK"), "Roaming data pack activated.", "26,10,08,07,00,00,+32", "0", 0),
        r(26, "10010", "test", "26,10,07,12,00,00,+32", "3", 1),
    ]
}

fn many(n: u64) -> Vec<SmsRow> {
    (1..=n)
        .rev()
        .map(|id| SmsRow {
            id,
            number: format!("+8869120{:05}", id % 100_000),
            content: hex(&format!(
                "第 {id} 条测试短信：这是一段比较长的内容，用来量一屏的大小。Message number {id}, a longer text to weigh the page."
            )),
            date: format!("26,10,{:02},{:02},{:02},00,+32", 1 + id % 28, id % 24, id % 60),
            tag: if id % 7 == 0 { "1" } else { "0" },
            store: if id % 5 == 0 { 0 } else { 1 },
        })
        .collect()
}

/// (delay before it in ms, event, data) from a recording.
fn load(raw: &str, speed: f64) -> (Value, Vec<(u64, String, Value)>) {
    let mut p = Parser::default();
    let frames = p.feed(raw.as_bytes());
    let mut snapshot = Value::Null;
    let mut out = Vec::new();
    let mut last_at: Option<u64> = None;
    for f in frames {
        let Ok(v) = serde_json::from_str::<Value>(&f.data) else {
            continue;
        };
        if f.event == "snapshot" {
            snapshot = v;
            continue;
        }
        let at = v["observed_at"].as_u64().or(last_at);
        let delay = match (last_at, at) {
            (Some(a), Some(b)) if b > a => ((b - a) as f64 * 1000.0 / speed) as u64,
            _ => 0,
        };
        if at.is_some() {
            last_at = at;
        }
        out.push((delay, f.event, v));
    }
    (snapshot, out)
}

impl Mock {
    /// `sms` and `sms_list` from the made-up inbox (u60d's shapes).
    fn publish_sms(&self) {
        let rows = self.sms.lock().unwrap().clone();
        let unread = rows.iter().filter(|r| r.tag == "1").count();
        let max_id = rows.iter().map(|r| r.id).max().unwrap_or(0);
        self.publish(
            "sms",
            json!({"unread": unread, "max_id": max_id, "count": rows.len()}),
            0,
            false,
        );
        let mut list = Vec::new();
        for store in [1, 0] {
            let mut r: Vec<&SmsRow> = rows.iter().filter(|r| r.store == store).collect();
            r.sort_by_key(|r| std::cmp::Reverse(r.id));
            for r in r.into_iter().take(8) {
                let d: Vec<&str> = r.date.split(',').collect();
                list.push(json!({"id": r.id, "num": r.number, "text": unhex(&r.content),
                    "date": format!("{}-{} {}:{}", d[1], d[2], d[3], d[4]),
                    "unread": i64::from(r.tag == "1")}));
            }
        }
        self.publish("sms_list", json!({"list": list}), 0, false);
    }

    fn frame(&self, event: &str, v: &Value) -> String {
        format!("event: {event}\ndata: {v}\n\n")
    }

    fn overlay_roam(&self, name: &str, data: &mut Value) {
        // recordings from before u60d had `net_select_mode` (u60-web.md W9)
        if name == "signal"
            && let Some(o) = data.as_object_mut()
        {
            o.entry("net_select_mode")
                .or_insert_with(|| json!("auto_select"));
        }
        if name == "signal"
            && let Some((lte, sa, nsa)) = self.bands.lock().unwrap().clone()
            && let Some(o) = data.as_object_mut()
        {
            o.insert("lte_bands".into(), json!(lte));
            o.insert("sa_bands".into(), json!(sa));
            o.insert("nsa_bands".into(), json!(nsa));
        }
        if name == "signal"
            && let Some(ns) = self.net_select.lock().unwrap().clone()
            && let Some(o) = data.as_object_mut()
        {
            o.insert("net_select".into(), json!(ns));
        }
        if name == "interfaces"
            && let Some(on) = *self.data_on.lock().unwrap()
            && let Some(c) = data.get_mut("cellular").and_then(Value::as_object_mut)
        {
            c.insert("enable".into(), json!(i64::from(on)));
            c.insert("connect_status".into(), json!(if on { "ipv4_ipv6_connected" } else { "disconnected" }));
        }
        if name == "signal"
            && let Some(o) = data.as_object_mut()
        {
            let (nr, lte) = self.cells.lock().unwrap().clone();
            o.insert("nr_cell_lock".into(), json!(nr));
            o.insert("lte_cell_lock".into(), json!(lte));
        }
        if name == "sim"
            && let Some(o) = data.as_object_mut()
        {
            match self.dual.lock().unwrap().clone() {
                Some(d) => o.insert("dual".into(), d),
                None => o.remove("dual"),
            };
        }
        if name == "interfaces"
            && let Some(r) = *self.roam.lock().unwrap()
            && let Some(c) = data.get_mut("cellular").and_then(Value::as_object_mut)
        {
            c.insert("roam_enable".into(), json!(r));
        }
    }

    /// Publish one block like u60d's hub: new seq + revision, keep it, send it.
    fn publish(&self, name: &str, mut data: Value, observed_at: u64, stale: bool) {
        self.overlay_roam(name, &mut data);
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let body =
            json!({"revision": seq, "observed_at": observed_at, "stale": stale, "data": data});
        self.blocks
            .lock()
            .unwrap()
            .insert(name.to_owned(), body.clone());
        let epoch = self.epoch.lock().unwrap().clone();
        let msg = json!({"epoch": epoch, "seq": seq, "name": name, "revision": seq, "observed_at": observed_at, "stale": stale, "data": body["data"]});
        let _ = self.tx.send(self.frame("block", &msg));
    }

    fn heartbeat(&self, recorded_age: u64) {
        let age = match self.exec_age.load(Ordering::SeqCst) {
            u64::MAX => recorded_age,
            a => a,
        };
        let seq = self.seq.load(Ordering::SeqCst);
        let epoch = self.epoch.lock().unwrap().clone();
        let msg = json!({"epoch": epoch, "seq": seq, "blocks": {}, "exec_age_ms": age});
        let _ = self.tx.send(self.frame("heartbeat", &msg));
    }

    fn snapshot(&self) -> String {
        let blocks: serde_json::Map<String, Value> = self
            .blocks
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let msg = json!({"epoch": *self.epoch.lock().unwrap(), "seq": self.seq.load(Ordering::SeqCst), "blocks": blocks});
        self.frame("snapshot", &msg)
    }
}

async fn replay(m: Arc<Mock>, frames: Vec<(u64, String, Value)>) {
    if frames.is_empty() {
        return;
    }
    loop {
        for (delay, event, v) in &frames {
            tokio::time::sleep(Duration::from_millis(*delay)).await;
            match event.as_str() {
                "block" => {
                    let name = v["name"].as_str().unwrap_or_default();
                    // the recording's messages are real: never served (the made-up inbox is)
                    if name == "sms" || name == "sms_list" {
                        continue;
                    }
                    m.publish(
                        name,
                        v["data"].clone(),
                        v["observed_at"].as_u64().unwrap_or(0),
                        v["stale"].as_bool().unwrap_or(false),
                    );
                }
                "heartbeat" => m.heartbeat(v["exec_age_ms"].as_u64().unwrap_or(0)),
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

struct Open(Arc<Mock>);
impl Drop for Open {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn events(State(m): State<Arc<Mock>>) -> Response {
    m.open.fetch_add(1, Ordering::SeqCst);
    m.total.fetch_add(1, Ordering::SeqCst);
    let guard = Open(m.clone());
    let rx = m.tx.subscribe();
    let first = m.snapshot();
    let mut dropped = m.drop_gen.subscribe();
    dropped.borrow_and_update();
    let rest = BroadcastStream::new(rx).filter_map(|r| async move { r.ok() });
    let ended = async move {
        let _ = dropped.changed().await;
    };
    let counter = m.clone();
    let s = futures_util::stream::once(async move { first })
        .chain(rest)
        .take_until(ended)
        .map(move |s: String| {
            let _keep = &guard;
            counter.sent.fetch_add(s.len() as u64, Ordering::Relaxed);
            Ok::<_, std::convert::Infallible>(s)
        });
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(s))
        .unwrap()
}

async fn control(State(m): State<Arc<Mock>>, body: axum::Json<Value>) -> Response {
    let action = body["action"].as_str().unwrap_or_default().to_owned();
    if action.starts_with("sms.") {
        m.sms_ops.lock().unwrap().push(action.clone());
    }
    if action == "journal.list" {
        return journal_list(&m);
    }
    if action == "sms.list_after" {
        return axum::Json(json!({"ok": true, "action": action, "result": list_after(&m, &body["params"])}))
            .into_response();
    }
    if action == "op.receipt" {
        return receipt::control(&m, &body);
    }
    // the decision loop's approval (core api/approve.rs): started, as the core answers it
    if action == "loop.approve" {
        let id = body["params"]["proposal_id"].as_str().unwrap_or_default();
        return axum::Json(json!({"ok": true, "action": action, "result": {"op_id": format!("loop-{id}"), "started": true}}))
            .into_response();
    }
    m.op_ids
        .lock()
        .unwrap()
        .push(body["op_id"].as_str().unwrap_or_default().to_owned());
    let (fail, noreadback) = std::mem::take(&mut *m.next_write.lock().unwrap());
    if fail {
        let e = json!({"ok":false,"action":action,"error":{"code":"device_call_failed","message":"zwrt_data set_wwaniface: timeout"}});
        return (StatusCode::BAD_GATEWAY, axum::Json(e)).into_response();
    }
    if action == "op.notice_ack" {
        m.notice_acks.fetch_add(1, Ordering::SeqCst);
        let cur = m.blocks.lock().unwrap().get("op").cloned();
        if let Some(mut b) = cur
            && b["data"].get("notice").is_some()
        {
            b["data"].as_object_mut().unwrap().remove("notice");
            m.publish(
                "op",
                b["data"].clone(),
                b["observed_at"].as_u64().unwrap_or(0),
                false,
            );
        }
        return axum::Json(
            json!({"ok":true,"action":action,"result":{"notice":"rollback_on","acked":true}}),
        )
        .into_response();
    }
    if action.starts_with("sms.") {
        return sms_write(&m, &action, &body["params"], noreadback).await;
    }
    if matches!(
        action.as_str(),
        "network.set_mode" | "band.set_lte" | "band.set_nr" | "band.reset" | "cell.lock_lte" | "cell.lock_nr"
            | "sim.dual" | "apn.set_mode" | "apn.enable" | "apn.modify"
    ) && let Some(doing) = active_op(&m)
    {
        let e = json!({"error": {"code": "busy", "message": "another change is in progress"}, "doing": doing});
        return (StatusCode::CONFLICT, axum::Json(e)).into_response();
    }
    if action == "network.set_mode" {
        return txn(&m, &body);
    }
    if action.starts_with("band.") {
        return band_txn(&m, &body);
    }
    if action.starts_with("cell.") {
        return cell_txn(&m, &body);
    }
    if action == "sim.dual" {
        return dual_txn(&m, &body);
    }
    if action.starts_with("apn.") {
        return apn_write(&m, &body);
    }
    if let Some(r) = batch4_control(&m, &body, noreadback) {
        return r;
    }
    if action == "usb.set" || action == "vendor.call" {
        return usb_write(&m, &body, noreadback);
    }
    if action == "device.reboot" {
        m.reboots.fetch_add(1, Ordering::SeqCst);
        return axum::Json(json!({"ok":true,"action":action,"result":{"result":"success"}})).into_response();
    }
    if action != "cellular.set" {
        let e = json!({"ok":false,"action":action,"error":{"code":"unknown_action","message":"mock only knows cellular.set"}});
        return (StatusCode::BAD_REQUEST, axum::Json(e)).into_response();
    }
    let flag = |v: &Value| v.as_bool().or_else(|| v.as_i64().map(|n| n != 0));
    if let Some(on) = flag(&body["params"]["enabled"]) {
        m.apn_ops.lock().unwrap().push(body.0.clone());
        if !noreadback {
            let m = m.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                *m.data_on.lock().unwrap() = Some(on);
                let cur = m.blocks.lock().unwrap().get("interfaces").cloned();
                if let Some(b) = cur {
                    m.publish("interfaces", b["data"].clone(), b["observed_at"].as_u64().unwrap_or(0) + 1, false);
                }
            });
        }
        return axum::Json(json!({"ok":true,"action":action,"result":{"result":"success"}})).into_response();
    }
    let Some(on) = body["params"]["roaming"].as_bool() else {
        let e = json!({"ok":false,"action":action,"error":{"code":"invalid_parameter","message":"roaming"}});
        return (StatusCode::BAD_REQUEST, axum::Json(e)).into_response();
    };
    if !noreadback {
        let m = m.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            *m.roam.lock().unwrap() = Some(on as i64);
            let cur = m.blocks.lock().unwrap().get("interfaces").cloned();
            if let Some(b) = cur {
                m.publish(
                    "interfaces",
                    b["data"].clone(),
                    b["observed_at"].as_u64().unwrap_or(0) + 1,
                    false,
                );
            }
        });
    }
    axum::Json(json!({"ok":true,"action":action,"result":{"result":"success"}})).into_response()
}

/// `sms.list_after {after_id, limit}`: ascending ids above `after_id`, both stores, each row
/// with the store it came from (u60d's reply).
fn list_after(m: &Mock, p: &Value) -> Value {
    let after = p["after_id"].as_u64().unwrap_or(0);
    let limit = p["limit"].as_u64().unwrap_or(50).clamp(1, 50) as usize;
    let mut rows: Vec<SmsRow> = m
        .sms
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.id > after)
        .cloned()
        .collect();
    rows.sort_by_key(|r| r.id);
    let more = rows.len() > limit;
    let items: Vec<Value> = rows
        .iter()
        .take(limit)
        .map(|r| json!({"id": r.id, "number": r.number, "content": r.content, "date": r.date,
                        "tag": r.tag, "mem_store": r.store}))
        .collect();
    json!({"items": items, "has_more": more})
}

fn ids_of(p: &Value) -> Vec<u64> {
    p["ids"]
        .as_str()
        .unwrap_or("")
        .split(';')
        .filter_map(|x| x.trim().parse().ok())
        .collect()
}

/// The SMS writes on the made-up inbox. `noreadback`: accepted, nothing changes.
async fn sms_write(m: &Arc<Mock>, action: &str, p: &Value, noreadback: bool) -> Response {
    let ids = ids_of(p);
    let ok = |result: Value| {
        axum::Json(json!({"ok": true, "action": action, "result": result})).into_response()
    };
    match action {
        "sms.mark_read" => {
            if !noreadback {
                for r in m.sms.lock().unwrap().iter_mut() {
                    if ids.contains(&r.id) && r.tag == "1" {
                        r.tag = "0";
                    }
                }
                m.publish_sms();
            }
            ok(json!({"result": 3}))
        }
        "sms.delete" => {
            // the firmware deletes NV rows; SIM rows answer the same and stay
            if !noreadback {
                m.sms
                    .lock()
                    .unwrap()
                    .retain(|r| !(ids.contains(&r.id) && r.store == 1));
                m.publish_sms();
            }
            ok(json!({"result": 3}))
        }
        "sms.db_delete" => {
            if !noreadback {
                m.sms.lock().unwrap().retain(|r| !ids.contains(&r.id));
                m.publish_sms();
            }
            ok(json!({"deleted": ids.len()}))
        }
        "sms.send_raw" => {
            m.sms_sends.lock().unwrap().push(p.clone());
            // the firmware's command status takes a moment
            tokio::time::sleep(Duration::from_millis(1200)).await;
            if !noreadback {
                let mut rows = m.sms.lock().unwrap();
                let id = rows.iter().map(|r| r.id).max().unwrap_or(0) + 1;
                rows.push(SmsRow {
                    id,
                    number: p["number"].as_str().unwrap_or("").into(),
                    content: p["message_hex"].as_str().unwrap_or("").into(),
                    date: "26,10,12,10,00,00,+32".into(),
                    tag: "2",
                    store: 1,
                });
                drop(rows);
                m.publish_sms();
            }
            ok(json!({"sender": "host", "status": 3}))
        }
        _ => (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"ok": false, "action": action,
                "error": {"code": "unknown_action", "message": "mock has no such sms action"}})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct SetSms {
    #[serde(default)]
    count: u64,
}

/// The inbox: `count` made-up rows, or the default six (`count=0`).
async fn set_sms(State(m): State<Arc<Mock>>, Query(q): Query<SetSms>) -> StatusCode {
    *m.sms.lock().unwrap() = if q.count == 0 {
        default_inbox()
    } else {
        many(q.count.min(2000))
    };
    m.publish_sms();
    StatusCode::NO_CONTENT
}

async fn stats(State(m): State<Arc<Mock>>) -> axum::Json<Value> {
    axum::Json(json!({
        "connections": m.open.load(Ordering::SeqCst),
        "total": m.total.load(Ordering::SeqCst),
        "sent_bytes": m.sent.load(Ordering::Relaxed),
        "roam": *m.roam.lock().unwrap(),
        "notice_acks": m.notice_acks.load(Ordering::SeqCst),
        "op_ids": m.op_ids.lock().unwrap().clone(),
        "sms_ops": m.sms_ops.lock().unwrap().clone(),
        "sms_sends": m.sms_sends.lock().unwrap().clone(),
        "sms_ids": m.sms.lock().unwrap().iter().map(|r| r.id).collect::<Vec<_>>(),
        "txns": m.txns.lock().unwrap().clone(),
        "usb_ops": m.usb_ops.lock().unwrap().clone(),
        "bands": *m.bands.lock().unwrap(),
        "cells": m.cells.lock().unwrap().clone(),
        "apn_ops": m.apn_ops.lock().unwrap().clone(),
        "reboots": m.reboots.load(Ordering::SeqCst),
        "page_ops": m.page_ops.lock().unwrap().clone(),
        "receipts": receipt::log(),
    }))
}

fn alerts_data(unread: u64) -> Value {
    let ev = |seq: u64, kind: &str, zh: &str, en: &str, text: &str| {
        json!({"seq": seq, "time": null, "uptime": seq * 100, "kind": kind, "label": zh,
               "label_en": en, "text": text, "unread": seq + unread > 2})
    };
    json!({"events": [
        ev(2, "wifi_down", "Wi-Fi 掉了", "Wi-Fi dropped", "已自动恢复"),
        ev(1, "core_restart", "数据服务重启", "Data service restarted", ""),
    ], "unread": unread, "read": 2 - unread, "uptime_now": 300, "clock_set": false,
       "sms": {"configured": false, "number": null, "recent": []}})
}

/// `POST /mock/alerts-to-agent?on=true`: `/v2/api/alerts` answers "not mine", so the /alerts page
/// (5a) reads the fake agent's full summary (with the SMS part) instead of this short one.
static ALERTS_TO_AGENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

async fn screen_alerts(State(m): State<Arc<Mock>>) -> Response {
    if ALERTS_TO_AGENT.load(Ordering::SeqCst) {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"ok": false, "error": "not_in_compat_yet"})),
        )
            .into_response();
    }
    let mut d = alerts_data(m.unread.load(Ordering::SeqCst));
    poller::alerts_extra(&mut d);
    axum::Json(json!({"ok": true, "data": d})).into_response()
}

#[derive(serde::Deserialize)]
struct OnOff {
    on: bool,
}

async fn alerts_to_agent(Query(q): Query<OnOff>) -> StatusCode {
    ALERTS_TO_AGENT.store(q.on, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}

async fn screen_status(State(m): State<Arc<Mock>>) -> axum::Json<Value> {
    let mut data = json!({
        "alerts": {"unread": m.unread.load(Ordering::SeqCst)},
        "services": {"chill": {"state": "running", "reason": null, "on": true}},
    });
    home::public_extra(&mut data);
    poller::status_extra(&mut data);
    axum::Json(json!({"ok": true, "data": data}))
}

#[derive(serde::Deserialize)]
struct SetAlerts {
    unread: u64,
}

async fn set_alerts(State(m): State<Arc<Mock>>, Query(q): Query<SetAlerts>) -> StatusCode {
    m.unread.store(q.unread.min(2), Ordering::SeqCst);
    StatusCode::NO_CONTENT
}

/// The `op` block as given (u60d's shape: `{rollback_enabled, active, last, notice?}`).
async fn set_op(State(m): State<Arc<Mock>>, body: axum::Json<Value>) -> StatusCode {
    m.publish("op", body.0, 0, false);
    StatusCode::NO_CONTENT
}

#[derive(serde::Deserialize)]
struct Stuck {
    /// The executor age every heartbeat now says; absent = back to the recording's.
    age: Option<u64>,
}

/// u60d's executor stuck (or not): heartbeats carry this `exec_age_ms`, one goes out now.
async fn set_stuck(State(m): State<Arc<Mock>>, Query(q): Query<Stuck>) -> StatusCode {
    m.exec_age
        .store(q.age.unwrap_or(u64::MAX), Ordering::SeqCst);
    m.heartbeat(0);
    StatusCode::NO_CONTENT
}

async fn drop_all(State(m): State<Arc<Mock>>) -> StatusCode {
    m.drop_gen.send_modify(|g| *g += 1);
    StatusCode::NO_CONTENT
}

async fn new_epoch(State(m): State<Arc<Mock>>) -> StatusCode {
    {
        let mut e = m.epoch.lock().unwrap();
        *e = format!("{e}x");
    }
    m.drop_gen.send_modify(|g| *g += 1);
    StatusCode::NO_CONTENT
}

#[derive(serde::Deserialize)]
struct NextWrite {
    #[serde(default)]
    fail: bool,
    #[serde(default)]
    noreadback: bool,
}

async fn next_write(State(m): State<Arc<Mock>>, Query(q): Query<NextWrite>) -> StatusCode {
    *m.next_write.lock().unwrap() = (q.fail, q.noreadback);
    StatusCode::NO_CONTENT
}

// ---- the change log ----

fn mode_words(m: &str) -> (&'static str, &'static str) {
    match m {
        "WL_AND_5G" => ("自动", "Auto"),
        "Only_LTE" => ("只用 4G", "4G only"),
        "Only_5G" => ("只用 5G SA", "5G SA only"),
        _ => ("其他", "Other"),
    }
}

fn undo_view(ok: bool, redo: bool, why: Option<(&str, &str)>, back_to: &str) -> Value {
    json!({"ok": ok, "label_zh": if redo { "重做" } else { "撤销" }, "label_en": if redo { "Redo" } else { "Undo" },
           "why_zh": why.map(|w| w.0), "why_en": why.map(|w| w.1),
           "request": {"action": "network.set_mode", "undo": true, "params": {"mode": back_to}}})
}

/// The old web's mock rows (fixtures/ops.ts ENTRIES): made up, never the recording's.
fn default_journal() -> Vec<Value> {
    vec![
        json!({"source": "screen", "action": "op.ack", "result": "ok", "hide": true, "what_zh": "op.ack", "what_en": "op.ack"}),
        json!({"op_id": "web-20", "action": "network.set_mode", "item": "network.mode", "source": "web", "undo": false,
               "new": "Only_LTE", "old": "WL_AND_5G", "t": "2026-10-03 14:32:07", "what_zh": "制式", "what_en": "Network mode",
               "change_zh": "自动 → 只用 4G", "change_en": "Auto → 4G only", "result_zh": "已切到只用 4G", "result_en": "Now 4G only",
               "mark": "ok", "source_zh": "网页", "source_en": "Web", "hide": false,
               "undo_view": undo_view(true, false, None, "WL_AND_5G")}),
        json!({"action": "cellular.set", "source": "screen", "params": {"enabled": 0}, "result": "failed", "t": "2026-10-03 09:01:00",
               "what_zh": "移动数据", "what_en": "Mobile data", "change_zh": "关掉数据", "change_en": "Turn data off",
               "result_zh": "没改成", "result_en": "Not changed", "mark": "bad", "source_zh": "触屏", "source_en": "Screen",
               "hide": false, "undo_view": null}),
        json!({"action": "wifi.apply", "item": "wifi", "source": "scenario", "result": "skipped", "skip": "end", "count": 5,
               "t": "2026-10-02 22:10:00", "what_zh": "Wi-Fi", "what_en": "Wi-Fi", "change_zh": "", "change_en": "",
               "result_zh": "情景跳过 ×5（你手动改过）", "result_en": "Scene skipped ×5 (you changed it by hand)",
               "mark": "warn", "source_zh": "情景", "source_en": "Scene", "hide": false, "undo_view": null}),
        json!({"action": "esim.switch", "source": "web", "journal_append": true, "result": "ok", "old": "CMLink", "new": "Ubigi",
               "t": "2026-10-02 08:00:00", "what_zh": "eSIM", "what_en": "eSIM", "change_zh": "CMLink → Ubigi", "change_en": "CMLink → Ubigi",
               "result_zh": "已改", "result_en": "Done", "mark": "ok", "source_zh": "网页", "source_en": "Web", "hide": false, "undo_view": null}),
        json!({"op_id": "screen-3-1", "action": "network.set_mode", "item": "network.mode", "source": "screen", "undo": false,
               "new": "WL_AND_5G", "old": "Only_5G", "t": "2026-10-01 19:45:00", "what_zh": "制式", "what_en": "Network mode",
               "change_zh": "只用 5G SA → 自动", "change_en": "5G SA only → Auto", "result_zh": "已切到自动", "result_en": "Now Auto",
               "mark": "ok", "source_zh": "触屏", "source_en": "Screen", "hide": false,
               "undo_view": undo_view(false, false, Some(("之后又改过", "Changed since")), "Only_5G")}),
    ]
}

fn journal_list(m: &Mock) -> Response {
    let mode = m.journal_mode.lock().unwrap().clone();
    let (status, v) = match mode.as_str() {
        "down" => (
            StatusCode::BAD_GATEWAY,
            json!({"ok": false, "action": "journal.list", "error": {"code": "device_call_failed", "message": "journal unreadable"}}),
        ),
        "old" => (
            StatusCode::BAD_REQUEST,
            json!({"ok": false, "action": "journal.list", "error": {"code": "unknown_action", "message": "unknown action: journal.list"}}),
        ),
        _ => {
            let entries = if mode == "empty" {
                vec![]
            } else {
                m.journal.lock().unwrap().clone()
            };
            (
                StatusCode::OK,
                json!({"ok": true, "action": "journal.list", "result": {"entries": entries, "owners": {}}}),
            )
        }
    };
    (status, axum::Json(v)).into_response()
}

/// network.set_mode as a transaction: active (applying, then verifying), then last.
fn txn(m: &Arc<Mock>, body: &Value) -> Response {
    let id = body["op_id"].as_str().unwrap_or("mock-1").to_owned();
    let target = body["params"]["mode"].as_str().unwrap_or("").to_owned();
    let undo = body["undo"].as_bool().unwrap_or(false);
    m.txns.lock().unwrap().push(
        json!({"op_id": id, "action": "network.set_mode", "undo": undo, "params": body["params"], "source": body["source"]}),
    );
    let old = m.mode.lock().unwrap().clone();
    let end = m.txn_end.lock().unwrap().clone();
    let (ow, nw) = (mode_words(&old), mode_words(&target));
    let base = json!({"op_id": id, "action": "network.set_mode", "item": "network.mode", "source": "web", "undo": undo,
        "source_zh": "网页", "source_en": "Web", "what_zh": "制式", "what_en": "Network mode",
        "target": target, "target_zh": nw.0, "target_en": nw.1, "old": old, "old_zh": ow.0, "old_en": ow.1,
        "rollback_to": old, "rollback_to_zh": ow.0, "rollback_to_en": ow.1, "readback_zh": null, "readback_en": null,
        "note_zh": null, "note_en": null, "reason": null, "can_revert": false, "can_keep": false,
        "revert_label_zh": format!("退回{}", ow.0), "revert_label_en": format!("Revert to {}", ow.1),
        "keep_label_zh": format!("保留{}", nw.0), "keep_label_en": format!("Keep {}", nw.1), "steps": []});
    let with = |o: Value| {
        let mut v = base.clone();
        for (k, x) in o.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    let applying = with(
        json!({"phase": "applying", "say_zh": "正在切换", "say_en": "Switching", "mark": null, "stay": "live",
        "next_zh": null, "next_en": null, "remaining_ms": null}),
    );
    let verifying = with(
        json!({"phase": "verifying", "say_zh": "正在确认", "say_en": "Checking", "mark": null, "stay": "live",
        "next_zh": format!("{{t}} 后没通就退回到{}", ow.0), "next_en": format!("Back to {} in {{t}} if no data", ow.1),
        "remaining_ms": 102000, "can_revert": true, "can_keep": true}),
    );
    let ok = end == "confirmed";
    let last = with(if ok {
        json!({"phase": "confirmed", "reason": "verified", "mark": "ok", "stay": "brief", "acked": false, "needs_ack": false,
               "say_zh": format!("已切到{}", nw.0), "say_en": format!("Now {}", nw.1)})
    } else {
        json!({"phase": "rolled_back", "reason": "timeout", "mark": "warn", "stay": "sticky", "acked": false, "needs_ack": true,
               "say_zh": format!("没通 · 已退回{}", ow.0), "say_en": format!("No data · back to {}", ow.1)})
    });
    let publish = |m: &Mock, active: Value, last: Value| {
        let cur = m.blocks.lock().unwrap().get("op").cloned();
        let (mut data, at) = match cur {
            Some(b) => (b["data"].clone(), b["observed_at"].as_u64().unwrap_or(0)),
            None => (json!({"rollback_enabled": true}), 0),
        };
        if !data.is_object() {
            data = json!({"rollback_enabled": true});
        }
        data["active"] = active;
        if !last.is_null() {
            data["last"] = last;
        }
        m.publish("op", data, at + 1, false);
    };
    publish(m, applying.clone(), Value::Null);
    let m2 = m.clone();
    let reply = json!({"ok": true, "action": "network.set_mode", "result": {"result": "success"},
                      "op": {"op_id": id, "phase": "applying"}});
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        publish(&m2, verifying, Value::Null);
        if end == "live" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2000)).await;
        if ok {
            *m2.mode.lock().unwrap() = target.clone();
            *m2.net_select.lock().unwrap() = Some(target.clone());
            let cur = m2.blocks.lock().unwrap().get("signal").cloned();
            if let Some(b) = cur {
                m2.publish("signal", b["data"].clone(), b["observed_at"].as_u64().unwrap_or(0) + 1, false);
            }
        }
        {
            let mut j = m2.journal.lock().unwrap();
            for e in j.iter_mut().filter(|e| e["action"] == "network.set_mode") {
                e["undo_view"]["ok"] = json!(false);
                e["undo_view"]["why_zh"] = json!("之后又改过");
                e["undo_view"]["why_en"] = json!("Changed since");
            }
            j.insert(0, json!({"op_id": id, "action": "network.set_mode", "item": "network.mode", "source": "web", "undo": undo,
                "new": target, "old": old, "t": "2026-10-03 15:10:00", "what_zh": "制式", "what_en": "Network mode",
                "change_zh": format!("{} → {}", ow.0, nw.0), "change_en": format!("{} → {}", ow.1, nw.1),
                "result_zh": last["say_zh"], "result_en": last["say_en"], "mark": last["mark"],
                "source_zh": "网页", "source_en": "Web", "hide": false,
                "undo_view": if ok { undo_view(true, undo, None, &old) } else { undo_view(false, undo, Some(("设置没变 · 不用撤销", "Nothing to undo")), &old) }}));
        }
        publish(&m2, Value::Null, last);
    });
    axum::Json(reply).into_response()
}

#[derive(serde::Deserialize)]
struct SetJournal {
    mode: String,
}

async fn set_journal(State(m): State<Arc<Mock>>, Query(q): Query<SetJournal>) -> StatusCode {
    if q.mode == "full" {
        *m.journal.lock().unwrap() = default_journal();
        *m.mode.lock().unwrap() = "Only_LTE".into();
    }
    *m.journal_mode.lock().unwrap() = q.mode;
    StatusCode::NO_CONTENT
}

#[derive(serde::Deserialize)]
struct SetTxn {
    end: String,
}

async fn set_txn(State(m): State<Arc<Mock>>, Query(q): Query<SetTxn>) -> StatusCode {
    *m.txn_end.lock().unwrap() = q.end;
    StatusCode::NO_CONTENT
}

/// Change one field of a block the mock holds (from the recording's snapshot) and publish it.
fn set_field(m: &Mock, block: &str, key: &str, v: Value) {
    let cur = m.blocks.lock().unwrap().get(block).cloned();
    let (mut data, at) = match cur {
        Some(b) => (b["data"].clone(), b["observed_at"].as_u64().unwrap_or(0) + 1),
        None => (json!({}), 0),
    };
    if let Some(o) = data.as_object_mut() {
        o.insert(key.into(), v);
    }
    m.publish(block, data, at, false);
}

/// `usb.set {mode}` → `qos.usb_mode`; `vendor.call` powerbank set → `powerbank.state`.
fn usb_write(m: &Arc<Mock>, body: &Value, noreadback: bool) -> Response {
    let action = body["action"].as_str().unwrap_or_default().to_owned();
    m.usb_ops.lock().unwrap().push(body.clone());
    let p = &body["params"];
    let bad = |msg: &str| {
        let e = json!({"ok":false,"action":action,"error":{"code":"invalid_parameter","message":msg}});
        (StatusCode::BAD_REQUEST, axum::Json(e)).into_response()
    };
    let (block, key, v) = if action == "usb.set" {
        let Some(mode) = p["mode"].as_str() else {
            return bad("missing parameter: mode");
        };
        ("qos", "usb_mode", json!(mode))
    } else {
        if p["object"] != "zwrt_bsp.powerbank" || p["method"] != "set" {
            return bad("not a vendor call core makes");
        }
        let Some(state) = p["args"]["state"].as_i64() else {
            return bad("args.state");
        };
        ("powerbank", "state", json!(state))
    };
    if !noreadback {
        let m = m.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            set_field(&m, block, key, v);
        });
    }
    axum::Json(json!({"ok":true,"action":action,"result":{"result":"success"}})).into_response()
}

#[derive(serde::Deserialize)]
struct SetUsb {
    cc: i64,
}

async fn set_usb(State(m): State<Arc<Mock>>, Query(q): Query<SetUsb>) -> StatusCode {
    set_field(&m, "typec", "cc_attch_state", json!(q.cc));
    StatusCode::NO_CONTENT
}

/// The mock's router for a recording (`raw` is the text of a `/v2/events` capture).
/// Fields recordings from before a contract change don't have: `net_select_mode` (u60-web.md W9).
fn fill_missing(name: &str, data: &mut Value) {
    if name == "signal"
        && let Some(o) = data.as_object_mut()
    {
        o.entry("net_select_mode")
            .or_insert_with(|| json!("auto_select"));
    }
}

pub fn router(raw: &str, speed: f64) -> Router {
    let (snapshot, frames) = load(raw, speed);
    let mut blocks = BTreeMap::new();
    if let Some(bs) = snapshot["blocks"].as_object() {
        for (k, v) in bs {
            let mut v = v.clone();
            fill_missing(k, &mut v["data"]);
            blocks.insert(k.clone(), v);
        }
    }
    let roam = blocks
        .get("interfaces")
        .and_then(|b| b["data"]["cellular"]["roam_enable"].as_i64());
    let m = Arc::new(Mock {
        epoch: Mutex::new(snapshot["epoch"].as_str().unwrap_or("mock").to_owned()),
        seq: AtomicU64::new(snapshot["seq"].as_u64().unwrap_or(0)),
        blocks: Mutex::new(blocks),
        roam: Mutex::new(roam),
        next_write: Mutex::new((false, false)),
        tx: broadcast::channel(1024).0,
        drop_gen: watch::channel(0).0,
        open: AtomicUsize::new(0),
        total: AtomicU64::new(0),
        sent: AtomicU64::new(0),
        unread: AtomicU64::new(0),
        exec_age: AtomicU64::new(u64::MAX),
        notice_acks: AtomicU64::new(0),
        op_ids: Mutex::new(Vec::new()),
        sms: Mutex::new(default_inbox()),
        sms_ops: Mutex::new(Vec::new()),
        sms_sends: Mutex::new(Vec::new()),
        journal: Mutex::new(default_journal()),
        journal_mode: Mutex::new("full".into()),
        txn_end: Mutex::new("confirmed".into()),
        txns: Mutex::new(Vec::new()),
        mode: Mutex::new("Only_LTE".into()),
        usb_ops: Mutex::new(Vec::new()),
        bands: Mutex::new(None),
        cells: Mutex::new((String::new(), String::new())),
        dual: Mutex::new(None),
        apn_ops: Mutex::new(Vec::new()),
        net_select: Mutex::new(None),
        data_on: Mutex::new(None),
        reboots: AtomicU64::new(0),
        page_ops: Mutex::new(Vec::new()),
    });
    // the recording's messages are real: replaced before anyone connects
    m.blocks.lock().unwrap().remove("sms_list");
    m.publish_sms();
    m.publish("apn", default_apn(), 0, false);
    for seed in BATCH4_SEED {
        seed(&m);
    }
    tokio::spawn(replay(m.clone(), frames));
    batch4_routes(Router::new())
        .route("/v2/events", get(events))
        .route("/control", post(control))
        .route("/mock/stats", get(stats))
        .route("/mock/drop", post(drop_all))
        .route("/mock/epoch", post(new_epoch))
        .route("/mock/control", post(next_write))
        .route("/mock/alerts", post(set_alerts))
        .route("/mock/alerts-to-agent", post(alerts_to_agent))
        .route("/mock/op", post(set_op))
        .route("/mock/stuck", post(set_stuck))
        .route("/mock/sms", post(set_sms))
        .route("/mock/journal", post(set_journal))
        .route("/mock/txn", post(set_txn))
        .route("/mock/usb", post(set_usb))
        .route("/mock/bands", post(set_bands))
        .route("/mock/cells", post(set_cells))
        .route("/mock/dual", post(set_dual))
        .route("/mock/apn", post(reset_apn))
        .route("/mock/net", post(reset_net))
        .route("/v2/api/alerts", get(screen_alerts))
        .route("/v2/api/public/status", get(screen_status))
        // the agent-shaped reads u60d serves since 8d-1b are not mocked here: answer like a u60d
        // without the route, so luyaz-web asks the fake agent, which keeps the e2e data (agent.rs)
        .fallback(|| async {
            (
                axum::http::StatusCode::NOT_FOUND,
                axum::Json(json!({"ok": false, "error": "not_in_compat_yet"})),
            )
        })
        .with_state(m)
}

// ---- batch 4 (Wi-Fi and LAN pages): one file per page under src/mock/ ----

mod router_filter;
mod router_firewall;
mod router_lan;
mod router_qos;
mod home;
mod poller;
mod receipt;
mod router_vpn;
mod wifi_guest;
mod wifi_main;

type Batch4Control = fn(&Arc<Mock>, &Value, bool) -> Option<Response>;

const BATCH4_CONTROL: &[Batch4Control] = &[
    router_qos::control,
    router_vpn::control,
    router_filter::control,
    router_firewall::control,
    router_lan::control,
    wifi_guest::control,
    wifi_main::control,
    home::control,
    poller::control,
];

const BATCH4_SEED: &[fn(&Arc<Mock>)] = &[
    router_qos::seed,
    router_vpn::seed,
    router_filter::seed,
    router_firewall::seed,
    router_lan::seed,
    wifi_guest::seed,
    wifi_main::seed,
    home::seed,
    poller::seed,
];

/// The first batch-4 page that claims the write answers it (logged in `page_ops` by the page).
fn batch4_control(m: &Arc<Mock>, body: &Value, noreadback: bool) -> Option<Response> {
    BATCH4_CONTROL.iter().find_map(|f| f(m, body, noreadback))
}

fn batch4_routes(r: Router<Arc<Mock>>) -> Router<Arc<Mock>> {
    let r = router_qos::routes(r);
    let r = router_vpn::routes(r);
    let r = router_filter::routes(r);
    let r = router_firewall::routes(r);
    let r = router_lan::routes(r);
    let r = wifi_guest::routes(r);
    let r = wifi_main::routes(r);
    let r = receipt::routes(r);
    let r = poller::routes(r);
    home::routes(r)
}

// ---- band lock ----

/// `op.active` while a transaction runs (u60d's `doing` in a 409).
fn active_op(m: &Mock) -> Option<Value> {
    let b = m.blocks.lock().unwrap().get("op").cloned()?;
    let a = &b["data"]["active"];
    a.is_object().then(|| json!({"op_id": a["op_id"], "item": a["item"], "phase": a["phase"]}))
}

/// u60d's canonical band list (`spec::bands`); None = not digits and commas.
fn canon_bands(s: &str) -> Option<String> {
    let mut v = Vec::new();
    for p in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        v.push(p.parse::<u32>().ok()?);
    }
    v.sort_unstable();
    v.dedup();
    Some(v.iter().map(u32::to_string).collect::<Vec<_>>().join(","))
}

/// The signal block's (lte, sa, nsa) for `keys` (`*_bands` or `*_supported_bands`).
fn signal_bands(m: &Mock, keys: [&str; 3]) -> (String, String, String) {
    let sig = m.blocks.lock().unwrap().get("signal").map(|b| b["data"].clone()).unwrap_or(Value::Null);
    let f = |k: &str| canon_bands(sig[k].as_str().unwrap_or("")).unwrap_or_default();
    (f(keys[0]), f(keys[1]), f(keys[2]))
}

fn band_words(prefix: &str, s: &str) -> String {
    let v: Vec<&str> = s.split(',').filter(|b| !b.is_empty()).collect();
    if v.len() > 6 {
        return format!("{} 个频段|{} bands", v.len(), v.len());
    }
    let w = v.iter().map(|b| format!("{prefix}{b}")).collect::<Vec<_>>().join(" ");
    format!("{w}|{w}")
}

/// (zh, en) of a band target as u60d's `value_words` writes it.
fn band_value(item: &str, v: &Value) -> (String, String) {
    let split = |s: String| {
        let (a, b) = s.split_once('|').unwrap();
        (a.to_owned(), b.to_owned())
    };
    if item == "band.lte" {
        return split(band_words("B", v.as_str().unwrap_or("")));
    }
    let mut zh = Vec::new();
    let mut en = Vec::new();
    for (k, label, prefix) in [("lte", "4G", "B"), ("sa", "SA", "n"), ("nsa", "NSA", "n")] {
        if let Some(s) = v[k].as_str() {
            let (z, e) = split(band_words(prefix, s));
            zh.push(format!("{label} {z}"));
            en.push(format!("{label} {e}"));
        }
    }
    (zh.join(" · "), en.join(" · "))
}

/// band.set_lte / band.set_nr / band.reset as a transaction (the network.set_mode flow).
fn band_txn(m: &Arc<Mock>, body: &Value) -> Response {
    let action = body["action"].as_str().unwrap_or_default().to_owned();
    let p = &body["params"];
    let bad = |msg: String| {
        let e = json!({"ok": false, "action": action, "error": {"code": "invalid_parameter", "message": msg}});
        (StatusCode::BAD_REQUEST, axum::Json(e)).into_response()
    };
    let list = |k: &str| -> Result<String, String> {
        let raw = p[k].as_str().ok_or(format!("missing parameter: {k}"))?;
        let v = canon_bands(raw).ok_or(format!("{k} must contain only numbers and commas"))?;
        if v.is_empty() {
            return Err(format!("{k} must list at least one band"));
        }
        Ok(v)
    };
    let (lte0, sa0, nsa0) = signal_bands(m, ["lte_bands", "sa_bands", "nsa_bands"]);
    let (item, what, target, old, next): (&str, (&str, &str), Value, Value, (String, String, String)) =
        match action.as_str() {
            "band.set_lte" => match list("bands") {
                Err(e) => return bad(e),
                Ok(v) => ("band.lte", ("4G 频段", "4G bands"), json!(v), json!(lte0), (v, sa0.clone(), nsa0.clone())),
            },
            "band.set_nr" => match (list("sa"), list("nsa")) {
                (Err(e), _) | (_, Err(e)) => return bad(e),
                (Ok(sa), Ok(nsa)) => ("band.nr", ("5G 频段", "5G bands"), json!({"sa": sa, "nsa": nsa}),
                                      json!({"sa": sa0, "nsa": nsa0}), (lte0.clone(), sa, nsa)),
            },
            "band.reset" => {
                let to = if p.as_object().is_none_or(|o| o.is_empty()) {
                    signal_bands(m, ["lte_supported_bands", "nr_sa_supported_bands", "nr_nsa_supported_bands"])
                } else {
                    match (list("lte"), list("sa"), list("nsa")) {
                        (Ok(a), Ok(b), Ok(c)) => (a, b, c),
                        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return bad(e),
                    }
                };
                ("band.all", ("频段和小区锁", "Bands and cell lock"),
                 json!({"lte": to.0, "sa": to.1, "nsa": to.2}),
                 json!({"lte": lte0, "sa": sa0, "nsa": nsa0}), to)
            }
            _ => {
                let e = json!({"ok": false, "action": action, "error": {"code": "unknown_action", "message": "unknown action"}});
                return (StatusCode::BAD_REQUEST, axum::Json(e)).into_response();
            }
        };
    let (tw, ow) = (band_value(item, &target), band_value(item, &old));
    let request = match item {
        "band.lte" => json!({"action": "band.set_lte", "undo": true, "params": {"bands": old}}),
        "band.nr" => json!({"action": "band.set_nr", "undo": true, "params": old}),
        _ => json!({"action": "band.reset", "undo": true, "params": old}),
    };
    let first = if item == "band.all" { "正在恢复默认频段" } else { "正在锁频" };
    let all = item == "band.all";
    run_txn(m, body, Txn { item, what, target, old, words: (tw, ow), first, request },
        Box::new(move |m: &Mock| {
            *m.bands.lock().unwrap() = Some(next);
            if all {
                *m.cells.lock().unwrap() = (String::new(), String::new());
            }
        }))
}

/// One transaction as the mock runs it (the network.set_mode flow).
struct Txn {
    item: &'static str,
    what: (&'static str, &'static str),
    target: Value,
    old: Value,
    /// (target, old) as (zh, en).
    words: ((String, String), (String, String)),
    /// The applying phase's `say_zh`.
    first: &'static str,
    /// The journal row's undo request.
    request: Value,
}

/// Record the body, publish applying → verifying → the end (`/mock/txn?end=`); on `confirmed` run
/// `on_ok` (the device state changes), republish `signal`, add the journal row.
fn run_txn(m: &Arc<Mock>, body: &Value, t: Txn, on_ok: Box<dyn FnOnce(&Mock) + Send>) -> Response {
    let action = body["action"].as_str().unwrap_or_default().to_owned();
    let p = &body["params"];
    let Txn { item, what, target, old, words: (tw, ow), first, request } = t;
    let id = body["op_id"].as_str().unwrap_or("mock-1").to_owned();
    let undo = body["undo"].as_bool().unwrap_or(false);
    m.txns.lock().unwrap().push(
        json!({"op_id": id, "action": action, "undo": undo, "params": p, "source": body["source"]}),
    );
    let end = m.txn_end.lock().unwrap().clone();
    let base = json!({"op_id": id, "action": action, "item": item, "source": "web", "undo": undo,
        "source_zh": "网页", "source_en": "Web", "what_zh": what.0, "what_en": what.1,
        "target": target, "target_zh": tw.0, "target_en": tw.1, "old": old, "old_zh": ow.0, "old_en": ow.1,
        "rollback_to": old, "rollback_to_zh": ow.0, "rollback_to_en": ow.1, "readback_zh": null, "readback_en": null,
        "note_zh": null, "note_en": null, "reason": null, "can_revert": false, "can_keep": false, "steps": []});
    let with = |o: Value| {
        let mut v = base.clone();
        for (k, x) in o.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    let applying = with(json!({"phase": "applying", "say_zh": first, "say_en": "Applying", "mark": null, "stay": "live",
        "next_zh": null, "next_en": null, "remaining_ms": null}));
    let verifying = with(json!({"phase": "verifying", "say_zh": "正在确认", "say_en": "Checking", "mark": null, "stay": "live",
        "next_zh": "{t} 内确认", "next_en": "confirming within {t}", "remaining_ms": 102000}));
    let receipt = body["receipt"].as_bool() == Some(true);
    let ok = end == "confirmed";
    let waiting = {
        let mut v = verifying.clone();
        v["receipt"] = receipt::view(receipt::wait_ms());
        v
    };
    let mut last = with(if ok {
        json!({"phase": "confirmed", "reason": "verified", "mark": "ok", "stay": "brief", "acked": false, "needs_ack": false,
               "say_zh": format!("已改成{}", tw.0), "say_en": format!("Now {}", tw.1)})
    } else {
        json!({"phase": "rolled_back", "reason": "timeout", "mark": "warn", "stay": "sticky", "acked": false, "needs_ack": true,
               "say_zh": format!("没通 · 已退回 {}", ow.0), "say_en": format!("No data · back to {}", ow.1)})
    });
    let publish = |m: &Mock, active: Value, last: Value| {
        let cur = m.blocks.lock().unwrap().get("op").cloned();
        let (mut data, at) = match cur {
            Some(b) if b["data"].is_object() => (b["data"].clone(), b["observed_at"].as_u64().unwrap_or(0)),
            _ => (json!({"rollback_enabled": true}), 0),
        };
        data["active"] = active;
        if !last.is_null() {
            data["last"] = last;
        }
        m.publish("op", data, at + 1, false);
    };
    publish(m, applying, Value::Null);
    let m2 = m.clone();
    let reply = json!({"ok": true, "action": action, "result": {"result": "success"},
                      "op": {"op_id": id, "phase": "applying"}});
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        publish(&m2, verifying, Value::Null);
        if end == "live" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2000)).await;
        // O3: a remote change also waits for the page's receipt (receipt.rs)
        if ok && receipt {
            publish(&m2, waiting, Value::Null);
            if receipt::wait(&id).await {
                last["reason"] = json!("receipt");
            } else {
                last = no_receipt(last, (&ow.0, &ow.1));
            }
        }
        let ok = last["phase"] == "confirmed";
        if ok {
            on_ok(&m2);
            for name in ["signal", "apn", "sim"] {
                let cur = m2.blocks.lock().unwrap().get(name).cloned();
                if let Some(b) = cur {
                    m2.publish(name, b["data"].clone(), b["observed_at"].as_u64().unwrap_or(0) + 1, false);
                }
            }
        }
        m2.journal.lock().unwrap().insert(0, json!({"op_id": id, "action": action, "item": item, "source": "web", "undo": undo,
            "new": target, "old": old, "t": "2026-10-03 15:20:00", "what_zh": what.0, "what_en": what.1,
            "change_zh": format!("{} → {}", ow.0, tw.0), "change_en": format!("{} → {}", ow.1, tw.1),
            "result_zh": last["say_zh"], "result_en": last["say_en"], "mark": last["mark"],
            "source_zh": "网页", "source_en": "Web", "hide": false,
            "undo_view": {"ok": ok, "label_zh": if undo { "重做" } else { "撤销" }, "label_en": if undo { "Redo" } else { "Undo" },
                          "why_zh": if ok { Value::Null } else { json!("设置没变 · 不用撤销") },
                          "why_en": if ok { Value::Null } else { json!("Nothing to undo") }, "request": request}}));
        publish(&m2, Value::Null, last);
    });
    axum::Json(reply).into_response()
}

/// O3: the core's (RolledBack, NoReceipt) words over a transaction's `last`.
fn no_receipt(mut last: Value, old: (&str, &str)) -> Value {
    for (k, x) in [("phase", json!("rolled_back")), ("reason", json!("no_receipt")), ("mark", json!("warn")),
        ("stay", json!("sticky")), ("needs_ack", json!(true)),
        ("say_zh", json!(format!("远端没回来 · 已退回{}", old.0))),
        ("say_en", json!(format!("Remote did not come back · back to {}", old.1)))]
    {
        last[k] = x;
    }
    last
}

#[derive(serde::Deserialize)]
struct SetBands {
    #[serde(default)]
    lte: String,
    #[serde(default)]
    sa: String,
    #[serde(default)]
    nsa: String,
}

/// Set the locks as the touch screen would (empty = back to the recording's), publish `signal`.
async fn set_bands(State(m): State<Arc<Mock>>, Query(q): Query<SetBands>) -> StatusCode {
    *m.bands.lock().unwrap() = if q.lte.is_empty() && q.sa.is_empty() && q.nsa.is_empty() {
        None
    } else {
        let (l0, s0, n0) = signal_bands(&m, ["lte_supported_bands", "nr_sa_supported_bands", "nr_nsa_supported_bands"]);
        let pick = |v: &str, d: String| if v.is_empty() { d } else { canon_bands(v).unwrap_or(d) };
        Some((pick(&q.lte, l0), pick(&q.sa, s0), pick(&q.nsa, n0)))
    };
    let cur = m.blocks.lock().unwrap().get("signal").cloned();
    if let Some(b) = cur {
        m.publish("signal", b["data"].clone(), b["observed_at"].as_u64().unwrap_or(0) + 1, false);
    }
    StatusCode::NO_CONTENT
}

// ---- cell lock ----

/// u60d's `cell_num`: digits, no leading zeros.
fn cell_num(p: &Value, k: &str) -> Result<String, String> {
    let raw = match &p[k] {
        Value::String(v) => v.trim().trim_start_matches(['n', 'N']).to_owned(),
        Value::Number(n) => n.to_string(),
        Value::Null => return Err(format!("missing parameter: {k}")),
        _ => return Err(format!("{k} must be a string")),
    };
    match raw.parse::<u64>() {
        Ok(n) if raw.bytes().all(|b| b.is_ascii_digit()) => Ok(n.to_string()),
        _ => Err(format!("{k} must be a number")),
    }
}

fn cell_words(v: &Value) -> (String, String) {
    if v["off"] == json!(true) || !v.is_object() {
        return ("不锁小区".into(), "No cell lock".into());
    }
    let w = if v["band"].is_string() {
        format!("PCI {} · ARFCN {} · n{}", v["pci"].as_str().unwrap_or(""), v["arfcn"].as_str().unwrap_or(""), v["band"].as_str().unwrap_or(""))
    } else {
        format!("PCI {} · EARFCN {}", v["pci"].as_str().unwrap_or(""), v["earfcn"].as_str().unwrap_or(""))
    };
    (w.clone(), w)
}

/// The raw signal field as a value (`{off: true}` when not locked).
fn cell_value(raw: &str, nr: bool) -> Value {
    let v: Vec<&str> = raw.split(',').collect();
    if raw.is_empty() || v.iter().all(|x| x.trim() == "0") {
        return json!({"off": true});
    }
    if nr && v.len() == 3 {
        json!({"pci": v[0], "arfcn": v[1], "band": v[2]})
    } else if !nr && v.len() == 2 {
        json!({"pci": v[0], "earfcn": v[1]})
    } else {
        json!({"off": true})
    }
}

fn cell_txn(m: &Arc<Mock>, body: &Value) -> Response {
    let action = body["action"].as_str().unwrap_or_default().to_owned();
    let p = &body["params"];
    let nr = action == "cell.lock_nr";
    if !nr && action != "cell.lock_lte" {
        let e = json!({"ok": false, "action": action, "error": {"code": "unknown_action", "message": "unknown action"}});
        return (StatusCode::BAD_REQUEST, axum::Json(e)).into_response();
    }
    let target = if p["off"] == json!(true) {
        Ok(json!({"off": true}))
    } else if nr {
        (|| Ok::<_, String>(json!({"pci": cell_num(p, "pci")?, "arfcn": cell_num(p, "arfcn")?, "band": cell_num(p, "band")?})))()
    } else {
        (|| Ok::<_, String>(json!({"pci": cell_num(p, "pci")?, "earfcn": cell_num(p, "earfcn")?})))()
    };
    let target = match target {
        Ok(t) => t,
        Err(msg) => {
            let e = json!({"ok": false, "action": action, "error": {"code": "invalid_parameter", "message": msg}});
            return (StatusCode::BAD_REQUEST, axum::Json(e)).into_response();
        }
    };
    let (nr0, lte0) = m.cells.lock().unwrap().clone();
    let old = cell_value(if nr { &nr0 } else { &lte0 }, nr);
    let raw = if target["off"] == json!(true) {
        String::new()
    } else if nr {
        format!("{},{},{}", target["pci"].as_str().unwrap(), target["arfcn"].as_str().unwrap(), target["band"].as_str().unwrap())
    } else {
        format!("{},{}", target["pci"].as_str().unwrap(), target["earfcn"].as_str().unwrap())
    };
    let item = if nr { "cell.nr" } else { "cell.lte" };
    let what = if nr { ("5G 小区锁", "5G cell lock") } else { ("4G 小区锁", "4G cell lock") };
    let words = (cell_words(&target), cell_words(&old));
    let request = json!({"action": action, "undo": true, "params": old});
    run_txn(m, body, Txn { item, what, target, old, words, first: "正在锁小区", request },
        Box::new(move |m: &Mock| {
            let mut c = m.cells.lock().unwrap();
            if nr { c.0 = raw } else { c.1 = raw }
        }))
}

/// Dual SIM (`sim.dual`, /router/dualsim): the value is u60d's (`{mode, smart?, active_slot?}`);
/// on confirm the `dual` view takes it (slots switched on to match, smart only in dual).
fn dual_txn(m: &Arc<Mock>, body: &Value) -> Response {
    let p = body["params"].clone();
    let Some(cur) = m.dual.lock().unwrap().clone() else {
        let e = json!({"ok": false, "action": "sim.dual", "error": {"code": "invalid_parameter", "message": "this device has no dual SIM"}});
        return (StatusCode::BAD_REQUEST, axum::Json(e)).into_response();
    };
    let value_of = |d: &Value| match (d["mode"].as_str(), d["smart"] == json!(true)) {
        (Some("dual"), true) => json!({"mode": "dual", "smart": true}),
        (Some("dual"), false) => json!({"mode": "dual", "smart": false, "active_slot": d["active_slot"]}),
        _ => json!({"mode": "single", "active_slot": d["active_slot"]}),
    };
    let old = value_of(&cur);
    // u60d's words (core ops/ui.rs dual_sim_words)
    let words = |v: &Value| {
        let n = v["active_slot"].as_i64().unwrap_or(0);
        match (v["mode"].as_str(), v["smart"] == json!(true)) {
            (Some("dual"), true) => ("双卡 · 智能切换".to_string(), "Dual SIM · smart switching".to_string()),
            (Some("dual"), false) => (format!("双卡 · 主卡 卡槽 {n}"), format!("Dual SIM · main slot {n}")),
            _ => (format!("单卡 · 卡槽 {n}"), format!("Single SIM · slot {n}")),
        }
    };
    let request = json!({"action": "sim.dual", "undo": true, "params": old});
    let t = Txn { item: "sim.dual", what: ("双卡", "Dual SIM"), words: (words(&p), words(&old)), target: p.clone(), old, first: "正在切换", request };
    run_txn(m, body, t, Box::new(move |m: &Mock| {
        let mut g = m.dual.lock().unwrap();
        let Some(d) = g.as_mut() else { return };
        let dual = p["mode"] == "dual";
        let smart = dual && p["smart"] == true;
        d["mode"] = json!(if dual { "dual" } else { "single" });
        d["smart"] = json!(smart);
        if let Some(n) = p["active_slot"].as_i64() {
            d["active_slot"] = json!(n);
        }
        let active = d["active_slot"].as_i64().unwrap_or(1);
        if let Some(slots) = d["slots"].as_array_mut() {
            for s in slots {
                let on = dual || s["slot"].as_i64() == Some(active);
                s["provisioned"] = json!(on);
                s["ready"] = if on { json!(true) } else { Value::Null };
            }
        }
    }))
}

/// Make the mock a dual-SIM device (body = the `dual` view) or single-slot again (`null`).
async fn set_dual(State(m): State<Arc<Mock>>, body: axum::Json<Value>) -> StatusCode {
    *m.dual.lock().unwrap() = (!body.0.is_null()).then(|| body.0.clone());
    let cur = m.blocks.lock().unwrap().get("sim").cloned();
    if let Some(b) = cur {
        m.publish("sim", b["data"].clone(), b["observed_at"].as_u64().unwrap_or(0) + 1, false);
    }
    StatusCode::NO_CONTENT
}

#[derive(serde::Deserialize)]
struct SetCells {
    #[serde(default)]
    nr: String,
    #[serde(default)]
    lte: String,
}

/// Set the raw cell lock fields (as the touch screen or a band reset would), publish `signal`.
async fn set_cells(State(m): State<Arc<Mock>>, Query(q): Query<SetCells>) -> StatusCode {
    *m.cells.lock().unwrap() = (q.nr, q.lte);
    let cur = m.blocks.lock().unwrap().get("signal").cloned();
    if let Some(b) = cur {
        m.publish("signal", b["data"].clone(), b["observed_at"].as_u64().unwrap_or(0) + 1, false);
    }
    StatusCode::NO_CONTENT
}

// ---- APN ----

fn default_apn() -> Value {
    let p = |id: &str, name: &str, apn: &str, user: &str, pw: bool, auth: i64, pdp: i64, en: bool| {
        json!({"id": id, "name": name, "apn": apn, "username": user, "has_password": pw, "auth": auth,
               "pdp": pdp, "roaming_pdp": pdp, "enabled": en})
    };
    json!({"mode": 0, "current": "auto109590",
        "manual": [p("manu1", "China Mobile", "cmnet", "", false, 0, 3, true), p("manu2", "Work", "corp.example", "u1", true, 2, 1, false),
                   p("manu3", "", "", "", false, 0, 0, false)],
        "auto": [p("auto109590", "China Telecom", "ctiot", "", false, 0, 3, true), p("auto109600", "China Telecom 4G", "ctnet", "", false, 0, 3, false)]})
}

fn apn_now(m: &Mock) -> Value {
    m.blocks.lock().unwrap().get("apn").map(|b| b["data"].clone()).unwrap_or_else(default_apn)
}

fn apn_edit(m: &Mock, f: impl FnOnce(&mut Value)) {
    let cur = m.blocks.lock().unwrap().get("apn").cloned();
    let at = cur.as_ref().and_then(|b| b["observed_at"].as_u64()).unwrap_or(0);
    let mut d = cur.map(|b| b["data"].clone()).unwrap_or_else(default_apn);
    f(&mut d);
    m.publish("apn", d, at + 1, false);
}

fn apn_words(v: &Value) -> (String, String) {
    match (v["mode"].as_i64(), v["manual"].as_str()) {
        (Some(0), _) => ("自动".into(), "Auto".into()),
        (Some(1), Some(id)) if !id.is_empty() => (format!("手动 · {id}"), format!("Manual · {id}")),
        _ => ("手动".into(), "Manual".into()),
    }
}

/// Apply a profile write to the block's data.
fn apn_apply(d: &mut Value, action: &str, p: &Value) {
    let id = p["profile_id"].as_str().unwrap_or("").to_owned();
    let list = d["manual"].as_array_mut().unwrap();
    match action {
        "apn.enable" => {
            for x in list.iter_mut() {
                x["enabled"] = json!(x["id"] == json!(id));
            }
        }
        "apn.delete" => list.retain(|x| x["id"] != json!(id)),
        "apn.modify" | "apn.add" => {
            let mut x = if action == "apn.add" {
                let n = list.len() + 1;
                json!({"id": format!("manu{n}"), "enabled": false, "has_password": false, "username": ""})
            } else {
                match list.iter().find(|x| x["id"] == json!(id)) {
                    Some(x) => x.clone(),
                    None => return,
                }
            };
            x["name"] = p["name"].clone();
            x["apn"] = p["apn"].clone();
            if let Some(u) = p.get("username") {
                x["username"] = u.clone();
            }
            if p["password"].as_str().is_some_and(|s| !s.is_empty()) {
                x["has_password"] = json!(true);
            }
            x["auth"] = p["auth_mode"].clone();
            x["pdp"] = p["pdp_type"].clone();
            x["roaming_pdp"] = p["roaming_pdp_type"].clone();
            if action == "apn.add" {
                list.retain(|y| !(y["name"] == json!("") && y["apn"] == json!("")));
                list.push(x);
            } else if let Some(y) = list.iter_mut().find(|y| y["id"] == json!(id)) {
                *y = x;
            }
        }
        _ => {}
    }
    if action == "apn.set_mode" {
        d["mode"] = p["mode"].clone();
    }
}

fn apn_write(m: &Arc<Mock>, body: &Value) -> Response {
    let action = body["action"].as_str().unwrap_or_default().to_owned();
    let p = body["params"].clone();
    m.apn_ops.lock().unwrap().push(body.clone());
    let bad = |code: &str, msg: &str| {
        let e = json!({"ok": false, "action": action, "error": {"code": code, "message": msg}});
        (StatusCode::BAD_REQUEST, axum::Json(e)).into_response()
    };
    let d = apn_now(m);
    let manual_mode = d["mode"] == json!(1);
    let id = p["profile_id"].as_str().unwrap_or("").to_owned();
    let in_use = manual_mode && d["manual"].as_array().is_some_and(|l| l.iter().any(|x| x["id"] == json!(id) && x["enabled"] == json!(true)));
    let txn = match action.as_str() {
        "apn.set_mode" => match p["mode"].as_i64() {
            Some(0 | 1) => true,
            _ => return bad("invalid_parameter", "mode must be 0 or 1"),
        },
        "apn.enable" => manual_mode,
        "apn.modify" => in_use,
        "apn.delete" if in_use => return bad("invalid_parameter", "cannot delete the APN profile in use"),
        "apn.add" | "apn.delete" | "apn.set_pdp_type" => false,
        _ => return bad("unknown_action", "unknown action"),
    };
    if txn {
        let cur = d["manual"].as_array().and_then(|l| l.iter().find(|x| x["enabled"] == json!(true))).and_then(|x| x["id"].as_str()).unwrap_or("").to_owned();
        let old = json!({"mode": d["mode"], "manual": if manual_mode { json!(cur) } else { json!("") }});
        let target = match action.as_str() {
            "apn.set_mode" => json!({"mode": p["mode"]}),
            _ => json!({"mode": 1, "manual": id}),
        };
        let words = (apn_words(&target), apn_words(&old));
        let request = json!({"action": "apn.set_mode", "undo": true, "params": {"restore": old, "expect": target}});
        let (a2, p2) = (action.clone(), p.clone());
        return run_txn(m, body, Txn { item: "apn", what: ("APN", "APN"), target, old, words, first: "正在换 APN", request },
            Box::new(move |m: &Mock| apn_edit(m, |d| apn_apply(d, &a2, &p2))));
    }
    if action != "apn.set_pdp_type" {
        let m2 = m.clone();
        let (a2, p2) = (action.clone(), p.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1000)).await;
            apn_edit(&m2, |d| apn_apply(d, &a2, &p2));
        });
    }
    axum::Json(json!({"ok": true, "action": action, "result": {"result": "success"}})).into_response()
}

/// Back to the made-up APN block.
async fn reset_apn(State(m): State<Arc<Mock>>) -> StatusCode {
    apn_edit(&m, |d| *d = default_apn());
    StatusCode::NO_CONTENT
}

/// Network mode and mobile data back to the recording's.
async fn reset_net(State(m): State<Arc<Mock>>) -> StatusCode {
    *m.net_select.lock().unwrap() = None;
    *m.data_on.lock().unwrap() = None;
    *m.roam.lock().unwrap() = None;
    for name in ["signal", "interfaces"] {
        let cur = m.blocks.lock().unwrap().get(name).cloned();
        if let Some(b) = cur {
            m.publish(name, b["data"].clone(), b["observed_at"].as_u64().unwrap_or(0) + 1, false);
        }
    }
    StatusCode::NO_CONTENT
}
