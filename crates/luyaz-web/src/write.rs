//! Three-tier writes (DESIGN.md §5 三档写操作; the old web's `useWriteOp`, `ConfirmInline`,
//! `ConfirmDialog`, `OpResult`), shared by every page that changes the device through u60d.
//!
//! ```text
//!  tier 1 (Direct)  button ─▶ POST set ───────────────────────────────┐
//!  tier 2 (Inline)  button ─▶ POST ask ─▶ confirm in the page ─▶ set ─┤  "正在…" ─▶ /control
//!  tier 3 (Dialog)  button ─▶ POST ask ─▶ <dialog> (what / how long /  │  (fresh op_id) ─▶ wait for
//!                   how to recover, Tailscale warning, typed word) ─▶ ┘  the block readback ─▶ result
//! ```
//! - The page owns the control (switch, button) and one area under it ([`Area`], `write.html`):
//!   the confirm, the dialog, "正在…" and the result line. Only the write's own replies patch the
//!   area; the page's push stream sends the control's value alone, so an open confirm or a result
//!   survives every push (the roaming switch on /signal is the first user).
//! - Every send carries a new `op_id` ([`next_op_id`]), at the top level of the `/control` body like
//!   the agent's `ops.rs`. u60d dedups by it only for actions with a transaction spec (today
//!   `network.set_mode`): a resend of the same id reads back instead of writing twice. For the other
//!   actions it is ignored, so not writing twice is this module's job: one POST is one send, never a
//!   retry (the confirm buttons tell Datastar `retryMaxCount: 0`, whose default would re-POST when
//!   the reply stream drops).
//! - After a send: read back from the hub's blocks until the predicate holds or the timeout; no
//!   readback → 「设备已接受，还没读回」, never "成功". No reply at all (timeout, dropped) → read
//!   back anyway; still nothing → 「连接中断，还没确认是否生效」.
//! - "Remote" for the Tailscale warning is `topbar::remote` (peer address or Host), the same test
//!   as the top bar's remote badge, so the dialog and the badge never disagree.
//! - A send whose reply drops (the page lost the connection mid-write) is not retried, so the
//!   area would stay on "正在…": the area listens for Datastar's `retries-failed` of its own send
//!   button (`data-write="{id}"`) and posts `cancel?lost=true`, which the page answers with
//!   [`cancel_phase`] → 「连接中断，还没确认是否生效」 and the control's current value. (No probing
//!   until the device is back, as the old web's `waitDevice` did; not built.)
//! - Not built: multi-step writes shown step by step and "send only the steps not done" (no batch-2
//!   page has one; u60-web.md).

use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use askama::Template;
use axum::response::sse::{Event, Sse};
use futures_util::Stream;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::hub::{Hub, St};
use crate::i18n::Lang;
use crate::pages::{async_stream, patch};

/// The confirm of tiers 2 and 3. Tier 1 has none: its button posts the send at once with
/// [`post_once`] and the reply goes through the same [`respond`] / [`execute`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// Confirm in the page, stating the consequence.
    Inline,
    /// A dialog: what happens, how long it is offline, how to recover.
    Dialog,
}

/// What a tier-2 or tier-3 confirm says. Strings are already in the page's language.
pub struct Confirm {
    pub tier: Tier,
    /// Dialog heading; the inline confirm's group label.
    pub title: String,
    /// The consequence (inline text; the dialog's "what happens").
    pub what: String,
    /// Dialog only.
    pub downtime: Option<String>,
    /// Dialog only.
    pub recovery: Option<String>,
    /// The confirm button ("确认：关掉数据漫游").
    pub action: String,
    /// Can cut the connection this page came over: a remote browser gets the Tailscale warning.
    pub cuts_uplink: bool,
    /// The word to type before the confirm button works (factory reset: 「恢复出厂」); checked
    /// again on the server ([`Write::typed`]). No quotes or backslashes (it goes into a JS string).
    pub word: Option<String>,
    pub danger: bool,
}

/// How a send ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Read back: the page's own sentence ("已关掉数据漫游").
    Applied(String),
    /// u60d took it; the readback did not show it in time (or there is nothing to read back).
    Accepted,
    /// [`Outcome::Accepted`] in the page's own words (an SMS has no readback: 「设备已接受：发给 …
    /// 的短信」). Same tone; never "成功".
    AcceptedSaid(String),
    /// No reply (timeout, dropped) and no readback: may or may not have happened.
    Unknown,
    /// Not sent or refused, with the reason.
    Failed(String),
    /// Refused because another change holds the device; try again later.
    Busy,
    /// Not sent, nothing wrong: why not yet ("还没读到漫游设置，稍后再试"), as it is.
    NotYet(String),
    /// A transaction's own final words from u60d's `op` block ("没通 · 已退回自动") with u60d's
    /// tone (`ok` / `warn` / `bad`): the readback of a change that ran but may not have held.
    Ended(String, &'static str),
}

impl Outcome {
    pub fn text(&self, l: Lang) -> String {
        match self {
            Outcome::Applied(s)
            | Outcome::NotYet(s)
            | Outcome::AcceptedSaid(s)
            | Outcome::Ended(s, _) => s.clone(),
            Outcome::Accepted => l
                .pick(
                    "Accepted by the device, not read back yet",
                    "设备已接受，还没读回",
                )
                .into(),
            Outcome::Unknown => l
                .pick(
                    "Connection dropped; not yet confirmed whether it took effect",
                    "连接中断，还没确认是否生效",
                )
                .into(),
            Outcome::Failed(e) => format!("{}{e}", l.pick("Not applied: ", "没有生效：")),
            Outcome::Busy => l
                .pick(
                    "Not applied: another change is in progress; try again when it ends",
                    "没有生效：另一项改动正在进行，等它结束再试",
                )
                .into(),
        }
    }
    /// `t-note--*`.
    pub fn tone(&self) -> &'static str {
        match self {
            Outcome::Applied(_) => "ok",
            Outcome::Accepted | Outcome::AcceptedSaid(_) => "neutral",
            Outcome::Unknown | Outcome::Busy | Outcome::NotYet(_) => "warn",
            Outcome::Failed(_) => "bad",
            Outcome::Ended(_, t) => t,
        }
    }
    /// Results that need attention are alerts; the rest are status.
    pub fn role(&self) -> &'static str {
        match self {
            Outcome::Applied(_)
            | Outcome::Accepted
            | Outcome::AcceptedSaid(_)
            | Outcome::NotYet(_) => "status",
            Outcome::Ended(_, t) if *t == "ok" => "status",
            _ => "alert",
        }
    }
}

/// What the area under a control shows.
pub enum Phase {
    Idle,
    Ask(Confirm),
    /// "正在…" (empty: the control itself says it).
    Pending(String),
    Done(Outcome),
}

/// The area under a control: confirm, dialog, progress, result (`write.html`). Patched only by the
/// write's own replies; its id is `{id}-op`.
#[derive(Template)]
#[template(path = "write.html")]
pub struct Area<'a> {
    pub id: &'a str,
    pub l: Lang,
    /// The browser came over Tailscale (`topbar::remote`).
    pub remote: bool,
    /// POST target of the confirm button (the send).
    pub set: &'a str,
    /// POST target of Cancel / Esc.
    pub cancel: &'a str,
    pub phase: &'a Phase,
}

impl Area<'_> {
    pub fn html(&self) -> String {
        self.render().unwrap()
    }
    fn said(&self, o: &Outcome) -> String {
        o.text(self.l)
    }
    /// The page-local signal holding the typed word (`_` = never sent to the server by Datastar).
    pub fn sig(&self) -> String {
        format!("_w_{}", self.id.replace('-', "_"))
    }
    /// Where the area posts when its send's reply dropped (`cancel` with `lost=true`).
    pub fn lost(&self) -> String {
        let sep = if self.cancel.contains('?') { '&' } else { '?' };
        format!("{}{sep}lost=true", self.cancel)
    }
    /// The confirm button's action: the send, with the typed word when there is one.
    pub fn set_js(&self, word: bool) -> String {
        if word {
            let sep = if self.set.contains('?') { '&' } else { '?' };
            format!(
                "@post('{}{sep}word=' + encodeURIComponent(${} ?? ''), {{retryMaxCount: 0}})",
                self.set,
                self.sig()
            )
        } else {
            post_once(self.set)
        }
    }
}

/// The query of a page's cancel endpoint: `lost=true` when the area's send lost its reply.
#[derive(serde::Deserialize, Default)]
pub struct CancelQuery {
    #[serde(default)]
    pub lost: bool,
}

/// What the area shows after a cancel: nothing, or "connection dropped, not confirmed".
pub fn cancel_phase(q: &CancelQuery) -> Phase {
    if q.lost {
        Phase::Done(Outcome::Unknown)
    } else {
        Phase::Idle
    }
}

/// The Datastar action of a button that sends a write (a tier-1 button also carries
/// `data-write="{area id}"`, so a lost reply is reported in its area) (tier 1, and the confirm of tiers 2/3):
/// never retried (Datastar re-POSTs on a dropped reply by default; that would be a second write
/// with a new op_id). `url` is a same-site path without quotes.
pub fn post_once(url: &str) -> String {
    format!("@post('{url}', {{retryMaxCount: 0}})")
}

/// True once the hub's blocks show the change.
pub type Readback = Box<dyn Fn(&St) -> bool + Send + Sync>;

/// One write to u60d.
pub struct Write {
    pub action: &'static str,
    pub params: Value,
    /// True once the hub's blocks show the change. None: nothing to read it back from.
    pub readback: Option<Readback>,
    /// Said when read back.
    pub applied: String,
    pub timeout: Duration,
    /// Tier 3 with a typed word: (expected, what came with the request). A mismatch is not sent.
    pub typed: Option<(String, String)>,
    /// How long the `/control` request itself may take (None: the shared client's 10 s). An
    /// action that waits on the firmware (an SMS send polls its result for up to 10 s) needs
    /// more, or a slow success reads as 「连接中断」 and invites a second, paid send.
    pub request_timeout: Option<Duration>,
}

/// How long a write waits for its readback before saying "accepted, not read back".
pub const READBACK: Duration = Duration::from_secs(15);

/// A new id for every send: `lw-<start>-<n>` (1–64 of `[A-Za-z0-9._-]`, u60d `valid_op_id`).
pub fn next_op_id() -> String {
    static START: OnceLock<u64> = OnceLock::new();
    static N: AtomicU64 = AtomicU64::new(0);
    let start = *START.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    });
    format!("lw-{start}-{}", N.fetch_add(1, Ordering::Relaxed) + 1)
}

/// The typed word matches (surrounding spaces ignored, like the old dialog).
pub fn word_ok(expected: &str, typed: &str) -> bool {
    typed.trim() == expected
}

/// How a send went when it got through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sent {
    Ok,
    /// No reply: it may have happened.
    NoReply,
}

impl Sent {
    /// What to say when the readback did not show it.
    pub fn not_read(self) -> Outcome {
        match self {
            Sent::Ok => Outcome::Accepted,
            Sent::NoReply => Outcome::Unknown,
        }
    }
}

/// Send `w` once (a fresh `op_id`, never retried). `Err` = it did not go through, and why.
pub async fn send(
    http: &reqwest::Client,
    control_url: &str,
    w: &Write,
    l: Lang,
) -> Result<Sent, Outcome> {
    if let Some((want, got)) = &w.typed
        && !word_ok(want, got)
    {
        return Err(Outcome::Failed(
            l.pick(
                "the confirmation word did not match",
                "输入的确认词不对",
            )
            .into(),
        ));
    }
    let body = json!({
        "action": w.action,
        "params": w.params,
        "source": "web",
        "op_id": next_op_id(),
    });
    send_body(http, control_url, &body, w.request_timeout, l).await
}

/// Post a `/control` body as it is, once (never retried). The caller has put a fresh `op_id`
/// ([`next_op_id`]) in it and knows it: a readback that follows the transaction by its id
/// (/changes' undo, `undo: true` at the top level) needs it before the send.
pub async fn send_body(
    http: &reqwest::Client,
    control_url: &str,
    body: &Value,
    request_timeout: Option<Duration>,
    l: Lang,
) -> Result<Sent, Outcome> {
    send_body_reply(http, control_url, body, request_timeout, l)
        .await
        .map(|(s, _)| s)
}

/// [`send_body`], with u60d's reply (`Null` when there was none): its `op` says whether u60d runs
/// the write as a transaction (an APN change it may also do as a plain write).
pub async fn send_body_reply(
    http: &reqwest::Client,
    control_url: &str,
    body: &Value,
    request_timeout: Option<Duration>,
    l: Lang,
) -> Result<(Sent, Value), Outcome> {
    let mut rb = http.post(control_url).json(body);
    if let Some(t) = request_timeout {
        rb = rb.timeout(t);
    }
    match rb.send().await {
        Err(e) if e.is_connect() => Err(Outcome::Failed(
            l.pick("can't reach the core service", "连不上核心服务").into(),
        )),
        Err(_) => Ok((Sent::NoReply, Value::Null)),
        Ok(r) => match r.json::<Value>().await {
            Err(_) => Ok((Sent::NoReply, Value::Null)),
            Ok(v) if v["ok"].as_bool() == Some(true) => Ok((Sent::Ok, v)),
            Ok(v) => {
                let code = v["error"]["code"].as_str().unwrap_or("");
                if matches!(code, "busy" | "op_busy") {
                    return Err(Outcome::Busy);
                }
                Err(Outcome::Failed(
                    v["error"]["message"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(l.pick("the core service refused", "核心服务拒绝了"))
                        .to_owned(),
                ))
            }
        },
    }
}

/// Send `w` once and wait for its readback.
pub async fn execute(
    http: &reqwest::Client,
    control_url: &str,
    hub: &Hub,
    w: &Write,
    l: Lang,
) -> Outcome {
    // subscribe before sending, so a readback that lands during the request is not missed
    let mut changes = hub.watch();
    let sent = match send(http, control_url, w, l).await {
        Ok(s) => s,
        Err(o) => return o,
    };
    let Some(check) = &w.readback else {
        return sent.not_read();
    };
    let deadline = tokio::time::Instant::now() + w.timeout;
    loop {
        if hub.read(|st| check(st)) {
            return Outcome::Applied(w.applied.clone());
        }
        match tokio::time::timeout_at(deadline, changes.recv()).await {
            Ok(Ok(_)) | Ok(Err(RecvError::Lagged(_))) => continue,
            _ => return sent.not_read(),
        }
    }
}

/// A readback the hub's blocks can't answer (it needs a read of its own, e.g. the SMS list):
/// `check` runs at once, then after each hub change (but not sooner than `min` after the last
/// check) or every `max`, until it holds or `timeout` passes. Whether it held.
pub async fn read_back<F, Fut>(
    hub: &Hub,
    timeout: Duration,
    min: Duration,
    max: Duration,
    mut check: F,
) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let mut changes = hub.watch();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let last = tokio::time::Instant::now();
        if check().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        let wake = (tokio::time::Instant::now() + max).min(deadline);
        let _ = tokio::time::timeout_at(wake, changes.recv()).await;
        tokio::time::sleep_until((last + min).min(deadline)).await;
    }
}

/// The reply of a send endpoint: `pending` at once, then whatever `done` renders.
pub fn respond<F>(pending: String, done: F) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    F: std::future::Future<Output = String> + Send + 'static,
{
    Sse::new(async_stream(move |tx| async move {
        if tx.send(patch(&pending)).await.is_err() {
            // the tab went away before anything was sent: don't write
            return;
        }
        let html = done.await;
        let _ = tx.send(patch(&html)).await;
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::Frame;
    use std::sync::{Arc, Mutex};

    fn feed(hub: &Hub, event: &str, v: Value) {
        hub.apply(&Frame {
            event: event.into(),
            data: v.to_string(),
        });
    }

    fn ifaces(rev: u64, roam: i64) -> Value {
        json!({"epoch":"e","name":"interfaces","revision":rev,"observed_at":rev,"stale":false,
               "data":{"cellular":{"roam_enable":roam}}})
    }

    /// A stand-in `/control`: records every body, answers with `reply`.
    async fn control(reply: Value) -> (String, Arc<Mutex<Vec<Value>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let app = axum::Router::new().route(
            "/control",
            axum::routing::post(move |axum::Json(b): axum::Json<Value>| {
                let (s, reply) = (s.clone(), reply.clone());
                async move {
                    s.lock().unwrap().push(b);
                    axum::Json(reply)
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/control", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (url, seen)
    }

    fn roam_write(target: bool, timeout: Duration) -> Write {
        Write {
            action: "cellular.set",
            params: json!({"roaming": target}),
            readback: Some(Box::new(move |st| {
                crate::view::roaming(st) == Some(target)
            })),
            applied: "已打开数据漫游".into(),
            timeout,
            typed: None,
            request_timeout: None,
        }
    }

    #[tokio::test]
    async fn a_fresh_op_id_on_every_send_at_the_top_level() {
        let (url, seen) = control(json!({"ok": true})).await;
        let hub = Hub::new();
        let http = reqwest::Client::new();
        let w = roam_write(true, Duration::from_millis(50));
        execute(&http, &url, &hub, &w, Lang::Zh).await;
        execute(&http, &url, &hub, &w, Lang::Zh).await;
        let b = seen.lock().unwrap().clone();
        assert_eq!(b.len(), 2, "one POST per send, no retries");
        let ids: Vec<&str> = b.iter().map(|v| v["op_id"].as_str().unwrap()).collect();
        assert_ne!(ids[0], ids[1]);
        for (v, id) in b.iter().zip(&ids) {
            assert!(
                id.starts_with("lw-")
                    && id.len() <= 64
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)),
                "{id}"
            );
            assert_eq!(v["source"], "web");
            assert_eq!(v["action"], "cellular.set");
            assert!(v["params"].get("op_id").is_none(), "top level, not params");
        }
    }

    #[tokio::test]
    async fn no_readback_in_time_says_accepted_never_success() {
        let (url, _) = control(json!({"ok": true})).await;
        let hub = Hub::new();
        feed(&hub, "snapshot", json!({"epoch":"e","blocks":{"interfaces":ifaces(1, 0)}}));
        let o = execute(
            &reqwest::Client::new(),
            &url,
            &hub,
            &roam_write(true, Duration::from_millis(200)),
            Lang::Zh,
        )
        .await;
        assert_eq!(o, Outcome::Accepted);
        let t = o.text(Lang::Zh);
        assert!(t.contains("设备已接受") && !t.contains("成功"), "{t}");
        assert_eq!(o.tone(), "neutral");
        assert!(Outcome::Accepted.text(Lang::En).starts_with("Accepted by the device"));
    }

    #[tokio::test]
    async fn readback_that_lands_later_is_applied() {
        let (url, _) = control(json!({"ok": true})).await;
        let hub = Hub::new();
        feed(&hub, "snapshot", json!({"epoch":"e","blocks":{"interfaces":ifaces(1, 0)}}));
        let h2 = hub.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            feed(&h2, "block", ifaces(2, 1));
        });
        let o = execute(
            &reqwest::Client::new(),
            &url,
            &hub,
            &roam_write(true, Duration::from_secs(5)),
            Lang::Zh,
        )
        .await;
        assert_eq!(o, Outcome::Applied("已打开数据漫游".into()));
        assert_eq!(o.tone(), "ok");
    }

    #[tokio::test]
    async fn nothing_to_read_back_is_accepted_at_once() {
        let (url, _) = control(json!({"ok": true})).await;
        let mut w = roam_write(true, Duration::from_secs(30));
        w.readback = None;
        let t0 = std::time::Instant::now();
        let o = execute(&reqwest::Client::new(), &url, &Hub::new(), &w, Lang::Zh).await;
        assert_eq!(o, Outcome::Accepted);
        assert!(t0.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn refusals_say_why_and_busy_is_its_own_sentence() {
        let (url, _) = control(json!({"ok":false,"error":{"code":"device_call_failed","message":"zwrt_data: timeout"}})).await;
        let o = execute(
            &reqwest::Client::new(),
            &url,
            &Hub::new(),
            &roam_write(true, READBACK),
            Lang::Zh,
        )
        .await;
        assert_eq!(o.text(Lang::Zh), "没有生效：zwrt_data: timeout");
        assert_eq!(o.tone(), "bad");
        let (url, _) = control(json!({"ok":false,"error":{"code":"op_busy","message":"a change is being confirmed"}})).await;
        let o = execute(
            &reqwest::Client::new(),
            &url,
            &Hub::new(),
            &roam_write(true, READBACK),
            Lang::En,
        )
        .await;
        assert_eq!(o, Outcome::Busy);
        assert!(o.text(Lang::Zh).contains("另一项改动正在进行"));
        assert!(!o.text(Lang::En).chars().any(|c| c > '\u{2fff}'));
    }

    #[tokio::test]
    async fn unreachable_fails_without_a_readback_wait() {
        let o = execute(
            &reqwest::Client::new(),
            "http://127.0.0.1:1/control",
            &Hub::new(),
            &roam_write(true, Duration::from_secs(30)),
            Lang::Zh,
        )
        .await;
        assert_eq!(o.text(Lang::Zh), "没有生效：连不上核心服务");
    }

    #[tokio::test]
    async fn a_wrong_typed_word_is_never_sent() {
        let (url, seen) = control(json!({"ok": true})).await;
        let mut w = roam_write(true, Duration::from_millis(50));
        w.typed = Some(("恢复出厂".into(), "恢复".into()));
        let o = execute(&reqwest::Client::new(), &url, &Hub::new(), &w, Lang::Zh).await;
        assert!(matches!(o, Outcome::Failed(_)), "{o:?}");
        assert!(seen.lock().unwrap().is_empty());
        w.typed = Some(("恢复出厂".into(), " 恢复出厂 ".into()));
        execute(&reqwest::Client::new(), &url, &Hub::new(), &w, Lang::Zh).await;
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    fn ask(tier: Tier, word: Option<&str>) -> Phase {
        Phase::Ask(Confirm {
            tier,
            title: "重启设备".into(),
            what: "设备重启，所有连接断开。".into(),
            downtime: Some("约 90 秒".into()),
            recovery: Some("等它自己回来；不回来就长按电源键。".into()),
            action: "确认：重启".into(),
            cuts_uplink: true,
            word: word.map(Into::into),
            danger: word.is_some(),
        })
    }

    fn area(phase: &Phase, remote: bool) -> String {
        Area {
            id: "x-y",
            l: Lang::Zh,
            remote,
            set: "/x/set?a=1",
            cancel: "/x/cancel",
            phase,
        }
        .html()
    }

    #[test]
    fn the_three_tiers_render() {
        let idle = area(&Phase::Idle, false);
        assert!(idle.contains("id=\"x-y-op\"") && !idle.contains("<dialog"), "{idle}");

        let inline = area(&ask(Tier::Inline, None), true);
        assert!(inline.contains("id=\"x-y-confirm\"") && inline.contains("nd-confirm"));
        assert!(inline.contains("设备重启，所有连接断开。"));
        assert!(!inline.contains("<dialog") && inline.contains("Tailscale"));
        assert!(!area(&ask(Tier::Inline, None), false).contains("Tailscale"));
        assert_eq!(post_once("/x/go?id=3"), "@post('/x/go?id=3', {retryMaxCount: 0})");
        assert!(inline.contains("{retryMaxCount: 0}"), "a confirm is never retried");
        assert!(inline.contains("el.focus()"), "focus on Cancel");
        assert!(inline.contains("data-write=\"x-y\"") && inline.contains("/x/cancel?lost=true"), "a lost reply is reported");
        assert!(matches!(cancel_phase(&CancelQuery { lost: true }), Phase::Done(Outcome::Unknown)));
        assert!(matches!(cancel_phase(&CancelQuery::default()), Phase::Idle));

        let dlg = area(&ask(Tier::Dialog, None), false);
        assert!(dlg.contains("<dialog") && dlg.contains("el.showModal()"));
        for s in ["会发生什么", "断开多久", "约 90 秒", "怎么恢复", "确认：重启"] {
            assert!(dlg.contains(s), "{s}: {dlg}");
        }
        assert!(!dlg.contains("Tailscale"), "local browser: no remote warning");
        assert!(area(&ask(Tier::Dialog, None), true).contains("Tailscale"));

        let typed = area(&ask(Tier::Dialog, Some("恢复出厂")), false);
        assert!(typed.contains("输入「恢复出厂」确认"), "{typed}");
        assert!(typed.contains("data-bind=\"_w_x_y\""));
        assert!(typed.contains("/x/set?a=1&#38;word=") && typed.contains("nd-btn--danger"), "{typed}");
        assert!(typed.contains("data-attr:disabled"));

        let pend = area(&Phase::Pending("正在重启…".into()), false);
        assert!(pend.contains("正在重启…") && pend.contains("role=\"status\""));
        let done = area(&Phase::Done(Outcome::Accepted), false);
        assert!(done.contains("t-note--neutral") && done.contains("设备已接受"));
    }

    #[test]
    fn ids_are_fresh_and_valid() {
        let a = next_op_id();
        let b = next_op_id();
        assert_ne!(a, b);
        assert!(a.len() <= 64);
    }
}
