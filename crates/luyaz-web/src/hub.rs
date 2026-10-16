//! One upstream `/v2/events` connection to u60d, shared by every open tab (u60-web.md E2).
//!
//! ```text
//!  tabs ──subscribe()──▶ Hub ──(subs > 0)──▶ connect GET /v2/events ──▶ snapshot, block, heartbeat
//!   ▲                     │                     │ error / 30 s silence ─▶ Down, retry 1,2,4,5 s
//!   └──── Change ◀────────┘                     │ subs == 0 for `idle` ─▶ drop it, Idle
//! ```
//! u60d allows 16 SSE clients and the touchscreen needs one, so the browser never talks to it directly.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::{Notify, broadcast, watch};

use crate::sse::{Frame, Parser};

/// How long the trend keeps samples (seconds of device time).
pub const TREND_SECS: u64 = 120;
/// No bytes for this long (u60d heartbeats every few seconds) = the connection is dead.
const STALL: Duration = Duration::from_secs(30);
/// u60d's executor has not moved for this long (heartbeat `exec_age_ms`): writes are off (the
/// agent's `EXEC_STALL`, write-op-layer.md DD8/D12).
pub const EXEC_STALL_MS: u64 = 20_000;

#[derive(Debug, Clone)]
pub struct Block {
    pub revision: u64,
    pub observed_at: u64,
    pub stale: bool,
    pub data: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// Nobody is watching; no upstream connection.
    Idle,
    Connecting,
    Live,
    /// Lost u60d; retrying while someone watches.
    Down,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// Everything may have changed (new snapshot).
    All,
    Block(String),
    Link,
}

pub struct St {
    pub epoch: String,
    pub blocks: HashMap<String, Block>,
    pub link: Link,
    /// When the last snapshot/block/heartbeat arrived (our clock, not the device's).
    pub last_ok: Option<Instant>,
    /// Serving-cell RSRP by device `observed_at`, last `TREND_SECS`.
    pub history: VecDeque<(u64, i64)>,
    /// Bumped on every change; a page rendered at `gen` knows whether its stream must catch up.
    pub generation: u64,
    /// Upstream connections opened so far (tests, diagnostics).
    pub connects: u64,
    /// What the frame reads besides the blocks, polled while tabs are open (topbar.rs).
    pub side: Side,
    /// When the `op` block last arrived (our clock): its `remaining_ms` was as of then.
    pub op_at: Option<Instant>,
    /// The last heartbeat's `exec_age_ms` was ≥ [`EXEC_STALL_MS`]; tabs hear `Block("exec")`.
    pub stuck: bool,
    /// /trends: one sample per `live` block, by arrival (our clock), last [`crate::trends::WINDOW`].
    pub samples: VecDeque<crate::trends::Sample>,
}

/// Replies polled by `topbar::poll`; `None` until the first answer, and again after a failed one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Side {
    /// u60d `/v2/api/public/status` data: `alerts.unread`.
    pub public: Option<Value>,
    /// u60d `/v2/api/alerts` data: the alert list.
    pub alerts: Option<Value>,
    /// The status Tailscale is read from: u60d's public status since 8d-2 (was the agent's).
    pub agent: Option<Value>,
}

pub struct Hub {
    st: Mutex<St>,
    tx: broadcast::Sender<Change>,
    subs: watch::Sender<usize>,
    kick: Notify,
}

/// A tab's subscription; dropping it is how the hub learns the tab went away.
pub struct Sub {
    hub: Arc<Hub>,
    pub rx: broadcast::Receiver<Change>,
}

impl Drop for Sub {
    fn drop(&mut self) {
        self.hub.subs.send_modify(|n| *n -= 1);
    }
}

impl Hub {
    pub fn new() -> Arc<Hub> {
        Arc::new(Hub {
            st: Mutex::new(St {
                epoch: String::new(),
                blocks: HashMap::new(),
                link: Link::Idle,
                last_ok: None,
                history: VecDeque::new(),
                generation: 0,
                connects: 0,
                side: Side::default(),
                op_at: None,
                stuck: false,
                samples: VecDeque::new(),
            }),
            tx: broadcast::channel(256).0,
            subs: watch::channel(0).0,
            kick: Notify::new(),
        })
    }

    pub fn subscribe(self: &Arc<Self>) -> Sub {
        let rx = self.tx.subscribe();
        self.subs.send_modify(|n| *n += 1);
        Sub {
            hub: self.clone(),
            rx,
        }
    }

    /// Changes only, no subscriber count (writes waiting for their readback).
    pub fn watch(&self) -> broadcast::Receiver<Change> {
        self.tx.subscribe()
    }

    pub fn subscribers(&self) -> usize {
        *self.subs.borrow()
    }

    /// The subscriber count as it changes (the side poller runs only while it is above 0).
    pub fn subs_watch(&self) -> watch::Receiver<usize> {
        self.subs.subscribe()
    }

    /// Replaces part of the side data; tabs hear `Change::Block("side")` only if it changed.
    pub fn set_side(&self, f: impl FnOnce(&mut Side)) {
        self.change(|st| {
            let before = st.side.clone();
            f(&mut st.side);
            (st.side != before).then(|| Change::Block("side".into()))
        });
    }

    pub fn read<R>(&self, f: impl FnOnce(&St) -> R) -> R {
        f(&self.st.lock().unwrap())
    }

    /// "Retry now": skip the rest of the backoff.
    pub fn retry(&self) {
        self.kick.notify_one();
    }

    fn change(&self, f: impl FnOnce(&mut St) -> Option<Change>) {
        let c = {
            let mut st = self.st.lock().unwrap();
            let c = f(&mut st);
            if c.is_some() {
                st.generation += 1;
            }
            c
        };
        if let Some(c) = c {
            let _ = self.tx.send(c);
        }
    }

    #[cfg(test)]
    pub fn set_link_for_test(&self, link: Link) {
        self.set_link(link);
    }

    /// Change the state by hand (tests), telling the tabs everything may have changed.
    #[cfg(test)]
    pub fn change_for_test(&self, f: impl FnOnce(&mut St)) {
        self.change(|st| {
            f(st);
            Some(Change::All)
        });
    }

    fn set_link(&self, link: Link) {
        self.change(|st| {
            if st.link == link {
                return None;
            }
            st.link = link;
            Some(Change::Link)
        });
    }

    /// Applies one u60d frame. Public for tests.
    pub fn apply(&self, f: &Frame) {
        let Ok(v) = serde_json::from_str::<Value>(&f.data) else {
            return;
        };
        match f.event.as_str() {
            "snapshot" => self.change(|st| {
                st.epoch = v["epoch"].as_str().unwrap_or_default().to_owned();
                st.blocks.clear();
                if let Some(bs) = v["blocks"].as_object() {
                    for (name, b) in bs {
                        st.blocks.insert(name.clone(), block(b));
                    }
                }
                st.link = Link::Live;
                st.last_ok = Some(Instant::now());
                st.op_at = st.blocks.contains_key("op").then(Instant::now);
                st.stuck = false; // the new connection's heartbeats say again
                record_trend(st);
                crate::trends::record(st, Instant::now());
                Some(Change::All)
            }),
            "block" => self.change(|st| {
                st.last_ok = Some(Instant::now());
                let name = v["name"].as_str()?.to_owned();
                if v["epoch"].as_str() != Some(st.epoch.as_str()) {
                    return None; // a new epoch always starts with its own snapshot
                }
                let b = block(&v);
                if st
                    .blocks
                    .get(&name)
                    .is_some_and(|old| old.revision >= b.revision)
                {
                    return None;
                }
                st.blocks.insert(name.clone(), b);
                if name == "signal" {
                    record_trend(st);
                }
                if name == "op" {
                    st.op_at = Some(Instant::now());
                }
                if name == "live" {
                    crate::trends::record(st, Instant::now());
                }
                Some(Change::Block(name))
            }),
            "heartbeat" => self.change(|st| {
                st.last_ok = Some(Instant::now());
                let stuck = v["exec_age_ms"]
                    .as_u64()
                    .is_some_and(|ms| ms >= EXEC_STALL_MS);
                if stuck == st.stuck {
                    return None;
                }
                st.stuck = stuck;
                Some(Change::Block("exec".into()))
            }),
            _ => {}
        }
    }
}

fn block(v: &Value) -> Block {
    Block {
        revision: v["revision"].as_u64().unwrap_or(0),
        observed_at: v["observed_at"].as_u64().unwrap_or(0),
        stale: v["stale"].as_bool().unwrap_or(false),
        data: v["data"].clone(),
    }
}

fn record_trend(st: &mut St) {
    let Some(b) = st.blocks.get("signal") else {
        return;
    };
    let Some(rsrp) = crate::view::serving_rsrp(&b.data) else {
        return;
    };
    let at = b.observed_at;
    if st.history.back().is_some_and(|&(t, _)| t >= at) {
        return;
    }
    st.history.push_back((at, rsrp));
    while st
        .history
        .front()
        .is_some_and(|&(t, _)| t + TREND_SECS < at)
    {
        st.history.pop_front();
    }
}

/// Resolves once nobody has been subscribed for `idle` without a break.
async fn idle_for(rx: &mut watch::Receiver<usize>, idle: Duration) {
    loop {
        while *rx.borrow_and_update() > 0 {
            if rx.changed().await.is_err() {
                return;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(idle) => return,
            r = rx.changed() => if r.is_err() { return },
        }
    }
}

/// The upstream loop. Runs forever; connects only while someone watches.
pub async fn run(hub: Arc<Hub>, events_url: String, idle: Duration) {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .expect("http client");
    let mut subs = hub.subs.subscribe();
    let mut backoff = 1u64;
    loop {
        while *subs.borrow_and_update() == 0 {
            hub.set_link(Link::Idle);
            if subs.changed().await.is_err() {
                return;
            }
        }
        hub.set_link(Link::Connecting);
        hub.change(|st| {
            st.connects += 1;
            None
        });
        let resp = client
            .get(&events_url)
            .send()
            .await
            .and_then(|r| r.error_for_status());
        let gone = match resp {
            Err(_) => false,
            Ok(resp) => {
                let mut body = resp.bytes_stream();
                let mut parser = Parser::default();
                let mut gone = false;
                // One future for the whole connection: recreating it per chunk would restart its
                // timer on every heartbeat and the connection would never close.
                let idle_gone = idle_for(&mut subs, idle);
                tokio::pin!(idle_gone);
                loop {
                    tokio::select! {
                        next = tokio::time::timeout(STALL, body.next()) => match next {
                            Ok(Some(Ok(bytes))) => {
                                for f in parser.feed(&bytes) {
                                    hub.apply(&f);
                                }
                                backoff = 1;
                            }
                            _ => break,
                        },
                        _ = &mut idle_gone => { gone = true; break; }
                    }
                }
                gone
            }
        };
        if gone {
            continue; // the top of the loop marks it Idle and waits for the next tab
        }
        hub.set_link(Link::Down);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
            _ = hub.kick.notified() => {}
        }
        backoff = (backoff * 2).min(5);
    }
}

#[cfg(test)]
mod tests {
    //! Against the real mock over real TCP (a fake stream would not catch fd/timer mistakes).
    use super::*;
    use serde_json::json;

    fn capture() -> String {
        let snap = json!({"epoch":"e1","seq":1,"blocks":{
            "signal":{"revision":1,"observed_at":1000,"stale":false,"data":{"type":"SA","bars":4,"nr_rsrp":-97}},
            "interfaces":{"revision":1,"observed_at":1000,"stale":false,"data":{"cellular":{"roam_enable":0}}}}});
        let mut s = format!("event: snapshot\ndata: {snap}\n\n");
        for i in 1..=4u64 {
            let b = json!({"epoch":"e1","seq":1+i,"name":"signal","revision":1+i,"observed_at":1000+i,"stale":false,"data":{"type":"SA","bars":4,"nr_rsrp":-97-(i as i64)}});
            s.push_str(&format!("event: block\ndata: {b}\n\n"));
            s.push_str(&format!(
                "event: heartbeat\ndata: {}\n\n",
                json!({"epoch":"e1","seq":1+i,"blocks":[],"exec_age_ms":5})
            ));
        }
        s
    }

    async fn start_mock() -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        // speed 10: one recorded second = 100 ms
        let r = crate::mock::router(&capture(), 10.0);
        tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
        format!("http://{addr}")
    }

    async fn mock_open(base: &str) -> u64 {
        let v: Value = reqwest::get(format!("{base}/mock/stats"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        v["connections"].as_u64().unwrap()
    }

    async fn until(what: &str, mut f: impl AsyncFnMut() -> bool) {
        for _ in 0..100 {
            if f().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {what}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn two_tabs_share_one_upstream_and_it_closes_after_idle() {
        let base = start_mock().await;
        let hub = Hub::new();
        tokio::spawn(run(
            hub.clone(),
            format!("{base}/v2/events"),
            Duration::from_millis(600),
        ));
        assert_eq!(mock_open(&base).await, 0, "nobody watching: no connection");
        let a = hub.subscribe();
        let b = hub.subscribe();
        until("live", async || hub.read(|s| s.link) == Link::Live).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(mock_open(&base).await, 1, "two tabs, one upstream");
        assert!(hub.read(|s| s.blocks.contains_key("signal")));
        drop(a);
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert_eq!(mock_open(&base).await, 1, "one tab left keeps it open");
        drop(b);
        // heartbeats keep arriving every 100 ms; the idle timer must still run out
        until("upstream closed", async || mock_open(&base).await == 0).await;
        until("idle", async || hub.read(|s| s.link) == Link::Idle).await;
        let _c = hub.subscribe();
        until("reconnected", async || mock_open(&base).await == 1).await;
        assert_eq!(hub.read(|s| s.connects), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropped_upstream_reconnects_and_new_epoch_replaces_blocks() {
        let base = start_mock().await;
        let hub = Hub::new();
        tokio::spawn(run(
            hub.clone(),
            format!("{base}/v2/events"),
            Duration::from_secs(10),
        ));
        let _tab = hub.subscribe();
        until("live", async || hub.read(|s| s.link) == Link::Live).await;
        let http = reqwest::Client::new();
        http.post(format!("{base}/mock/drop")).send().await.unwrap();
        until("down", async || hub.read(|s| s.link) == Link::Down).await;
        hub.retry();
        until("live again", async || hub.read(|s| s.link) == Link::Live).await;
        assert_eq!(hub.read(|s| s.connects), 2);
        http.post(format!("{base}/mock/epoch"))
            .send()
            .await
            .unwrap();
        hub.retry();
        until("new epoch", async || {
            hub.read(|s| s.epoch == "e1x" && s.link == Link::Live)
        })
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unreachable_upstream_is_down_not_live() {
        let hub = Hub::new();
        tokio::spawn(run(
            hub.clone(),
            "http://127.0.0.1:9/v2/events".into(),
            Duration::from_secs(10),
        ));
        let _tab = hub.subscribe();
        until("down", async || hub.read(|s| s.link) == Link::Down).await;
        assert!(hub.read(|s| s.blocks.is_empty()));
    }

    #[test]
    fn old_revisions_and_foreign_epochs_are_ignored_and_trend_keeps_two_minutes() {
        let hub = Hub::new();
        let f = |e: &str, d: Value| Frame {
            event: e.into(),
            data: d.to_string(),
        };
        hub.apply(&f("snapshot", json!({"epoch":"e","blocks":{"signal":{"revision":5,"observed_at":100,"data":{"nr_rsrp":-90}}}})));
        hub.apply(&f("block", json!({"epoch":"e","name":"signal","revision":4,"observed_at":101,"data":{"nr_rsrp":-80}})));
        hub.apply(&f("block", json!({"epoch":"other","name":"signal","revision":9,"observed_at":102,"data":{"nr_rsrp":-70}})));
        assert_eq!(
            hub.read(|s| s.blocks["signal"].data["nr_rsrp"].clone()),
            json!(-90)
        );
        for i in 1..=30u64 {
            hub.apply(&f("block", json!({"epoch":"e","name":"signal","revision":5+i,"observed_at":100+i*6,"data":{"nr_rsrp":-100}})));
        }
        let (first, last, n) = hub.read(|s| {
            (
                s.history.front().unwrap().0,
                s.history.back().unwrap().0,
                s.history.len(),
            )
        });
        assert_eq!(last, 280);
        assert!(
            last - first <= TREND_SECS && n == 21,
            "{first}..{last} n={n}"
        );
    }
}
