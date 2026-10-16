//! T5 测试（docs/STATE_V2.md 第 1–4 节）。进程内的 `Hub` + `Feed`，不起 HTTP、不依赖真实时间。

use super::*;
use crate::block::BlockSpec;
use futures_util::FutureExt;
use serde_json::json;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};
use tokio::time::Instant;

fn setup(capacity: usize) -> (Arc<Hub>, Arc<Feed>) {
    let feed = Feed::new(capacity);
    let hub = Arc::new(Hub::new(
        vec![
            BlockSpec::new("a", "a", "list", Duration::from_secs(5)),
            BlockSpec::new("b", "b", "list", Duration::from_secs(5)),
        ],
        Box::new(FeedSink(feed.clone())),
    ));
    (hub, feed)
}

/// 一帧 SSE → (事件名, data)。检查格式：只有 event/data 两行，data 单行，没有 id:/retry:。
fn parse(b: &[u8]) -> (String, Value) {
    let s = std::str::from_utf8(b).unwrap();
    let body = s
        .strip_suffix("\n\n")
        .expect("frame ends with a blank line");
    let lines: Vec<&str> = body.split('\n').collect();
    assert_eq!(lines.len(), 2, "{s:?}");
    let event = lines[0].strip_prefix("event: ").expect("event line");
    let data = lines[1].strip_prefix("data: ").expect("data line");
    (event.into(), serde_json::from_str(data).unwrap())
}

fn seq(v: &Value) -> u64 {
    v["seq"].as_u64().unwrap()
}

/// 块 a 读到一个新值（一条 block 事件）。
fn bump(hub: &Hub, n: u64) {
    hub.record_read(0, Ok(json!({ "n": n })), Instant::now());
}

fn beat(hub: &Hub) {
    hub.round_end(Instant::now(), Duration::from_secs(1));
}

/// 流里现在已经有的帧（不等）。流结束时返回 `(帧, true)`。
fn drain<S: Stream<Item = Result<Bytes, Infallible>> + Unpin>(s: &mut S) -> (Vec<Bytes>, bool) {
    let mut out = Vec::new();
    loop {
        match s.next().now_or_never() {
            Some(Some(Ok(b))) => out.push(b),
            Some(None) => return (out, true),
            None => return (out, false),
        }
    }
}

/// V2-3：订阅方的判断——`seq` ≠ 上一条 + 1 就是不连续。
fn first_gap(seqs: &[u64]) -> Option<usize> {
    seqs.windows(2)
        .position(|w| w[1] != w[0] + 1)
        .map(|i| i + 1)
}

#[test]
fn v2_epoch_changes_on_restart() {
    let (a, b) = (Feed::new(CAPACITY), Feed::new(CAPACITY));
    assert_ne!(a.epoch(), b.epoch());
    assert_eq!(a.epoch().len(), 16);
    // 同一次启动里，事件和 /v2/state 都带同一个 epoch。
    let (hub, feed) = setup(CAPACITY);
    let (mut rx, snap) = subscribe(&hub, &feed);
    bump(&hub, 1);
    beat(&hub);
    let snap: Value = serde_json::from_str(&snap).unwrap();
    let state: Value = serde_json::from_str(&state_json(&hub, &feed)).unwrap();
    for v in [
        snap,
        state,
        parse(&rx.try_recv().unwrap()).1,
        parse(&rx.try_recv().unwrap()).1,
    ] {
        assert_eq!(v["epoch"], feed.epoch());
    }
}

#[test]
fn v2_seq_only_block_and_heartbeat_increment() {
    let (hub, feed) = setup(CAPACITY);
    let (mut rx, snap) = subscribe(&hub, &feed);
    assert_eq!(seq(&serde_json::from_str(&snap).unwrap()), 0);
    // snapshot、/v2/state、同样的值再读一次（不发）都不占号。
    let _ = subscribe(&hub, &feed);
    let _ = state_json(&hub, &feed);
    bump(&hub, 1);
    bump(&hub, 1);
    let _ = subscribe(&hub, &feed);
    beat(&hub);
    let _ = state_json(&hub, &feed);
    bump(&hub, 2);
    let got: Vec<(String, u64)> = std::iter::from_fn(|| rx.try_recv().ok())
        .map(|b| {
            let (e, v) = parse(&b);
            (e, seq(&v))
        })
        .collect();
    assert_eq!(
        got,
        [
            ("block".into(), 1),
            ("heartbeat".into(), 2),
            ("block".into(), 3)
        ]
    );
    let state: Value = serde_json::from_str(&state_json(&hub, &feed)).unwrap();
    assert_eq!(seq(&state), 3);
}

#[test]
fn v2_seq_contiguous_with_heartbeats_between_blocks() {
    let (hub, feed) = setup(CAPACITY);
    let (mut rx, _) = subscribe(&hub, &feed);
    for n in 0..10 {
        bump(&hub, n);
        beat(&hub);
        beat(&hub);
        hub.record_read(1, Ok(json!({ "m": n })), Instant::now());
    }
    let frames: Vec<(String, u64)> = std::iter::from_fn(|| rx.try_recv().ok())
        .map(|b| {
            let (e, v) = parse(&b);
            (e, seq(&v))
        })
        .collect();
    assert_eq!(frames.len(), 40);
    assert_eq!(frames.iter().filter(|f| f.0 == "heartbeat").count(), 20);
    let seqs: Vec<u64> = frames.iter().map(|f| f.1).collect();
    assert_eq!(seqs, (1..=40).collect::<Vec<_>>());
    assert_eq!(first_gap(&seqs), None);
}

#[test]
fn v2_missing_heartbeat_detected_as_gap() {
    let (hub, feed) = setup(CAPACITY);
    let (mut rx, _) = subscribe(&hub, &feed);
    bump(&hub, 1);
    beat(&hub);
    bump(&hub, 2);
    let mut frames: Vec<(String, u64)> = std::iter::from_fn(|| rx.try_recv().ok())
        .map(|b| {
            let (e, v) = parse(&b);
            (e, seq(&v))
        })
        .collect();
    assert_eq!(frames[1].0, "heartbeat");
    // 丢掉中间那条心跳：两条块事件的 seq 不相邻，订阅方发现不连续。
    frames.remove(1);
    let seqs: Vec<u64> = frames.iter().map(|f| f.1).collect();
    assert_eq!(seqs, [1, 3]);
    assert_eq!(first_gap(&seqs), Some(1));
}

#[test]
fn v2_snapshot_cut_then_next_is_plus_one() {
    let (hub, feed) = setup(CAPACITY);
    // 启动后还没发过广播事件：切点 0，下一条是 1。
    let mut s0 = Box::pin(stream(&hub, &feed, ()));
    let (f, _) = drain(&mut s0);
    let (e, v) = parse(&f[0]);
    assert_eq!((e.as_str(), seq(&v)), ("snapshot", 0));
    bump(&hub, 1);
    beat(&hub);
    bump(&hub, 2);
    let mut s = Box::pin(stream(&hub, &feed, ()));
    bump(&hub, 3);
    let (f, closed) = drain(&mut s);
    assert!(!closed);
    let (e, snap) = parse(&f[0]);
    assert_eq!((e.as_str(), seq(&snap)), ("snapshot", 3));
    // 切点之前的事件已经在快照里：a 是第 2 个值。
    assert_eq!(snap["blocks"]["a"]["data"], json!({"n":2}));
    assert_eq!(snap["blocks"]["a"]["revision"], 2);
    let (e, next) = parse(&f[1]);
    assert_eq!((e.as_str(), seq(&next)), ("block", 4));
    assert_eq!(f.len(), 2);
    assert_eq!(seq(&parse(&drain(&mut s0).0[0]).1), 1);
}

#[test]
fn v2_new_subscriber_keeps_existing_seq_contiguous() {
    let (hub, feed) = setup(CAPACITY);
    let mut a = Box::pin(stream(&hub, &feed, ()));
    let mut a_seqs = Vec::new();
    let mut b = None;
    for n in 0..20 {
        bump(&hub, n);
        beat(&hub);
        if n == 7 {
            b = Some(Box::pin(stream(&hub, &feed, ())));
        }
        for f in drain(&mut a).0 {
            let (e, v) = parse(&f);
            if e != "snapshot" {
                a_seqs.push(seq(&v));
            }
        }
    }
    assert_eq!(a_seqs, (1..=40).collect::<Vec<_>>(), "A 不受 B 影响");
    let (fb, _) = drain(b.as_mut().unwrap());
    let cut = seq(&parse(&fb[0]).1);
    assert_eq!(cut, 16, "B 在第 8 轮结束后连上");
    let b_seqs: Vec<u64> = fb[1..].iter().map(|f| seq(&parse(f).1)).collect();
    assert_eq!(b_seqs.first(), Some(&(cut + 1)));
    assert_eq!(first_gap(&b_seqs), None);
    assert_eq!(b_seqs.last(), Some(&40));
}

/// 把事件应用到快照上（订阅方的做法），得到它眼里的全部块。
/// 数据没变的读取不发 `block`，但 `observed_at` 会更新，这由心跳带过来。
fn apply(state: &mut Value, ev: &Value) {
    state["seq"] = ev["seq"].clone();
    if let Some(blocks) = ev["blocks"].as_object() {
        for (name, at) in blocks {
            state["blocks"][name]["observed_at"] = at.clone();
        }
        return;
    }
    let name = ev["name"].as_str().unwrap();
    state["blocks"][name] = json!({
        "revision": ev["revision"],
        "observed_at": ev["observed_at"],
        "stale": ev["stale"],
        "data": ev["data"],
    });
    state["seq"] = ev["seq"].clone();
}

#[test]
fn v2_concurrent_subscribe_no_loss_no_dup() {
    const ROUNDS: u64 = 400;
    const SUBSCRIBERS: usize = 8;
    for pass in 0..20 {
        // 容量足够大：这里测切点，不测 Lagged。
        let (hub, feed) = setup(1 << 16);
        let started = Arc::new(AtomicBool::new(false));
        let writer = {
            let (hub, started) = (hub.clone(), started.clone());
            thread::spawn(move || {
                started.store(true, Ordering::SeqCst);
                for n in 0..ROUNDS {
                    // 每轮：a 必变，b 隔轮变，心跳。
                    bump(&hub, n);
                    hub.record_read(1, Ok(json!({ "m": n / 2 })), Instant::now());
                    beat(&hub);
                }
            })
        };
        let final_seq = ROUNDS + ROUNDS.div_ceil(2) + ROUNDS;
        let readers: Vec<_> = (0..SUBSCRIBERS)
            .map(|k| {
                let (hub, feed, started) = (hub.clone(), feed.clone(), started.clone());
                thread::spawn(move || {
                    while !started.load(Ordering::SeqCst) {
                        thread::yield_now();
                    }
                    for _ in 0..(k * 50) {
                        thread::yield_now();
                    }
                    let (mut rx, snap) = subscribe(&hub, &feed);
                    let mut state: Value = serde_json::from_str(&snap).unwrap();
                    let mut last = seq(&state);
                    while last < final_seq {
                        let (e, v) = parse(&rx.blocking_recv().expect("no lag, no close"));
                        assert_eq!(seq(&v), last + 1, "pass {pass} reader {k}: 不丢不重");
                        last = seq(&v);
                        assert!(e == "block" || e == "heartbeat");
                        apply(&mut state, &v);
                    }
                    state
                })
            })
            .collect();
        writer.join().unwrap();
        let truth: Value = serde_json::from_str(&state_json(&hub, &feed)).unwrap();
        assert_eq!(seq(&truth), final_seq);
        for r in readers {
            let mut got = r.join().unwrap();
            got["seq"] = truth["seq"].clone();
            assert_eq!(got, truth, "pass {pass}: 快照 + 事件 = 最终状态");
        }
    }
}

#[test]
fn v2_state_endpoint_matches_snapshot_shape() {
    let (hub, feed) = setup(CAPACITY);
    bump(&hub, 1);
    beat(&hub);
    let (_rx, snap) = subscribe(&hub, &feed);
    let state = state_json(&hub, &feed);
    assert_eq!(state, snap, "同一时刻 /v2/state 和 snapshot 逐字节相同");
    let v: Value = serde_json::from_str(&state).unwrap();
    assert_eq!(seq(&v), 2);
    // 从没读成功的块：stale、data null、observed_at 0、revision 0（V2-14）。
    assert_eq!(
        v["blocks"]["b"],
        json!({"revision":0,"observed_at":0,"stale":true,"data":null})
    );
    // HTTP 响应：JSON，内容同上。
    let resp = state_response(&hub, &feed);
    assert_eq!(resp.headers()["content-type"], "application/json");
}

#[test]
fn v2_subscribers_receive_identical_bytes() {
    let (hub, feed) = setup(CAPACITY);
    let (mut r1, _) = subscribe(&hub, &feed);
    let (mut r2, _) = subscribe(&hub, &feed);
    bump(&hub, 1);
    beat(&hub);
    for _ in 0..2 {
        let (a, b) = (r1.try_recv().unwrap(), r2.try_recv().unwrap());
        assert_eq!(a, b);
        // 同一份缓冲区：只序列化了一次。
        assert_eq!(a.as_ptr(), b.as_ptr());
    }
}

#[test]
fn v2_slow_subscriber_lagged_is_closed() {
    let (hub, feed) = setup(CAPACITY);
    let mut slow = Box::pin(stream(&hub, &feed, ()));
    // 落后超过容量。
    for n in 0..(CAPACITY as u64 + 1) {
        bump(&hub, n);
    }
    let (f, closed) = drain(&mut slow);
    assert!(closed, "Lagged 之后流结束，SSE 连接关掉");
    assert_eq!(f.len(), 1, "只有连上时的 snapshot，不补发");
    assert_eq!(parse(&f[0]).0, "snapshot");
    // 落后正好等于容量：不算 Lagged。
    let mut ok = Box::pin(stream(&hub, &feed, ()));
    for n in 100..(100 + CAPACITY as u64) {
        bump(&hub, n);
    }
    let (f, closed) = drain(&mut ok);
    assert!(!closed);
    assert_eq!(f.len(), 1 + CAPACITY);
}

#[test]
fn v2_lagged_reconnect_gets_new_snapshot() {
    let (hub, feed) = setup(CAPACITY);
    let mut slow = Box::pin(stream(&hub, &feed, ()));
    for n in 0..100 {
        bump(&hub, n);
    }
    assert!(drain(&mut slow).1);
    // 重连：第一条是新的 snapshot，切点 = 当前 seq，数据是最新的。
    let mut again = Box::pin(stream(&hub, &feed, ()));
    bump(&hub, 1000);
    let (f, closed) = drain(&mut again);
    assert!(!closed);
    let (e, snap) = parse(&f[0]);
    assert_eq!((e.as_str(), seq(&snap)), ("snapshot", 100));
    assert_eq!(snap["blocks"]["a"]["data"], json!({"n":99}));
    assert_eq!(seq(&parse(&f[1]).1), 101);
}

#[test]
fn v2_other_subscriber_contiguous_during_lag() {
    let (hub, feed) = setup(CAPACITY);
    let mut slow = Box::pin(stream(&hub, &feed, ()));
    let mut fast = Box::pin(stream(&hub, &feed, ()));
    let mut seqs = Vec::new();
    for n in 0..300 {
        bump(&hub, n);
        if n % 10 == 0 {
            beat(&hub);
        }
        for f in drain(&mut fast).0 {
            let (e, v) = parse(&f);
            if e != "snapshot" {
                seqs.push(seq(&v));
            }
        }
    }
    assert!(drain(&mut slow).1, "慢的被关掉");
    assert_eq!(seqs, (1..=330).collect::<Vec<_>>(), "快的不受影响");
}

#[test]
fn v2_event_shapes_match_doc() {
    let (hub, feed) = setup(CAPACITY);
    bump(&hub, 1);
    let mut s = Box::pin(stream(&hub, &feed, ()));
    hub.record_read(1, Ok(json!({"x":"y"})), Instant::now());
    beat(&hub);
    let (f, _) = drain(&mut s);
    let keys = |v: &Value| {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    let (e, snap) = parse(&f[0]);
    assert_eq!(e, "snapshot");
    assert_eq!(keys(&snap), ["blocks", "epoch", "seq"]);
    assert!(snap["epoch"].is_string());
    let a = &snap["blocks"]["a"];
    assert_eq!(keys(a), ["data", "observed_at", "revision", "stale"]);
    assert!(
        a["observed_at"].as_u64().unwrap() > 1_000_000_000,
        "单位是秒"
    );
    assert!(
        a["observed_at"].as_u64().unwrap() < 10_000_000_000,
        "单位是秒"
    );
    let (e, block) = parse(&f[1]);
    assert_eq!(e, "block");
    assert_eq!(
        keys(&block),
        [
            "data",
            "epoch",
            "name",
            "observed_at",
            "revision",
            "seq",
            "stale"
        ]
    );
    assert_eq!(
        (&block["name"], &block["revision"], &block["stale"]),
        (&json!("b"), &json!(1), &json!(false))
    );
    assert_eq!(block["data"], json!({"x":"y"}));
    let (e, hb) = parse(&f[2]);
    assert_eq!(e, "heartbeat");
    assert_eq!(keys(&hb), ["blocks", "epoch", "exec_age_ms", "seq"]);
    assert!(hb["exec_age_ms"].is_u64());
    assert_eq!(keys(&hb["blocks"]), ["a", "b"]);
    assert!(hb["blocks"]["a"].is_u64());
    // 字段顺序照文档（epoch、seq 在前）。
    let text = std::str::from_utf8(&f[1]).unwrap();
    assert!(
        text.starts_with("event: block\ndata: {\"epoch\":"),
        "{text}"
    );
    // HTTP 响应头。
    let resp = events_response(stream(&hub, &feed, ()));
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
}
