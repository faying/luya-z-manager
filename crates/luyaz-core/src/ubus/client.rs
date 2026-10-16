//! 直连 ubusd unix socket 的 ubus 客户端（docs/STATE_V2.md V2-18、V2-19）。
//!
//! 协议（ubus/libubus.c、libubus-req.c、ubusd_proto.c，按记忆，见 blob.rs 顶部的 [Gate 0] 清单）：
//! - 连上后 ubusd 先发一帧 HELLO，头里的 peer 是分给本连接的 client id；客户端不发 HELLO。
//! - LOOKUP：peer=0，属性 OBJPATH；ubusd 对每个匹配对象回一帧 DATA（OBJPATH、OBJID、OBJTYPE、SIGNATURE），
//!   最后回 STATUS。找不到时 STATUS=NOT_FOUND。
//! - INVOKE：peer=对象 id，属性 OBJID、METHOD、DATA（blobmsg 表）；对象所属进程回 0 或 1 帧 DATA（属性 DATA），
//!   再回 STATUS。ubusd 转发回复时把头的 peer 改成对象 id（`ubusd_handle_response`），seq 保持请求的 seq。
//!   对象不存在时 ubusd 自己回 STATUS=NOT_FOUND（seq/peer 同请求）。
//! - 所以「当前请求的帧」= 头的 seq 和 peer 都等于请求的（libubus `ubus_find_request` 同样按 seq+peer 找）。
//!
//! 行为（R2）：
//! - 同一时间只有一个请求在途：`&mut self` 串行，不加锁、不起后台任务（单一执行者在 T4）。
//! - 只接受 seq+peer 都对得上的帧，其余丢弃并计数（`ClientStats::dropped_frames`）。
//! - 每次 `call` 一个截止时间（默认 2 秒，含 LOOKUP、INVOKE 和 NOT_FOUND 后的一次重试）。
//!   超时就关连接（下次调用重新连、重新收 HELLO），作废该对象的 ID 缓存，返回 `UbusError::Timeout`；
//!   其他对象的 LOOKUP 缓存保留。本轮跳过该对象用 `RoundSkips` + `call_in_round`。
//! - seq 跨重连单调递增（不从头开始），旧连接上迟到的回复即使被转到新连接也对不上。
//! - 对象 ID 会变（原厂服务重启后重新注册）：INVOKE 回 NOT_FOUND 时作废缓存、重新 LOOKUP，
//!   新 ID 和原来不同才再调一次；相同（服务自己用 NOT_FOUND 表示条目不存在）就原样返回，不重发；
//!   回 METHOD_NOT_FOUND 时只作废缓存（下次调用重新 LOOKUP），不在本次重试。
//! - 只在请求**确定没送到** ubusd 时自动重试（旧连接写失败，比如 ubusd 重启过；这时整个 ID 缓存作废，
//!   因为重启后 ID 全换、旧 ID 可能被别的对象用上）；读写错误断线同样清空 ID 缓存；
//!   写出去之后超时或断开都不重试，避免 `/control` 的写操作执行两次。

use super::blob::{self, Frame, MsgHdr, ReadError, attr, msg_type, status};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::UnixStream,
    time::{Instant, timeout_at},
};

/// 设备上 ubusd 的 socket（新版 OpenWrt；旧版是 /var/run/ubus.sock）[Gate 0]。
pub const DEFAULT_SOCKET: &str = "/var/run/ubus/ubus.sock";
/// 单个请求的超时（V2-19）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UbusError {
    /// 参数不合法（对象名、方法名、args 不是对象、消息过大），没有发请求。文字同 `state::ubus`。
    InvalidArgument(String),
    /// 在截止时间内没等到 STATUS。连接已关闭、该对象 ID 缓存已作废；本轮应跳过这个对象（V2-18）。
    Timeout { object: String, detail: String },
    /// 该对象本轮已经超时过、或还在跨轮冷却里（执行者），这次没有发请求。
    Skipped { object: String },
    /// 对象没注册（LOOKUP 回 NOT_FOUND，或 NOT_FOUND 后重新 LOOKUP 仍不行）。
    NotFound { object: String },
    /// ubusd 或服务回了非 0 状态。
    Status {
        object: String,
        method: String,
        code: i32,
    },
    /// 状态 OK 但没有 DATA。ubus CLI 这时什么都不打印，`state::ubus` 得到的是 JSON 解析错误；
    /// `detail` 就是那条错误文字（后端层用它保持一致）。
    NoData { object: String, detail: String },
    /// 连接、读写失败（连接已丢，下次自动重连）。CLI 后端的命令失败也归这里，文字同 `state::ubus`。
    Io(String),
    /// 帧或编码不合规（连接已丢）。
    Protocol(String),
}

impl UbusError {
    /// 算作「这个对象本轮跳过」的错误：超时，或本轮已跳过。
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout { .. } | Self::Skipped { .. })
    }
}

impl fmt::Display for UbusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument(m) | Self::Io(m) => f.write_str(m),
            Self::Timeout { detail, .. } | Self::NoData { detail, .. } => f.write_str(detail),
            Self::Skipped { object } => {
                write!(f, "ubus {object}: skipped after a recent timeout")
            }
            Self::NotFound { object } => {
                write!(f, "ubus {object}: {}", status::name(status::NOT_FOUND))
            }
            Self::Status {
                object,
                method,
                code,
            } => write!(
                f,
                "ubus {object} {method}: {} ({code})",
                status::name(*code)
            ),
            Self::Protocol(m) => write!(f, "ubus protocol error: {m}"),
        }
    }
}

impl std::error::Error for UbusError {}

/// 计数器（测试和日志用）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ClientStats {
    /// 成功建立（收到 HELLO）的连接数。
    pub connects: u64,
    /// 不属于当前请求而丢掉的帧。
    pub dropped_frames: u64,
    pub timeouts: u64,
    pub lookups: u64,
    pub invokes: u64,
}

/// 一轮里超时过的对象（V2-18：本轮剩余时间跳过）。执行者每轮开始时 `clear()`。
#[derive(Debug, Default, Clone)]
pub struct RoundSkips {
    timed_out: HashSet<String>,
}

impl RoundSkips {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }
    pub fn is_skipped(&self, object: &str) -> bool {
        self.timed_out.contains(object)
    }
    /// 记下错误；是超时就把对象加进跳过集合。
    pub fn record(&mut self, e: &UbusError) {
        if let UbusError::Timeout { object, .. } = e {
            self.timed_out.insert(object.clone());
        }
    }
    pub fn clear(&mut self) {
        self.timed_out.clear();
    }
    /// 本轮超时过的对象个数。
    pub fn timed_out(&self) -> usize {
        self.timed_out.len()
    }
}

struct Conn {
    stream: UnixStream,
}

/// 一个请求收齐的回复：每帧 DATA 的属性区，加最终状态。
struct Reply {
    data: Vec<Vec<u8>>,
    status: i32,
}

pub struct UbusClient {
    path: PathBuf,
    timeout: Duration,
    conn: Option<Conn>,
    seq: u16,
    ids: HashMap<String, u32>,
    stats: ClientStats,
    /// 本次 request 在旧连接上写失败（请求没送到）。
    stale_write: bool,
    /// 本次 `call` 的 INVOKE 帧可能已经到了 ubusd（写成功，或写到一半超时）。
    /// 为假时请求肯定没执行，后端可以改走 CLI（`backend::AutoBackend`）。
    invoke_maybe_sent: bool,
}

impl UbusClient {
    #[cfg(test)]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_timeout(path, DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(path: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            path: path.into(),
            timeout,
            conn: None,
            seq: 0,
            ids: HashMap::new(),
            stats: ClientStats::default(),
            stale_write: false,
            invoke_maybe_sent: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }
    pub fn stats(&self) -> ClientStats {
        self.stats
    }
    #[cfg(test)]
    pub fn is_connected(&self) -> bool {
        self.conn.is_some()
    }
    /// 缓存里的对象 id（测试用）。
    pub fn cached_id(&self, object: &str) -> Option<u32> {
        self.ids.get(object).copied()
    }

    /// 上一次 `call` 失败时请求肯定没到对象那里：连不上、没收到 HELLO、LOOKUP 失败（没有副作用），
    /// 或 INVOKE 帧没写出去。只有这种失败可以换 CLI 再做一次；INVOKE 写出去之后的失败（超时、断开、
    /// 坏回复）都不算，因为原厂可能已经执行了（写操作不能做两次）。
    pub fn last_call_not_sent(&self) -> bool {
        !self.invoke_maybe_sent
    }

    /// 本轮已超时的对象直接返回 `Skipped`，不发请求；这次超时就记进 `skips`。
    /// 测试用：程序里的跳过由执行者做（`executor.rs`，同样用 `RoundSkips`）。
    #[cfg(test)]
    pub async fn call_in_round(
        &mut self,
        skips: &mut RoundSkips,
        object: &str,
        method: &str,
        args: &Value,
    ) -> Result<Option<Value>, UbusError> {
        if skips.is_skipped(object) {
            return Err(UbusError::Skipped {
                object: object.to_string(),
            });
        }
        let r = self.call(object, method, args).await;
        if let Err(e) = &r {
            skips.record(e);
        }
        r
    }

    /// 调 `object.method(args)`。成功时 `Some(对象)`；状态 OK 但没 DATA 时 `None`。
    pub async fn call(
        &mut self,
        object: &str,
        method: &str,
        args: &Value,
    ) -> Result<Option<Value>, UbusError> {
        self.invoke_maybe_sent = false;
        super::validate_name(object).map_err(UbusError::InvalidArgument)?;
        super::validate_name(method).map_err(UbusError::InvalidArgument)?;
        let Some(map) = args.as_object() else {
            return Err(UbusError::InvalidArgument("args must be an object".into()));
        };
        let data = blob::blobmsg_table(map);
        if data.len() + 256 > blob::UBUS_MAX_MSGLEN {
            return Err(UbusError::InvalidArgument("ubus args too large".into()));
        }
        let deadline = Instant::now() + self.timeout;
        // 旧连接写失败：请求没送到，ID 缓存已清空（ubusd 重启后 ID 全换了），重新 LOOKUP 重发一次。
        let mut resent = false;
        loop {
            self.stale_write = false;
            match self.call_once(object, method, &data, deadline).await {
                Err(_) if self.stale_write && !resent => resent = true,
                r => return r,
            }
        }
    }

    async fn call_once(
        &mut self,
        object: &str,
        method: &str,
        data: &[u8],
        deadline: Instant,
    ) -> Result<Option<Value>, UbusError> {
        let mut retried = false;
        loop {
            let id = match self.ids.get(object) {
                Some(&id) => id,
                None => self.lookup(object, deadline).await?,
            };
            match self.invoke(object, id, method, data, deadline).await {
                Err(e @ UbusError::Status { code, .. })
                    if code == status::NOT_FOUND && !retried =>
                {
                    // 可能是对象重新注册换了 ID：重新 LOOKUP，ID 真变了才再调一次。
                    // ID 没变说明是服务自己用 NOT_FOUND 表示「条目不存在」，请求已经执行过，
                    // 不重发（写操作不能做两次），原样返回。
                    self.ids.remove(object);
                    if self.lookup(object, deadline).await? == id {
                        return Err(e);
                    }
                    retried = true;
                }
                Err(e) => {
                    if let UbusError::Status { code, .. } = &e
                        && (*code == status::NOT_FOUND || *code == status::METHOD_NOT_FOUND)
                    {
                        self.ids.remove(object);
                    }
                    return Err(e);
                }
                ok => return ok,
            }
        }
    }

    async fn lookup(&mut self, object: &str, deadline: Instant) -> Result<u32, UbusError> {
        self.stats.lookups += 1;
        let mut body = Vec::new();
        blob::put_string(&mut body, attr::OBJPATH, object);
        let reply = self
            .request(object, deadline, msg_type::LOOKUP, 0, &body)
            .await?;
        if reply.status == status::NOT_FOUND {
            return Err(UbusError::NotFound {
                object: object.into(),
            });
        }
        if reply.status != status::OK {
            return Err(UbusError::Status {
                object: object.into(),
                method: "lookup".into(),
                code: reply.status,
            });
        }
        for d in &reply.data {
            let a = blob::msg_attrs(d).map_err(|e| self.protocol(e.0))?;
            let (Some(path), Some(id)) = (&a[attr::OBJPATH as usize], &a[attr::OBJID as usize])
            else {
                continue;
            };
            if blob::get_string(path) == object {
                let id = blob::get_u32(id).map_err(|e| self.protocol(e.0))?;
                self.ids.insert(object.to_string(), id);
                return Ok(id);
            }
        }
        Err(UbusError::NotFound {
            object: object.into(),
        })
    }

    async fn invoke(
        &mut self,
        object: &str,
        id: u32,
        method: &str,
        data: &[u8],
        deadline: Instant,
    ) -> Result<Option<Value>, UbusError> {
        self.stats.invokes += 1;
        let mut body = Vec::new();
        blob::put_u32(&mut body, attr::OBJID, id);
        blob::put_string(&mut body, attr::METHOD, method);
        blob::put_attr(&mut body, attr::DATA, false, data);
        let reply = self
            .request(object, deadline, msg_type::INVOKE, id, &body)
            .await?;
        if reply.status != status::OK {
            return Err(UbusError::Status {
                object: object.into(),
                method: method.into(),
                code: reply.status,
            });
        }
        // 多帧 DATA 时取第一帧（CLI 会把每帧各打一行，state::ubus 解析会失败；实际服务只回一帧）。
        for d in &reply.data {
            let a = blob::msg_attrs(d).map_err(|e| self.protocol(e.0))?;
            if let Some(payload) = &a[attr::DATA as usize] {
                let map = blob::blobmsg_object(payload.data).map_err(|e| self.protocol(e.0))?;
                return Ok(Some(Value::Object(map)));
            }
        }
        Ok(None)
    }

    fn protocol(&mut self, msg: String) -> UbusError {
        self.conn = None;
        UbusError::Protocol(msg)
    }

    /// 连接因读写错误断掉（不是我们自己超时关的）：ubusd 可能重启过，重启后所有对象的 ID 都换了，
    /// 旧 ID 甚至可能被别的对象用上，所以整个 ID 缓存作废。
    fn lost_connection(&mut self) {
        self.conn = None;
        self.ids.clear();
    }

    fn next_seq(&mut self) -> u16 {
        self.seq = self.seq.wrapping_add(1);
        if self.seq == 0 {
            self.seq = 1;
        }
        self.seq
    }

    fn timed_out(&mut self, object: &str) -> UbusError {
        self.conn = None;
        self.ids.remove(object);
        self.stats.timeouts += 1;
        UbusError::Timeout {
            object: object.into(),
            detail: format!(
                "ubus {object} timed out after {} ms",
                self.timeout.as_millis()
            ),
        }
    }

    async fn connect(&mut self, object: &str, deadline: Instant) -> Result<(), UbusError> {
        let mut stream = match timeout_at(deadline, UnixStream::connect(&self.path)).await {
            Err(_) => return Err(self.timed_out(object)),
            Ok(Err(e)) => {
                return Err(UbusError::Io(format!(
                    "connect {}: {e}",
                    self.path.display()
                )));
            }
            Ok(Ok(s)) => s,
        };
        let hello = match timeout_at(deadline, blob::read_frame(&mut stream)).await {
            Err(_) => return Err(self.timed_out(object)),
            Ok(Err(ReadError::Io(e))) => return Err(UbusError::Io(format!("ubus hello: {e}"))),
            Ok(Err(ReadError::Protocol(e))) => return Err(UbusError::Protocol(e.0)),
            Ok(Ok(f)) => f,
        };
        if hello.hdr.msg_type != msg_type::HELLO {
            return Err(UbusError::Protocol(format!(
                "expected HELLO, got message type {}",
                hello.hdr.msg_type
            )));
        }
        // HELLO 头里的 peer 是分给本连接的 client id；客户端用不到，只要这一帧已经读走。
        self.conn = Some(Conn { stream });
        self.stats.connects += 1;
        Ok(())
    }

    /// 发一个请求，收齐它的 DATA 和 STATUS。
    async fn request(
        &mut self,
        object: &str,
        deadline: Instant,
        ty: u8,
        peer: u32,
        body: &[u8],
    ) -> Result<Reply, UbusError> {
        let seq = self.next_seq();
        let frame = blob::encode_frame(MsgHdr::new(ty, seq, peer), body);
        let fresh = self.conn.is_none();
        if fresh {
            self.connect(object, deadline).await?;
        }
        let conn = self.conn.as_mut().expect("connected");
        let write = timeout_at(deadline, conn.stream.write_all(&frame)).await;
        // 写成功或写到一半超时，INVOKE 都可能已经到了 ubusd；写出错（对端关了）则没有。
        if ty == msg_type::INVOKE && !matches!(write, Ok(Err(_))) {
            self.invoke_maybe_sent = true;
        }
        match write {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                // 旧连接写失败多半是 ubusd 重启过：请求没送到，让 call 清掉 ID 后重发一次。
                self.stale_write = !fresh;
                self.lost_connection();
                return Err(UbusError::Io(format!("ubus write: {e}")));
            }
            Err(_) => return Err(self.timed_out(object)),
        }
        let mut data = Vec::new();
        loop {
            let conn = self.conn.as_mut().expect("connected");
            let f: Frame = match timeout_at(deadline, blob::read_frame(&mut conn.stream)).await {
                Err(_) => return Err(self.timed_out(object)),
                Ok(Err(ReadError::Io(e))) => {
                    self.lost_connection();
                    return Err(UbusError::Io(format!("ubus read: {e}")));
                }
                Ok(Err(ReadError::Protocol(e))) => return Err(self.protocol(e.0)),
                Ok(Ok(f)) => f,
            };
            if f.hdr.seq != seq || f.hdr.peer != peer {
                self.stats.dropped_frames += 1;
                continue;
            }
            match f.hdr.msg_type {
                msg_type::DATA => data.push(f.body),
                msg_type::STATUS => {
                    let a = blob::msg_attrs(&f.body).map_err(|e| self.protocol(e.0))?;
                    let code = match &a[attr::STATUS as usize] {
                        Some(s) => blob::get_u32(s).map_err(|e| self.protocol(e.0))? as i32,
                        None => return Err(self.protocol("STATUS without status attribute".into())),
                    };
                    return Ok(Reply { data, status: code });
                }
                _ => self.stats.dropped_frames += 1,
            }
        }
    }
}
