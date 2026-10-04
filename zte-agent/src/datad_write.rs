//! The agent's device writes go through datad's `/control` (write-op-layer.md
//! T7): one writer on the device, every write with its source, journaled, and
//! the ones in datad's description table run as transactions (confirmed by
//! reading back, rolled back if they cut the device off).
//!
//! The source is per thread: HTTP handlers write as `web`; the scheduler and
//! the scenario engine run handlers in-process inside [`with_source`]; the
//! agent's own loops (charge policy, APN watcher, SMS forwarding) pass theirs
//! to [`send_as`]. A handler that hands work to another thread takes
//! [`current`] with it.
//!
//! When datad is not there (nothing listening on its port), writes that
//! affect connectivity fall back to the device's emergency script
//! (`fallback_write`, D19: only those); the rest answer 503 "数据服务没响应".

use std::cell::Cell;
use std::time::Duration;

use serde_json::{json, Value};

use crate::datad_feed::{self, PostError};

/// Who asks (datad `source`; `screen` and `legacy` are the touch screen's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Web,
    Scheduler,
    Scenario,
    Auto,
    /// netinfo's guard reverting a manual network pick (T7b, D17)
    #[allow(dead_code)]
    Guard,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Web => "web",
            Source::Scheduler => "scheduler",
            Source::Scenario => "scenario",
            Source::Auto => "auto",
            Source::Guard => "guard",
        }
    }
}

thread_local! {
    static SOURCE: Cell<Source> = const { Cell::new(Source::Web) };
    /// The datad search session this thread's writes belong to (D17).
    static SESSION: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// This thread's source (`web` unless inside [`with_source`]).
pub fn current() -> Source {
    SOURCE.with(Cell::get)
}

/// Run `f` with writes on this thread attributed to `s`.
pub fn with_source<T>(s: Source, f: impl FnOnce() -> T) -> T {
    let before = SOURCE.with(|c| c.replace(s));
    struct Restore(Source);
    impl Drop for Restore {
        fn drop(&mut self) {
            SOURCE.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(before);
    f()
}

/// How long to wait for datad's answer: it may wait up to 15 s for the
/// cross-process write lock (D29), then up to 8 s for the vendor call.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// datad's answer to one write.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// Written (200). `op` is the transaction state for described actions.
    Done { result: Value, op: Option<Value> },
    /// Written by the emergency script while datad was gone.
    Fallback,
    /// The vendor call failed, or datad could not read the current value
    /// first (502 and the like). `op` as above.
    Failed { error: String, op: Option<Value> },
    /// The request itself was refused (400): bad parameters.
    Invalid(String),
    /// Another change is in progress (409 busy, with what it is), or datad's
    /// queue is full (503 busy).
    Busy { doing: Option<Value> },
    /// A network-mode transaction is confirming and this write is from an
    /// automatic source (409 `op_busy`, D40): not a failure, try again once
    /// it ends. `op` is datad's `{op_id, item, phase}`.
    OpBusy { message: String, op: Option<Value> },
    /// No answer from datad. `maybe_done`: the request was sent and may have
    /// been carried out.
    Unreachable { error: String, maybe_done: bool },
}

/// [`send_as`] with this thread's source.
pub fn send(action: &str, params: &Value) -> Reply {
    send_as(current(), action, params)
}

/// One write through datad as `source`; falls back as the module says.
pub fn send_as(source: Source, action: &str, params: &Value) -> Reply {
    // datad answers its heartbeat but its executor is stuck: a write would only
    // wait out the timeout, and the emergency script refuses while it runs
    if datad_feed::executor_stalled() {
        return Reply::Unreachable { error: "datad executor stuck".into(), maybe_done: false };
    }
    let mut body = json!({"action": action, "source": source.as_str(), "params": params});
    if let Some(id) = SESSION.with(|s| s.borrow().clone()) {
        body["session"] = json!(id);
    }
    match datad_feed::post_control(datad_feed::datad_addr(), &body, TIMEOUT) {
        Ok((status, reply)) => parse(status, &reply),
        Err(PostError::Connect(e)) => {
            if crate::fallback_write::covers(action) {
                crate::fallback_write::run(source, action, params)
            } else {
                Reply::Unreachable { error: e, maybe_done: false }
            }
        }
        Err(PostError::NoReply(e)) => Reply::Unreachable { error: e, maybe_done: true },
    }
}

/// A vendor setter datad makes on our behalf (`vendor.call`, its own
/// whitelist; write-op-layer.md T7c): same signature as the old direct
/// `ubus::write`, so callers keep their reply handling. `params` must be a
/// JSON object (or none).
pub fn vendor(object: &str, method: &str, params: Option<&str>) -> Result<Value, String> {
    let args: Value = match params {
        None => json!({}),
        Some(p) => serde_json::from_str(p).map_err(|e| format!("{object} {method}: params: {e}"))?,
    };
    if !args.is_object() {
        return Err(format!("{object} {method}: params must be an object"));
    }
    send("vendor.call", &json!({"object": object, "method": method, "args": args})).into_result()
}

/// A datad search session (D17): while it is open, datad refuses other
/// connectivity writes, and the search/register/back-to-automatic steps run
/// inside it. Renewed every 20 s by a thread of its own (datad takes it back
/// 60 s after the last renew: a dead agent does not hold the lock); closed
/// when dropped.
pub struct Session {
    id: String,
    source: Source,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
    result: std::sync::Mutex<String>,
}

pub const SESSION_RENEW: Duration = Duration::from_secs(20);
/// Opening, renewing and closing a session do not queue behind the executor:
/// a short wait, so a caller holding a lock is not stuck for long.
const SESSION_CALL: Duration = Duration::from_secs(5);

impl Session {
    /// Open one. `Err` is datad's answer: busy (another change or session),
    /// or not reachable.
    pub fn open(source: Source) -> Result<Session, Reply> {
        let body = json!({"action": "netselect.session.open", "source": source.as_str(), "params": {}});
        let reply = match datad_feed::post_control(datad_feed::datad_addr(), &body, SESSION_CALL) {
            Ok((status, v)) => parse(status, &v),
            Err(PostError::Connect(e)) => Reply::Unreachable { error: e, maybe_done: false },
            Err(PostError::NoReply(e)) => Reply::Unreachable { error: e, maybe_done: true },
        };
        let id = match reply {
            Reply::Done { result, .. } => match result["session"].as_str() {
                Some(id) => id.to_string(),
                None => return Err(Reply::Failed { error: "session.open: no session id".into(), op: None }),
            },
            other => return Err(other),
        };
        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let a = alive.clone();
        let renew_id = id.clone();
        let _ = std::thread::Builder::new().name("datad-session".into()).spawn(move || {
            let tick = Duration::from_millis(500);
            let mut waited = Duration::ZERO;
            while a.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(tick);
                waited += tick;
                if waited < SESSION_RENEW {
                    continue;
                }
                waited = Duration::ZERO;
                let body = json!({"action": "netselect.session.renew", "source": source.as_str(), "params": {"session": renew_id}});
                if let Ok((status, v)) = datad_feed::post_control(datad_feed::datad_addr(), &body, SESSION_CALL) {
                    if status == 409 && v["error"]["code"] == "invalid_state" {
                        eprintln!("[datad_write] session {renew_id} ended on datad's side");
                        return;
                    }
                }
            }
        });
        Ok(Session { id, source, alive, result: std::sync::Mutex::new("done".into()) })
    }

    /// Run `f` with this thread's writes in the session, as its source.
    pub fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        let before = SESSION.with(|s| s.replace(Some(self.id.clone())));
        struct Restore(Option<String>);
        impl Drop for Restore {
            fn drop(&mut self) {
                SESSION.with(|s| *s.borrow_mut() = self.0.take());
            }
        }
        let _restore = Restore(before);
        with_source(self.source, f)
    }

    /// What the journal line says when the session closes.
    pub fn set_result(&self, r: &str) {
        *self.result.lock().unwrap_or_else(|e| e.into_inner()) = r.to_string();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.alive.store(false, std::sync::atomic::Ordering::Relaxed);
        let result = self.result.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let body = json!({"action": "netselect.session.close", "source": self.source.as_str(), "params": {"session": self.id, "result": result}});
        let _ = datad_feed::post_control(datad_feed::datad_addr(), &body, SESSION_CALL);
    }
}

/// datad's HTTP reply → [`Reply`].
pub fn parse(status: u16, reply: &Value) -> Reply {
    let op = reply.get("op").filter(|v| !v.is_null()).cloned();
    let code = reply["error"]["code"].as_str().unwrap_or("");
    let message = reply["error"]["message"]
        .as_str()
        .or_else(|| reply["error"].as_str())
        .unwrap_or("failed")
        .to_string();
    match status {
        200 if reply["ok"] == true => Reply::Done { result: reply.get("result").cloned().unwrap_or(Value::Null), op },
        409 | 503 if code == "busy" => Reply::Busy { doing: reply.get("doing").filter(|v| !v.is_null()).cloned() },
        409 if code == "op_busy" => Reply::OpBusy { message, op: reply["error"].get("op").filter(|v| !v.is_null()).cloned() },
        400 => Reply::Invalid(message),
        _ => Reply::Failed { error: message, op },
    }
}

const DATAD_DOWN: &str = "数据服务没响应，写操作暂停；可用触屏";
const DATAD_DOWN_EN: &str = "The data service is not responding; changes are paused. Use the touch screen";
const NO_ANSWER: &str = "数据服务没回答，不知道改没改成；稍后看一下当前设置";
const NO_ANSWER_EN: &str = "The data service did not answer; it may or may not have changed. Check the setting in a moment";

impl Reply {
    #[allow(dead_code)] // charge control and the scheduler journal use it
    pub fn ok(&self) -> bool {
        matches!(self, Reply::Done { .. } | Reply::Fallback)
    }

    /// The agent's usual HTTP answer: 200 `{"ok":true,"data":…}` (+ `op`),
    /// 409 busy, 400 invalid, 503 for a failed vendor call (as before: the web
    /// page reads "method not found" there as "not supported") and for datad
    /// not answering.
    pub fn into_http(self) -> (u16, Value) {
        match self {
            Reply::Done { result, op } => {
                let mut v = json!({"ok": true, "data": result});
                if let Some(op) = op {
                    v["op"] = op;
                }
                (200, v)
            }
            Reply::Fallback => (200, json!({"ok": true, "data": null, "fallback": true})),
            Reply::Failed { error, op } => {
                let mut v = json!({"ok": false, "error": error});
                if let Some(op) = op {
                    v["op"] = op;
                }
                (503, v)
            }
            Reply::Invalid(e) => (400, json!({"ok": false, "error": e})),
            Reply::Busy { doing } => {
                let what = doing.as_ref().and_then(|d| d["action"].as_str()).unwrap_or("");
                // datad's own sentence when it has one (V2-39: what, who, how long)
                let say = |k: &str| doing.as_ref().and_then(|d| d[k].as_str()).filter(|s| !s.is_empty()).map(str::to_string);
                (
                    409,
                    json!({
                        "ok": false,
                        "error": say("say_zh").unwrap_or_else(|| if what.is_empty() { "设备正在改别的设置，等它结束再试".to_string() } else { format!("设备正在改别的设置（{what}），等它结束再试") }),
                        "error_en": say("say_en").unwrap_or_else(|| if what.is_empty() { "Another change is in progress; try again when it finishes".to_string() } else { format!("Another change is in progress ({what}); try again when it finishes") }),
                        "busy": "datad",
                        "doing": doing,
                    }),
                )
            }
            Reply::OpBusy { message, op } => (
                409,
                json!({
                    "ok": false,
                    "error": OP_BUSY_ZH,
                    "error_en": OP_BUSY_EN,
                    "detail": message,
                    "busy": "op",
                    "op": op,
                }),
            ),
            Reply::Unreachable { error, maybe_done } => {
                let (zh, en) = if maybe_done { (NO_ANSWER, NO_ANSWER_EN) } else { (DATAD_DOWN, DATAD_DOWN_EN) };
                (503, json!({"ok": false, "error": zh, "error_en": en, "detail": error, "datad": "down"}))
            }
        }
    }

    /// For the agent's own loops: `Ok` or the error text.
    pub fn into_result(self) -> Result<Value, String> {
        match self {
            Reply::Done { result, .. } => Ok(result),
            Reply::Fallback => Ok(Value::Null),
            Reply::Failed { error, .. } | Reply::Invalid(error) => Err(error),
            Reply::Busy { doing } => Err(format!(
                "datad busy{}",
                doing.and_then(|d| d["action"].as_str().map(|a| format!(" ({a})"))).unwrap_or_default()
            )),
            Reply::OpBusy { message, .. } => Err(format!("{OP_BUSY_ERR}: {message}")),
            Reply::Unreachable { error, .. } => Err(format!("datad: {error}")),
        }
    }

    /// datad held this automatic write off while a transaction confirms (D40).
    /// The agent's loops use [`retry_after`] on `into_result`; this is for a
    /// caller that keeps the reply.
    #[allow(dead_code)]
    pub fn is_op_busy(&self) -> bool {
        matches!(self, Reply::OpBusy { .. })
    }
}

const OP_BUSY_ZH: &str = "正在确认刚换的网络模式，等它结束再改";
const OP_BUSY_EN: &str = "Checking the new network mode; try again when it finishes";

/// How [`Reply::into_result`] starts the error for [`Reply::OpBusy`].
pub const OP_BUSY_ERR: &str = "datad op_busy";

/// An [`Reply::into_result`] error that is an op_busy, not a failure.
pub fn is_op_busy_err(e: &str) -> bool {
    e.starts_with(OP_BUSY_ERR)
}

/// How long an automatic writer waits before trying again after op_busy: a
/// transaction confirms for up to 120 s, so a few looks are enough and none
/// of them is a busy loop.
pub const OP_BUSY_RETRY: Duration = Duration::from_secs(10);

/// For an automatic writer's loop: `Some(wait)` = datad said op_busy, try
/// again after `wait` without counting it as a failed attempt; `None` = the
/// result stands (success or a real failure).
pub fn retry_after<T>(r: &Result<T, String>) -> Option<Duration> {
    match r {
        Err(e) if is_op_busy_err(e) => Some(OP_BUSY_RETRY),
        _ => None,
    }
}

/// Before a user's write that does not go through datad (eSIM switch, AT
/// terminal; D40): tell datad, so a network-mode transaction that is still
/// confirming is cancelled as `other_change` instead of being rolled back
/// over the user's change. Best effort and short: datad gone, stuck, or too
/// old to know `op.interrupt` (404 `unknown_action`) only gets a log line;
/// the user's action goes ahead either way. datad takes it only from the
/// user (`screen`/`web`): from any other source nothing is sent.
pub fn interrupt_as(source: Source, what: &str) {
    if source != Source::Web {
        return;
    }
    if datad_feed::executor_stalled() {
        eprintln!("[datad_write] op.interrupt {what}: datad executor stuck, not sent");
        return;
    }
    let body = json!({"action": "op.interrupt", "source": source.as_str(), "params": {"what": what}});
    match datad_feed::post_control(datad_feed::datad_addr(), &body, INTERRUPT_TIMEOUT) {
        Ok((200, v)) if v["ok"] == true => {
            if v["result"]["interrupted"] == true {
                eprintln!("[datad_write] op.interrupt {what}: cancelled {}", v["result"]["op_id"]);
            }
        }
        Ok((status, v)) => eprintln!("[datad_write] op.interrupt {what}: {status} {}", v["error"]["code"].as_str().unwrap_or("")),
        Err(e) => eprintln!("[datad_write] op.interrupt {what}: {e:?}"),
    }
}

/// [`interrupt_as`] with this thread's source.
pub fn interrupt(what: &str) {
    interrupt_as(current(), what)
}

/// datad answers `op.interrupt` at once (no vendor call).
const INTERRUPT_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_uses_datads_sentence() {
        let doing = json!({"action":"network.set_mode","say_zh":"正在换制式（触屏发起，32 秒），稍等","say_en":"Busy: network mode (Screen)"});
        let (s, v) = Reply::Busy { doing: Some(doing) }.into_http();
        assert_eq!((s, v["error"].as_str(), v["error_en"].as_str()), (409, Some("正在换制式（触屏发起，32 秒），稍等"), Some("Busy: network mode (Screen)")));
        let (_, v) = Reply::Busy { doing: Some(json!({"action":"network.set_mode"})) }.into_http();
        assert_eq!(v["error"], "设备正在改别的设置（network.set_mode），等它结束再试");
    }

    #[test]
    fn replies() {
        assert_eq!(
            parse(200, &json!({"ok":true,"action":"network.set_mode","result":{"result":"success"},"op":{"op_id":"web-1","phase":"applying"}})),
            Reply::Done { result: json!({"result":"success"}), op: Some(json!({"op_id":"web-1","phase":"applying"})) }
        );
        assert_eq!(parse(200, &json!({"ok":true,"result":null})), Reply::Done { result: Value::Null, op: None });
        assert_eq!(
            parse(409, &json!({"ok":false,"error":{"code":"busy","message":"busy"},"doing":{"action":"network.set_mode"}})),
            Reply::Busy { doing: Some(json!({"action":"network.set_mode"})) }
        );
        assert_eq!(parse(503, &json!({"ok":false,"error":{"code":"busy","message":"control queue full"}})), Reply::Busy { doing: None });
        assert_eq!(
            parse(502, &json!({"ok":false,"error":{"code":"failed","message":"ubus call failed: Method not found"},"op":{"phase":"verifying"}})),
            Reply::Failed { error: "ubus call failed: Method not found".into(), op: Some(json!({"phase":"verifying"})) }
        );
        assert_eq!(parse(400, &json!({"ok":false,"error":{"code":"invalid","message":"mode must be a string"}})), Reply::Invalid("mode must be a string".into()));
        // 200 without ok:true is not success
        assert!(matches!(parse(200, &json!({"ok":false})), Reply::Failed { .. }));
    }

    #[test]
    fn op_busy_is_its_own_reply() {
        let r = parse(409, &json!({"ok":false,"error":{"code":"op_busy","message":"network mode is being confirmed","op":{"op_id":"web-1","item":"network.mode","phase":"verifying"}}}));
        assert_eq!(r, Reply::OpBusy { message: "network mode is being confirmed".into(), op: Some(json!({"op_id":"web-1","item":"network.mode","phase":"verifying"})) });
        assert!(r.is_op_busy() && !r.ok());
        // plain busy is still Busy, and other 409s are not op_busy
        assert!(!parse(409, &json!({"ok":false,"error":{"code":"busy","message":"busy"}})).is_op_busy());
        assert!(!parse(409, &json!({"ok":false,"error":{"code":"invalid_state","message":"x"}})).is_op_busy());
        assert!(!parse(503, &json!({"ok":false,"error":{"code":"op_busy","message":"x"}})).is_op_busy());
        // the page gets a 409 with the op, not a 503 "failed"
        let (s, v) = r.clone().into_http();
        assert_eq!((s, v["busy"].as_str(), v["op"]["op_id"].as_str()), (409, Some("op"), Some("web-1")));
        // an automatic loop can tell it from a failure
        let e = r.into_result().unwrap_err();
        assert!(is_op_busy_err(&e), "{e}");
        assert!(!is_op_busy_err("datad busy (network.set_mode)"));
    }

    #[test]
    fn retry_only_on_op_busy() {
        let busy: Result<(), String> = Reply::OpBusy { message: "m".into(), op: None }.into_result().map(|_| ());
        assert_eq!(retry_after(&busy), Some(OP_BUSY_RETRY));
        assert!(OP_BUSY_RETRY >= Duration::from_secs(10));
        assert_eq!(retry_after(&Ok::<(), String>(())), None);
        assert_eq!(retry_after(&Err::<(), String>("ubus call failed".into())), None);
        assert_eq!(retry_after(&Reply::Busy { doing: None }.into_result()), None);
        assert_eq!(retry_after(&Reply::Unreachable { error: "x".into(), maybe_done: false }.into_result()), None);
    }

    #[test]
    fn http_answers() {
        let (s, v) = Reply::Done { result: json!(1), op: Some(json!({"op_id":"x"})) }.into_http();
        assert_eq!((s, v["ok"].clone(), v["data"].clone(), v["op"]["op_id"].clone()), (200, json!(true), json!(1), json!("x")));
        let (s, v) = Reply::Busy { doing: Some(json!({"action":"network.set_mode"})) }.into_http();
        assert_eq!((s, v["busy"].as_str()), (409, Some("datad")));
        assert!(v["error"].as_str().unwrap().contains("network.set_mode"));
        let (s, v) = Reply::Failed { error: "Method not found".into(), op: None }.into_http();
        assert_eq!((s, v["error"].as_str()), (503, Some("Method not found")));
        let (s, v) = Reply::Unreachable { error: "connect: refused".into(), maybe_done: false }.into_http();
        assert_eq!((s, v["error"].as_str(), v["datad"].as_str()), (503, Some(DATAD_DOWN), Some("down")));
        let (_, v) = Reply::Unreachable { error: "read: timed out".into(), maybe_done: true }.into_http();
        assert_eq!(v["error"].as_str(), Some(NO_ANSWER));
    }

    #[test]
    fn source_is_per_thread_and_restored() {
        assert_eq!(current(), Source::Web);
        with_source(Source::Scheduler, || {
            assert_eq!(current(), Source::Scheduler);
            with_source(Source::Scenario, || assert_eq!(current(), Source::Scenario));
            assert_eq!(current(), Source::Scheduler);
            std::thread::spawn(|| assert_eq!(current(), Source::Web)).join().unwrap();
        });
        assert_eq!(current(), Source::Web);
    }

    /// A fake datad on loopback: every migrated route sends the expected
    /// `/control` request (action, params, source) and nothing touches ubus.
    #[test]
    fn migrated_routes_send_these_requests() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::sync::{Arc, Mutex};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for c in listener.incoming() {
                let Ok(mut c) = c else { continue };
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut len = 0;
                loop {
                    let mut l = String::new();
                    if r.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
                        break;
                    }
                    if let Some(v) = l.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; len];
                r.read_exact(&mut body).unwrap();
                let v: Value = serde_json::from_slice(&body).unwrap();
                log.lock().unwrap().push(v.clone());
                let result = if v["action"] == "netselect.session.open" { json!({"session": "s1"}) } else { json!({"fake": true}) };
                let reply = json!({"ok": true, "action": v["action"], "result": result}).to_string();
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len());
            }
        });
        std::env::set_var("ZTE_AGENT_DATAD_V2", addr.to_string());
        let state = crate::handlers::AppState::new();
        use tiny_http::Method::{Post, Put};
        let cases: Vec<(tiny_http::Method, &str, Value, &str, Value)> = vec![
            (Put, "/api/modem/data", json!({"cid":1,"enable":0,"connect_status":"disconnected"}), "cellular.set", json!({"enabled":0})),
            (Put, "/api/modem/data", json!({"cid":1,"roam_enable":1}), "cellular.set", json!({"roaming":1})),
            (Put, "/api/modem/network-mode", json!({"net_select":"Only_LTE"}), "network.set_mode", json!({"mode":"Only_LTE"})),
            (Post, "/api/cell/lock/lte", json!({"pci":"12","earfcn":"1850"}), "cell.lock_lte", json!({"pci":"12","earfcn":"1850"})),
            (Post, "/api/cell/lock/nr", json!({"pci":"5","earfcn":"627264","band":"78"}), "cell.lock_nr", json!({"pci":"5","arfcn":"627264","band":"78"})),
            (Post, "/api/cell/lock/reset", Value::Null, "band.reset", json!({})),
            (Post, "/api/cell/band/reset", Value::Null, "band.reset", json!({})),
            (Post, "/api/cell/band/nr", json!({"nr5g_type":"nsa","nr5g_band":"41,78"}), "band.set_nr_nsa", json!({"bands":"41,78"})),
            (Post, "/api/cell/band/nr", json!({"nr5g_type":"sa","nr5g_band":"78"}), "band.set_nr_sa", json!({"bands":"78"})),
            (Post, "/api/cell/band/lte", json!({"lte_band":"1,3,28"}), "band.set_lte", json!({"bands":"1,3,28"})),
            (Put, "/api/router/apn/mode", json!({"apn_mode":1}), "apn.set_mode", json!({"mode":1})),
            (Post, "/api/router/apn/profiles", json!({"profilename":"cm","wanapn":"cmnet","pdpType":3,"pppAuthMode":0,"username":"","password":""}), "apn.add",
                json!({"name":"cm","apn":"cmnet","username":"","password":"","auth_mode":0,"pdp_type":3})),
            (Put, "/api/router/apn/profiles", json!({"profileId":"2","profilename":"cm","wanapn":"cmnet","pdpType":1,"pppAuthMode":0}), "apn.modify",
                json!({"profile_id":"2","name":"cm","apn":"cmnet","auth_mode":0,"pdp_type":1})),
            (Post, "/api/router/apn/profiles/delete", json!({"profileId":"2"}), "apn.delete", json!({"profile_id":"2"})),
            (Post, "/api/router/apn/profiles/activate", json!({"profileId":"2"}), "apn.enable", json!({"profile_id":"2"})),
            (Post, "/api/device/reboot", Value::Null, "device.reboot", json!({})),
            (Post, "/api/device/poweroff", Value::Null, "device.poweroff", json!({})),
            (Put, "/api/nfc", json!({"enabled":true}), "nfc.set", json!({"enabled":1,"flag":2})),
            (Put, "/api/wifi/power-save", json!({"enabled":false}), "wifi.power_save", json!({"enabled":false})),
            (Put, "/api/device/charge-control", json!({"charging_stopped":true}), "power.direct_supply.set", json!({"enabled":true})),
            (Put, "/api/usb/mode", json!({"mode":"rndis"}), "usb.set", json!({"mode":"rndis"})),
            (Post, "/api/sms/delete", json!({"id":"5;6;"}), "sms.delete", json!({"ids":"5;6;"})),
            (Post, "/api/sms/read", json!({"id":"5","tag":0}), "sms.mark_read", json!({"ids":"5","tag":0})),
            (Post, "/api/modem/online", Value::Null, "modem.online", json!({})),
            (Post, "/api/modem/airplane", json!({"operate_mode":"LPM"}), "modem.airplane", json!({"operate_mode":"LPM"})),
            (Put, "/api/router/wan-ipv6", json!({"enabled":false}), "apn.set_pdp_type", json!({"ipv6":false})),
            (Put, "/api/router/firewall/switch", json!({"firewall_switch":"1"}), "vendor.call",
                json!({"object":"zwrt_router.api","method":"router_set_firewall_switch","args":{"firewall_switch":"1"}})),
            (Post, "/api/sim/pin/verify", json!({"pin_num":"1234","puk_num":"","pin_encode_flag":"0"}), "vendor.call",
                json!({"object":"zwrt_zte_mdm.api","method":"sim_verify_pin_puk","args":{"pin_num":"1234","puk_num":"","pin_encode_flag":"0"}})),
            (Put, "/api/device/fast-boot", json!({"fast_boot":"1"}), "vendor.call",
                json!({"object":"zwrt_mc.device.manager","method":"set_device_info","args":{"deviceInfoList":{"quicken_power_on":"1"}}})),
            (Put, "/api/usb/powerbank", json!({"state":1}), "vendor.call",
                json!({"object":"zwrt_bsp.powerbank","method":"set","args":{"state":1}})),
            (Post, "/api/doh/disable", Value::Null, "dns.doh", json!({"enabled":false})),
        ];
        for (method, path, body, action, params) in cases {
            seen.lock().unwrap().clear();
            let b = if body.is_null() { Vec::new() } else { body.to_string().into_bytes() };
            let (status, reply) = crate::server::route(&method, path, &state, &b);
            assert_eq!(status, 200, "{path}: {reply}");
            let got = seen.lock().unwrap().clone();
            assert_eq!(got.len(), 1, "{path}: {got:?}");
            assert_eq!(got[0], json!({"action": action, "source": "web", "params": params}), "{path}");
        }
        // the scheduler's run of the same handler carries its source
        seen.lock().unwrap().clear();
        let out = crate::action::exec(&state, Source::Scheduler, "PUT", "/api/modem/network-mode", br#"{"net_select":"WL_AND_5G"}"#);
        assert_eq!(out.status, Some(200));
        assert_eq!(seen.lock().unwrap()[0]["source"], "scheduler");
        // and a bad body never reaches datad
        seen.lock().unwrap().clear();
        for (method, path, body) in [
            (Put, "/api/modem/data", json!({"cid":1,"apn":"x"})),
            (Post, "/api/cell/lock/nr", json!({"pci":"5","earfcn":"1"})),
            (Post, "/api/cell/band/nr", json!({"nr5g_type":"x","nr5g_band":"78"})),
            (Put, "/api/router/apn/mode", json!({"apn_mode":7})),
        ] {
            let (status, _) = crate::server::route(&method, path, &state, body.to_string().as_bytes());
            assert_eq!(status, 400, "{path}");
        }
        assert!(seen.lock().unwrap().is_empty());

        // a search session: steps inside carry its id and its source, it is
        // closed (with the result) when dropped
        seen.lock().unwrap().clear();
        let sess = Session::open(Source::Guard).unwrap();
        sess.run(|| {
            assert!(send("netselect.auto", &json!({})).ok());
            assert!(send("cellular.redial", &json!({})).ok());
        });
        assert!(send("sms.delete", &json!({"ids":"1;"})).ok(), "outside run: no session");
        sess.set_result("reverted");
        drop(sess);
        let got = seen.lock().unwrap().clone();
        let brief: Vec<_> = got.iter().map(|v| (v["action"].as_str().unwrap().to_string(), v["source"].clone(), v["session"].clone())).collect();
        assert_eq!(
            brief,
            vec![
                ("netselect.session.open".into(), json!("guard"), Value::Null),
                ("netselect.auto".into(), json!("guard"), json!("s1")),
                ("cellular.redial".into(), json!("guard"), json!("s1")),
                ("sms.delete".into(), json!("web"), Value::Null),
                ("netselect.session.close".into(), json!("guard"), Value::Null),
            ]
        );
        assert_eq!(got[4]["params"], json!({"session": "s1", "result": "reverted"}));

        // op.interrupt (D40): only the user's source sends it, as it is
        seen.lock().unwrap().clear();
        for s in [Source::Scheduler, Source::Scenario, Source::Auto, Source::Guard] {
            interrupt_as(s, "at");
        }
        assert!(seen.lock().unwrap().is_empty());
        interrupt_as(Source::Web, "esim");
        with_source(Source::Web, || interrupt("at"));
        assert_eq!(
            seen.lock().unwrap().clone(),
            vec![
                json!({"action": "op.interrupt", "source": "web", "params": {"what": "esim"}}),
                json!({"action": "op.interrupt", "source": "web", "params": {"what": "at"}}),
            ]
        );

        // "Got it" on the auto-revert notice (DD18) needs no op id
        seen.lock().unwrap().clear();
        assert_eq!(crate::ops::act(br#"{"act":"notice_ack"}"#).0, 200);
        assert_eq!(seen.lock().unwrap().clone(), vec![json!({"action": "op.notice_ack", "source": "web", "params": {"notice": "rollback_on"}})]);
    }
}
