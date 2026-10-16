//! `/v2/events`、`/v2/state`（docs/STATE_V2.md 第 1–4 节）。
//!
//! - `Feed` 是 `Hub` 的事件出口：每条 `block`/`heartbeat` 在 `Hub` 的锁里序列化一次成 SSE 帧，
//!   `broadcast` 给所有订阅者，大家共用同一份 `Bytes`（V2-7）。
//! - 新连接在同一把锁里先订阅再拍快照（`Hub::snapshot_with`），snapshot 的 `seq` 是切点（V2-4、V2-5）。
//! - 订阅者落后超过容量（`Lagged`）就结束它的流，也就是关掉 SSE；重连先收新的 snapshot（V2-8）。
//! - 不发 `retry:`、`id:`，不发 keep-alive 注释：心跳就是保活（V2-22）。

use crate::block::{BlockEvent, BlockView, Event, EventSink, Heartbeat, Hub};
use axum::{
    body::{Body, Bytes},
    http::header,
    response::Response,
};
use futures_util::stream::{self, Stream};
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeMap, convert::Infallible, sync::Arc};
use tokio::sync::broadcast;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

/// V2-7：broadcast 容量。
pub const CAPACITY: usize = 64;

/// 这次启动的流：epoch + broadcast。
pub struct Feed {
    epoch: String,
    tx: broadcast::Sender<Bytes>,
}

impl Feed {
    pub fn new(capacity: usize) -> Arc<Self> {
        let (tx, _) = broadcast::channel(capacity);
        Arc::new(Self {
            epoch: new_epoch(),
            tx,
        })
    }

    #[cfg(test)]
    pub fn epoch(&self) -> &str {
        &self.epoch
    }
}

/// V2-1：每次启动随机生成。
fn new_epoch() -> String {
    format!("{:016x}", rand::random::<u64>())
}

/// `Hub` 的事件出口（在 `Hub` 的锁里调用）。
pub struct FeedSink(pub Arc<Feed>);

impl EventSink for FeedSink {
    fn emit(&self, event: &Event) {
        // 没有订阅者时 send 返回 Err，照常丢掉。
        let _ = self.0.tx.send(frame(&self.0.epoch, event));
    }
}

#[derive(Serialize)]
struct BlockMsg<'a> {
    epoch: &'a str,
    seq: u64,
    name: &'a str,
    revision: u64,
    observed_at: u64,
    stale: bool,
    data: &'a Value,
}

#[derive(Serialize)]
struct HeartbeatMsg<'a> {
    epoch: &'a str,
    seq: u64,
    blocks: BTreeMap<&'a str, u64>,
    exec_age_ms: u64,
}

#[derive(Serialize)]
struct BlockBody<'a> {
    revision: u64,
    observed_at: u64,
    stale: bool,
    data: &'a Value,
}

#[derive(Serialize)]
struct SnapshotMsg<'a> {
    epoch: &'a str,
    seq: u64,
    blocks: BTreeMap<&'a str, BlockBody<'a>>,
}

fn sse(event: &str, json: &str) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {json}\n\n"))
}

fn to_json(v: &impl Serialize) -> String {
    serde_json::to_string(v).expect("event serializes")
}

fn frame(epoch: &str, event: &Event) -> Bytes {
    match event {
        Event::Block(BlockEvent {
            seq,
            name,
            revision,
            observed_at,
            stale,
            data,
        }) => sse(
            "block",
            &to_json(&BlockMsg {
                epoch,
                seq: *seq,
                name,
                revision: *revision,
                observed_at: *observed_at,
                stale: *stale,
                data,
            }),
        ),
        Event::Heartbeat(Heartbeat {
            seq,
            blocks,
            exec_age_ms,
        }) => sse(
            "heartbeat",
            &to_json(&HeartbeatMsg {
                epoch,
                seq: *seq,
                blocks: blocks.iter().copied().collect(),
                exec_age_ms: *exec_age_ms,
            }),
        ),
    }
}

/// snapshot 的 JSON（`/v2/state` 和 `snapshot` 事件共用）。
fn snapshot_json(epoch: &str, cut: u64, views: &[BlockView]) -> String {
    to_json(&SnapshotMsg {
        epoch,
        seq: cut,
        blocks: views
            .iter()
            .map(|v| {
                (
                    v.name,
                    BlockBody {
                        revision: v.revision,
                        observed_at: v.observed_at,
                        stale: v.stale,
                        data: &v.data,
                    },
                )
            })
            .collect(),
    })
}

/// V2-5：在 `Hub` 的锁里先订阅、再拍快照。返回订阅和 snapshot 的 JSON。
pub fn subscribe(hub: &Hub, feed: &Feed) -> (broadcast::Receiver<Bytes>, String) {
    let (rx, (cut, views)) = hub.snapshot_with(|| feed.tx.subscribe());
    (rx, snapshot_json(&feed.epoch, cut, &views))
}

/// `GET /v2/state`（V2-6）：和 snapshot 一样的内容，不订阅。
pub fn state_json(hub: &Hub, feed: &Feed) -> String {
    let ((), (cut, views)) = hub.snapshot_with(|| ());
    snapshot_json(&feed.epoch, cut, &views)
}

/// 一个订阅者的 SSE 字节流：snapshot，然后 broadcast 里的事件；`Lagged` 或通道关闭就结束。
/// `guard`（连接名额）跟着流走，流结束就释放。
pub fn stream<G: Send + Sync + 'static>(
    hub: &Hub,
    feed: &Feed,
    guard: G,
) -> impl Stream<Item = Result<Bytes, Infallible>> + Send + 'static {
    let (rx, snapshot) = subscribe(hub, feed);
    let rest = BroadcastStream::new(rx).map_while(Result::ok);
    stream::once(async move { sse("snapshot", &snapshot) })
        .chain(rest)
        .map(move |b| {
            let _keep = &guard;
            Ok(b)
        })
}

/// `/v2/events` 的响应。
/// 流是 `stream` 再包一层（server.rs 让它在退出时结束）。
pub fn events_response(
    s: impl Stream<Item = Result<Bytes, Infallible>> + Send + 'static,
) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(s))
        .expect("static headers")
}

/// `/v2/state` 的响应。
pub fn state_response(hub: &Hub, feed: &Feed) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(state_json(hub, feed)))
        .expect("static headers")
}

#[cfg(test)]
mod tests;
