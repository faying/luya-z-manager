//! /tools/cpu (CPU 与内存): overall CPU and memory, a 5-minute CPU
//! trend, per-core use and clock, and the memory breakdown. Read-only.
//!
//! The old page polled the agent's `/api/cpu` and `/api/memory` every 5 s. Here the numbers are
//! u60d's `live` block (same /proc/stat and /proc/meminfo, sampled by u60d about every 5 s):
//! `runtime.cpu_usage_tenths` (overall), `cpu_cores` (per core, tenths, -1 = not known yet),
//! `cpu_freq_mhz` (cur/max), `memory_kb` (used = total − available, like the agent). The trend
//! is the hub's samples (trends.rs, by arrival, 5 minutes), not a per-tab list.
//!
//! What u60d does not have comes from the agent's `/api/cpu`, every 30 s per tab: **offline
//! cores** (/proc/stat drops them, so u60d does not list them; the agent reads sysfs) and each
//! core's `cpuinfo_max_freq` (what the old page showed as the max; u60d's `max` is
//! `scaling_max_freq`, used when the agent has not answered).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use askama::Template;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::agent::{Caller, Polled};
use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, split_leaves};

const PATH: &str = "/tools/cpu";
const AGENT_EVERY: Duration = Duration::from_secs(30);
const W: f64 = 320.0;
const H: f64 = 64.0;

/// One core as the agent's sysfs walk sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentCore {
    pub id: u32,
    pub online: bool,
    pub max_mhz: Option<i64>,
}

/// u60d's view of one online core: use %, cur MHz, max MHz.
type U60dCore = (Option<f64>, Option<i64>, Option<i64>);

pub struct Core {
    pub label: String,
    pub value: String,
}

pub struct View {
    pub l: Lang,
    /// "neutral" | "stale" | "bad"
    pub tone: &'static str,
    pub state: String,
    pub reason: Option<String>,
    pub retry: bool,
    pub stale: bool,
    /// The trend line's points (empty = collecting).
    pub points: String,
    pub cores: Vec<Core>,
    /// Used, available, cache and buffers, total (MiB), when memory is known.
    pub mem: Option<[(&'static str, String); 4]>,
}

impl View {
    pub fn symbol(&self) -> &'static str {
        match self.tone {
            "bad" => "■",
            "stale" => "▲",
            _ => "●",
        }
    }
}

fn mib(kb: f64) -> String {
    format!("{} MiB", (kb / 1024.0).round())
}

pub fn agent_cores(v: &Value) -> Option<Vec<AgentCore>> {
    let a = v["per_core"].as_array()?;
    Some(
        a.iter()
            .filter_map(|c| {
                Some(AgentCore {
                    id: c["id"].as_u64()? as u32,
                    online: c["online"].as_bool().unwrap_or(true),
                    max_mhz: c["max_mhz"].as_i64().filter(|m| *m > 0),
                })
            })
            .collect(),
    )
}

pub fn cores(rt: &Value, agent: Option<&[AgentCore]>, l: Lang) -> Vec<Core> {
    // u60d: "cpu3" → 3
    let mut u: BTreeMap<u32, U60dCore> = BTreeMap::new();
    if let Some(m) = rt["cpu_cores"].as_object() {
        for (k, v) in m {
            let Some(id) = k.strip_prefix("cpu").and_then(|i| i.parse().ok()) else {
                continue;
            };
            let f = &rt["cpu_freq_mhz"][k];
            u.insert(
                id,
                (
                    v.as_i64().filter(|t| *t >= 0).map(|t| t as f64 / 10.0),
                    f["cur"].as_i64().filter(|c| *c > 0),
                    f["max"].as_i64().filter(|c| *c > 0),
                ),
            );
        }
    }
    let mut ids: Vec<u32> = u.keys().copied().collect();
    for c in agent.unwrap_or_default() {
        if !ids.contains(&c.id) {
            ids.push(c.id);
        }
    }
    ids.sort_unstable();
    ids.into_iter()
        .map(|id| {
            let a = agent.and_then(|a| a.iter().find(|c| c.id == id));
            let value = match u.get(&id) {
                Some((usage, cur, umax)) => {
                    let max = a.and_then(|a| a.max_mhz).or(*umax);
                    [
                        Some(usage.map_or("—".into(), |x| format!("{x:.1}%"))),
                        cur.map(|c| match max {
                            Some(m) => format!("{c} / {m} MHz"),
                            None => format!("{c} MHz"),
                        }),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ")
                }
                // only the agent lists it: offline (/proc/stat drops offline cores)
                None if a.is_some_and(|a| !a.online) => l.pick("Offline", "离线").into(),
                None => "—".into(),
            };
            Core {
                label: format!("CPU {id}"),
                value,
            }
        })
        .collect()
}

fn trend(st: &St, now: Instant) -> String {
    let s: Vec<(Instant, f64)> = st
        .samples
        .iter()
        .filter(|s| now.saturating_duration_since(s.at) <= crate::trends::WINDOW)
        .filter_map(|s| s.cpu.map(|c| (s.at, c)))
        .collect();
    if s.len() < 2 {
        return String::new();
    }
    let end = s.last().unwrap().0;
    let win = crate::trends::WINDOW.as_secs_f64();
    s.iter()
        .map(|(t, v)| {
            let x = (1.0 - end.saturating_duration_since(*t).as_secs_f64() / win) * W;
            let y = 2.0 + (100.0 - v.clamp(0.0, 100.0)) / 100.0 * (H - 4.0);
            format!("{x:.0},{y:.0}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn view(st: &St, agent: Option<&[AgentCore]>, now: Instant, l: Lang) -> View {
    let b = st.blocks.get("live");
    let stale = b.is_some_and(|b| b.stale) || (b.is_some() && st.link != Link::Live);
    let null = Value::Null;
    let rt = b.map_or(&null, |b| &b.data["runtime"]);
    let overall = rt["cpu_usage_tenths"]
        .as_i64()
        .filter(|t| *t >= 0)
        .map(|t| t as f64 / 10.0);
    let m = &rt["memory_kb"];
    let mem = match (m["total"].as_f64(), m["available"].as_f64()) {
        (Some(t), Some(a)) if t > 0.0 => Some((t, a)),
        _ => None,
    };
    let failed = b.is_none() && matches!(st.link, Link::Live | Link::Down);
    let (tone, state) = if failed {
        (
            "bad",
            l.pick("Can't read CPU usage", "读不到 CPU 占用")
                .to_string(),
        )
    } else {
        (
            if stale { "stale" } else { "neutral" },
            match overall {
                Some(p) => format!("CPU {p:.1}%"),
                None => l.pick("Reading…", "正在读取…").into(),
            },
        )
    };
    let reason = if failed {
        Some(match st.link {
            Link::Down => l
                .pick("Can't reach the data service", "连不上数据服务")
                .into(),
            _ => l
                .pick(
                    "The data service has no CPU numbers.",
                    "数据服务没有 CPU 数据。",
                )
                .into(),
        })
    } else {
        mem.map(|(t, a)| {
            let pct = ((t - a) / t * 100.0).round();
            match l {
                Lang::En => format!("Memory {} of {} used ({pct}%)", mib(t - a), mib(t)),
                Lang::Zh => format!("内存已用 {} / {}（{pct}%）", mib(t - a), mib(t)),
            }
        })
    };
    View {
        l,
        tone,
        state,
        reason,
        retry: failed,
        stale,
        points: trend(st, now),
        cores: cores(rt, agent, l),
        mem: mem.map(|(t, a)| {
            let cached = m["cached"].as_f64().unwrap_or(0.0) + m["buffers"].as_f64().unwrap_or(0.0);
            [
                (l.pick("Used", "已用"), mib(t - a)),
                (l.pick("Available", "可用"), mib(a)),
                (l.pick("Cache and buffers", "缓存"), mib(cached)),
                (l.pick("Total", "总量"), mib(t)),
            ]
        }),
    }
}

// ---- templates and the stream ----

#[derive(Template)]
#[template(path = "cpu_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

pub struct Poll {
    who: Caller,
    l: Lang,
    agent: Option<Vec<AgentCore>>,
}

impl Poll {
    async fn fetch(&mut self, app: &Shared) {
        // extra (offline cores, cpuinfo max): keep the last answer when it fails
        if let Ok(v) = crate::agent::get(app, &self.who, "/api/cpu").await
            && let Some(c) = agent_cores(&v)
        {
            self.agent = Some(c);
        }
    }

    fn leaves(&self, app: &Shared) -> Vec<(String, String)> {
        self.leaves_with(app, true)
    }

    /// `line: false` = without the trend's points: the page leaves them out (a full 5-minute hub
    /// is several KB of them) and the stream's first push, which sends every leaf, brings them.
    fn leaves_with(&self, app: &Shared, line: bool) -> Vec<(String, String)> {
        app.hub.read(|st| {
            let mut v = view(st, self.agent.as_deref(), Instant::now(), self.l);
            if !line {
                v.points.clear();
            }
            split_leaves(&Leaves { v: &v }.render().unwrap())
        })
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        self.fetch(app).await;
        (self.leaves(app), AGENT_EVERY)
    }
    fn watches(&self, block: &str) -> bool {
        block == "live"
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(self.leaves(app))
    }
}

#[derive(Template)]
#[template(path = "cpu.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    leaves: Vec<(String, String)>,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let mut p = Poll {
        who: Caller::of(&app, &req),
        l: ctx.lang,
        agent: None,
    };
    p.fetch(&app).await;
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(
            st,
            &ctx,
            Some(format!("{PATH}/events?g={}", st.generation)),
        )
    });
    let html = Page {
        shell,
        l: ctx.lang,
        leaves: p.leaves_with(&app, false),
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(
        &headers,
        "text/html; charset=utf-8",
        html.into_bytes(),
        "no-store",
    )
}

pub async fn events(
    State(app): State<Shared>,
    Query(q): Query<StreamQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll {
        who: Caller::of(&app, &req),
        l: ctx.lang,
        agent: None,
    };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// Retry: reconnect to u60d now, and ask the agent again.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    app.hub.retry();
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::Hub;
    use crate::sse::Frame;
    use serde_json::json;

    fn live(rev: u64, cpu: i64) -> String {
        json!({"epoch":"e","name":"live","revision":rev,"observed_at":rev,"stale":false,
            "data":{"runtime":{"cpu_usage_tenths":cpu,
                "cpu_cores":{"cpu0":101,"cpu1":-1,"cpu2":92},
                "cpu_freq_mhz":{"cpu0":{"cur":2208,"max":2208},"cpu1":{"cur":0,"max":0},"cpu2":{"cur":825,"max":1800}},
                "memory_kb":{"total":1_628_400,"available":1_017_172,"free":569_064,"cached":426_640,"buffers":51_412}}}})
        .to_string()
    }

    fn hub(n: u64) -> std::sync::Arc<Hub> {
        let hub = Hub::new();
        hub.apply(&Frame {
            event: "snapshot".into(),
            data: json!({"epoch":"e","blocks":{}}).to_string(),
        });
        for i in 0..n {
            hub.apply(&Frame {
                event: "block".into(),
                data: live(i + 1, 120 + i as i64),
            });
        }
        hub.set_link_for_test(Link::Live);
        hub
    }

    #[test]
    fn numbers_like_the_old_page() {
        let v = hub(1).read(|st| view(st, None, Instant::now(), Lang::Zh));
        assert_eq!(v.state, "CPU 12.0%");
        assert_eq!(
            v.reason.as_deref(),
            Some("内存已用 597 MiB / 1590 MiB（38%）")
        );
        let mem = v.mem.unwrap();
        assert_eq!(mem[0], ("已用", "597 MiB".into()));
        assert_eq!(mem[1], ("可用", "993 MiB".into()));
        assert_eq!(mem[2], ("缓存", "467 MiB".into()));
        assert_eq!(mem[3], ("总量", "1590 MiB".into()));
        let c: Vec<_> = v
            .cores
            .iter()
            .map(|c| (c.label.as_str(), c.value.as_str()))
            .collect();
        assert_eq!(
            c,
            [
                ("CPU 0", "10.1% · 2208 / 2208 MHz"),
                ("CPU 1", "—"),
                ("CPU 2", "9.2% · 825 / 1800 MHz"),
            ]
        );
        assert!(v.points.is_empty(), "one sample: collecting");
    }

    #[test]
    fn the_agent_adds_offline_cores_and_the_cpuinfo_max() {
        let a = agent_cores(&json!({"per_core":[
            {"id":0,"online":true,"usage":9.0,"freq_mhz":1516,"max_mhz":2400},
            {"id":2,"online":true,"usage":9.0,"freq_mhz":1516,"max_mhz":2400},
            {"id":3,"online":false,"usage":null,"freq_mhz":null,"max_mhz":2208}]}))
        .unwrap();
        let v = hub(1).read(|st| view(st, Some(&a), Instant::now(), Lang::En));
        let c: Vec<_> = v
            .cores
            .iter()
            .map(|c| (c.label.as_str(), c.value.as_str()))
            .collect();
        assert_eq!(
            c,
            [
                ("CPU 0", "10.1% · 2208 / 2400 MHz"),
                ("CPU 1", "—"),
                ("CPU 2", "9.2% · 825 / 2400 MHz"),
                ("CPU 3", "Offline"),
            ]
        );
    }

    #[test]
    fn the_trend_draws_from_two_samples_and_no_block_says_so() {
        let v = hub(3).read(|st| view(st, None, Instant::now(), Lang::En));
        assert_eq!(v.points.split(' ').count(), 3);
        let h = Hub::new();
        h.set_link_for_test(Link::Down);
        let v = h.read(|st| view(st, None, Instant::now(), Lang::En));
        assert_eq!((v.tone, v.state.as_str()), ("bad", "Can't read CPU usage"));
        assert!(v.retry && v.mem.is_none());
        // not connected yet: reading, no error
        let v = Hub::new().read(|st| view(st, None, Instant::now(), Lang::Zh));
        assert_eq!((v.tone, v.state.as_str()), ("neutral", "正在读取…"));
    }

    #[test]
    fn the_english_page_has_no_chinese() {
        let a = vec![AgentCore {
            id: 3,
            online: false,
            max_mhz: None,
        }];
        for h in [hub(3), Hub::new()] {
            let v = h.read(|st| view(st, Some(&a), Instant::now(), Lang::En));
            let html = Leaves { v: &v }.render().unwrap();
            assert!(
                !html.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)
                    || ('\u{3000}'..='\u{303f}').contains(&c)
                    || ('\u{ff00}'..='\u{ffef}').contains(&c)),
                "{html}"
            );
        }
    }
}
