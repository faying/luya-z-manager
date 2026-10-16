//! 蜂窝口最近 30 秒发了多少包、收了多少包，给 `/v2/screen` 判「连上了但不通」（stall）。
//!
//! 设计在 manager `docs/designs/slow-diagnosis.md` §4.1、§13（工程复核 D3/D5）：
//! 时间窗记在采样循环里（每轮 `state::collect` 记一次），算好了当作普通输入交给
//! `screen::story()`，`story()` 仍是「给什么数据出什么结论」。门槛（≥ 20 包发、0 包收）
//! 在 screen.rs，这里只数。
//!
//! 计数来源：厂商的 `real_rx_packets/real_tx_packets`（`zwrt_data get_wwandst`，每轮
//! `collect` 本来就读，TTL 0），由 `collect` 传进来。不用 `rmnet_data0` 网卡计数：
//! 2026-10-02 真机核对（ER1），客户端大流量走 IPA 硬件转发，6 分钟里厂商计数收了
//! 37062 个包，`rmnet_data0` 只有 8805、`br-lan` 发往客户端 10931；只有设备自己的
//! 流量时两边一致（一分钟 649 对 595），所以厂商计数也包括设备本机的流量。
//!
//! 同一份读数还喂安静折叠（manager `docs/designs/u60-radio.md` §4，评审 D2/D10）：连续几轮
//! 收发包增量都是 0 记一段空闲，整段 ≥ 10 s 计入安静（跟诊断档 `diag::quiet_stats` 同定义，
//! 按轮间隔约 5 s 粒度）；长期层每次检查（60 s）取走一次，进 `hour_power`。时间是本进程的
//! MONOTONIC 毫秒，休眠不算在内。

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

/// The window the screen judges: at least this long between baseline and latest.
pub const SPAN_MS: u64 = 30_000;
/// Longer than this between two samples = the rounds stopped; the window says nothing.
const GAP_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sample {
    rx: u64,
    tx: u64,
    at_ms: u64,
}

/// Packets over the last ≥ 30 s on the cellular interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellWindow {
    pub tx_packets: u64,
    pub rx_packets: u64,
    pub span_ms: u64,
}

#[derive(Default)]
struct Ring(VecDeque<Sample>);

impl Ring {
    /// One round's reading. A counter going backwards (interface recreated on
    /// redial) starts over; samples older than the 30 s baseline are dropped.
    fn push(&mut self, s: Sample) {
        if self
            .0
            .back()
            .is_some_and(|last| s.rx < last.rx || s.tx < last.tx || s.at_ms < last.at_ms)
        {
            self.0.clear();
        }
        self.0.push_back(s);
        let cut = s.at_ms.saturating_sub(SPAN_MS);
        // keep the newest sample that is ≥ 30 s old as the baseline
        while self.0.len() >= 2 && self.0[1].at_ms <= cut {
            self.0.pop_front();
        }
    }

    /// No reading this round: whatever came before can't be compared any more.
    fn lose(&mut self) {
        self.0.clear();
    }

    fn window(&self) -> Option<CellWindow> {
        let (first, last) = (self.0.front()?, self.0.back()?);
        let span = last.at_ms - first.at_ms;
        if span < SPAN_MS {
            return None;
        }
        if self
            .0
            .iter()
            .zip(self.0.iter().skip(1))
            .any(|(a, b)| b.at_ms - a.at_ms > GAP_MS)
        {
            return None;
        }
        Some(CellWindow {
            tx_packets: last.tx - first.tx,
            rx_packets: last.rx - first.rx,
            span_ms: span,
        })
    }
}

/// A quiet run counts from this long (diag's `quiet10`).
pub const QUIET_MS: u64 = 10_000;

/// What the quiet fold saw between two takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuietTake {
    /// Milliseconds in zero-packet runs of ≥ [`QUIET_MS`].
    pub quiet_ms: u64,
    /// Milliseconds between readings that could be compared.
    pub obs_ms: u64,
    /// Packets (rx + tx) over those milliseconds.
    pub pkts: u64,
}

/// The quiet fold: consecutive rounds with no packet are one run; a run is credited once it
/// reaches [`QUIET_MS`], then as it grows. A part not yet credited when the run ends is dropped.
#[derive(Default)]
struct Quiet {
    last: Option<Sample>,
    run_ms: u64,
    pending_ms: u64,
    acc: QuietTake,
}

impl Quiet {
    fn push(&mut self, s: Sample) {
        let prev = self.last.replace(s);
        let Some(p) = prev else { return };
        if s.rx < p.rx || s.tx < p.tx || s.at_ms < p.at_ms {
            // redial / restart: nothing to compare
            self.run_ms = 0;
            self.pending_ms = 0;
            return;
        }
        let dt = s.at_ms - p.at_ms;
        let d = (s.rx - p.rx) + (s.tx - p.tx);
        self.acc.obs_ms += dt;
        self.acc.pkts += d;
        if d > 0 {
            self.run_ms = 0;
            self.pending_ms = 0;
            return;
        }
        self.run_ms += dt;
        self.pending_ms += dt;
        if self.run_ms >= QUIET_MS {
            self.acc.quiet_ms += self.pending_ms;
            self.pending_ms = 0;
        }
    }

    fn lose(&mut self) {
        self.last = None;
        self.run_ms = 0;
        self.pending_ms = 0;
    }

    fn take(&mut self) -> QuietTake {
        std::mem::take(&mut self.acc)
    }
}

static RING: OnceLock<Mutex<Ring>> = OnceLock::new();
static QUIET: OnceLock<Mutex<Quiet>> = OnceLock::new();

fn quiet() -> std::sync::MutexGuard<'static, Quiet> {
    QUIET
        .get_or_init(|| Mutex::new(Quiet::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The quiet fold since the last take (the long-term layer, once a check).
pub fn take_quiet() -> QuietTake {
    quiet().take()
}

fn ring() -> std::sync::MutexGuard<'static, Ring> {
    RING.get_or_init(|| Mutex::new(Ring::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The vendor counters out of one `get_wwandst` reply; None when either is
/// missing or not a number.
pub fn counts(traffic: &serde_json::Value) -> Option<(u64, u64)> {
    let n = |k: &str| match traffic.get(k)? {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    };
    Some((n("real_rx_packets")?, n("real_tx_packets")?))
}

/// Called once per sampling round with (rx, tx) packets; None = not read.
pub fn sample(now_ms: u64, counts: Option<(u64, u64)>) {
    match counts {
        Some((rx, tx)) => {
            let s = Sample {
                rx,
                tx,
                at_ms: now_ms,
            };
            ring().push(s);
            quiet().push(s);
        }
        None => {
            ring().lose();
            quiet().lose();
        }
    }
}

/// The current window for `/v2/screen`; `None` until 30 s of readings exist.
pub fn current() -> Option<CellWindow> {
    ring().window()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(r: &mut Ring, pts: &[(u64, u64, u64)]) {
        for &(at_ms, tx, rx) in pts {
            r.push(Sample { rx, tx, at_ms });
        }
    }

    #[test]
    fn needs_thirty_seconds() {
        let mut r = Ring::default();
        feed(&mut r, &[(0, 0, 0), (5_000, 5, 0), (25_000, 25, 0)]);
        assert_eq!(r.window(), None);
        feed(&mut r, &[(30_000, 30, 0)]);
        assert_eq!(
            r.window(),
            Some(CellWindow {
                tx_packets: 30,
                rx_packets: 0,
                span_ms: 30_000
            })
        );
    }

    #[test]
    fn baseline_is_the_newest_sample_thirty_seconds_back() {
        let mut r = Ring::default();
        let pts: Vec<_> = (0..=20).map(|k| (k * 5_000, k * 10, k)).collect();
        feed(&mut r, &pts);
        // latest at 100 s; baseline at 70 s
        assert_eq!(
            r.window(),
            Some(CellWindow {
                tx_packets: 60,
                rx_packets: 6,
                span_ms: 30_000
            })
        );
        assert!(r.0.len() <= 8);
    }

    #[test]
    fn one_second_rounds_keep_a_thirty_second_span() {
        let mut r = Ring::default();
        let pts: Vec<_> = (0..=120).map(|k| (k * 1_000 + 300, k, 0)).collect();
        feed(&mut r, &pts);
        let w = r.window().unwrap();
        assert_eq!((w.span_ms, w.tx_packets), (30_000, 30));
    }

    #[test]
    fn counter_going_back_starts_over() {
        let mut r = Ring::default();
        feed(&mut r, &[(0, 100, 50), (30_000, 200, 50), (35_000, 3, 0)]);
        assert_eq!(r.window(), None);
        feed(&mut r, &[(65_000, 40, 0)]);
        assert_eq!(r.window().unwrap().tx_packets, 37);
    }

    #[test]
    fn a_long_gap_says_nothing() {
        let mut r = Ring::default();
        feed(&mut r, &[(0, 0, 0), (61_000, 30, 0)]);
        assert_eq!(r.window(), None);
        feed(&mut r, &[(62_000, 31, 0)]);
        assert_eq!(r.window(), None);
        feed(&mut r, &[(91_000, 50, 0)]);
        assert_eq!(r.window().unwrap().tx_packets, 20);
    }

    #[test]
    fn vendor_counts() {
        let v = serde_json::json!({"real_rx_packets": 318245, "real_tx_packets": "223291", "real_rx_bytes": 1});
        assert_eq!(counts(&v), Some((318245, 223291)));
        assert_eq!(counts(&serde_json::json!({"real_rx_packets": 1})), None);
        assert_eq!(
            counts(&serde_json::json!({"real_rx_packets": "--", "real_tx_packets": 2})),
            None
        );
    }

    fn q(pts: &[(u64, u64)]) -> Quiet {
        let mut q = Quiet::default();
        for &(at_ms, rx) in pts {
            q.push(Sample { rx, tx: 0, at_ms });
        }
        q
    }

    #[test]
    fn quiet_counts_whole_runs_of_ten_seconds() {
        // 5 s rounds: one zero round (5 s) is not quiet; two (10 s) are, whole
        let mut a = q(&[(0, 0), (5_000, 1), (10_000, 1), (15_000, 2)]);
        assert_eq!(
            a.take(),
            QuietTake {
                quiet_ms: 0,
                obs_ms: 15_000,
                pkts: 2
            }
        );
        let mut b = q(&[(0, 0), (5_000, 0), (10_000, 0), (15_000, 0), (20_000, 3)]);
        assert_eq!(
            b.take(),
            QuietTake {
                quiet_ms: 15_000,
                obs_ms: 20_000,
                pkts: 3
            }
        );
        // same answer as diag's per-second definition
        let per_sec: Vec<u64> = (0..20).map(|s| u64::from(s >= 15)).collect();
        assert_eq!(crate::diag::quiet_stats(&per_sec).quiet10_pm, 750);
    }

    #[test]
    fn a_run_crossing_a_take_is_credited_once_it_reaches_ten_seconds() {
        let mut a = q(&[(0, 0), (5_000, 0)]);
        assert_eq!(a.take().quiet_ms, 0, "5 s so far: not yet quiet");
        a.push(Sample {
            rx: 0,
            tx: 0,
            at_ms: 10_000,
        });
        a.push(Sample {
            rx: 0,
            tx: 0,
            at_ms: 15_000,
        });
        let t = a.take();
        assert_eq!(
            (t.quiet_ms, t.obs_ms),
            (15_000, 10_000),
            "the earlier 5 s comes along"
        );
        a.push(Sample {
            rx: 0,
            tx: 0,
            at_ms: 20_000,
        });
        assert_eq!(a.take().quiet_ms, 5_000);
    }

    #[test]
    fn quiet_starts_over_on_a_reset_or_a_lost_reading() {
        let mut a = q(&[(0, 100), (5_000, 100), (10_000, 3), (15_000, 3)]);
        let t = a.take();
        assert_eq!((t.quiet_ms, t.obs_ms, t.pkts), (0, 10_000, 0));
        a.lose();
        a.push(Sample {
            rx: 3,
            tx: 0,
            at_ms: 20_000,
        });
        a.push(Sample {
            rx: 3,
            tx: 0,
            at_ms: 25_000,
        });
        let t = a.take();
        assert_eq!((t.quiet_ms, t.obs_ms), (0, 5_000));
    }

    #[test]
    fn a_missing_reading_starts_over() {
        let mut r = Ring::default();
        feed(&mut r, &[(0, 0, 0), (30_000, 30, 0)]);
        r.lose();
        assert_eq!(r.window(), None);
    }
}
