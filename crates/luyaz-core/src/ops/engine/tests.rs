//! 引擎 + 假基带（write-op-layer.md 测试计划 Critical Paths 第 1 条）：可控时钟（`start_paused`），
//! 假基带按脚本接受/忽略/拒绝写，可以换卡、断注册、模拟整机重启（换 boot_id、时钟归零）。

use super::*;
use crate::ops::{
    record::Record,
    spec::{NETWORK_MODE, SPECS},
    txn::{Conn, DataPath, SimId},
};
use std::path::PathBuf;
use tokio::time::Instant;

#[derive(Clone, Copy, PartialEq)]
enum Apply {
    /// 写进去，读回马上变。
    Take,
    /// 回成功但读回不变（基带 SSR 后 set_netselect 只记日志不干活）。
    Ignore,
    /// 写调用报错，读回不变。
    Fail,
    /// 写调用一直不回。
    Hang,
    /// 写调用超时，原厂过一会才真改过去（D28）。
    TimeoutThenTake,
}

struct DevState {
    net_select: String,
    registered: bool,
    sim: SimId,
    apply: Apply,
    read_fail: bool,
    writes: Vec<String>,
    reads: usize,
    boot: String,
    clock_base: Instant,
    write_delay: Duration,
    /// 数据通路：应当有数据（None = 读不到）、已连接、连接身份、DNS 探测通不通。
    expected: Option<bool>,
    connected: bool,
    conn: Conn,
    probe_ok: bool,
    /// 每次探测绑定的接口。
    probes: Vec<String>,
    /// `resolve` 怎么回：None = 原样；Some(Ok(())) = 不算事务；Some(Err) = 拒绝。
    resolve: Option<Result<(), String>>,
}

#[derive(Clone)]
struct Dev(Arc<Mutex<DevState>>);

fn sim(n: u8) -> SimId {
    SimId {
        iccid: format!("8986000000000000000{n}"),
        slot: 1,
    }
}

impl Dev {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(DevState {
            net_select: "WL_AND_5G".into(),
            registered: true,
            sim: sim(1),
            apply: Apply::Take,
            read_fail: false,
            writes: vec![],
            reads: 0,
            boot: "boot-a".into(),
            clock_base: Instant::now(),
            write_delay: Duration::ZERO,
            expected: Some(true),
            connected: true,
            conn: Conn {
                ipv4: "10.0.0.1".into(),
                up_since_ms: Some(1),
                ..Conn::default()
            },
            probe_ok: true,
            probes: vec![],
            resolve: None,
        })))
    }
    fn s(&self) -> MutexGuard<'_, DevState> {
        self.0.lock().unwrap()
    }
    /// 整机重启：新 boot_id，时钟从 0 开始。
    fn reboot(&self, boot: &str) {
        let mut s = self.s();
        s.boot = boot.into();
        s.clock_base = Instant::now();
    }
}

impl Device for Dev {
    async fn read(&self, _spec: &'static Spec) -> Result<Reading, String> {
        let mut s = self.s();
        s.reads += 1;
        if s.read_fail {
            return Err("read failed".into());
        }
        Ok(Reading {
            value: Some(json!(s.net_select)),
            registered: s.registered,
            sim: s.sim.clone(),
            data: Some(DataPath {
                expected: s.expected,
                connected: s.connected,
                conn: s.conn.clone(),
                iface: "rmnet_data0".into(),
                dns: vec!["192.0.2.53".into()],
            }),
            probe: None,
        })
    }
    async fn probe(&self, target: &ProbeTarget) -> Result<(), String> {
        let mut s = self.s();
        // IPv6 探测记成「接口/v6」（D41）
        s.probes.push(if target.v6 {
            format!("{}/v6", target.iface)
        } else {
            target.iface.clone()
        });
        if s.probe_ok {
            Ok(())
        } else {
            Err("no answer".into())
        }
    }
    async fn resolve(
        &self,
        _spec: &'static Spec,
        target: Value,
        _params: &Value,
    ) -> Result<Option<Value>, String> {
        match self.s().resolve.clone() {
            None => Ok(Some(target)),
            Some(Ok(())) => Ok(None),
            Some(Err(e)) => Err(e),
        }
    }
    async fn write(
        &self,
        _spec: &'static Spec,
        value: &Value,
        _params: &Value,
    ) -> Result<Value, WriteError> {
        let value = value.as_str().unwrap_or_default();
        let (apply, delay) = {
            let mut s = self.s();
            s.writes.push(value.into());
            (s.apply, s.write_delay)
        };
        tokio::time::sleep(delay).await;
        match apply {
            Apply::Take => {
                self.s().net_select = value.into();
                Ok(json!({"result":"success"}))
            }
            Apply::Ignore => Ok(json!({"result":"success"})),
            Apply::Fail => Err(WriteError {
                message: "ubus call failed".into(),
                timed_out: false,
            }),
            Apply::Hang => std::future::pending().await,
            Apply::TimeoutThenTake => {
                let d = self.clone();
                let v = value.to_owned();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(6)).await;
                    d.s().net_select = v;
                });
                Err(WriteError {
                    message: "Request timed out".into(),
                    timed_out: true,
                })
            }
        }
    }
    fn now_ms(&self) -> u64 {
        // 从 1 秒开始，避免 0 当成「没有」
        1_000 + self.s().clock_base.elapsed().as_millis() as u64
    }
    fn boot_id(&self) -> String {
        self.s().boot.clone()
    }
}

const DEADLINE: u64 = 120_000;

fn cfg(rollback: bool) -> Config {
    Config {
        rollback,
        poll: Duration::from_secs(2),
        legacy_ttl_ms: 120_000,
    }
}

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("datad-ops-{:016x}", rand::random::<u64>()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 「进程没了」：把落盘的文件复制到一个新目录（旧引擎的驱动在测试里还活着，会继续往旧目录写）。
fn crashed(dir: &std::path::Path) -> PathBuf {
    let new = temp_dir();
    for f in ["pending.json", "takeover"] {
        if let Ok(b) = std::fs::read(dir.join(f)) {
            std::fs::write(new.join(f), b).unwrap();
        }
    }
    new
}

fn engine(dev: &Dev, rollback: bool, dir: Option<PathBuf>) -> Engine<Dev> {
    Engine::new(
        dev.clone(),
        cfg(rollback),
        Store::open(dir),
        Record::default(),
    )
}

fn spec() -> &'static Spec {
    &SPECS[0]
}

fn req(target: &str, source: Source) -> Request {
    Request {
        spec: spec(),
        target: json!(target),
        params: json!({ "mode": target }),
        source,
        approved_by: source,
        op_id: None,
        undo: false,
        rollback: None,
        expect_iccid: None,
    }
}

fn op_of(s: &Submit) -> Value {
    match s {
        Submit::Applied { op, .. } | Submit::Existing(op) => op.clone(),
        other => panic!("not applied: {other:?}"),
    }
}

fn id(op: &Value) -> String {
    op["op_id"].as_str().unwrap().to_owned()
}

/// 等到这个事务结束（最多 10 分钟假时间），返回最后的状态。
async fn settle(e: &Engine<Dev>, op_id: &str) -> Value {
    for _ in 0..600 {
        let s = e.status(Some(op_id));
        let phase = s["phase"].as_str().unwrap_or_default();
        if !matches!(
            phase,
            "accepted" | "applying" | "verifying" | "rolling_back"
        ) {
            return s;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("{op_id} never finished: {}", e.status(Some(op_id)));
}

fn end(s: &Value) -> (String, String) {
    (
        s["phase"].as_str().unwrap_or_default().into(),
        s["reason"].as_str().unwrap_or_default().into(),
    )
}

fn pair(p: &str, r: &str) -> (String, String) {
    (p.into(), r.into())
}

#[tokio::test(start_paused = true)]
async fn confirms_when_readback_matches_and_registered() {
    let dev = Dev::new();
    let e = engine(&dev, false, None);
    let s = e.submit(req("Only_LTE", Source::Screen)).await;
    let op = op_of(&s);
    assert_eq!(op["phase"], "verifying");
    assert_eq!(op["old"], "WL_AND_5G");
    let fin = settle(&e, &id(&op)).await;
    assert_eq!(end(&fin), pair("confirmed", "verified"));
    assert_eq!(fin["data_ok"], true);
    assert_eq!(dev.s().writes, ["Only_LTE"]);
    // 数据通是在蜂窝接口上探测出来的
    assert_eq!(dev.s().probes, ["rmnet_data0"]);
}

#[tokio::test(start_paused = true)]
async fn connected_but_dns_dead_rolls_back() {
    let dev = Dev::new();
    dev.s().probe_ok = false;
    let e = engine(&dev, true, None);
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    let d = dev.clone();
    tokio::spawn(async move {
        // 退回发出去以后旧设置是通的
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let mut s = d.s();
            if s.writes.len() == 2 {
                s.probe_ok = true;
                break;
            }
        }
    });
    let fin = settle(&e, &id(&op)).await;
    assert_eq!(end(&fin), pair("rolled_back", "timeout"));
    assert_eq!(fin["ever_matched"], true);
    assert_eq!(dev.s().writes, ["Only_LTE", "WL_AND_5G"]);
    // 新设置上：每拍 2 秒，第 2、8、14 秒一轮，隔 30 秒在 44、50、56 秒，再在 86、92、98 秒，
    // 下一轮 128 秒已过 120 秒的时限 → 9 次；退回后再探测一次就通了
    assert_eq!(dev.s().probes.len(), 10);
}

#[tokio::test(start_paused = true)]
async fn roaming_with_roaming_off_confirms_without_a_single_probe() {
    let dev = Dev::new();
    dev.s().expected = Some(false);
    dev.s().connected = false;
    let e = engine(&dev, true, None);
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    assert_eq!(
        end(&settle(&e, &id(&op)).await),
        pair("confirmed", "verified")
    );
    assert!(dev.s().probes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn unreadable_data_path_does_not_confirm() {
    let dev = Dev::new();
    dev.s().expected = None;
    let e = engine(&dev, false, None);
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(e.status(Some(&id(&op)))["phase"], "verifying");
    dev.s().expected = Some(true);
    assert_eq!(
        end(&settle(&e, &id(&op)).await),
        pair("confirmed", "verified")
    );
    assert!(dev.s().probes.len() == 1);
}

#[tokio::test(start_paused = true)]
async fn waits_for_registration() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let op = op_of(&e.submit(req("Only_LTE", Source::Web)).await);
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(e.status(Some(&id(&op)))["phase"], "verifying");
    dev.s().registered = true;
    assert_eq!(
        end(&settle(&e, &id(&op)).await),
        pair("confirmed", "verified")
    );
}

#[tokio::test(start_paused = true)]
async fn not_through_with_rollback_off_is_unverified() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, false, None);
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    let fin = settle(&e, &id(&op)).await;
    assert_eq!(end(&fin), pair("unverified", "no_rollback"));
    // 自动退回关着：只写了一次
    assert_eq!(dev.s().writes, ["Only_LTE"]);
}

#[tokio::test(start_paused = true)]
async fn not_through_rolls_back_once_and_confirms_the_rollback() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let t0 = Instant::now();
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    // 退回发出后，旧制式能注册上
    let d = dev.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let mut s = d.s();
            if s.net_select == "WL_AND_5G" {
                s.registered = true;
                break;
            }
        }
    });
    let fin = settle(&e, &id(&op)).await;
    assert_eq!(end(&fin), pair("rolled_back", "timeout"));
    assert_eq!(dev.s().writes, ["Only_LTE", "WL_AND_5G"]);
    assert!(t0.elapsed() >= Duration::from_millis(DEADLINE));
}

#[tokio::test(start_paused = true)]
async fn rollback_that_does_not_come_through_fails_and_stops() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    let fin = settle(&e, &id(&op)).await;
    assert_eq!(end(&fin), pair("rollback_failed", "rollback_timeout"));
    tokio::time::sleep(Duration::from_secs(600)).await;
    // 不再自动动它
    assert_eq!(dev.s().writes, ["Only_LTE", "WL_AND_5G"]);
}

#[tokio::test(start_paused = true)]
async fn ignored_write_is_not_applied_without_rollback() {
    let dev = Dev::new();
    dev.s().apply = Apply::Ignore;
    let e = engine(&dev, true, None);
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    let fin = settle(&e, &id(&op)).await;
    assert_eq!(end(&fin), pair("not_applied", "ignored"));
    assert_eq!(dev.s().writes, ["Only_LTE"]);
}

#[tokio::test(start_paused = true)]
async fn failed_write_reports_error_and_ends_quickly() {
    let dev = Dev::new();
    dev.s().apply = Apply::Fail;
    let e = engine(&dev, true, None);
    let t0 = Instant::now();
    let s = e.submit(req("Only_LTE", Source::Screen)).await;
    let Submit::Applied { result, op } = s else {
        panic!("{s:?}")
    };
    assert_eq!(result, Err("ubus call failed".into()));
    let fin = settle(&e, &id(&op)).await;
    assert_eq!(end(&fin), pair("not_applied", "ignored"));
    assert!(t0.elapsed() < Duration::from_secs(10));
}

#[tokio::test(start_paused = true)]
async fn third_value_mid_wait_is_manual_change() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(5)).await;
    dev.s().net_select = "Only_5G".into();
    let fin = settle(&e, &id(&op)).await;
    assert_eq!(end(&fin), pair("cancelled", "manual_change"));
    assert_eq!(dev.s().writes, ["Only_LTE"]);
}

#[tokio::test(start_paused = true)]
async fn sim_change_abandons_without_writing_old_value() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(5)).await;
    dev.s().sim = sim(2);
    let fin = settle(&e, &id(&op)).await;
    assert_eq!(end(&fin), pair("cancelled", "sim_changed"));
    tokio::time::sleep(Duration::from_secs(300)).await;
    assert_eq!(dev.s().writes, ["Only_LTE"]);
}

#[tokio::test(start_paused = true)]
async fn other_writers_get_busy_with_who_and_what() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let first = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(3)).await;
    for src in [Source::Scenario, Source::Scheduler, Source::Auto] {
        match e.submit(req("Only_5G", src)).await {
            Submit::Busy(d) => {
                assert_eq!(d["op_id"], first["op_id"]);
                assert_eq!(d["source"], "screen");
                assert_eq!(d["action"], "network.set_mode");
                assert!(d["age_ms"].as_u64().unwrap() >= 3_000);
                // V2-39：带一句话
                assert_eq!(d["say_zh"], "正在换制式（触屏发起，3 秒），稍等");
                assert_eq!(d["say_en"], "Busy: network mode (Screen)");
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(dev.s().writes, ["Only_LTE"]);
}

#[tokio::test(start_paused = true)]
async fn same_item_user_write_supersedes_and_inherits_rollback_target() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let a = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let b = op_of(&e.submit(req("Only_5G", Source::Web)).await);
    assert_eq!(
        end(&e.status(Some(&id(&a)))),
        pair("cancelled", "superseded")
    );
    assert_eq!(b["old"], "Only_LTE");
    // D35：退回到最后确认过的值，不是没确认的中间值
    assert_eq!(b["rollback_to"], "WL_AND_5G");
    let fin = settle(&e, &id(&b)).await;
    assert_eq!(fin["phase"], "rollback_failed");
    assert_eq!(dev.s().writes, ["Only_LTE", "Only_5G", "WL_AND_5G"]);
}

#[tokio::test(start_paused = true)]
async fn guard_may_override_its_own_item() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let a = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let b = op_of(&e.submit(req("WL_AND_5G", Source::Guard)).await);
    assert_eq!(e.status(Some(&id(&a)))["reason"], "superseded");
    assert_eq!(b["source"], "guard");
}

#[tokio::test(start_paused = true)]
async fn write_in_flight_cannot_be_overridden() {
    let dev = Dev::new();
    dev.s().write_delay = Duration::from_secs(5);
    let e = engine(&dev, true, None);
    let e2 = e.clone();
    let first = tokio::spawn(async move { e2.submit(req("Only_LTE", Source::Screen)).await });
    tokio::time::sleep(Duration::from_secs(1)).await;
    match e.submit(req("Only_5G", Source::Screen)).await {
        Submit::Busy(d) => assert_eq!(d["phase"], "applying"),
        other => panic!("{other:?}"),
    }
    first.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn user_revert_and_keep() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, false, None);
    let a = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(5)).await;
    dev.s().registered = true;
    // 自动退回关着，「立即退回」照样能用
    let v = e.revert(&id(&a)).unwrap();
    assert_eq!(v["phase"], "rolling_back");
    assert_eq!(v["rollback_reason"], "user_revert");
    assert_eq!(
        end(&settle(&e, &id(&a)).await),
        pair("rolled_back", "user_revert")
    );
    assert_eq!(dev.s().writes, ["Only_LTE", "WL_AND_5G"]);
    assert!(e.revert(&id(&a)).is_err());

    dev.s().registered = false;
    let b = op_of(&e.submit(req("Only_LTE", Source::Web)).await);
    let v = e.keep(&id(&b)).unwrap();
    assert_eq!(end(&v), pair("confirmed", "user_keep"));
    assert!(e.keep(&id(&b)).is_err());
}

#[tokio::test(start_paused = true)]
async fn op_id_is_idempotent() {
    let dev = Dev::new();
    let e = engine(&dev, false, None);
    let mut r = req("Only_LTE", Source::Screen);
    r.op_id = Some("web-1".into());
    let a = op_of(&e.submit(r).await);
    assert_eq!(a["op_id"], "web-1");
    let mut r = req("Only_LTE", Source::Screen);
    r.op_id = Some("web-1".into());
    assert!(matches!(e.submit(r).await, Submit::Existing(_)));
    settle(&e, "web-1").await;
    let mut r = req("Only_5G", Source::Screen);
    r.op_id = Some("web-1".into());
    let again = op_of(&e.submit(r).await);
    assert_eq!(again["target"], "Only_LTE");
    assert_eq!(dev.s().writes, ["Only_LTE"]);
}

#[tokio::test(start_paused = true)]
async fn capture_failure_does_not_touch_the_device() {
    let dev = Dev::new();
    dev.s().read_fail = true;
    let e = engine(&dev, true, None);
    assert!(matches!(
        e.submit(req("Only_LTE", Source::Screen)).await,
        Submit::NoCapture(_)
    ));
    assert!(dev.s().writes.is_empty());
    // 锁没被占着
    dev.s().read_fail = false;
    op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
}

#[tokio::test(start_paused = true)]
async fn safety_write_preempts_and_clears_legacy_queue() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let a = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(3)).await;
    e.lock().legacy.push_back(LegacyReq {
        item: NETWORK_MODE,
        action: "network.set_mode".into(),
        params: json!({"mode":"Only_5G"}),
        at_ms: e.now(),
    });
    assert_eq!(e.legacy_gate(None, true), LegacyGate::Pass);
    assert_eq!(
        end(&e.status(Some(&id(&a)))),
        pair("cancelled", "preempted")
    );
    assert!(e.lock().legacy.is_empty());
    tokio::time::sleep(Duration::from_secs(300)).await;
    assert_eq!(dev.s().writes, ["Only_LTE"]);
}

fn recording_runner(e: &Engine<Dev>, busy_first: usize) -> Arc<Mutex<Vec<(String, Value)>>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let c = calls.clone();
    let left = Arc::new(Mutex::new(busy_first));
    e.set_legacy_runner(Arc::new(move |action, params| {
        let c = c.clone();
        let left = left.clone();
        Box::pin(async move {
            let mut l = left.lock().unwrap();
            if *l > 0 {
                *l -= 1;
                return false;
            }
            c.lock().unwrap().push((action, params));
            true
        })
    }));
    calls
}

#[tokio::test(start_paused = true)]
async fn legacy_requests_queue_while_a_write_is_in_flight_and_run_after() {
    let dev = Dev::new();
    dev.s().write_delay = Duration::from_secs(5);
    let e = engine(&dev, true, None);
    let calls = recording_runner(&e, 0);
    let e2 = e.clone();
    let first = tokio::spawn(async move { e2.submit(req("Only_LTE", Source::Screen)).await });
    tokio::time::sleep(Duration::from_secs(1)).await;
    // 旧请求：同一项但写还在途 → 排队；只留最新的
    assert_eq!(e.legacy_gate(Some(NETWORK_MODE), false), LegacyGate::Queue);
    e.enqueue_legacy(NETWORK_MODE, "network.set_mode", json!({"mode":"Only_5G"}));
    e.enqueue_legacy(
        NETWORK_MODE,
        "network.set_mode",
        json!({"mode":"WL_AND_5G"}),
    );
    assert!(calls.lock().unwrap().is_empty());
    let op = op_of(&first.await.unwrap());
    settle(&e, &id(&op)).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        *calls.lock().unwrap(),
        [("network.set_mode".to_string(), json!({"mode":"WL_AND_5G"}))]
    );
}

#[tokio::test(start_paused = true)]
async fn legacy_same_item_while_verifying_supersedes() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let a = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(e.legacy_gate(Some(NETWORK_MODE), false), LegacyGate::Pass);
    assert_eq!(e.status(Some(&id(&a)))["reason"], "superseded");
    // 不在描述表里的动作照旧
    assert_eq!(e.legacy_gate(None, false), LegacyGate::Pass);
}

#[tokio::test(start_paused = true)]
async fn legacy_queue_retries_when_executor_is_full_and_expires() {
    let dev = Dev::new();
    let e = engine(&dev, true, None);
    let calls = recording_runner(&e, 2);
    e.enqueue_legacy(NETWORK_MODE, "network.set_mode", json!({"mode":"Only_LTE"}));
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(calls.lock().unwrap().len(), 1);

    // 锁一直被占着：120 秒后丢掉
    let dev = Dev::new();
    dev.s().registered = false;
    let e = Engine::new(
        dev.clone(),
        Config {
            legacy_ttl_ms: 60_000,
            ..cfg(false)
        },
        Store::open(None),
        Record::default(),
    );
    let calls = recording_runner(&e, 0);
    let a = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    e.enqueue_legacy(NETWORK_MODE, "network.set_mode", json!({"mode":"Only_5G"}));
    settle(&e, &id(&a)).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(calls.lock().unwrap().is_empty());
    assert!(e.lock().legacy.is_empty());
}

// ---- 落盘与续跑 ----

#[tokio::test(start_paused = true)]
async fn no_pending_file_means_no_device_reads_at_start() {
    let dev = Dev::new();
    let e = engine(&dev, true, Some(temp_dir()));
    e.start().await;
    assert_eq!(dev.s().reads, 0);
    assert_eq!(e.status(None), Value::Null);
}

#[tokio::test(start_paused = true)]
async fn finished_change_leaves_no_pending_file() {
    let dir = temp_dir();
    let dev = Dev::new();
    let e = engine(&dev, false, Some(dir.clone()));
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    assert!(dir.join("pending.json").exists());
    settle(&e, &id(&op)).await;
    assert!(!dir.join("pending.json").exists());
}

#[tokio::test(start_paused = true)]
async fn crash_mid_apply_resumes_without_reapplying() {
    let dir = temp_dir();
    let dev = Dev::new();
    dev.s().apply = Apply::Hang;
    let e = engine(&dev, true, Some(dir.clone()));
    let e2 = e.clone();
    tokio::spawn(async move { e2.submit(req("Only_LTE", Source::Screen)).await });
    tokio::time::sleep(Duration::from_secs(1)).await;
    let saved: Txn =
        serde_json::from_slice(&std::fs::read(dir.join("pending.json")).unwrap()).unwrap();
    assert_eq!(saved.phase, Phase::Applying);

    // datad 被杀、拉起：新引擎，同一个目录。设备其实已经切过去了。
    let dev2 = Dev::new();
    dev2.s().net_select = "Only_LTE".into();
    let e3 = engine(&dev2, true, Some(dir.clone()));
    e3.start().await;
    let fin = settle(&e3, &saved.op_id).await;
    assert_eq!(end(&fin), pair("confirmed", "verified"));
    assert!(dev2.s().writes.is_empty());
}

#[tokio::test(start_paused = true)]
/// 崩在退回写已经生效、还没记下「已发」：重启后读回已经是退回目标，不再写（O7 的另一半）。
async fn crash_mid_rollback_never_sends_a_second_rollback() {
    let dir = temp_dir();
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, Some(dir.clone()));
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    // 退回的写一直不回：停在「退回意图已落盘」
    tokio::time::sleep(Duration::from_secs(60)).await;
    dev.s().apply = Apply::Hang;
    tokio::time::sleep(Duration::from_secs(70)).await;
    let saved: Txn =
        serde_json::from_slice(&std::fs::read(dir.join("pending.json")).unwrap()).unwrap();
    assert_eq!(saved.phase, Phase::RollingBack);
    assert_eq!(saved.intent, Some(crate::ops::txn::Intent::Rollback));

    for boot in ["boot-a", "boot-b"] {
        let dir = crashed(&dir);
        let dev2 = Dev::new();
        dev2.reboot(boot);
        dev2.s().net_select = "WL_AND_5G".into();
        let e2 = engine(&dev2, true, Some(dir.clone()));
        e2.start().await;
        let fin = settle(&e2, &id(&op)).await;
        assert_eq!(end(&fin), pair("rolled_back", "timeout"), "{boot}");
        assert!(dev2.s().writes.is_empty(), "{boot}");
    }
}

/// O7：崩在「退回意图已落盘、还没写」：重启后读回还是目标值，照发一次退回（同一次开机和整机重启都一样）。
#[tokio::test(start_paused = true)]
async fn crash_before_the_rollback_write_sends_it_after_restart() {
    let dir = temp_dir();
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, Some(dir.clone()));
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(60)).await;
    // 发退回之前一直核对不了 SIM：停在「退回意图已落盘」，还没写
    dev.s().sim = SimId {
        iccid: String::new(),
        slot: 0,
    };
    tokio::time::sleep(Duration::from_secs(70)).await;
    let saved: Txn =
        serde_json::from_slice(&std::fs::read(dir.join("pending.json")).unwrap()).unwrap();
    assert_eq!(saved.phase, Phase::RollingBack);
    assert_eq!(saved.intent, Some(crate::ops::txn::Intent::Rollback));
    assert_eq!(dev.s().writes, ["Only_LTE"]);

    for boot in ["boot-a", "boot-b"] {
        let dir = crashed(&dir);
        let dev2 = Dev::new();
        dev2.reboot(boot);
        dev2.s().net_select = "Only_LTE".into();
        let e2 = engine(&dev2, true, Some(dir.clone()));
        e2.start().await;
        let fin = settle(&e2, &id(&op)).await;
        assert_eq!(end(&fin), pair("rolled_back", "timeout"), "{boot}");
        assert_eq!(dev2.s().writes, ["WL_AND_5G"], "{boot}");
    }
}

/// O7：重启后一直读不到设备（核对不了 SIM）：退回发不出去也不永远占着锁，到点记 rollback_failed。
#[tokio::test(start_paused = true)]
async fn rollback_that_cannot_be_sent_after_restart_gives_up() {
    let dir = temp_dir();
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, Some(dir.clone()));
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(60)).await;
    dev.s().apply = Apply::Hang;
    tokio::time::sleep(Duration::from_secs(70)).await;

    let dir = crashed(&dir);
    let dev2 = Dev::new();
    dev2.s().net_select = "Only_LTE".into();
    dev2.s().read_fail = true;
    let e2 = engine(&dev2, true, Some(dir.clone()));
    e2.start().await;
    let fin = settle(&e2, &id(&op)).await;
    assert_eq!(end(&fin), pair("rollback_failed", "rollback_timeout"));
    assert!(dev2.s().writes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn reboot_restarts_the_clock_then_second_reboot_rolls_back() {
    let dir = temp_dir();
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, Some(dir.clone()));
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(30)).await;
    let dir1 = crashed(&dir);

    // 第 1 次开机：接着等，时限从开机重新计
    let dev1 = Dev::new();
    dev1.reboot("boot-b");
    dev1.s().net_select = "Only_LTE".into();
    dev1.s().registered = false;
    let e1 = engine(&dev1, true, Some(dir1.clone()));
    e1.start().await;
    let s = e1.status(Some(&id(&op)));
    assert_eq!(s["phase"], "verifying");
    assert_eq!(s["remaining_ms"], DEADLINE);
    assert!(dev1.s().writes.is_empty());
    tokio::time::sleep(Duration::from_secs(20)).await;
    let dir2 = crashed(&dir1);
    let t1: Txn =
        serde_json::from_slice(&std::fs::read(dir2.join("pending.json")).unwrap()).unwrap();
    assert_eq!(t1.boots, 1);

    // 第 2 次开机仍未确认：开机马上退回
    let dev2 = Dev::new();
    dev2.reboot("boot-c");
    dev2.s().net_select = "Only_LTE".into();
    let e2 = engine(&dev2, true, Some(dir2.clone()));
    e2.start().await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(dev2.s().writes, ["WL_AND_5G"]);
    let fin = settle(&e2, &id(&op)).await;
    assert_eq!(end(&fin), pair("rolled_back", "reboot_loop"));
}

#[tokio::test(start_paused = true)]
async fn takeover_marker_abandons_the_pending_change() {
    let dir = temp_dir();
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, Some(dir.clone()));
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(5)).await;
    std::fs::write(dir.join("takeover"), b"").unwrap();
    let dir = crashed(&dir);

    let dev2 = Dev::new();
    let e2 = engine(&dev2, true, Some(dir.clone()));
    e2.start().await;
    assert_eq!(
        end(&e2.status(Some(&id(&op)))),
        pair("cancelled", "takeover")
    );
    assert!(!dir.join("takeover").exists());
    assert!(!dir.join("pending.json").exists());
    tokio::time::sleep(Duration::from_secs(300)).await;
    assert_eq!(dev2.s().reads, 0);
    assert!(dev2.s().writes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn unreadable_pending_file_is_set_aside() {
    let dir = temp_dir();
    std::fs::write(dir.join("pending.json"), b"{not json").unwrap();
    let dev = Dev::new();
    let e = engine(&dev, true, Some(dir.clone()));
    e.start().await;
    assert!(dir.join("pending.json.bad").exists());
    assert_eq!(dev.s().reads, 0);
}

#[test]
fn op_ids() {
    assert!(valid_op_id("web-1.a_B"));
    assert!(!valid_op_id(""));
    assert!(!valid_op_id("a b"));
    assert!(!valid_op_id(&"x".repeat(65)));
}

// ---- D32：退回之前也核对 SIM；身份临时读空不算换卡 ----

#[tokio::test(start_paused = true)]
async fn second_boot_with_another_sim_does_not_roll_back() {
    let dir = temp_dir();
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, Some(dir.clone()));
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let mut t: Txn =
        serde_json::from_slice(&std::fs::read(dir.join("pending.json")).unwrap()).unwrap();
    t.boots = 1;
    let dir = crashed(&dir);
    std::fs::write(dir.join("pending.json"), serde_json::to_vec(&t).unwrap()).unwrap();

    // 第 2 次开机，卡换了：不往新卡上写旧卡的值
    let dev2 = Dev::new();
    dev2.reboot("boot-c");
    dev2.s().sim = sim(2);
    let e2 = engine(&dev2, true, Some(dir.clone()));
    e2.start().await;
    let fin = settle(&e2, &id(&op)).await;
    assert_eq!(end(&fin), pair("cancelled", "sim_changed"));
    assert!(dev2.s().writes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn revert_after_sim_change_does_not_write() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, false, None);
    let a = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    // 读不到设备的那几拍里换了卡
    dev.s().read_fail = true;
    tokio::time::sleep(Duration::from_secs(5)).await;
    dev.s().sim = sim(2);
    e.revert(&id(&a)).unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    dev.s().read_fail = false;
    let fin = settle(&e, &id(&a)).await;
    assert_eq!(end(&fin), pair("cancelled", "sim_changed"));
    assert_eq!(dev.s().writes, ["Only_LTE"]);
}

#[tokio::test(start_paused = true)]
async fn sim_identity_blank_for_a_while_is_not_a_sim_change() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let a = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    // 重新注册时 ICCID 和卡槽临时读空两拍
    dev.s().sim = SimId {
        iccid: String::new(),
        slot: 0,
    };
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(e.status(Some(&id(&a)))["phase"], "verifying");
    {
        let mut s = dev.s();
        s.sim = sim(1);
        s.registered = true;
    }
    assert_eq!(
        end(&settle(&e, &id(&a)).await),
        pair("confirmed", "verified")
    );
}

#[tokio::test(start_paused = true)]
async fn no_sim_device_still_confirms() {
    let dev = Dev::new();
    dev.s().sim = SimId {
        iccid: String::new(),
        slot: 0,
    };
    let e = engine(&dev, true, None);
    let a = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    assert_eq!(
        end(&settle(&e, &id(&a)).await),
        pair("confirmed", "verified")
    );
}

#[tokio::test(start_paused = true)]
async fn timed_out_write_is_unknown_and_confirmed_by_readback() {
    let dev = Dev::new();
    dev.s().apply = Apply::TimeoutThenTake;
    let e = engine(&dev, true, None);
    let s = e.submit(req("Only_LTE", Source::Screen)).await;
    let Submit::Applied { result, op } = s else {
        panic!("{s:?}")
    };
    assert_eq!(result, Err("Request timed out".into()));
    assert_eq!(op["phase"], "verifying");
    // 前几拍读回还是旧值：不判 not_applied
    let fin = settle(&e, &id(&op)).await;
    assert_eq!(end(&fin), pair("confirmed", "verified"));
    assert_eq!(fin["data_ok"], true);
    assert_eq!(dev.s().writes, ["Only_LTE"]);
    // 数据通是在蜂窝接口上探测出来的
    assert_eq!(dev.s().probes, ["rmnet_data0"]);
}

#[tokio::test(start_paused = true)]
async fn finished_changes_and_legacy_queue_are_journaled() {
    let dir = temp_dir();
    let dev = Dev::new();
    let e = Engine::new(
        dev.clone(),
        cfg(false),
        Store::open(None),
        Record::open_with(Some(dir.clone()), 1 << 20),
    );
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    settle(&e, &id(&op)).await;
    e.enqueue_legacy(NETWORK_MODE, "network.set_mode", json!({"mode":"A"}));
    e.enqueue_legacy(NETWORK_MODE, "network.set_mode", json!({"mode":"B"}));
    e.clear_legacy("device.reboot");
    // 写者是真线程，测试的时钟是暂停的：按真实时间等它落盘。
    let mut lines = vec![];
    for _ in 0..200 {
        lines = e.record().list(10);
        if lines.len() >= 5 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let what: Vec<(&str, &str)> = lines
        .iter()
        .rev()
        .map(|l| {
            (
                l["source"].as_str().unwrap_or_default(),
                l["result"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        what,
        [
            ("screen", "confirmed"),
            ("legacy", "queued"),
            ("legacy", "replaced"),
            ("legacy", "queued"),
            ("legacy", "dropped"),
        ]
    );
    let done = &lines[4];
    assert_eq!(
        (
            done["old"].as_str(),
            done["new"].as_str(),
            done["readback"].as_str()
        ),
        (Some("WL_AND_5G"), Some("Only_LTE"), Some("Only_LTE"))
    );
    assert_eq!(done["sim"], "0001/1");
    let owner = &e.record().owners()["network.mode"];
    assert_eq!(
        (owner["source"].as_str(), owner["value"].as_str()),
        (Some("screen"), Some("Only_LTE"))
    );
}

// ── 搜网会话（D17，E4 T7b）──────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn session_holds_the_lock_for_connectivity_writes_only() {
    let dev = Dev::new();
    let e = engine(&dev, false, None);
    let open = e.session_open(Source::Web).unwrap();
    let sid = open["session"].as_str().unwrap().to_owned();
    // 网络模式、其他影响上网的写：409，说清是会话
    match e.submit(req("Only_LTE", Source::Screen)).await {
        Submit::Busy(d) => assert_eq!(d["action"], "netselect.session"),
        other => panic!("{other:?}"),
    }
    for a in ["cellular.set", "band.set_lte", "apn.enable", "modem.online"] {
        assert!(
            matches!(e.session_gate(a, None), Err(SessionError::Busy(_))),
            "{a}"
        );
    }
    // 短信、USB 这些照常
    for a in ["sms.delete", "sms.mark_read", "usb.set", "journal.append"] {
        assert_eq!(e.session_gate(a, None), Ok(()), "{a}");
    }
    // 步骤：要带当前会话号
    assert_eq!(e.session_gate("netselect.scan", Some(&sid)), Ok(()));
    assert!(matches!(
        e.session_gate("netselect.register", None),
        Err(SessionError::Busy(_))
    ));
    assert!(matches!(
        e.session_gate("netselect.register", Some("other")),
        Err(SessionError::Busy(_))
    ));
    assert_eq!(e.session_gate("netselect.auto", Some(&sid)), Ok(()));
    // 第二个会话、旧请求
    assert!(matches!(
        e.session_open(Source::Web),
        Err(SessionError::Busy(_))
    ));
    assert_eq!(e.legacy_gate(Some(NETWORK_MODE), false), LegacyGate::Queue);
    // 关掉以后一切照常
    e.session_close(&sid, Some("registered")).unwrap();
    assert_eq!(e.session_gate("cellular.set", None), Ok(()));
    assert!(matches!(
        e.submit(req("Only_LTE", Source::Screen)).await,
        Submit::Applied { .. }
    ));
    assert_eq!(e.session_close(&sid, None), Err(SessionError::Gone));
}

#[tokio::test(start_paused = true)]
async fn steps_need_a_session_but_revert_and_redial_do_not() {
    let dev = Dev::new();
    let e = engine(&dev, false, None);
    assert_eq!(
        e.session_gate("netselect.scan", None),
        Err(SessionError::Gone)
    );
    assert_eq!(
        e.session_gate("netselect.register", Some("x")),
        Err(SessionError::Gone)
    );
    // guard 在会话结束后退回自动、重拨（D15）
    assert_eq!(e.session_gate("netselect.auto", None), Ok(()));
    assert_eq!(e.session_gate("netselect.auto", Some("ended-one")), Ok(()));
    assert_eq!(e.session_gate("cellular.redial", None), Ok(()));
}

#[tokio::test(start_paused = true)]
async fn session_cannot_open_over_a_change_in_progress() {
    let dev = Dev::new();
    let e = engine(&dev, false, None);
    let op = op_of(&e.submit(req("Only_LTE", Source::Web)).await);
    match e.session_open(Source::Web) {
        Err(SessionError::Busy(d)) => assert_eq!(d["op_id"], op["op_id"]),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn an_agent_that_stops_renewing_loses_the_session_and_queued_requests_run() {
    let dev = Dev::new();
    let e = engine(&dev, false, None);
    let calls = recording_runner(&e, 0);
    let sid = e.session_open(Source::Web).unwrap()["session"]
        .as_str()
        .unwrap()
        .to_owned();
    // 续着就一直在（续到 2 分钟）
    for _ in 0..4 {
        tokio::time::sleep(Duration::from_secs(30)).await;
        e.session_renew(&sid).unwrap();
    }
    // 会话期间来的旧请求排着（120 秒内不过期）
    e.enqueue_legacy(NETWORK_MODE, "network.set_mode", json!({"mode":"Only_5G"}));
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(calls.lock().unwrap().is_empty());
    // 不续了：租约 60 秒后收回，排着的旧请求接着做
    tokio::time::sleep(Duration::from_millis(SESSION_LEASE_MS + 2_000)).await;
    assert_eq!(e.session_renew(&sid), Err(SessionError::Gone));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(e.session_gate("cellular.set", None), Ok(()));
}

#[tokio::test(start_paused = true)]
async fn a_session_never_outlives_seven_minutes() {
    let dev = Dev::new();
    let e = engine(&dev, false, None);
    let sid = e.session_open(Source::Web).unwrap()["session"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut alive = 0;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(20)).await;
        if e.session_renew(&sid).is_ok() {
            alive += 1;
        }
    }
    // 7 分钟 = 21 次 20 秒
    assert!((20..=21).contains(&alive), "{alive}");
    assert_eq!(
        e.session_gate("netselect.scan", Some(&sid)),
        Err(SessionError::Gone)
    );
}

// ---- E4 T13：op 块、「知道了」（STATE_V2.md V2-34、V2-37） ----

fn observed(e: &Engine<Dev>) -> Arc<Mutex<Vec<Value>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    e.set_observer(Arc::new(move |v| s.lock().unwrap().push(v)));
    seen
}

fn latest(seen: &Arc<Mutex<Vec<Value>>>) -> Value {
    seen.lock().unwrap().last().cloned().unwrap()
}

#[tokio::test(start_paused = true)]
async fn op_block_follows_the_engine() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let seen = observed(&e);
    // 接上时马上交一次：没有事务
    assert_eq!(
        latest(&seen),
        json!({"rollback_enabled": true, "active": null, "last": null, "notice": "rollback_on"})
    );
    let op = op_of(&e.submit(req("Only_LTE", Source::Web)).await);
    let b = latest(&seen);
    assert_eq!(b["active"]["op_id"], op["op_id"]);
    assert_eq!(b["active"]["phase"], "verifying");
    assert_eq!(b["active"]["say_zh"], "正在确认");
    assert_eq!(b["active"]["source_zh"], "网页");
    assert!(b["last"].is_null());
    // 写之前已经交过一次「正在换制式」（意图落盘时）
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|v| v["active"]["phase"] == "applying" && v["active"]["say_zh"] == "正在换制式")
    );
    // 首页：进行中
    let so = e.screen_op().unwrap();
    assert_eq!(so.kind, crate::project::screen::ScreenOpKind::Live);
    // 立即退回：块马上变
    e.revert(&id(&op)).unwrap();
    assert_eq!(latest(&seen)["active"]["phase"], "rolling_back");
    assert_eq!(latest(&seen)["active"]["can_revert"], false);
    dev.s().registered = true;
    let fin = settle(&e, &id(&op)).await;
    assert_eq!(end(&fin), pair("rolled_back", "user_revert"));
    let b = latest(&seen);
    assert!(b["active"].is_null());
    assert_eq!(b["last"]["op_id"], op["op_id"]);
    assert_eq!(b["last"]["say_zh"], "已退回自动");
    assert_eq!(b["last"]["stay"], "brief");
    assert_eq!(b["last"]["needs_ack"], false);
    assert_eq!(b, e.block());
    // 3 秒类的不叠到首页
    assert!(e.screen_op().is_none());
}

#[tokio::test(start_paused = true)]
async fn op_block_is_quiet_without_a_change() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, false, None);
    let seen = observed(&e);
    e.publish_now();
    tokio::time::sleep(Duration::from_secs(5)).await;
    e.publish_now();
    {
        let s = seen.lock().unwrap();
        assert!(s.len() >= 2 && s.iter().all(|v| *v == s[0]), "{s:?}");
    }
    // 进行中：倒计时每轮都在变（新订阅的拿到准的）
    op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    let a = e.block();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let b = e.block();
    let r = |v: &Value| v["active"]["remaining_ms"].as_u64().unwrap();
    assert!(r(&b) < r(&a), "{a} {b}");
    assert_eq!(a["rollback_enabled"], false);
    assert_eq!(a["active"]["next_zh"], "还剩 {t} · 自动退回没开");
    // 结束以后：结果行不随时间变
    dev.s().registered = true;
    let id = a["active"]["op_id"].as_str().unwrap().to_owned();
    settle(&e, &id).await;
    let a = e.block();
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(a, e.block());
    assert!(a["last"].get("age_ms").is_none());
}

#[tokio::test(start_paused = true)]
async fn ack_is_shared_and_journaled() {
    let dev = Dev::new();
    dev.s().registered = false;
    let dir = temp_dir();
    let e = Engine::new(
        dev.clone(),
        cfg(false),
        Store::open(Some(dir.clone())),
        Record::open(Some(dir.clone())),
    );
    let op = op_of(&e.submit(req("Only_LTE", Source::Web)).await);
    let fin = settle(&e, &id(&op)).await;
    assert_eq!(end(&fin), pair("unverified", "no_rollback"));
    let b = e.block();
    assert_eq!(b["last"]["needs_ack"], true);
    assert_eq!(b["last"]["say_zh"], "没通 · 还是只用 4G");
    assert_eq!(
        e.screen_op().unwrap().kind,
        crate::project::screen::ScreenOpKind::Sticky
    );
    // 只能点最近结束的那个
    assert!(e.ack("other", Source::Web).is_err());
    // 触屏点了，网页那边也收起
    let r = e.ack(&id(&op), Source::Screen).unwrap();
    assert_eq!(r["acked"], true);
    let b = e.block();
    assert_eq!(
        (b["last"]["acked"].clone(), b["last"]["needs_ack"].clone()),
        (json!(true), json!(false))
    );
    assert!(e.screen_op().is_none());
    // 网页再点：照样成功，不重复记账
    assert!(e.ack(&id(&op), Source::Web).is_ok());
    // flush 的 2 秒上限按测试的暂停时钟算，会提前到点：按真实时间等写者线程
    let acks = || -> Vec<Value> {
        e.record()
            .list(50)
            .into_iter()
            .filter(|l| l["action"] == "op.ack")
            .collect()
    };
    for _ in 0..200 {
        if !acks().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    let acks = acks();
    assert_eq!(acks.len(), 1, "{acks:?}");
    assert_eq!(acks[0]["source"], "screen");
    assert_eq!(acks[0]["op_id"], op["op_id"]);
    assert_eq!(acks[0]["item"], NETWORK_MODE);
}

#[tokio::test(start_paused = true)]
async fn last_result_survives_restart() {
    let dev = Dev::new();
    dev.s().registered = false;
    let dir = temp_dir();
    let e = engine(&dev, false, Some(dir.clone()));
    let op = op_of(&e.submit(req("Only_LTE", Source::Web)).await);
    settle(&e, &id(&op)).await;
    // datad 重启：结果还在，还没点
    let e2 = engine(&dev, false, Some(dir.clone()));
    let b = e2.block();
    assert_eq!(b["last"]["op_id"], op["op_id"]);
    assert_eq!(b["last"]["needs_ack"], true);
    e2.ack(&id(&op), Source::Web).unwrap();
    // 再重启：点过的不再出现
    let e3 = engine(&dev, false, Some(dir));
    assert_eq!(e3.block()["last"]["acked"], true);
    assert!(e3.screen_op().is_none());
    // 下一次同一项的事务结束后接替
    dev.s().registered = true;
    let op2 = op_of(&e3.submit(req("Only_5G", Source::Screen)).await);
    settle(&e3, &id(&op2)).await;
    assert_eq!(e3.block()["last"]["op_id"], op2["op_id"]);
    // 旧的那条在最近列表里：后来又改过，不能撤销
    let old = e3.status(Some(&id(&op)));
    assert!(old.is_null() || old["undo"]["why_zh"] == "之后又改过");
}

/// 确认中（自动退回开、数据探测不通）的一个网页事务。
async fn verifying(dev: &Dev, e: &Engine<Dev>) -> String {
    dev.s().probe_ok = false;
    let op = op_of(&e.submit(req("Only_LTE", Source::Web)).await);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(e.status(Some(&id(&op)))["phase"], "verifying");
    id(&op)
}

/// D40：每个来源 × 有没有进行中的事务。安全类写不走 other_write（server 先排除，照旧 preempt），单独一行。
#[tokio::test(start_paused = true)]
async fn other_write_by_source_while_confirming() {
    use Source::*;
    for src in [Screen, Web, Legacy, Guard, Scenario, Scheduler, Auto] {
        // 没有进行中的事务：谁都照做
        let dev = Dev::new();
        let e = engine(&dev, true, None);
        assert_eq!(e.other_write(src), Ok(()), "{src:?} idle");
        // 有：用户的照做并取消（other_change），自动来源回进行中的事务
        let op_id = verifying(&dev, &e).await;
        match e.other_write(src) {
            Ok(()) => {
                assert!(src.is_user(), "{src:?}");
                assert_eq!(
                    end(&e.status(Some(&op_id))),
                    pair("cancelled", "other_change"),
                    "{src:?}"
                );
            }
            Err(op) => {
                assert!(!src.is_user(), "{src:?}");
                assert_eq!(
                    op,
                    json!({"op_id": op_id, "item": NETWORK_MODE, "phase": "verifying"})
                );
                assert_eq!(e.status(Some(&op_id))["phase"], "verifying", "{src:?}");
            }
        }
    }
    // 安全类写照旧插队（preempted），不是 other_change
    let dev = Dev::new();
    let e = engine(&dev, true, None);
    let op_id = verifying(&dev, &e).await;
    assert_eq!(e.legacy_gate(None, true), LegacyGate::Pass);
    assert_eq!(end(&e.status(Some(&op_id))), pair("cancelled", "preempted"));
}

/// D40 的要点：确认中用户改了别的设置，到点也不会再发退回。
#[tokio::test(start_paused = true)]
async fn user_write_while_confirming_never_rolls_back() {
    let dev = Dev::new();
    let e = engine(&dev, true, None);
    let seen = observed(&e);
    let op_id = verifying(&dev, &e).await;
    e.other_write(Source::Screen).unwrap();
    tokio::time::sleep(Duration::from_millis(DEADLINE + 60_000)).await;
    assert_eq!(dev.s().writes, ["Only_LTE"]);
    let b = latest(&seen);
    assert!(b["active"].is_null());
    assert_eq!(b["last"]["op_id"], op_id);
    assert_eq!(b["last"]["reason"], "other_change");
    assert_eq!(b["last"]["say_zh"], "改了别的设置，不再自动退回");
    assert_eq!(b["last"]["needs_ack"], true);
    // 锁空了：新的写能做
    let next = op_of(&e.submit(req("Only_5G", Source::Scenario)).await);
    assert_eq!(next["phase"], "verifying");
}

#[tokio::test(start_paused = true)]
async fn interrupt_cancels_and_records_what() {
    let dev = Dev::new();
    let dir = temp_dir();
    let e = Engine::new(
        dev.clone(),
        cfg(true),
        Store::open(Some(dir.clone())),
        Record::open(Some(dir.clone())),
    );
    assert_eq!(
        e.interrupt(Source::Web, "esim"),
        json!({"interrupted": false})
    );
    let op_id = verifying(&dev, &e).await;
    assert_eq!(
        e.interrupt(Source::Web, "at"),
        json!({"interrupted": true, "op_id": op_id})
    );
    assert_eq!(
        end(&e.status(Some(&op_id))),
        pair("cancelled", "other_change")
    );
    tokio::time::sleep(Duration::from_millis(DEADLINE + 60_000)).await;
    assert_eq!(dev.s().writes, ["Only_LTE"]);
    let lines = || -> Vec<Value> {
        e.record()
            .list(50)
            .into_iter()
            .filter(|l| l["action"] == "op.interrupt")
            .collect()
    };
    for _ in 0..200 {
        if !lines().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let l = lines();
    assert_eq!(l.len(), 1, "{l:?}");
    assert_eq!(l[0]["what"], "at");
    assert_eq!(l[0]["source"], "web");
    assert_eq!(l[0]["op_id"], op_id);
    // 事务那一行记的是 other_change
    let txn: Vec<Value> = e
        .record()
        .list(50)
        .into_iter()
        .filter(|l| l["op_id"] == op_id && l.get("new").is_some())
        .collect();
    assert_eq!(txn[0]["reason"], "other_change");
}

/// DD18（V2-44）：自动退回开着、没人点过「知道了」才有 notice；点了两边都没有，重启后也没有。
#[tokio::test(start_paused = true)]
async fn rollback_notice_until_acked() {
    let dev = Dev::new();
    let dir = temp_dir();
    // 关着：没有这个键（块和以前一样）
    let off = engine(&dev, false, Some(dir.clone()));
    assert!(off.block().get("notice").is_none());
    let on = engine(&dev, true, Some(dir.clone()));
    let seen = observed(&on);
    assert_eq!(latest(&seen)["notice"], "rollback_on");
    assert_eq!(on.screen().1["notice"], "rollback_on");
    assert_eq!(
        on.notice_ack(Source::Screen),
        json!({"notice": "rollback_on", "acked": true})
    );
    assert!(latest(&seen).get("notice").is_none());
    assert!(on.screen().1.get("notice").is_none());
    // 再点照样成功
    assert_eq!(on.notice_ack(Source::Web)["acked"], true);
    let saved: Value =
        serde_json::from_slice(&std::fs::read(dir.join("notice.json")).unwrap()).unwrap();
    assert_eq!(saved, json!({"acked": ["rollback_on"]}));
    // datad 重启：不再出现
    let again = engine(&dev, true, Some(dir.clone()));
    assert!(again.block().get("notice").is_none());
    // 不落盘时只在内存里记
    let mem = engine(&dev, true, None);
    assert_eq!(mem.block()["notice"], "rollback_on");
    mem.notice_ack(Source::Web);
    assert!(mem.block().get("notice").is_none());
}

/// D41：只有 IPv6 的连接，确认时向 IPv6 DNS 探测，绑定同一个蜂窝接口。
#[tokio::test(start_paused = true)]
async fn ipv6_only_connection_confirms_with_an_ipv6_probe() {
    let dev = Dev::new();
    dev.s().conn = Conn {
        ipv6: "2001:db8::1".into(),
        up_since_ms: Some(1),
        ..Conn::default()
    };
    let e = engine(&dev, true, None);
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    let fin = settle(&e, &id(&op)).await;
    assert_eq!(end(&fin), pair("confirmed", "verified"));
    assert_eq!(dev.s().probes, ["rmnet_data0/v6"]);
}

// ── T22（D23）：谁提的 / 谁批准的 ──────────────────────────────────────

fn mcp(target: &str, approved_by: Source) -> Request {
    Request {
        approved_by,
        ..req(target, Source::Mcp)
    }
}

/// 人在触屏确认过的 MCP 写：能覆盖进行中的用户事务、带自动退回、算用户值；来源照旧记 mcp。
#[tokio::test(start_paused = true)]
async fn confirmed_mcp_write_is_the_users_but_keeps_its_origin() {
    let dir = temp_dir();
    let dev = Dev::new();
    dev.s().registered = false;
    let e = Engine::new(
        dev.clone(),
        cfg(true),
        Store::open(None),
        Record::open_with(Some(dir), 1 << 20),
    );
    let a = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let b = op_of(&e.submit(mcp("Only_5G", Source::Screen)).await);
    assert_eq!(e.status(Some(&id(&a)))["reason"], "superseded");
    assert_eq!(b["source"], "mcp");
    assert_eq!(b["approved_by"], "screen");
    assert_eq!(b["rollback_enabled"], true);
    assert_eq!(b["source_zh"], "MCP 提议 · 你在触屏确认");
    assert_eq!(b["source_en"], "Proposed by MCP · confirmed on the screen");
    let owner = &e.record().owners()["network.mode"];
    assert_eq!(owner["source"], "mcp");
    assert_eq!(owner["approved_by"], "screen");
    assert_eq!(owner["user"], true);
    // busy 回复也说清楚
    match e.submit(req("Only_LTE", Source::Scenario)).await {
        Submit::Busy(d) => {
            assert_eq!(d["source"], "mcp");
            assert_eq!(d["approved_by"], "screen");
            assert_eq!(
                d["say_zh"],
                "正在换制式（MCP 提议 · 你在触屏确认，0 秒），稍等"
            );
        }
        other => panic!("{other:?}"),
    }
    // D40：确认过的 MCP 写对别的影响上网的写算用户写
    let who = Who {
        source: Source::Mcp,
        approved_by: Source::Web,
    };
    assert_eq!(e.other_write(who), Ok(()));
    assert_eq!(
        end(&e.status(Some(&id(&b)))),
        pair("cancelled", "other_change")
    );
    // 流水账：事务行来源是 mcp、批准者 screen
    let mut lines = vec![];
    for _ in 0..200 {
        lines = e.record().list(10);
        if lines.iter().any(|l| l["op_id"] == b["op_id"]) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let line = lines.iter().find(|l| l["op_id"] == b["op_id"]).unwrap();
    assert_eq!(line["source"], "mcp");
    assert_eq!(line["approved_by"], "screen");
    // 没有另外批准者的那行不带这个键
    let first = lines.iter().find(|l| l["op_id"] == a["op_id"]).unwrap();
    assert!(first.get("approved_by").is_none());
}

/// 没人确认的 MCP 写和自动来源一样：不能插队、不算用户值、挡在 D40 外面。
#[tokio::test(start_paused = true)]
async fn unconfirmed_mcp_write_is_automatic() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let a = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    tokio::time::sleep(Duration::from_secs(3)).await;
    match e.submit(mcp("Only_5G", Source::Mcp)).await {
        Submit::Busy(d) => {
            assert_eq!(d["op_id"], a["op_id"]);
            assert!(d.get("approved_by").is_none());
        }
        other => panic!("{other:?}"),
    }
    assert!(e.other_write(Source::Mcp).is_err());
    assert_eq!(e.status(Some(&id(&a)))["phase"], "verifying");
    // 空闲时照做：事务和 owners 都不算用户的
    let dev = Dev::new();
    let e = Engine::new(
        dev.clone(),
        cfg(false),
        Store::open(None),
        Record::open_with(Some(temp_dir()), 1 << 20),
    );
    let b = op_of(&e.submit(mcp("Only_5G", Source::Mcp)).await);
    assert_eq!(b["source"], "mcp");
    assert!(b.get("approved_by").is_none());
    assert_eq!(b["source_zh"], "MCP");
    let owner = &e.record().owners()["network.mode"];
    assert_eq!(owner["user"], false);
    assert!(owner.get("approved_by").is_none());
}

/// 落盘：批准者和来源相同时 pending.json 没有这个键（和以前逐字节一样、datad 也读得懂）；
/// 不同时写进去，重启后还在。旧文件没有这个键 = 和来源相同。
#[tokio::test(start_paused = true)]
async fn pending_file_carries_approved_by_only_when_it_differs() {
    let dev = Dev::new();
    dev.s().registered = false;
    let dir = temp_dir();
    let e = engine(&dev, true, Some(dir.clone()));
    op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    let raw = std::fs::read_to_string(dir.join("pending.json")).unwrap();
    assert!(!raw.contains("approved_by"), "{raw}");
    let t: Txn = serde_json::from_str(&raw).unwrap();
    assert_eq!(t.who(), Who::from(Source::Screen));

    let dir = temp_dir();
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, Some(dir.clone()));
    let op = op_of(&e.submit(mcp("Only_LTE", Source::Web)).await);
    let raw = std::fs::read_to_string(dir.join("pending.json")).unwrap();
    assert!(raw.contains(r#""approved_by":"web""#), "{raw}");
    let e2 = engine(&dev, true, Some(crashed(&dir)));
    e2.start().await;
    let s = e2.status(Some(&id(&op)));
    assert_eq!(
        (s["source"].as_str(), s["approved_by"].as_str()),
        (Some("mcp"), Some("web"))
    );
}

#[tokio::test(start_paused = true)]
async fn confirmed_ack_and_session_record_both() {
    let dir = temp_dir();
    let dev = Dev::new();
    let e = Engine::new(
        dev.clone(),
        cfg(false),
        Store::open(None),
        Record::open_with(Some(dir), 1 << 20),
    );
    let who = Who::new(Source::Mcp, Some(Source::Screen)).unwrap();
    let s = e.session_open(who).unwrap();
    let sid = s["session"].as_str().unwrap().to_owned();
    match e.submit(req("Only_LTE", Source::Web)).await {
        Submit::Busy(d) => {
            assert_eq!(d["source"], "mcp");
            assert_eq!(d["approved_by"], "screen");
        }
        other => panic!("{other:?}"),
    }
    e.session_close(&sid, None).unwrap();
    e.record().flush().await;
    let lines = e.record().list(10);
    assert!(
        lines
            .iter()
            .all(|l| l["source"] == "mcp" && l["approved_by"] == "screen"),
        "{lines:?}"
    );
}

/// 3-3：resolve 说「不算事务」就回 Plain、说不行就回 Refused，都不动设备、不落盘；
/// 有事务进行中时「不算事务」的写也要排队（Busy）。
#[tokio::test(start_paused = true)]
async fn plain_and_refused_leave_no_transaction() {
    let dir = temp_dir();
    let dev = Dev::new();
    let e = engine(&dev, false, Some(dir.clone()));
    dev.s().resolve = Some(Ok(()));
    assert!(matches!(
        e.submit(req("Only_LTE", Source::Web)).await,
        Submit::Plain
    ));
    dev.s().resolve = Some(Err("changed since".into()));
    match e.submit(req("Only_LTE", Source::Web)).await {
        Submit::Refused(m) => assert_eq!(m, "changed since"),
        other => panic!("{other:?}"),
    }
    assert!(dev.s().writes.is_empty());
    assert!(!dir.join("pending.json").exists());

    dev.s().resolve = None;
    dev.s().registered = false;
    let _ = op_of(&e.submit(req("Only_LTE", Source::Web)).await);
    dev.s().resolve = Some(Ok(()));
    assert!(matches!(
        e.submit(req("Only_5G", Source::Web)).await,
        Submit::Busy(_)
    ));
    assert_eq!(dev.s().writes, ["Only_LTE"]);
}

/// 第四批 4-0b：Wi-Fi 设置的事务不影响上网，别的影响上网的写、关数据、op.interrupt 都不取消它
/// （自动来源也不挡）；确认不看数据、不探测。
#[tokio::test(start_paused = true)]
async fn network_writes_leave_a_wifi_change_alone() {
    let dev = Dev::new();
    dev.s().registered = false; // Wi-Fi：AP 还没起来
    let e = engine(&dev, true, None);
    let mut r = req("Only_LTE", Source::Web);
    r.spec = crate::ops::spec::find("wifi.settings").unwrap();
    let op_id = id(&op_of(&e.submit(r).await));
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(e.other_write(Source::Auto), Ok(()));
    assert_eq!(e.other_write(Source::Screen), Ok(()));
    assert_eq!(
        e.interrupt(Source::Screen, "esim"),
        json!({"interrupted": false})
    );
    e.preempt();
    assert_eq!(e.status(Some(&op_id))["phase"], "verifying");
    dev.s().registered = true;
    dev.s().expected = None; // 数据通路读不到也不影响
    let fin = settle(&e, &op_id).await;
    assert_eq!(end(&fin), pair("confirmed", "verified"));
    assert!(dev.s().probes.is_empty());
}

// ── 第二期（D4、O9、人优先）──────────────────────────────────────

/// D4：自动退回的全局开关关着时，Agent 自己批准的写照样到点不通就退回；人的写照旧不退。
#[tokio::test(start_paused = true)]
async fn agent_writes_always_roll_back_even_with_the_switch_off() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, false, None);
    let a = op_of(&e.submit(mcp("Only_LTE", Source::Mcp)).await);
    assert_eq!(a["rollback_enabled"], true);
    let fin = settle(&e, &id(&a)).await;
    assert_eq!(end(&fin), pair("rollback_failed", "rollback_timeout"));
    assert_eq!(dev.s().writes, ["Only_LTE", "WL_AND_5G"]);

    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, false, None);
    let b = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    let fin = settle(&e, &id(&b)).await;
    assert_eq!(end(&fin), pair("unverified", "no_rollback"));
    assert_eq!(dev.s().writes, ["Only_LTE"]);

    // 切前就确定不通（D10）：调用方关掉退回
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, false, None);
    let c = op_of(
        &e.submit(Request {
            rollback: Some(false),
            ..mcp("Only_LTE", Source::Mcp)
        })
        .await,
    );
    let fin = settle(&e, &id(&c)).await;
    assert_eq!(end(&fin), pair("unverified", "no_rollback"));
}

/// O9：判定时的卡和写之前读到的不一样：拒绝，不动设备。
#[tokio::test(start_paused = true)]
async fn expected_sim_must_still_be_there() {
    let dev = Dev::new();
    let e = engine(&dev, true, None);
    match e
        .submit(Request {
            expect_iccid: Some(sim(2).iccid),
            ..mcp("Only_LTE", Source::Mcp)
        })
        .await
    {
        Submit::Refused(_) => {}
        other => panic!("{other:?}"),
    }
    assert!(dev.s().writes.is_empty());
    let a = op_of(
        &e.submit(Request {
            expect_iccid: Some(sim(1).iccid),
            ..mcp("Only_LTE", Source::Mcp)
        })
        .await,
    );
    assert_eq!(a["phase"], "verifying");
}

/// 人优先（用户 10-17 定）：Agent 的试用进行中，人改别的项照做，Agent 的那个记 other_change、不退回。
#[tokio::test(start_paused = true)]
async fn a_person_never_waits_for_an_agent_trial() {
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let a = op_of(&e.submit(mcp("Only_LTE", Source::Mcp)).await);
    tokio::time::sleep(Duration::from_secs(3)).await;
    // 别的自动来源照旧 busy
    assert!(matches!(
        e.submit(req("Only_5G", Source::Scenario)).await,
        Submit::Busy(_)
    ));
    let b = op_of(&e.submit(req("Only_5G", Source::Web)).await);
    assert_eq!(
        end(&e.status(Some(&id(&a)))),
        pair("cancelled", "other_change")
    );
    assert_eq!(b["phase"], "verifying");
    // 旧请求（没有 source）也算人
    let dev = Dev::new();
    dev.s().registered = false;
    let e = engine(&dev, true, None);
    let a = op_of(&e.submit(mcp("Only_LTE", Source::Mcp)).await);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(e.legacy_gate(Some("band.lte"), false), LegacyGate::Pass);
    assert_eq!(
        end(&e.status(Some(&id(&a)))),
        pair("cancelled", "other_change")
    );
}
