//! 事务准入（decision-execution-loop §4.3 E1）：网络事务写前在诊断 Gate 上预留，等探测结束，
//! 等不到就在写入前回 busy；预留跟着进行中的事务，覆盖时接着拿，结束、插队、取消都会放。

use super::*;
use luyaz_api::diagnose::admit::Probe;
use std::sync::atomic::AtomicBool;

struct G {
    gate: Arc<Mutex<Gate>>,
    others: Arc<AtomicBool>,
}

impl G {
    fn held(&self) -> bool {
        self.gate.lock().unwrap().txn.is_some()
    }
    fn probe(&self, on: bool) {
        let mut g = self.gate.lock().unwrap();
        if on {
            g.probe_enter(Probe::Finding, 1).unwrap();
        } else {
            g.probe_exit(Probe::Finding);
        }
    }
}

fn gated_engine(dev: &Dev, wait_ms: u64) -> (Engine<Dev>, G) {
    let e = engine(dev, false, None);
    let g = G {
        gate: Arc::default(),
        others: Arc::default(),
    };
    let others = g.others.clone();
    e.set_gate(TxnGate {
        gate: g.gate.clone(),
        others: Arc::new(move || others.load(Ordering::SeqCst)),
        wait_ms,
    });
    (e, g)
}

#[tokio::test]
async fn waits_for_a_running_probe_and_starts_its_clock_after_the_wait() {
    let dev = Dev::new();
    dev.s().apply = Apply::Ignore; // 停在确认里，看预留拿着
    let (e, g) = gated_engine(&dev, 5_000);
    g.probe(true);
    let gate = g.gate.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(600)).await;
        gate.lock().unwrap().probe_exit(Probe::Finding);
    });
    let before = dev.now_ms();
    op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    assert_eq!(dev.s().writes, ["Only_LTE"]);
    // 等待在事务建立之前：确认时限从写的时候算，不被等的这段吃掉
    let start = e.lock().active.as_ref().unwrap().wait_start_ms;
    assert!(start >= before + 500, "{start} vs {before}");
    assert!(g.held(), "held while the change is being confirmed");
    // 有预留时新的探测进不来
    assert_eq!(
        g.gate.lock().unwrap().probe_enter(Probe::Finding, 2),
        Err("txn")
    );
}

#[tokio::test(start_paused = true)]
async fn gives_up_before_writing_and_leaves_no_reservation() {
    let dev = Dev::new();
    let (e, g) = gated_engine(&dev, 1_000);
    g.probe(true);
    match e.submit(req("Only_LTE", Source::Screen)).await {
        Submit::Busy(v) => assert_eq!(v["busy"], "txn"),
        other => panic!("{other:?}"),
    }
    assert!(dev.s().writes.is_empty(), "nothing written");
    assert!(!g.held());
    assert!(e.lock().active.is_none(), "no transaction made");
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_wait_leaves_no_reservation() {
    let dev = Dev::new();
    let (e, g) = gated_engine(&dev, 20_000);
    g.others.store(true, Ordering::SeqCst); // 测速在跑
    let waited = tokio::time::timeout(
        Duration::from_secs(2),
        e.submit(req("Only_LTE", Source::Screen)),
    )
    .await;
    assert!(waited.is_err(), "still waiting when dropped");
    assert!(!g.held());
    assert!(dev.s().writes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_panic_while_waiting_leaves_no_reservation() {
    let dev = Dev::new();
    let (e, g) = gated_engine(&dev, 20_000);
    g.others.store(true, Ordering::SeqCst);
    let e2 = e.clone();
    let task = tokio::spawn(async move {
        tokio::select! {
            _ = e2.submit(req("Only_LTE", Source::Screen)) => {}
            _ = tokio::time::sleep(Duration::from_secs(1)) => panic!("boom while waiting"),
        }
    });
    assert!(task.await.unwrap_err().is_panic());
    assert!(!g.held());
}

#[tokio::test(start_paused = true)]
async fn the_reservation_ends_with_the_transaction() {
    let dev = Dev::new();
    let (e, g) = gated_engine(&dev, 5_000);
    let op = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    assert!(g.held());
    assert_eq!(end(&settle(&e, &id(&op)).await), pair("confirmed", "verified"));
    assert!(!g.held());
}

#[tokio::test(start_paused = true)]
async fn superseding_the_same_item_takes_over_the_reservation_without_waiting() {
    let dev = Dev::new();
    dev.s().apply = Apply::Ignore;
    let (e, g) = gated_engine(&dev, 20_000);
    let a = op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    // 前一个在确认里时测速开始了：重新排队就得等，接着拿就不用
    g.others.store(true, Ordering::SeqCst);
    let t0 = Instant::now();
    let b = op_of(&e.submit(req("Only_5G", Source::Screen)).await);
    assert!(t0.elapsed() < Duration::from_millis(100));
    assert_eq!(e.status(Some(&id(&a)))["reason"], "superseded");
    assert!(g.held());
    assert_eq!(e.status(Some(&id(&b)))["phase"], "verifying");
}

#[tokio::test(start_paused = true)]
async fn a_busy_answer_keeps_the_running_transactions_reservation() {
    let dev = Dev::new();
    dev.s().write_delay = Duration::from_secs(5);
    let (e, g) = gated_engine(&dev, 5_000);
    let e2 = e.clone();
    let first = tokio::spawn(async move { e2.submit(req("Only_LTE", Source::Screen)).await });
    tokio::time::sleep(Duration::from_secs(1)).await;
    match e.submit(req("Only_5G", Source::Screen)).await {
        Submit::Busy(d) => assert_eq!(d["phase"], "applying"),
        other => panic!("{other:?}"),
    }
    assert!(g.held(), "the first change still holds it");
    first.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_safety_preempt_releases_the_reservation() {
    let dev = Dev::new();
    dev.s().apply = Apply::Ignore;
    let (e, g) = gated_engine(&dev, 5_000);
    op_of(&e.submit(req("Only_LTE", Source::Screen)).await);
    assert!(g.held());
    e.preempt();
    assert!(!g.held());
}

#[test]
fn only_wifi_skips_the_gate() {
    for s in SPECS.iter() {
        assert_eq!(gated(s), !s.action.starts_with("wifi."), "{}", s.action);
    }
    assert!(
        SPECS
            .iter()
            .any(|s| s.action.starts_with("chill.") && gated(s))
    );
    assert!(
        SPECS
            .iter()
            .any(|s| s.action.starts_with("wifi.") && !gated(s))
    );
}
