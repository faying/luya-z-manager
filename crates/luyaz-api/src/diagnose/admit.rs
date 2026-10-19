//! Write-transaction admission against diagnosis / Finding probes / speed tests
//! (decision-execution-loop.md §1.3 D9, §4.1 D1). Lives in the same [`Gate`] under the same
//! `Mutex`; times are CLOCK_BOOTTIME milliseconds with the boot id, passed in by the caller.
//!
//! - A transaction registers first ([`Gate::txn_reserve`]): from then on no new probe starts
//!   ([`Gate::probe_enter`] refuses, [`super::refusal_at`] answers 409).
//! - "Running" also covers what the gate doesn't hold itself: a speed test or a network
//!   search/register (the host's `others_busy`), passed in as `others` and read under the lock.
//! - A probe already running is never taken over: it ends by its own `RUN_MAX` / `LAYER_MAX` /
//!   Finding limits and calls [`Gate::probe_exit`]. The transaction waits for that, at most
//!   [`TXN_WAIT_MAX_MS`]; past that the reservation is dropped **before any write** and the
//!   caller answers busy. It never forces the gate, unlike `ops/write_lock.rs`.
//! - Granted, it keeps the reservation until its final state ([`Gate::txn_release`]).
//! - The gate is memory only: a core restart or a reboot starts empty. A hold from another
//!   boot id is dropped.
//! - Waiting here happens before the transaction exists, so it never moves `Txn::applied()`'s
//!   confirm deadline.
//!
//! First batch: only the probe side is wired; the engine calls [`txn_acquire`] in the second.

use super::Gate;
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::Duration;

/// Longest a transaction waits for running probes before giving up (design: ≤25 s, includes
/// scheduling slack over `RUN_MAX` = 20 s).
pub const TXN_WAIT_MAX_MS: u64 = 25_000;
/// How often [`txn_acquire`] looks again.
pub const TXN_POLL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxnHold {
    /// Who holds it (an op id or a request id); only the owner can poll or release it.
    pub owner: String,
    pub boot_id: String,
    pub since_ms: u64,
    /// Waiting past this (BOOTTIME ms, same boot) gives up.
    pub deadline_ms: u64,
    /// No probe runs any more: the transaction may read and write.
    pub granted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    /// Exclusive: start capturing / writing.
    Granted,
    /// Reserved; a probe is still finishing.
    Waiting,
    /// Another transaction holds it.
    Busy,
    /// Waited [`TXN_WAIT_MAX_MS`]: the reservation is gone, nothing may be written.
    TimedOut,
    /// Not (or no longer) this owner's, or the boot changed: nothing may be written.
    Lost,
    /// No boot clock: unknown never becomes a write.
    NoClock,
}

/// Which probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Diagnose,
    Finding,
}

impl Gate {
    fn probe_running(&self) -> bool {
        self.diag_since.is_some() || self.finding_since.is_some()
    }

    /// Register a transaction. `wait_ms` is capped at [`TXN_WAIT_MAX_MS`]; `others` = a speed
    /// test or search/register is running (read under this lock).
    pub fn txn_reserve(
        &mut self,
        owner: &str,
        boot_id: &str,
        now_ms: u64,
        wait_ms: u64,
        others: bool,
    ) -> Admit {
        if boot_id.is_empty() {
            return Admit::NoClock;
        }
        if let Some(h) = &self.txn {
            // a waiter that never came back (panicked / cancelled before releasing): its own
            // deadline is over, so it could never be granted; drop it rather than block for good
            let abandoned = h.boot_id == boot_id && !h.granted && now_ms >= h.deadline_ms;
            if h.boot_id == boot_id && !abandoned {
                return Admit::Busy;
            }
            // left over from another boot (only possible if the gate ever came back from disk)
            self.txn = None;
        }
        let granted = !self.probe_running() && !others;
        self.txn = Some(TxnHold {
            owner: owner.to_owned(),
            boot_id: boot_id.to_owned(),
            since_ms: now_ms,
            deadline_ms: now_ms.saturating_add(wait_ms.min(TXN_WAIT_MAX_MS)),
            granted,
        });
        if granted {
            Admit::Granted
        } else {
            Admit::Waiting
        }
    }

    /// Look again while [`Admit::Waiting`].
    pub fn txn_poll(&mut self, owner: &str, boot_id: &str, now_ms: u64, others: bool) -> Admit {
        let running = self.probe_running() || others;
        let Some(h) = self.txn.as_mut().filter(|h| h.owner == owner) else {
            return Admit::Lost;
        };
        if h.boot_id != boot_id {
            self.txn = None;
            return Admit::Lost;
        }
        if h.granted {
            return Admit::Granted;
        }
        // the deadline wins even when the probe has just ended: a late poll never writes
        if now_ms >= h.deadline_ms {
            self.txn = None;
            return Admit::TimedOut;
        }
        if !running {
            h.granted = true;
            return Admit::Granted;
        }
        Admit::Waiting
    }

    /// The transaction's final state, or it gave up. Another owner's hold stays.
    pub fn txn_release(&mut self, owner: &str) -> bool {
        if self.txn.as_ref().is_some_and(|h| h.owner == owner) {
            self.txn = None;
            return true;
        }
        false
    }

    /// A probe asks to start (`now` = device-clock seconds, as `diag_since` / `finding_since`
    /// keep). Err = who holds it.
    pub fn probe_enter(&mut self, p: Probe, now: u64) -> Result<(), &'static str> {
        if self.txn.is_some() {
            return Err("txn");
        }
        if self.diag_since.is_some() {
            return Err("diagnose");
        }
        if self.finding_since.is_some() {
            return Err("finding");
        }
        match p {
            Probe::Diagnose => self.diag_since = Some(now),
            Probe::Finding => self.finding_since = Some(now),
        }
        Ok(())
    }

    pub fn probe_exit(&mut self, p: Probe) {
        match p {
            Probe::Diagnose => self.diag_since = None,
            Probe::Finding => self.finding_since = None,
        }
    }
}

/// The 409 body while a transaction holds the gate.
pub fn busy_json() -> Value {
    json!({
        "ok": false,
        "error": "正在改设置，稍后再试",
        "error_en": "Applying a setting; try again shortly",
        "busy": "txn",
        "retry_after_s": TXN_WAIT_MAX_MS / 1000,
    })
}

/// Reserve, then wait for running probes. `now` gives (boot id, BOOTTIME ms) or None when
/// unreadable; `others` is the host's speed test / search-register check, called under the gate
/// lock; `sleep` is injected for tests. Anything but [`Admit::Granted`] leaves no reservation.
///
/// It blocks: the async engine (second batch) loops `txn_reserve` / `txn_poll` with its own
/// sleep instead, and must release the hold on every exit (a leaked one stops every probe until
/// a restart).
pub fn txn_acquire(
    gate: &Mutex<Gate>,
    owner: &str,
    wait_ms: u64,
    mut now: impl FnMut() -> Option<(String, u64)>,
    mut others: impl FnMut() -> bool,
    mut sleep: impl FnMut(Duration),
) -> Admit {
    let lock = || gate.lock().unwrap_or_else(|e| e.into_inner());
    let Some((boot, t)) = now() else {
        return Admit::NoClock;
    };
    let mut a = {
        let mut g = lock();
        g.txn_reserve(owner, &boot, t, wait_ms, others())
    };
    while a == Admit::Waiting {
        sleep(TXN_POLL);
        let Some((b, t)) = now() else {
            lock().txn_release(owner);
            return Admit::NoClock;
        };
        let mut g = lock();
        a = g.txn_poll(owner, &b, t, others());
    }
    a
}

#[cfg(test)]
mod tests;
