//! `radio.band_flap` evidence (decision-execution-loop.md §4.11): a 30-minute ring of data drops
//! with the band, cell and SINR at the time, fed by the rtnetlink producer, turned into
//! `luyaz_api::execution::band_flap::Evidence` for `/api/diagnose`. Read-only: the replay never
//! carries an action.
//!
//! - A drop is a global address leaving an rmnet link (RTM_DELADDR, scope universe): a same-IP
//!   redial deletes and adds, so it counts; IPv4 and IPv6 going together merge within
//!   [`MERGE_S`]. Not the `connect_status` reading (B31 says connected during an outage, §4.9).
//! - "Authoritative" (owner's O5 standard, "as observed"): the rtnetlink subscription has been
//!   live, with no overrun or resubscribe, for the whole window. A core restart, a lost
//!   subscription or a card event starts the window again.
//! - Original recovery: a modem SSR from crashwatch inside the window. Without crashwatch, unknown.
//! - In memory only; no new reads: band, cell and SINR come from the published `signal` block.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use luyaz_api::execution::band_flap::{Evidence, MERGE_S, WINDOW_S, replay};
use serde_json::{Value, json};

/// Below this SINR (dB) a drop is put down to the radio, not the band.
pub const WEAK_SINR_DB: f64 = 0.0;
/// This many different cells among the window's drops = moving (or reselecting around).
pub const MOVING_CELLS: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct Drop {
    pub at_ms: u64,
    pub band: Option<String>,
    pub cell: Option<String>,
    pub sinr: Option<f64>,
    /// Our own transaction was running (the `op` block's `active`); None = unread.
    pub ours: Option<bool>,
}

#[derive(Debug, Default)]
pub struct Ring {
    /// BOOTTIME ms the subscription went live (None = not live).
    live_since: Option<u64>,
    drops: VecDeque<Drop>,
}

impl Ring {
    pub fn live(&mut self, on: bool, now_ms: u64) {
        self.live_since = on.then_some(now_ms);
        self.drops.clear();
    }

    /// A card event: drops from another card don't count, the window starts again.
    pub fn sim_changed(&mut self, now_ms: u64) {
        if self.live_since.is_some() {
            self.live_since = Some(now_ms);
        }
        self.drops.clear();
    }

    pub fn drop_seen(&mut self, d: Drop) {
        if self.live_since.is_none() {
            return;
        }
        if let Some(last) = self.drops.back_mut()
            && d.at_ms.saturating_sub(last.at_ms) <= MERGE_S * 1000
        {
            // the same drop (IPv4 and IPv6 leaving together): keep the first, fill gaps
            last.ours = match (last.ours, d.ours) {
                (Some(a), Some(b)) => Some(a || b),
                _ => None,
            };
            return;
        }
        self.drops.push_back(d);
        self.trim(self.drops.back().map_or(0, |d| d.at_ms));
    }

    fn trim(&mut self, now_ms: u64) {
        let from = now_ms.saturating_sub(WINDOW_S * 1000);
        while self.drops.front().is_some_and(|d| d.at_ms < from) {
            self.drops.pop_front();
        }
    }

    /// `last_ssr_ms`: None = crashwatch is off; Some(0) = no SSR this boot.
    pub fn evidence(&mut self, now_ms: u64, last_ssr_ms: Option<u64>) -> Evidence {
        self.trim(now_ms);
        let covered_s = self.live_since.map(|t| now_ms.saturating_sub(t) / 1000);
        let full = covered_s.is_some_and(|c| c >= WINDOW_S);
        let ds = &self.drops;
        let all = |f: &dyn Fn(&Drop) -> Option<bool>| -> Option<bool> {
            let mut any = false;
            for d in ds {
                match f(d) {
                    None => return None,
                    Some(true) => any = true,
                    Some(false) => {}
                }
            }
            Some(any)
        };
        let band = {
            let mut b = ds.iter().map(|d| d.band.clone());
            match b.next().flatten() {
                Some(first) if b.all(|x| x.as_deref() == Some(first.as_str())) => Some(first),
                _ => None,
            }
        };
        let moving = if ds.iter().any(|d| d.cell.is_none()) {
            None
        } else {
            let mut cells: Vec<&str> = ds.iter().filter_map(|d| d.cell.as_deref()).collect();
            cells.sort_unstable();
            cells.dedup();
            Some(cells.len() >= MOVING_CELLS)
        };
        let from = now_ms.saturating_sub(WINDOW_S * 1000);
        Evidence {
            drops: self.live_since.map(|_| ds.len() as u32),
            band,
            same_sim: self.live_since.map(|_| true),
            epochs_authoritative: Some(full),
            covered_s,
            moving,
            weak_sinr: all(&|d| d.sinr.map(|s| s < WEAK_SINR_DB)),
            expected_overlap: all(&|d| d.ours),
            stock_recovery: last_ssr_ms.map(|t| t != 0 && t >= from),
        }
    }
}

static RING: Mutex<Ring> = Mutex::new(Ring {
    live_since: None,
    drops: VecDeque::new(),
});
/// BOOTTIME ms of crashwatch's last SSR (0 = none this boot).
static LAST_SSR_MS: AtomicU64 = AtomicU64::new(0);

fn ring() -> std::sync::MutexGuard<'static, Ring> {
    RING.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn boot_ms() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime into a timespec we own.
    unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
    ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
}

/// rtnetlink: the subscription went live (dump done) or was lost.
pub fn live(on: bool) {
    ring().live(on, boot_ms());
}

pub fn sim_changed() {
    ring().sim_changed(boot_ms());
}

/// crashwatch: a modem SSR at this BOOTTIME ms.
pub fn ssr(up_ms: u64) {
    LAST_SSR_MS.store(up_ms.max(1), Ordering::Relaxed);
}

/// What the published blocks say at a drop.
pub fn drop_from(signal: Option<&Value>, op: Option<&Value>, at_ms: u64) -> Drop {
    let s = |k: &str| -> Option<String> {
        match signal?.get(k)? {
            Value::String(x) if !x.is_empty() => Some(x.clone()),
            Value::Number(x) => Some(x.to_string()),
            _ => None,
        }
    };
    let f = |k: &str| -> Option<f64> {
        match signal?.get(k)? {
            Value::String(x) => x.trim().parse().ok(),
            Value::Number(x) => x.as_f64(),
            _ => None,
        }
    };
    let n = crate::crashwatch::Net::from_blocks(signal, None, None);
    let nr = n.nrband.is_some();
    Drop {
        at_ms,
        band: n
            .nrband
            .map(|b| {
                if b.starts_with('n') {
                    b
                } else {
                    format!("n{b}")
                }
            })
            .or(n.band),
        cell: if nr { s("nr_cell_id") } else { s("cell_id") },
        sinr: if nr { f("nr_snr") } else { f("lte_snr") },
        ours: op.map(|o| !o.get("active").is_none_or(Value::is_null)),
    }
}

/// rtnetlink: a global address left an rmnet link.
pub fn drop_seen(hub: &crate::block::Hub) {
    let sig = hub.view("signal").filter(|v| !v.stale);
    let op = hub.view("op").filter(|v| !v.stale);
    let d = drop_from(
        sig.as_ref().map(|v| &v.data),
        op.as_ref().map(|v| &v.data),
        boot_ms(),
    );
    ring().drop_seen(d);
}

/// `/api/diagnose`'s `band_flap`: the replay's answer and the window it saw.
pub fn view() -> Value {
    let ssr = crate::crashwatch::io::enabled().then(|| LAST_SSR_MS.load(Ordering::Relaxed));
    let e = ring().evidence(boot_ms(), ssr);
    json!({
        "advice": replay(&e),
        "drops": e.drops,
        "covered_s": e.covered_s,
        "window_s": WINDOW_S,
    })
}
