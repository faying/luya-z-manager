use super::*;
use crate::diagnose::refusal_at;
use std::cell::Cell;
use std::sync::Arc;

const BOOT: &str = "boot-a";

#[test]
fn a_reserved_gate_refuses_new_probes_and_speed_tests() {
    let mut g = Gate::default();
    assert_eq!(
        g.txn_reserve("op1", BOOT, 1_000, TXN_WAIT_MAX_MS, false),
        Admit::Granted
    );
    assert_eq!(g.probe_enter(Probe::Diagnose, 10), Err("txn"));
    assert_eq!(g.probe_enter(Probe::Finding, 10), Err("txn"));
    let (code, body) = refusal_at(&g, 10).unwrap();
    assert_eq!((code, body["busy"].as_str()), (409, Some("txn")));
    assert!(g.diag_since.is_none() && g.finding_since.is_none());
    assert_eq!(
        g.txn_reserve("op2", BOOT, 1_001, TXN_WAIT_MAX_MS, false),
        Admit::Busy
    );
    assert!(g.txn_release("op1"));
    assert_eq!(g.probe_enter(Probe::Finding, 11), Ok(()));
}

#[test]
fn a_running_probe_is_waited_for_not_taken_over() {
    let mut g = Gate::default();
    g.probe_enter(Probe::Finding, 5).unwrap();
    assert_eq!(
        g.txn_reserve("op1", BOOT, 0, TXN_WAIT_MAX_MS, false),
        Admit::Waiting
    );
    // the probe keeps its field: nothing cleared it for the transaction
    assert_eq!(g.finding_since, Some(5));
    assert_eq!(g.txn_poll("op1", BOOT, 14_000, false), Admit::Waiting);
    // no new probe joins while it waits
    assert_eq!(g.probe_enter(Probe::Diagnose, 6), Err("txn"));
    g.probe_exit(Probe::Finding);
    assert_eq!(g.txn_poll("op1", BOOT, 14_250, false), Admit::Granted);
    assert!(g.txn.as_ref().unwrap().granted);
}

#[test]
fn waiting_past_the_limit_gives_up_before_any_write_and_leaves_nothing() {
    let mut g = Gate::default();
    g.probe_enter(Probe::Diagnose, 5).unwrap();
    // asking for longer than the cap does not stretch it
    assert_eq!(
        g.txn_reserve("op1", BOOT, 1_000, 999_999, false),
        Admit::Waiting
    );
    assert_eq!(g.txn.as_ref().unwrap().deadline_ms, 1_000 + TXN_WAIT_MAX_MS);
    assert_eq!(
        g.txn_poll("op1", BOOT, 1_000 + TXN_WAIT_MAX_MS - 1, false),
        Admit::Waiting
    );
    assert_eq!(
        g.txn_poll("op1", BOOT, 1_000 + TXN_WAIT_MAX_MS, false),
        Admit::TimedOut
    );
    assert!(g.txn.is_none());
    // the probe was never touched
    assert_eq!(g.diag_since, Some(5));
    assert_eq!(g.txn_poll("op1", BOOT, 30_000, false), Admit::Lost);
}

#[test]
fn only_the_owner_polls_or_releases() {
    let mut g = Gate::default();
    g.txn_reserve("op1", BOOT, 0, 1_000, false);
    assert_eq!(g.txn_poll("op2", BOOT, 1, false), Admit::Lost);
    assert!(!g.txn_release("op2"));
    assert!(g.txn.is_some());
}

#[test]
fn another_boot_or_no_clock_never_writes() {
    let mut g = Gate::default();
    assert_eq!(g.txn_reserve("op1", "", 0, 1_000, false), Admit::NoClock);
    assert!(g.txn.is_none());
    g.probe_enter(Probe::Finding, 1).unwrap();
    g.txn_reserve("op1", BOOT, 0, 1_000, false);
    assert_eq!(g.txn_poll("op1", "boot-b", 10, false), Admit::Lost);
    assert!(g.txn.is_none());
    // a hold from another boot does not block the new boot's transaction
    g.probe_exit(Probe::Finding);
    g.txn = Some(TxnHold {
        owner: "old".into(),
        boot_id: "boot-0".into(),
        since_ms: 0,
        deadline_ms: 0,
        granted: true,
    });
    assert_eq!(g.txn_reserve("op1", BOOT, 0, 1_000, false), Admit::Granted);
    assert_eq!(g.txn.as_ref().unwrap().owner, "op1");
}

#[test]
fn acquire_with_an_injected_clock() {
    let gate = Mutex::new(Gate::default());
    gate.lock().unwrap().probe_enter(Probe::Finding, 1).unwrap();
    let t = Cell::new(0u64);
    let polls = Cell::new(0u32);
    let a = txn_acquire(
        &gate,
        "op1",
        TXN_WAIT_MAX_MS,
        || Some((BOOT.into(), t.get())),
        || false,
        |d| {
            t.set(t.get() + d.as_millis() as u64);
            polls.set(polls.get() + 1);
            // the probe ends on its own after 14 s
            if t.get() >= 14_000 {
                gate.lock().unwrap().probe_exit(Probe::Finding);
            }
        },
    );
    assert_eq!(a, Admit::Granted);
    assert_eq!(t.get(), 14_000);
    assert_eq!(polls.get(), 56);
    assert!(gate.lock().unwrap().txn.as_ref().unwrap().granted);
}

#[test]
fn acquire_times_out_and_unknown_clock_gives_up() {
    let gate = Mutex::new(Gate::default());
    gate.lock()
        .unwrap()
        .probe_enter(Probe::Diagnose, 1)
        .unwrap();
    let t = Cell::new(0u64);
    let a = txn_acquire(
        &gate,
        "op1",
        TXN_WAIT_MAX_MS,
        || Some((BOOT.into(), t.get())),
        || false,
        |d| t.set(t.get() + d.as_millis() as u64),
    );
    assert_eq!(a, Admit::TimedOut);
    assert_eq!(t.get(), TXN_WAIT_MAX_MS);
    assert!(gate.lock().unwrap().txn.is_none());

    let n = Cell::new(0);
    let a = txn_acquire(
        &gate,
        "op2",
        TXN_WAIT_MAX_MS,
        || {
            n.set(n.get() + 1);
            (n.get() == 1).then(|| (BOOT.to_string(), 0))
        },
        || false,
        |_| {},
    );
    assert_eq!(a, Admit::NoClock);
    assert!(gate.lock().unwrap().txn.is_none());
}

#[test]
fn a_poisoned_gate_still_works() {
    let gate = Arc::new(Mutex::new(Gate::default()));
    let g2 = gate.clone();
    let _ = std::thread::spawn(move || {
        let _g = g2.lock().unwrap();
        panic!("poison");
    })
    .join();
    assert!(gate.is_poisoned());
    let a = txn_acquire(&gate, "op1", 0, || Some((BOOT.into(), 0)), || false, |_| {});
    assert_eq!(a, Admit::Granted);
}

/// Threads: probes and one transaction race; a probe never runs while a granted transaction
/// holds the gate, and the transaction never sees a probe once granted.
#[test]
fn probes_and_a_transaction_never_overlap() {
    let gate = Arc::new(Mutex::new(Gate::default()));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut probes = Vec::new();
    for i in 0..4 {
        let (gate, stop) = (gate.clone(), stop.clone());
        probes.push(std::thread::spawn(move || {
            let p = if i % 2 == 0 {
                Probe::Diagnose
            } else {
                Probe::Finding
            };
            let mut ran = 0;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let ok = gate.lock().unwrap().probe_enter(p, 1).is_ok();
                if ok {
                    {
                        let g = gate.lock().unwrap();
                        assert!(g.txn.as_ref().is_none_or(|h| !h.granted));
                    }
                    ran += 1;
                    std::thread::sleep(Duration::from_micros(200));
                    gate.lock().unwrap().probe_exit(p);
                }
                std::thread::yield_now();
            }
            ran
        }));
    }
    let start = std::time::Instant::now();
    let mut granted = 0;
    for k in 0..50 {
        let owner = format!("op{k}");
        let a = txn_acquire(
            &gate,
            &owner,
            TXN_WAIT_MAX_MS,
            || Some((BOOT.into(), start.elapsed().as_millis() as u64)),
            || false,
            |_| std::thread::sleep(Duration::from_micros(100)),
        );
        if a == Admit::Granted {
            granted += 1;
            {
                let g = gate.lock().unwrap();
                assert!(g.diag_since.is_none() && g.finding_since.is_none());
            }
            std::thread::sleep(Duration::from_micros(300));
            assert!(gate.lock().unwrap().txn_release(&owner));
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for p in probes {
        p.join().unwrap();
    }
    assert_eq!(granted, 50);
}

/// A speed test or search/register (outside the gate) counts as running too.
#[test]
fn a_speed_test_or_search_is_waited_for_too() {
    let mut g = Gate::default();
    assert_eq!(
        g.txn_reserve("op1", BOOT, 0, TXN_WAIT_MAX_MS, true),
        Admit::Waiting
    );
    assert_eq!(g.txn_poll("op1", BOOT, 1_000, true), Admit::Waiting);
    assert_eq!(
        g.txn_poll("op1", BOOT, TXN_WAIT_MAX_MS, true),
        Admit::TimedOut
    );
    assert!(g.txn.is_none());

    let gate = Mutex::new(Gate::default());
    let t = Cell::new(0u64);
    let a = txn_acquire(
        &gate,
        "op2",
        TXN_WAIT_MAX_MS,
        || Some((BOOT.into(), t.get())),
        || t.get() < 3_000,
        |d| t.set(t.get() + d.as_millis() as u64),
    );
    assert_eq!((a, t.get()), (Admit::Granted, 3_000));
}

/// A waiter that vanished without releasing does not block the next transaction for good.
#[test]
fn an_abandoned_waiter_is_dropped_after_its_deadline() {
    let mut g = Gate::default();
    g.probe_enter(Probe::Finding, 1).unwrap();
    assert_eq!(g.txn_reserve("gone", BOOT, 0, 1_000, false), Admit::Waiting);
    assert_eq!(g.txn_reserve("op2", BOOT, 999, 1_000, false), Admit::Busy);
    g.probe_exit(Probe::Finding);
    assert_eq!(
        g.txn_reserve("op2", BOOT, 1_000, 1_000, false),
        Admit::Granted
    );
    assert_eq!(g.txn.as_ref().unwrap().owner, "op2");
    // a granted hold is never taken this way
    assert_eq!(
        g.txn_reserve("op3", BOOT, 99_000, 1_000, false),
        Admit::Busy
    );
}

/// A poll that comes late, after the probe ended but past the deadline, still times out.
#[test]
fn a_late_poll_times_out_even_if_the_probe_ended() {
    let mut g = Gate::default();
    g.probe_enter(Probe::Finding, 1).unwrap();
    g.txn_reserve("op1", BOOT, 0, TXN_WAIT_MAX_MS, false);
    g.probe_exit(Probe::Finding);
    assert_eq!(g.txn_poll("op1", BOOT, 26_000, false), Admit::TimedOut);
    assert!(g.txn.is_none());
}
