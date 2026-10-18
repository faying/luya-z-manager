//! Small /proc counters folded on the existing long-term check, never a new timer.
//! No process scan, netlink registration, subprocess, network call or separate writer.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// Allow one missed minute check; a longer awake gap is not continuous observation.
const MAX_AWAKE_GAP_S: i64 = 120;

#[derive(Clone, Debug, PartialEq)]
pub struct Process {
    pub pid: u32,
    pub start: u64,
    pub cpu: u64,
    /// Waited-for descendants' CPU, not live children or a fork counter.
    pub children: Option<u64>,
}

pub fn process(text: &str) -> Option<Process> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let pid = text[..open].trim().parse().ok()?;
    let fields: Vec<_> = text[close + 1..].split_ascii_whitespace().collect();
    let field = |n: usize| fields.get(n - 3)?.parse::<u64>().ok();
    Some(Process {
        pid,
        start: field(22)?,
        cpu: field(14)?.checked_add(field(15)?)?,
        children: field(16).and_then(|u| field(17)?.checked_add(u)),
    })
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct System {
    pub busy: Option<u64>,
    pub total: Option<u64>,
    pub tasks: Option<u64>,
    pub ctxt: Option<u64>,
}

pub fn system(text: &str) -> System {
    let mut out = System::default();
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        match fields.next() {
            Some("cpu") => {
                // First eight columns only: guest/guest_nice are already included
                // in user/nice. Missing or malformed columns are unknown, not zero.
                let ticks: Option<Vec<u64>> = fields.take(8).map(|v| v.parse().ok()).collect();
                if let Some(t) = ticks.filter(|v| v.len() == 8) {
                    out.total = t.iter().try_fold(0u64, |s, v| s.checked_add(*v));
                    out.busy = [0, 1, 2, 5, 6, 7]
                        .iter()
                        .try_fold(0u64, |s, i| s.checked_add(t[*i]));
                }
            }
            Some("processes") => out.tasks = fields.next().and_then(|v| v.parse().ok()),
            Some("ctxt") => out.ctxt = fields.next().and_then(|v| v.parse().ok()),
            _ => {}
        }
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Read {
    pub process: Option<Process>,
    pub system: System,
    pub hz: Option<u64>,
}

pub fn read(root: &Path, pid: u32) -> Read {
    static HZ: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let hz = *HZ.get_or_init(|| {
        // SAFETY: sysconf reads the process's clock-tick configuration, no pointer arguments.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        u64::try_from(hz).ok().filter(|v| *v > 0)
    });
    Read {
        process: std::fs::read_to_string(root.join(pid.to_string()).join("stat"))
            .ok()
            .and_then(|s| process(&s))
            .filter(|p| p.pid == pid),
        system: std::fs::read_to_string(root.join("stat"))
            .map(|s| system(&s))
            .unwrap_or_default(),
        hz,
    }
}

#[derive(Clone, Debug)]
struct Stamp {
    boot: String,
    up: i64,
    mono: i64,
    read: Read,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Counter {
    value: u64,
    observed_s: u64,
}

impl Counter {
    fn add(&mut self, delta: u64, dt: u64) -> bool {
        let (Some(value), Some(observed_s)) = (
            self.value.checked_add(delta),
            self.observed_s.checked_add(dt),
        ) else {
            return false;
        };
        self.value = value;
        self.observed_s = observed_s;
        true
    }

    fn count(&self) -> Value {
        json!({"count": (self.observed_s > 0).then_some(self.value),
               "observed_s": self.observed_s,
               "per_second": (self.observed_s > 0).then(|| self.value as f64 / self.observed_s as f64)})
    }

    fn cpu(&self, hz: Option<u64>) -> Value {
        let seconds = hz
            .filter(|v| *v > 0)
            .filter(|_| self.observed_s > 0)
            .map(|h| self.value as f64 / h as f64);
        json!({"ticks": (self.observed_s > 0).then_some(self.value),
               "observed_s": self.observed_s, "cpu_s": seconds,
               "single_core_percent": seconds.map(|s| 100.0 * s / self.observed_s as f64)})
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Acc {
    hz: Option<u64>,
    samples: u64,
    rejected: BTreeMap<String, u64>,
    core: Counter,
    children: Counter,
    busy: Counter,
    total: Counter,
    tasks: Counter,
    ctxt: Counter,
    /// Never bridge a Core restart, including restoration of the same saved fold.
    #[serde(skip)]
    prev: Option<Stamp>,
}

impl Acc {
    fn reject(&mut self, reason: &str) {
        let n = self.rejected.entry(reason.into()).or_default();
        *n = n.saturating_add(1);
    }

    pub fn observe(&mut self, read: Option<&Read>, boot: &str, up: i64, mono: i64) {
        self.samples = self.samples.saturating_add(1);
        let Some(r) = read else {
            self.prev = None;
            self.reject("sample_missing");
            return;
        };
        if boot.is_empty() || up < 0 || mono < 0 || mono > up {
            self.prev = None;
            self.reject("clock_invalid");
            return;
        }
        let now = Stamp {
            boot: boot.into(),
            up,
            mono,
            read: r.clone(),
        };
        let previous = self.prev.replace(now);
        let Some(p) = previous else {
            self.reject("baseline_only");
            return;
        };
        let dt = up.checked_sub(p.up).filter(|v| *v > 0);
        let awake = mono.checked_sub(p.mono).filter(|v| *v >= 0);
        let (Some(dt), Some(awake)) = (dt, awake) else {
            self.reject("clock_invalid");
            return;
        };
        if boot != p.boot || awake > dt {
            self.reject("clock_or_boot_changed");
            return;
        }
        if awake > MAX_AWAKE_GAP_S {
            self.reject("awake_gap");
            return;
        }
        let (Some(old), Some(new)) = (&p.read.process, &r.process) else {
            self.reject("process_identity_unknown");
            return;
        };
        if (old.pid, old.start) != (new.pid, new.start) {
            self.reject("process_changed");
            return;
        }
        let Some(hz) = r.hz.filter(|v| *v > 0).filter(|h| Some(*h) == p.read.hz) else {
            self.reject("tick_hz_unknown_or_changed");
            return;
        };
        if self.hz.is_some_and(|old| old != hz) {
            self.reject("tick_hz_changed_in_hour");
            return;
        }
        self.hz = Some(hz);
        let dt = dt as u64;
        for (name, counter, old, new) in [
            ("core_cpu", &mut self.core, Some(old.cpu), Some(new.cpu)),
            (
                "reaped_children_cpu",
                &mut self.children,
                old.children,
                new.children,
            ),
            (
                "task_creation",
                &mut self.tasks,
                p.read.system.tasks,
                r.system.tasks,
            ),
            (
                "context_switches",
                &mut self.ctxt,
                p.read.system.ctxt,
                r.system.ctxt,
            ),
        ] {
            let delta = old.zip(new).and_then(|(a, b)| b.checked_sub(a));
            let reason = match delta {
                Some(d) if counter.add(d, dt) => continue,
                Some(_) => "overflow",
                None if old.is_none() || new.is_none() => "missing",
                None => "counter_reset",
            };
            let n = self.rejected.entry(format!("{name}_{reason}")).or_default();
            *n = n.saturating_add(1);
        }
        let cpu = p
            .read
            .system
            .busy
            .zip(r.system.busy)
            .and_then(|(a, b)| b.checked_sub(a))
            .zip(
                p.read
                    .system
                    .total
                    .zip(r.system.total)
                    .and_then(|(a, b)| b.checked_sub(a)),
            );
        if let Some((busy, total)) = cpu.filter(|(b, t)| t > &0 && b <= t) {
            let (mut b, mut t) = (self.busy.clone(), self.total.clone());
            if b.add(busy, dt) && t.add(total, dt) {
                self.busy = b;
                self.total = t;
            } else {
                self.reject("system_cpu_overflow");
            }
        } else {
            self.reject("system_cpu_missing_or_reset");
        }
    }

    pub fn value(&self) -> Value {
        json!({"v": 1, "clock": "boottime_seconds", "hz": self.hz,
            "samples": self.samples, "rejected": self.rejected,
            "core_cpu": self.core.cpu(self.hz),
            "reaped_children_cpu": self.children.cpu(self.hz),
            "system_cpu": {
                "busy_ticks": (self.busy.observed_s > 0).then_some(self.busy.value),
                "total_ticks": (self.total.observed_s > 0).then_some(self.total.value),
                "observed_s": self.busy.observed_s,
                "cpu_s": self.hz.filter(|_| self.busy.observed_s > 0).map(|hz| self.busy.value as f64 / hz as f64),
                "all_core_percent": (self.total.value > 0).then(|| 100.0 * self.busy.value as f64 / self.total.value as f64)},
            "task_creation": self.tasks.count(), "context_switches": self.ctxt.count(),
            "true_wakeups": null})
    }
}

#[cfg(test)]
mod tests;
