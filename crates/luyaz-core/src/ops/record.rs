//! 流水账和 owners（write-op-layer.md「流水账」、D27、R3-7、D16）。
//!
//! - 单一写者：一个系统线程独占 `journal.jsonl` 和 `owners.json`，和事务锁无关；调用方只往有界通道里
//!   放一行（不阻塞、不进执行者）。合并判断和放进通道在同一把锁里做，行的先后不会乱。
//! - 有上限：`journal.jsonl` 超过上限就改名成 `journal.1.jsonl`（盖掉更旧的），两份最多约 2 倍上限；
//!   每行也有上限（字符串字段截断，整行太长只留骨架），外部 `journal.append` 撑不爆它。通道满了记数，
//!   之后补一行 `dropped`。每行写完 fsync（写操作很少）。
//! - 密码不落盘：Wi-Fi 密码、APN 用户名/密码等字段只写 `(changed)`；短信的动作只记动作名，不记参数。
//! - skipped 合并（D27）：同一来源 + 同一项 + 同一原因，第一次记一行（`skip:start`），之后只计数；
//!   原因变了，或这个来源对这一项有了别的记录，再记一行结束（`skip:end`，`count` = 连第一次在内一共跳过几次）。
//!   计数只在内存里，datad 重启时进行中的那一段丢掉（已知限制）。
//! - owners（D16）：每一项最后一次是谁写的（来源、值、墙钟时间），单独存，流水账滚掉也不丢。
//! - 时间：系统时间按 UTC 拆成年月日时分秒，不换时区（设备时钟是「当地时间标成 UTC」）。
//! - 目录没设（测试、CI）就什么都不记，`list` 为空。

use super::txn::{Txn, Who};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender, TrySendError},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const JOURNAL: &str = "journal.jsonl";
const JOURNAL_OLD: &str = "journal.1.jsonl";
const OWNERS: &str = "owners.json";
/// 默认每份 256 KiB（两份约 512 KiB）。
const DEFAULT_MAX_BYTES: u64 = 256 * 1024;
const MAX_LINE: usize = 2_048;
const MAX_STRING: usize = 256;
const QUEUE: usize = 256;
pub const REDACTED: &str = "(changed)";

enum Msg {
    Line(Vec<u8>),
    Owners(Vec<u8>),
    Flush(tokio::sync::oneshot::Sender<()>),
}

#[derive(Default)]
struct RecSt {
    /// (来源, 项) → 进行中的跳过。
    skips: HashMap<(String, String), (String, u64)>,
    owners: Map<String, Value>,
    dropped: u64,
}

struct Shared {
    dir: PathBuf,
    tx: SyncSender<Msg>,
    st: Mutex<RecSt>,
}

#[derive(Clone, Default)]
pub struct Record {
    inner: Option<Arc<Shared>>,
}

impl Record {
    /// `None` = 不记。上限可用 `LUYAZ_JOURNAL_MAX_BYTES` 改。
    pub fn open(dir: Option<PathBuf>) -> Self {
        let max = std::env::var("LUYAZ_JOURNAL_MAX_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MAX_BYTES);
        Self::open_with(dir, max)
    }

    pub fn open_with(dir: Option<PathBuf>, max_bytes: u64) -> Self {
        let Some(dir) = dir else {
            return Self::default();
        };
        if let Err(e) = fs::create_dir_all(&dir) {
            eprintln!("ops: cannot use {} for the journal: {e}", dir.display());
            return Self::default();
        }
        let owners = fs::read(dir.join(OWNERS))
            .ok()
            .and_then(|b| serde_json::from_slice::<Map<String, Value>>(&b).ok())
            .unwrap_or_default();
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let wdir = dir.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("journal".into())
            .spawn(move || writer(wdir, max_bytes, rx))
        {
            eprintln!("ops: cannot start the journal writer: {e}");
            return Self::default();
        }
        Self {
            inner: Some(Arc::new(Shared {
                dir,
                tx,
                st: Mutex::new(RecSt {
                    owners,
                    ..RecSt::default()
                }),
            })),
        }
    }

    fn lock(s: &Shared) -> std::sync::MutexGuard<'_, RecSt> {
        s.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 记一行。带 `source` 和 `item`（或 `action`）的行会先结束这个来源对这一项进行中的跳过。
    pub fn append(&self, line: Value) {
        let Some(s) = &self.inner else { return };
        let mut st = Self::lock(s);
        if let Some(k) = key(&line)
            && let Some((reason, count)) = st.skips.remove(&k)
        {
            let end = skip_line(&k, &reason, "end", count);
            emit(s, &mut st, end);
        }
        emit(s, &mut st, line);
    }

    /// 一次跳过（D27）：同一来源 + 同一项 + 同一原因只记开头和结尾。
    pub fn skip(&self, source: &str, item: &str, reason: &str) {
        let Some(s) = &self.inner else { return };
        let k = (source.to_owned(), item.to_owned());
        let mut st = Self::lock(s);
        match st.skips.get_mut(&k) {
            Some((r, n)) if r == reason => {
                *n += 1;
                return;
            }
            Some(_) => {
                let (r, n) = st.skips.remove(&k).expect("just seen");
                let end = skip_line(&k, &r, "end", n);
                emit(s, &mut st, end);
            }
            None => {}
        }
        st.skips.insert(k.clone(), (reason.to_owned(), 1));
        let start = skip_line(&k, reason, "start", 1);
        emit(s, &mut st, start);
    }

    /// 这一项最后是谁写的（D16：screen/web/legacy 算用户写，见 `user` 字段）。
    /// `source` 记谁提的；`user` 按批准者算（T22，D23），和来源不同时另记 `approved_by`。
    pub fn set_owner(
        &self,
        item: &str,
        who: impl Into<Who>,
        undo: bool,
        value: &Value,
        op_id: Option<&str>,
    ) {
        let who = who.into();
        let Some(s) = &self.inner else { return };
        let mut st = Self::lock(s);
        let (ts, t) = now();
        let mut owner = json!({
            "source": who.source,
            "user": who.approved_by.is_user(),
            "undo": undo,
            "value": clip_value(value.clone()),
            "op_id": op_id,
            "ts": ts,
            "t": t,
        });
        who.tag(&mut owner);
        st.owners.insert(item.to_owned(), owner);
        let bytes = serde_json::to_vec(&st.owners).expect("owners serialize");
        if s.tx.try_send(Msg::Owners(bytes)).is_err() {
            st.dropped += 1;
        }
    }

    pub fn owners(&self) -> Value {
        match &self.inner {
            Some(s) => Value::Object(Self::lock(s).owners.clone()),
            None => json!({}),
        }
    }

    /// 等前面放进去的行都写进闪存（重启/关机前用）。通道满就稍等再放；一共最多等 2 秒。
    pub async fn flush(&self) {
        let Some(s) = &self.inner else { return };
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let mut msg = Msg::Flush(tx);
            loop {
                match s.tx.try_send(msg) {
                    Ok(()) => break,
                    Err(TrySendError::Full(m)) => {
                        msg = m;
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
            let _ = rx.await;
        })
        .await;
    }

    /// 最近的 `limit` 行，新的在前（两份文件）。读到写了一半的行就跳过。
    pub fn list(&self, limit: usize) -> Vec<Value> {
        let Some(s) = &self.inner else {
            return vec![];
        };
        let mut out = Vec::new();
        for name in [JOURNAL, JOURNAL_OLD] {
            let Ok(f) = File::open(s.dir.join(name)) else {
                continue;
            };
            let mut lines: Vec<Value> = BufReader::new(f)
                .lines()
                .map_while(Result::ok)
                .filter_map(|l| serde_json::from_str(&l).ok())
                .collect();
            lines.reverse();
            out.extend(lines);
            if out.len() >= limit {
                break;
            }
        }
        out.truncate(limit);
        out
    }
}

fn key(line: &Value) -> Option<(String, String)> {
    let source = line.get("source")?.as_str()?;
    let item = line
        .get("item")
        .and_then(Value::as_str)
        .or_else(|| line.get("action").and_then(Value::as_str))?;
    Some((source.to_owned(), item.to_owned()))
}

fn skip_line(k: &(String, String), reason: &str, which: &str, count: u64) -> Value {
    json!({
        "source": k.0,
        "item": k.1,
        "result": "skipped",
        "reason": reason,
        "skip": which,
        "count": count,
    })
}

/// 盖时间、截断、序列化、放进通道（调用方拿着 `st` 锁，所以先后不乱）。
fn emit(s: &Shared, st: &mut RecSt, line: Value) {
    if st.dropped > 0 {
        let n = st.dropped;
        if s.tx
            .try_send(Msg::Line(encode(json!({"result":"dropped","count":n}))))
            .is_ok()
        {
            st.dropped = 0;
        }
    }
    if s.tx.try_send(Msg::Line(encode(line))).is_err() {
        st.dropped += 1;
    }
}

fn encode(line: Value) -> Vec<u8> {
    let mut line = clip_value(line);
    if let Value::Object(m) = &mut line {
        let (ts, t) = now();
        m.entry("ts").or_insert(json!(ts));
        m.entry("t").or_insert(json!(t));
    }
    let mut bytes = serde_json::to_vec(&line).expect("line serializes");
    if bytes.len() > MAX_LINE {
        // 太长：只留骨架。
        let mut small = Map::new();
        for k in [
            "ts",
            "t",
            "op_id",
            "source",
            "approved_by",
            "action",
            "item",
            "result",
            "reason",
        ] {
            if let Some(v) = line.get(k) {
                small.insert(k.into(), v.clone());
            }
        }
        small.insert("truncated".into(), json!(true));
        bytes = serde_json::to_vec(&small).expect("line serializes");
    }
    bytes
}

fn clip(s: &str) -> String {
    if s.len() <= MAX_STRING {
        return s.to_owned();
    }
    let mut end = MAX_STRING;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn clip_value(v: Value) -> Value {
    match v {
        Value::String(s) => Value::String(clip(&s)),
        Value::Array(a) => Value::Array(a.into_iter().map(clip_value).collect()),
        Value::Object(m) => Value::Object(m.into_iter().map(|(k, v)| (k, clip_value(v))).collect()),
        other => other,
    }
}

fn writer(dir: PathBuf, max: u64, rx: mpsc::Receiver<Msg>) {
    let path = dir.join(JOURNAL);
    let mut size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let mut file: Option<File> = None;
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Line(mut b) => {
                if file.is_none() {
                    file = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .map_err(|e| eprintln!("ops: cannot open {JOURNAL}: {e}"))
                        .ok();
                }
                let Some(f) = file.as_mut() else { continue };
                b.push(b'\n');
                if let Err(e) = f.write_all(&b).and_then(|()| f.sync_data()) {
                    eprintln!("ops: cannot write {JOURNAL}: {e}");
                    file = None;
                    continue;
                }
                size += b.len() as u64;
                if size >= max {
                    file = None;
                    if let Err(e) = fs::rename(&path, dir.join(JOURNAL_OLD)) {
                        eprintln!("ops: cannot roll {JOURNAL}: {e}");
                    }
                    sync_dir(&dir);
                    size = 0;
                }
            }
            Msg::Owners(b) => {
                if let Err(e) = super::pending::write_atomic(&dir, OWNERS, &b) {
                    eprintln!("ops: cannot save {OWNERS}: {e}");
                }
            }
            Msg::Flush(ack) => {
                let _ = ack.send(());
            }
        }
    }
}

fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

/// (unix 秒, "YYYY-MM-DD HH:MM:SS")。按 UTC 拆，不换时区（设备时钟本来就是当地时间标成 UTC）。
fn now() -> (u64, String) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (secs, civil(secs))
}

pub(crate) fn civil(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant 的 civil_from_days。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// 密码类字段只写 `(changed)`，SIM 身份类字段只留后 4 位；短信动作不记参数（号码、内容）。
pub fn redact(action: &str, params: &Value) -> Value {
    if action.starts_with("sms.")
        || (action == "vendor.call"
            && params
                .get("method")
                .and_then(Value::as_str)
                .is_some_and(|m| m.contains("sms")))
    {
        return Value::Null;
    }
    redact_value(params)
}

fn secret(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    // eSIM 的激活码、确认码也算。
    [
        "pin",
        "code",
        "activation_code",
        "confirm_code",
        "confirmation_code",
        "matching_id",
    ]
    .contains(&k.as_str())
        || [
            "pass", "psk", "secret", "token", "user", "key", "puk", "cred",
        ]
        .iter()
        .any(|w| k.contains(w))
        // SIM PIN 和网络锁码：pin_num、new_pin_num、pin_num_m、nck（pin_mode、
        // pin_encode_flag 这类说明不是码）
        || k.split('_').any(|seg| seg == "nck")
        || (k.split('_').any(|seg| seg == "pin")
            && !k.ends_with("_mode")
            && !k.ends_with("_flag"))
}

/// SIM 身份类字段（ICCID、EID、IMSI、号码）只留后 4 位（D32：流水账只显示后 4 位）。
fn sim_id_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["iccid", "eid", "imsi", "msisdn"]
        .iter()
        .any(|w| k == *w || k.ends_with(&format!("_{w}")) || k.starts_with(&format!("{w}_")))
}

fn tail4(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars[chars.len().saturating_sub(4)..].iter().collect()
}

fn redact_value(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| {
                    let v = if secret(k) && !v.is_null() {
                        json!(REDACTED)
                    } else if sim_id_key(k) && !v.is_null() {
                        json!(tail4(&match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        }))
                    } else {
                        redact_value(v)
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(redact_value).collect()),
        other => other.clone(),
    }
}

/// 事务结束的一行。SIM 只记 ICCID 后 4 位 + 卡槽（D32）；退回目标和旧值都记（D35）。
pub fn txn_line(t: &Txn) -> Value {
    let tail = tail4(&t.sim.iccid);
    let mut line = json!({
        "op_id": t.op_id,
        "action": t.action,
        "item": t.item,
        "source": t.source,
        "undo": t.undo,
        "sim": format!("{tail}/{}", t.sim.slot),
        "old": t.old,
        "new": t.target,
        "rollback_to": t.rollback_to,
        "readback": t.readback,
        "result": t.phase,
        "reason": t.reason,
        "rollback_reason": t.rollback_reason,
        "took_ms": t.took_ms,
    });
    t.who().tag(&mut line);
    line
}

#[cfg(test)]
mod tests;
