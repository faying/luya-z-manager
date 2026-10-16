//! 可编程的 mock ubusd（T3）。datad 是二进制 crate，所以它由 `src/ubus/mod.rs` 用 `#[path]`
//! 引进 crate 自己的单元测试（cargo 不会把 tests/ 下的子目录当成测试目标）。
//!
//! 按 ubusd 的做法对答：连上先发 HELLO（peer = 分给这条连接的 client id）；LOOKUP 回每个匹配对象一帧
//! DATA（OBJPATH、OBJID、OBJTYPE）再回 STATUS；INVOKE 回 DATA（OBJID、DATA）再回 STATUS，头的 seq 同请求、
//! peer = 对象 id；对象不存在回 STATUS=NOT_FOUND，方法不存在回 METHOD_NOT_FOUND。
//!
//! 可编程：每个（对象，方法）一队一次性的 `Action`（延迟、不回也不断开、迟到、指定状态、无数据、回显参数），
//! 改对象 ID（模拟服务重新注册）、停止/重启（模拟 ubusd 重启）、在下一个 INVOKE 的回复前注入
//! 错 peer / 错 seq / 无关类型的帧。

use crate::ubus::blob::{self, MsgHdr, attr, msg_type, status};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    os::fd::AsFd,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::{UnixListener, UnixStream},
    task::JoinHandle,
};

/// 对一次 INVOKE 的处理（一次性，按调用顺序出队；队列空了就用方法的默认值正常回复）。
#[derive(Debug, Clone)]
pub enum Action {
    /// 等这么久再正常回复（期间这条连接不处理别的请求，同 ubusd 单线程的服务）。
    Delay(Duration),
    /// 不回复，也不断开。
    Hang,
    /// 现在不回复；等下一个 INVOKE（任意连接）到达时，先把这次的回复（原 seq、原 peer）发出去。
    Late,
    /// 只回 STATUS=code。
    Status(i32),
    /// STATUS OK，不带 DATA。
    NoData,
    /// 把收到的参数原样回复（测参数编码）。
    Echo,
}

/// 在下一个 INVOKE 的正常回复之前多发的帧。
#[derive(Debug, Clone)]
pub enum Inject {
    /// DATA + STATUS，seq 同当前请求，peer 不对。
    WrongPeer(Value),
    /// DATA + STATUS，peer 同当前请求，seq 不对。
    OtherSeq(Value),
    /// 一帧无关类型（NOTIFY），seq/peer 都同当前请求。
    Unrelated,
}

struct Obj {
    id: u32,
    methods: HashMap<String, Value>,
}

#[derive(Default)]
struct State {
    objects: HashMap<String, Obj>,
    scripts: HashMap<(String, String), VecDeque<Action>>,
    late: Vec<Vec<u8>>,
    inject: Vec<Inject>,
    lookups: u64,
    invokes: HashMap<String, u64>,
    next_client: u32,
}

/// 一条连接：服务任务，和同一个 socket 的一份 dup（stop 时用它 shutdown，dup 失败时为 None）。
type Conn = (JoinHandle<()>, Option<std::os::unix::net::UnixStream>);

struct Server {
    accept: JoinHandle<()>,
    conns: Arc<Mutex<Vec<Conn>>>,
}

pub struct MockUbusd {
    path: PathBuf,
    state: Arc<Mutex<State>>,
    connections: Arc<AtomicU64>,
    server: Option<Server>,
}

static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

impl MockUbusd {
    /// 在一个本进程独有的临时路径上起 mock。
    pub async fn start_new() -> Self {
        let n = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("zwrt-ubus-mock-{}-{n}.sock", std::process::id()));
        let mut m = Self {
            path,
            state: Arc::new(Mutex::new(State {
                next_client: 0x1000,
                ..State::default()
            })),
            connections: Arc::new(AtomicU64::new(0)),
            server: None,
        };
        m.start();
        m
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 连这个 mock 的 socket 后端（采集轮里的超时 = `timeout`），给执行者用。
    pub fn backend(&self, timeout: Duration) -> crate::ubus::backend::SocketBackend {
        crate::ubus::backend::SocketBackend::new(crate::ubus::client::UbusClient::with_timeout(
            &self.path, timeout,
        ))
    }

    /// 注册（或替换）对象的一个方法和它的默认回复。
    pub fn add_method(&self, object: &str, id: u32, method: &str, reply: Value) {
        let mut st = self.state.lock().unwrap();
        let obj = st.objects.entry(object.to_string()).or_insert_with(|| Obj {
            id,
            methods: HashMap::new(),
        });
        obj.id = id;
        obj.methods.insert(method.to_string(), reply);
    }

    /// 服务重新注册：同名对象换一个 ID，旧 ID 失效。
    pub fn change_id(&self, object: &str, new_id: u32) {
        self.state
            .lock()
            .unwrap()
            .objects
            .get_mut(object)
            .expect("object")
            .id = new_id;
    }

    pub fn remove_object(&self, object: &str) {
        self.state.lock().unwrap().objects.remove(object);
    }

    pub fn script(&self, object: &str, method: &str, action: Action) {
        self.state
            .lock()
            .unwrap()
            .scripts
            .entry((object.to_string(), method.to_string()))
            .or_default()
            .push_back(action);
    }

    pub fn inject_before_next_invoke(&self, inject: Inject) {
        self.state.lock().unwrap().inject.push(inject);
    }

    /// 接受过的连接数（每条都发过 HELLO）。
    pub fn connections(&self) -> u64 {
        self.connections.load(Ordering::SeqCst)
    }
    pub fn lookups(&self) -> u64 {
        self.state.lock().unwrap().lookups
    }
    /// 发给这个对象（按请求时的 ID 对上的名字；ID 已失效的记在 "?"）的 INVOKE 数。
    pub fn invokes(&self, object: &str) -> u64 {
        self.state
            .lock()
            .unwrap()
            .invokes
            .get(object)
            .copied()
            .unwrap_or(0)
    }

    /// 开始监听（先删掉旧 socket 文件）。
    pub fn start(&mut self) {
        assert!(self.server.is_none(), "mock ubusd already running");
        let _ = std::fs::remove_file(&self.path);
        let listener = UnixListener::bind(&self.path).expect("bind mock ubusd");
        let conns = Arc::new(Mutex::new(Vec::new()));
        let state = self.state.clone();
        let counter = self.connections.clone();
        let conns2 = conns.clone();
        let accept = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let dup = stream
                    .as_fd()
                    .try_clone_to_owned()
                    .ok()
                    .map(std::os::unix::net::UnixStream::from);
                let h = tokio::spawn(serve(stream, state.clone()));
                conns2.lock().unwrap().push((h, dup));
            }
        });
        self.server = Some(Server { accept, conns });
    }

    /// 停止：关掉监听和所有连接（等它们真的关掉再返回），删掉 socket 文件。
    ///
    /// 连接先 shutdown 再关：只关本进程的 fd 不够。整套测试并行时，别的用例起子进程的瞬间
    /// （fork 到 exec 之间）子进程也拿着这些 fd，socket 就还活着——客户端这时写旧连接会成功，
    /// 等子进程 exec 关掉它，客户端读到 ECONNRESET，而不是 ubusd 重启该有的「写不进去」
    /// （ubus_ubusd_restart_reconnects 曾因此约一成失败）。shutdown 作用在 socket 本身，谁拿着都一样。
    pub async fn stop(&mut self) {
        if let Some(s) = self.server.take() {
            s.accept.abort();
            let _ = s.accept.await;
            let conns: Vec<_> = s.conns.lock().unwrap().drain(..).collect();
            for (h, dup) in conns {
                if let Some(d) = &dup {
                    let _ = d.shutdown(std::net::Shutdown::Both);
                }
                h.abort();
                let _ = h.await;
            }
        }
        let _ = std::fs::remove_file(&self.path);
        self.state.lock().unwrap().late.clear();
    }

    /// ubusd 重启：全部断开，再在同一路径上监听。
    pub async fn restart(&mut self) {
        self.stop().await;
        self.start();
    }

    /// 替「正在 fork 的子进程」多拿一份现有连接的服务端 socket（测 stop 不靠「最后一个 fd 关掉」）。
    pub fn hold_server_sockets(&self) -> Vec<std::os::unix::net::UnixStream> {
        let Some(s) = &self.server else {
            return Vec::new();
        };
        s.conns
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(_, dup)| dup.as_ref()?.try_clone().ok())
            .collect()
    }
}

impl Drop for MockUbusd {
    fn drop(&mut self) {
        if let Some(s) = self.server.take() {
            s.accept.abort();
            for (h, dup) in s.conns.lock().unwrap().drain(..) {
                if let Some(d) = &dup {
                    let _ = d.shutdown(std::net::Shutdown::Both);
                }
                h.abort();
            }
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn status_frame(seq: u16, peer: u32, code: i32, objid: Option<u32>) -> Vec<u8> {
    let mut body = Vec::new();
    blob::put_u32(&mut body, attr::STATUS, code as u32);
    if let Some(id) = objid {
        blob::put_u32(&mut body, attr::OBJID, id);
    }
    blob::encode_frame(MsgHdr::new(msg_type::STATUS, seq, peer), &body)
}

fn data_frame(seq: u16, peer: u32, objid: u32, value: &Value) -> Vec<u8> {
    let mut body = Vec::new();
    blob::put_u32(&mut body, attr::OBJID, objid);
    let table = blob::blobmsg_table(value.as_object().expect("reply must be an object"));
    blob::put_attr(&mut body, attr::DATA, false, &table);
    blob::encode_frame(MsgHdr::new(msg_type::DATA, seq, peer), &body)
}

fn reply_frames(seq: u16, peer: u32, value: &Value) -> Vec<u8> {
    let mut out = data_frame(seq, peer, peer, value);
    out.extend(status_frame(seq, peer, status::OK, Some(peer)));
    out
}

async fn serve(mut stream: UnixStream, state: Arc<Mutex<State>>) {
    let client_id = {
        let mut st = state.lock().unwrap();
        st.next_client += 1;
        st.next_client
    };
    let hello = blob::encode_frame(MsgHdr::new(msg_type::HELLO, 0, client_id), &[]);
    if stream.write_all(&hello).await.is_err() {
        return;
    }
    loop {
        let Ok(req) = blob::read_frame(&mut stream).await else {
            return;
        };
        let out = match req.hdr.msg_type {
            msg_type::LOOKUP => lookup(&state, req.hdr, &req.body),
            msg_type::INVOKE => match invoke(&state, req.hdr, &req.body) {
                Step::Now(bytes) => bytes,
                Step::After(d, bytes) => {
                    tokio::time::sleep(d).await;
                    bytes
                }
                Step::Nothing(bytes) => {
                    if !bytes.is_empty() && stream.write_all(&bytes).await.is_err() {
                        return;
                    }
                    continue;
                }
            },
            _ => status_frame(req.hdr.seq, req.hdr.peer, status::INVALID_COMMAND, None),
        };
        if stream.write_all(&out).await.is_err() {
            return;
        }
    }
}

fn lookup(state: &Mutex<State>, hdr: MsgHdr, body: &[u8]) -> Vec<u8> {
    let mut st = state.lock().unwrap();
    st.lookups += 1;
    let a = blob::msg_attrs(body).expect("lookup attrs");
    let path = a[attr::OBJPATH as usize]
        .as_ref()
        .map(blob::get_string)
        .unwrap_or_default();
    let Some(obj) = st.objects.get(&path) else {
        return status_frame(hdr.seq, hdr.peer, status::NOT_FOUND, None);
    };
    let mut d = Vec::new();
    blob::put_string(&mut d, attr::OBJPATH, &path);
    blob::put_u32(&mut d, attr::OBJID, obj.id);
    blob::put_u32(&mut d, attr::OBJTYPE, obj.id ^ 0x5a5a);
    let mut out = blob::encode_frame(MsgHdr::new(msg_type::DATA, hdr.seq, hdr.peer), &d);
    out.extend(status_frame(hdr.seq, hdr.peer, status::OK, None));
    out
}

enum Step {
    Now(Vec<u8>),
    After(Duration, Vec<u8>),
    /// 不回复；参数是要先发出去的迟到帧和注入帧（可能为空）。
    Nothing(Vec<u8>),
}

fn invoke(state: &Mutex<State>, hdr: MsgHdr, body: &[u8]) -> Step {
    let mut st = state.lock().unwrap();
    let a = blob::msg_attrs(body).expect("invoke attrs");
    let id = a[attr::OBJID as usize]
        .as_ref()
        .map(|x| blob::get_u32(x).unwrap())
        .unwrap_or(0);
    let method = a[attr::METHOD as usize]
        .as_ref()
        .map(blob::get_string)
        .unwrap_or_default();
    let args = a[attr::DATA as usize]
        .as_ref()
        .map(|d| Value::Object(blob::blobmsg_object(d.data).expect("args blobmsg")))
        .unwrap_or_else(|| json!({}));

    // 先发迟到的回复和注入帧。
    let mut pre: Vec<u8> = st.late.drain(..).flatten().collect();
    for inj in std::mem::take(&mut st.inject) {
        match inj {
            Inject::WrongPeer(v) => {
                let wrong = hdr.peer ^ 0x00ff_0000;
                pre.extend(data_frame(hdr.seq, wrong, wrong, &v));
                pre.extend(status_frame(hdr.seq, wrong, status::OK, Some(wrong)));
            }
            Inject::OtherSeq(v) => {
                pre.extend(reply_frames(hdr.seq.wrapping_add(100), hdr.peer, &v));
            }
            Inject::Unrelated => {
                pre.extend(blob::encode_frame(
                    MsgHdr::new(msg_type::NOTIFY, hdr.seq, hdr.peer),
                    &[],
                ));
            }
        }
    }

    let Some((name, obj)) = st.objects.iter().find(|(_, o)| o.id == id) else {
        *st.invokes.entry("?".into()).or_default() += 1;
        pre.extend(status_frame(hdr.seq, hdr.peer, status::NOT_FOUND, None));
        return Step::Now(pre);
    };
    let name = name.clone();
    let default = obj.methods.get(&method).cloned();
    *st.invokes.entry(name.clone()).or_default() += 1;
    let Some(default) = default else {
        pre.extend(status_frame(
            hdr.seq,
            hdr.peer,
            status::METHOD_NOT_FOUND,
            Some(id),
        ));
        return Step::Now(pre);
    };
    let action = st
        .scripts
        .get_mut(&(name, method))
        .and_then(VecDeque::pop_front);
    match action {
        None => {
            pre.extend(reply_frames(hdr.seq, hdr.peer, &default));
            Step::Now(pre)
        }
        Some(Action::Echo) => {
            pre.extend(reply_frames(hdr.seq, hdr.peer, &args));
            Step::Now(pre)
        }
        Some(Action::Delay(d)) => {
            pre.extend(reply_frames(hdr.seq, hdr.peer, &default));
            Step::After(d, pre)
        }
        Some(Action::Hang) => Step::Nothing(pre),
        Some(Action::Late) => {
            st.late.push(reply_frames(hdr.seq, hdr.peer, &default));
            Step::Nothing(pre)
        }
        Some(Action::Status(code)) => {
            pre.extend(status_frame(hdr.seq, hdr.peer, code, Some(id)));
            Step::Now(pre)
        }
        Some(Action::NoData) => {
            pre.extend(status_frame(hdr.seq, hdr.peer, status::OK, Some(id)));
            Step::Now(pre)
        }
    }
}
