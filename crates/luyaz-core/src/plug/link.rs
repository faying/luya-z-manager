//! 插件连线：认人、hello、请求和回复（R1、R3、R26、R30、R31）。
//!
//! 一次只认一条插件连线；同一个插件重连（崩溃后 procd 拉起）就换成新的，旧连线上没回的请求算
//! 「结果未知」。core 发出的请求带 id 和截止时间（[`super::mono_ms`] + 时限），插件回同一个 id；
//! 认不得的 id 丢掉并写日志。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

use super::frame::{self, ReadError, SendError};
use super::{CONTRACT_MIN, CONTRACT_V, FRAME_MAX, HELLO_WAIT, IN_FLIGHT_MAX, OUT_MAX};

/// 插件回的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Ok(Value),
    /// 插件拒绝了（例如 `stale_revision`）；`message` 还没查过，用之前要当不可信文字。
    Refused {
        code: String,
        message: String,
    },
}

/// 请求没拿到插件的回复。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallError {
    /// 送出之前就没有连线（或连线刚断、请求没写出去）：确定没做（R27）。
    Offline,
    /// 这条连线在途请求已满（R31）。
    Busy,
    /// 插件的 hello 没声明这个操作。
    Unsupported,
    /// 送出后逾时或连线断了：插件可能做了也可能没做（R26、R30）。
    Unknown,
}

/// 一条认过人、hello 过的连线。
pub struct Conn {
    /// 第几条连线（换连线后缓存要重新取描述）。
    pub generation: u64,
    pub name: String,
    ops: Vec<String>,
    out: frame::Sender,
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    /// 插件发来、还没回的调用（[`super::dispatch`]）；同样最多 [`IN_FLIGHT_MAX`] 笔。
    calls: std::sync::atomic::AtomicUsize,
}

impl Conn {
    /// hello 里声明了这一组操作（`settings` 涵盖 `settings.describe/read/write`）。
    pub fn declares(&self, family: &str) -> bool {
        self.ops.iter().any(|o| o == family)
    }
}

#[derive(Default)]
pub struct Hub {
    installed: AtomicBool,
    current: Mutex<Option<Arc<Conn>>>,
    next_id: AtomicU64,
    generations: AtomicU64,
}

static HUB: OnceLock<Arc<Hub>> = OnceLock::new();

/// 进程里唯一的插件连线中心。
pub fn hub() -> Arc<Hub> {
    HUB.get_or_init(|| Arc::new(Hub::default())).clone()
}

impl Hub {
    /// 装了插件（专用用户存在）。没装的话网页不显示插件设置页。
    pub fn installed(&self) -> bool {
        self.installed.load(Ordering::Acquire)
    }

    pub fn set_installed(&self, yes: bool) {
        self.installed.store(yes, Ordering::Release);
    }

    pub fn current(&self) -> Option<Arc<Conn>> {
        self.current
            .lock()
            .unwrap()
            .clone()
            .filter(|c| !c.out.is_closed())
    }

    /// 往插件送一个请求，最多等 `wait`（写 socket 的时间也算在里面）。
    /// `extra` 并进请求帧（例如 `source`、`approved_by`）。
    pub async fn request(
        &self,
        op: &str,
        params: Value,
        extra: Map<String, Value>,
        wait: Duration,
    ) -> Result<Reply, CallError> {
        let start = tokio::time::Instant::now();
        let Some(conn) = self.current() else {
            return Err(CallError::Offline);
        };
        let family = op.split('.').next().unwrap_or(op);
        if !conn.declares(family) {
            return Err(CallError::Unsupported);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = conn.pending.lock().unwrap();
            if pending.len() >= IN_FLIGHT_MAX {
                return Err(CallError::Busy);
            }
            pending.insert(id, tx);
        }
        let deadline_ms = super::mono_ms() + wait.as_millis() as u64;
        let mut frame =
            json!({"t": "req", "id": id, "op": op, "deadline_ms": deadline_ms, "params": params});
        if let Some(obj) = frame.as_object_mut() {
            for (k, v) in extra {
                obj.entry(k).or_insert(v);
            }
        }
        if let Err(e) = conn.out.send(&frame) {
            conn.pending.lock().unwrap().remove(&id);
            if e == SendError::Overflow {
                eprintln!(
                    "[plug] {}: queue to the plugin is full: dropping the link",
                    conn.name
                );
            }
            // 没写出去：确定没做
            return Err(CallError::Offline);
        }
        match tokio::time::timeout_at(start + wait, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            // 发送端被丢掉 = 连线断了，送出去的那笔结果不明
            Ok(Err(_)) => Err(CallError::Unknown),
            Err(_) => {
                conn.pending.lock().unwrap().remove(&id);
                eprintln!("[plug] {} {op} #{id}: no reply in {wait:?}", conn.name);
                Err(CallError::Unknown)
            }
        }
    }

    /// 通知插件（hello 声明过这一组才送）：`{"t":"ev","ev":<family>,...}`。没连上就算了，
    /// 插件连上后自己按游标补读。
    pub fn event(&self, family: &str, payload: Value) {
        let Some(conn) = self.current().filter(|c| c.declares(family)) else {
            return;
        };
        let mut frame = json!({"t": "ev", "ev": family});
        if let (Some(f), Some(p)) = (frame.as_object_mut(), payload.as_object()) {
            for (k, v) in p {
                f.entry(k.clone()).or_insert(v.clone());
            }
        }
        if conn.out.send(&frame) == Err(SendError::Overflow) {
            eprintln!(
                "[plug] {}: queue to the plugin is full: dropping the link",
                conn.name
            );
        }
    }

    fn attach(&self, conn: Arc<Conn>) {
        let old = self.current.lock().unwrap().replace(conn);
        if let Some(old) = old {
            // 旧连线上没回的请求算结果未知（丢掉发送端）
            old.pending.lock().unwrap().clear();
            old.out.close();
        }
    }

    fn detach(&self, generation: u64) {
        let mut cur = self.current.lock().unwrap();
        if cur.as_ref().is_some_and(|c| c.generation == generation) {
            *cur = None;
        }
    }
}

/// 建 socket：删掉旧档，属组给插件用户，0660（R1）。
pub fn bind(path: &str, gid: u32) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    let p = std::path::Path::new(path);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let listener = UnixListener::bind(p)?;
    // SAFETY: geteuid 没有前提条件。不是 root（单元测试）时改不了属主，照原样。
    if unsafe { libc::geteuid() } == 0 {
        std::os::unix::fs::chown(p, Some(0), Some(gid))?;
    }
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o660))?;
    Ok(listener)
}

/// 收连线：只认 uid 是 `uid` 的对端，别的写一行日志就关（R1）。
pub async fn serve(hub: Arc<Hub>, listener: UnixListener, uid: u32) {
    loop {
        let stream = match listener.accept().await {
            Ok((s, _)) => s,
            Err(e) => {
                eprintln!("[plug] accept: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
        match stream.peer_cred() {
            Ok(cred) if cred.uid() == uid => {
                let hub = hub.clone();
                tokio::spawn(async move { handle(hub, stream).await });
            }
            Ok(cred) => eprintln!("[plug] refused uid {} (want {uid})", cred.uid()),
            Err(e) => eprintln!("[plug] peer credentials: {e}"),
        }
    }
}

/// 一条连线：先等 hello，再收回复，直到断开或超限。
async fn handle(hub: Arc<Hub>, stream: UnixStream) {
    let (rd, wr) = stream.into_split();
    let out = frame::spawn_writer(wr, OUT_MAX);
    let mut rd = BufReader::new(rd);
    let mut buf = Vec::new();

    let hello = match tokio::time::timeout(
        HELLO_WAIT,
        frame::read_line(&mut rd, FRAME_MAX, &mut buf),
    )
    .await
    {
        Ok(Ok(true)) => serde_json::from_slice::<Value>(&buf).ok(),
        _ => None,
    };
    let Some(hello) = hello.filter(|h| h["t"] == "hello") else {
        eprintln!("[plug] no hello: closing");
        out.close();
        return;
    };
    let name = hello["name"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(40)
        .collect::<String>();
    if !hello["v"]
        .as_u64()
        .is_some_and(|v| (CONTRACT_MIN..=CONTRACT_V).contains(&v))
    {
        eprintln!(
            "[plug] {name}: contract v{} (core speaks v{CONTRACT_MIN}–v{CONTRACT_V}): refused",
            hello["v"]
        );
        let _ =
            out.send(&json!({"t":"hello","ok":false,"v":CONTRACT_V,"error":{"code":"version"}}));
        // 让拒绝送出去再关
        tokio::time::sleep(Duration::from_millis(50)).await;
        out.close();
        return;
    }
    let ops = hello["ops"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .take(32)
                .collect()
        })
        .unwrap_or_default();
    let conn = Arc::new(Conn {
        generation: hub.generations.fetch_add(1, Ordering::Relaxed) + 1,
        name: name.clone(),
        ops,
        out: out.clone(),
        pending: Mutex::new(HashMap::new()),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    if out
        .send(&json!({"t":"hello","ok":true,"v":CONTRACT_V}))
        .is_err()
    {
        return;
    }
    eprintln!(
        "[plug] {name} connected (link {}, ops {:?})",
        conn.generation, conn.ops
    );
    hub.attach(conn.clone());

    loop {
        let got = tokio::select! {
            r = frame::read_line(&mut rd, FRAME_MAX, &mut buf) => r,
            _ = out.closed() => break,
        };
        match got {
            Ok(true) => on_frame(&conn, &buf),
            Ok(false) => break,
            Err(ReadError::TooLong) => {
                eprintln!("[plug] {name}: frame over {FRAME_MAX} bytes: dropping the link");
                break;
            }
            Err(ReadError::Io(e)) => {
                eprintln!("[plug] {name}: {e}");
                break;
            }
        }
    }
    out.close();
    conn.pending.lock().unwrap().clear();
    hub.detach(conn.generation);
    eprintln!("[plug] {name} disconnected (link {})", conn.generation);
}

fn on_frame(conn: &Arc<Conn>, buf: &[u8]) {
    let Ok(v) = serde_json::from_slice::<Value>(buf) else {
        eprintln!("[plug] {}: not JSON: ignored", conn.name);
        return;
    };
    match v["t"].as_str() {
        Some("res") => {
            let Some(id) = v["id"].as_u64() else {
                eprintln!("[plug] {}: reply without id: dropped", conn.name);
                return;
            };
            let Some(tx) = conn.pending.lock().unwrap().remove(&id) else {
                eprintln!("[plug] {}: reply for unknown id {id}: dropped", conn.name);
                return;
            };
            let reply = if v["ok"].as_bool() == Some(true) {
                Reply::Ok(v.get("result").cloned().unwrap_or(Value::Null))
            } else {
                Reply::Refused {
                    code: v["error"]["code"]
                        .as_str()
                        .unwrap_or("refused")
                        .chars()
                        .take(40)
                        .collect(),
                    message: v["error"]["message"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect(),
                }
            };
            let _ = tx.send(reply);
        }
        Some("call") => on_call(conn, &v),
        other => eprintln!("[plug] {}: frame type {other:?} not handled", conn.name),
    }
}

/// 插件发来的调用：hello 声明过的那一组才接；回 `{"t":"ret","id",...}`。
fn on_call(conn: &Arc<Conn>, v: &Value) {
    let Some(id) = v["id"].as_u64() else {
        eprintln!("[plug] {}: call without id: dropped", conn.name);
        return;
    };
    let op = v["op"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(40)
        .collect::<String>();
    let refuse = |code: &str| {
        let _ = conn
            .out
            .send(&json!({"t":"ret","id":id,"ok":false,"error":{"code":code}}));
    };
    if !conn.declares(op.split('.').next().unwrap_or("")) {
        return refuse("not_declared");
    }
    if conn.calls.fetch_add(1, Ordering::AcqRel) >= IN_FLIGHT_MAX {
        conn.calls.fetch_sub(1, Ordering::AcqRel);
        return refuse("busy");
    }
    let params = v.get("params").cloned().unwrap_or(Value::Null);
    let conn = conn.clone();
    tokio::spawn(async move {
        let frame = match super::dispatch(&op, &params).await {
            Ok(result) => json!({"t":"ret","id":id,"ok":true,"result":result}),
            Err((code, message)) => {
                json!({"t":"ret","id":id,"ok":false,"error":{"code":code,"message":message}})
            }
        };
        conn.calls.fetch_sub(1, Ordering::AcqRel);
        if conn.out.send(&frame) == Err(SendError::Overflow) {
            eprintln!(
                "[plug] {}: queue to the plugin is full: dropping the link",
                conn.name
            );
        }
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    /// 起一个 socket 服务，回 (hub, 路径)。`uid` 是要认的对端 uid。
    /// 测试用的临时目录，用完删掉。
    pub(crate) struct Dir(pub std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) async fn server(uid: u32) -> (Arc<Hub>, std::path::PathBuf, Dir) {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "plug-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let path = dir.join("core.sock");
        let listener = bind(path.to_str().unwrap(), unsafe { libc::getegid() }).unwrap();
        let hub = Arc::new(Hub::default());
        hub.set_installed(true);
        tokio::spawn(serve(hub.clone(), listener, uid));
        (hub, path, Dir(dir))
    }

    pub(crate) fn me() -> u32 {
        unsafe { libc::geteuid() }
    }

    /// 假插件：连上、送 hello，回 (读端, 写端)。
    pub(crate) async fn plugin(
        path: &std::path::Path,
        hello: Value,
    ) -> (
        BufReader<tokio::net::unix::OwnedReadHalf>,
        tokio::net::unix::OwnedWriteHalf,
    ) {
        let s = UnixStream::connect(path).await.unwrap();
        let (r, mut w) = s.into_split();
        let mut line = serde_json::to_vec(&hello).unwrap();
        line.push(b'\n');
        // 被拒的连线可能已经关了：写失败不算错
        let _ = w.write_all(&line).await;
        (BufReader::new(r), w)
    }

    pub(crate) async fn next(r: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> Option<Value> {
        let mut s = String::new();
        match r.read_line(&mut s).await {
            Ok(0) | Err(_) => None,
            Ok(_) => serde_json::from_str(&s).ok(),
        }
    }

    pub(crate) async fn reply(w: &mut tokio::net::unix::OwnedWriteHalf, v: Value) {
        let mut line = serde_json::to_vec(&v).unwrap();
        line.push(b'\n');
        w.write_all(&line).await.unwrap();
    }

    pub(crate) fn hello(ops: &[&str]) -> Value {
        json!({"t":"hello","v":CONTRACT_V,"name":"test","ops":ops})
    }

    pub(crate) async fn wait_connected(hub: &Hub) {
        for _ in 0..200 {
            if hub.current().is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("plugin never connected");
    }

    #[tokio::test]
    async fn a_peer_with_another_uid_is_refused() {
        let (hub, path, _d) = server(me().wrapping_add(1)).await;
        let (mut r, _w) = plugin(&path, hello(&["settings"])).await;
        assert_eq!(next(&mut r).await, None, "closed without a hello reply");
        assert!(hub.current().is_none());
    }

    #[tokio::test]
    async fn the_socket_file_is_0660() {
        use std::os::unix::fs::PermissionsExt;
        let (_hub, path, _d) = server(me()).await;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o660);
    }

    #[tokio::test]
    async fn a_wrong_contract_version_is_refused_and_logged() {
        let (hub, path, _d) = server(me()).await;
        let (mut r, _w) = plugin(
            &path,
            json!({"t":"hello","v":99,"name":"x","ops":["settings"]}),
        )
        .await;
        let v = next(&mut r).await.unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "version");
        assert_eq!(next(&mut r).await, None);
        assert!(hub.current().is_none());
    }

    /// v1 的 core 照收 v0 的插件（已上机的 phone）；没声明的那组操作照样拿不到。
    #[tokio::test]
    async fn a_v0_plugin_still_connects_and_undeclared_ops_are_refused() {
        let (hub, path, _d) = server(me()).await;
        let (mut r, mut w) =
            plugin(&path, json!({"t":"hello","v":0,"name":"old","ops":["sim"]})).await;
        let v = next(&mut r).await.unwrap();
        assert_eq!(
            (v["ok"].as_bool(), v["v"].as_u64()),
            (Some(true), Some(CONTRACT_V))
        );
        wait_connected(&hub).await;
        reply(
            &mut w,
            json!({"t":"call","id":1,"op":"sms.read","params":{}}),
        )
        .await;
        let v = next(&mut r).await.unwrap();
        assert_eq!(v["error"]["code"], "not_declared");
    }

    #[tokio::test]
    async fn a_reply_with_the_same_id_comes_back() {
        let (hub, path, _d) = server(me()).await;
        let (mut r, mut w) = plugin(&path, hello(&["settings"])).await;
        assert_eq!(next(&mut r).await.unwrap()["ok"], true);
        wait_connected(&hub).await;
        let h = hub.clone();
        let call = tokio::spawn(async move {
            h.request(
                "settings.read",
                json!({}),
                Map::new(),
                Duration::from_secs(3),
            )
            .await
        });
        let req = next(&mut r).await.unwrap();
        assert_eq!(req["t"], "req");
        assert_eq!(req["op"], "settings.read");
        assert!(req["deadline_ms"].as_u64().unwrap() > super::super::mono_ms());
        // 认不得的 id：丢掉，请求照样等正确的那条
        reply(
            &mut w,
            json!({"t":"res","id":req["id"].as_u64().unwrap() + 100,"ok":true,"result":1}),
        )
        .await;
        reply(
            &mut w,
            json!({"t":"res","id":req["id"],"ok":true,"result":{"rev":3}}),
        )
        .await;
        assert_eq!(call.await.unwrap(), Ok(Reply::Ok(json!({"rev":3}))));
    }

    #[tokio::test]
    async fn no_reply_in_time_is_result_unknown() {
        let (hub, path, _d) = server(me()).await;
        let (mut r, _w) = plugin(&path, hello(&["settings"])).await;
        next(&mut r).await;
        wait_connected(&hub).await;
        let got = hub
            .request(
                "settings.write",
                json!({}),
                Map::new(),
                Duration::from_millis(100),
            )
            .await;
        assert_eq!(got, Err(CallError::Unknown));
    }

    #[tokio::test]
    async fn a_link_lost_after_sending_is_result_unknown() {
        let (hub, path, _d) = server(me()).await;
        let (mut r, w) = plugin(&path, hello(&["settings"])).await;
        next(&mut r).await;
        wait_connected(&hub).await;
        let h = hub.clone();
        let call = tokio::spawn(async move {
            h.request(
                "settings.write",
                json!({}),
                Map::new(),
                Duration::from_secs(3),
            )
            .await
        });
        next(&mut r).await.unwrap();
        drop(w);
        drop(r);
        assert_eq!(call.await.unwrap(), Err(CallError::Unknown));
    }

    #[tokio::test]
    async fn without_a_link_the_request_is_offline_and_undeclared_ops_are_unsupported() {
        let (hub, path, _d) = server(me()).await;
        assert_eq!(
            hub.request(
                "settings.read",
                json!({}),
                Map::new(),
                Duration::from_secs(1)
            )
            .await,
            Err(CallError::Offline)
        );
        let (mut r, _w) = plugin(&path, hello(&[])).await;
        next(&mut r).await;
        wait_connected(&hub).await;
        assert_eq!(
            hub.request(
                "settings.read",
                json!({}),
                Map::new(),
                Duration::from_secs(1)
            )
            .await,
            Err(CallError::Unsupported)
        );
    }

    #[tokio::test]
    async fn the_fifth_request_in_flight_is_busy() {
        let (hub, path, _d) = server(me()).await;
        let (mut r, _w) = plugin(&path, hello(&["settings"])).await;
        next(&mut r).await;
        wait_connected(&hub).await;
        let mut calls = Vec::new();
        for _ in 0..IN_FLIGHT_MAX {
            let h = hub.clone();
            calls.push(tokio::spawn(async move {
                h.request(
                    "settings.read",
                    json!({}),
                    Map::new(),
                    Duration::from_millis(300),
                )
                .await
            }));
        }
        for _ in 0..IN_FLIGHT_MAX {
            next(&mut r).await.unwrap();
        }
        assert_eq!(
            hub.request(
                "settings.read",
                json!({}),
                Map::new(),
                Duration::from_millis(300)
            )
            .await,
            Err(CallError::Busy)
        );
        for c in calls {
            assert_eq!(c.await.unwrap(), Err(CallError::Unknown));
        }
    }

    #[tokio::test]
    async fn a_frame_without_newline_past_the_limit_drops_the_link() {
        let (hub, path, _d) = server(me()).await;
        let (mut r, mut w) = plugin(&path, hello(&["settings"])).await;
        next(&mut r).await;
        wait_connected(&hub).await;
        let chunk = vec![b'x'; 8192];
        for _ in 0..(FRAME_MAX / chunk.len() + 2) {
            if w.write_all(&chunk).await.is_err() {
                break;
            }
        }
        assert_eq!(next(&mut r).await, None, "core closed the link");
        for _ in 0..200 {
            if hub.current().is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("link still attached");
    }

    #[tokio::test]
    async fn a_new_link_replaces_the_old_one() {
        let (hub, path, _d) = server(me()).await;
        let (mut r1, _w1) = plugin(&path, hello(&["settings"])).await;
        next(&mut r1).await;
        wait_connected(&hub).await;
        let first = hub.current().unwrap().generation;
        let (mut r2, _w2) = plugin(&path, hello(&["settings"])).await;
        next(&mut r2).await;
        for _ in 0..200 {
            if hub.current().is_some_and(|c| c.generation != first) {
                assert_eq!(next(&mut r1).await, None, "old link closed");
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("second link never took over");
    }
}
