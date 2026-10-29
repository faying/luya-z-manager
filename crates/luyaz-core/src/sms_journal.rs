//! 短信持久日志（luyaz-phone 设计 O-2，R12、R14、R21–R23）。
//!
//! core 一读到新短信，就先把它（含全文）写进这份只追加、有上限的日志，之后 sms-forward 和进程插件
//! 才看得到它（R22）。插件按（世代, 序号）的游标补读，确认后才可以在超限时清掉。
//!
//! - 去重键是寄件人 + 时间 + 内容的 sha256，不用固件的短信 id（固件重读卡会重新编号，R21）。
//!   每次扫描都是整库扫，键见过就跳过，所以重新编号、编号变小都不会漏也不会重复。
//! - SIM 来源用身份键 `sha256(设备盐 + ICCID)` 前 16 字节（R14）；身份不稳（换卡中）、或者这次
//!   core 启动后第一次整库扫之前进来的（core 没在看的时候收到的），都标「来源未知」，不猜。
//! - 写法：临时档 + fsync + rename + 目录 fsync，旧的一份留成 `.prev`（R12）。主档坏了改名
//!   `.corrupt-<秒>`、读 `.prev` 并换世代（R23）；两份都不能用就重建：换世代、原厂库里已有的短信
//!   全部当基线（来源未知、不推送）。
//! - 上限 2000 条或 2 MB（R7）：先清已确认的最旧几条；还超就清未确认的最旧一条并记一笔事故。

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// 日志放在 core 的状态目录里。
pub const JOURNAL_PATH: &str = "/data/luyaz/core/sms-journal.json";
/// 设备盐（R14）：第一次用时生成，0600。
pub const SALT_PATH: &str = "/data/luyaz/core/sim-salt";

const MAX_ENTRIES: usize = 2000;
const MAX_BYTES: usize = 2 * 1024 * 1024;
/// 见过的键记多少个（比原厂短信库能放的多得多），多久。
const MAX_SEEN: usize = 4000;
const SEEN_KEEP_SECS: u64 = 90 * 86_400;
const MAX_INCIDENTS: usize = 20;
/// 一次读最多几条。
pub const READ_MAX: usize = 50;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    /// 去重键（[`key_of`]）。
    pub key: String,
    /// 收进来时固件的短信 id，只给 mark-read/删除和排查用，不当身份。
    pub fw_id: u64,
    pub sender: String,
    pub date: String,
    pub text: String,
    /// SIM 身份键；`None` = 来源未知。
    pub sim: Option<String>,
    /// 要不要推送（基线和重建时收进来的旧短信不推）。
    pub push: bool,
    /// 收进日志的时间（设备时钟秒）。
    pub at: u64,
}

impl Entry {
    fn weight(&self) -> usize {
        self.key.len() + self.sender.len() + self.date.len() + self.text.len() + 160
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Seen {
    key: String,
    at: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Incident {
    pub at: u64,
    pub what: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Data {
    v: u32,
    /// 建立、从 `.prev` 回复或重建时随机换新（R23）。
    generation: String,
    next_seq: u64,
    /// 插件确认到哪一条（这一世代内）。
    acked: u64,
    /// 下一次整库扫是基线：收进来的标来源未知、不推送。
    baseline_pending: bool,
    entries: Vec<Entry>,
    seen: Vec<Seen>,
    /// 超限时丢掉的未确认短信总数。
    dropped_unacked: u64,
    incidents: Vec<Incident>,
}

impl Data {
    fn fresh() -> Self {
        Data {
            v: 1,
            generation: new_generation(),
            next_seq: 1,
            acked: 0,
            baseline_pending: true,
            entries: Vec::new(),
            seen: Vec::new(),
            dropped_unacked: 0,
            incidents: Vec::new(),
        }
    }
}

fn new_generation() -> String {
    format!("{:016x}", rand::random::<u64>())
}

/// 这条短信从哪张卡来（R14、R20）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Sim(String),
    Unknown,
}

pub struct Journal {
    path: PathBuf,
    data: Data,
    /// 这次 core 启动后做完第一次整库扫了没有；之前进来的标来源未知。
    caught_up: bool,
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 去重键：寄件人、时间、内容（固件给的原样）的 sha256 前 16 字节。
pub fn key_of(sender: &str, date: &str, content: &str) -> String {
    let mut h = Sha256::new();
    for part in [sender, date, content] {
        h.update(part.as_bytes());
        h.update([0x1f]);
    }
    hex16(&h.finalize())
}

fn hex16(d: &[u8]) -> String {
    d.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// SIM 身份键（R14）：`sha256(盐 + ICCID)` 前 16 字节。
pub fn sim_key(salt: &[u8], iccid: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(iccid.as_bytes());
    hex16(&h.finalize())
}

/// 固件的内容多半是 UCS-2 hex；是的话解成文字（含代理对），不是就照原样。
pub fn text_of(content: &str) -> String {
    let t = content.trim().as_bytes();
    if t.is_empty() || !t.len().is_multiple_of(4) || !t.iter().all(u8::is_ascii_hexdigit) {
        return content.to_owned();
    }
    let units = t
        .chunks(4)
        .filter_map(|q| std::str::from_utf8(q).ok())
        .filter_map(|q| u16::from_str_radix(q, 16).ok());
    char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

fn read_data(p: &Path) -> Option<Result<Data, String>> {
    match std::fs::read(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(Err(e.to_string())),
        Ok(b) => Some(serde_json::from_slice::<Data>(&b).map_err(|e| e.to_string())),
    }
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

impl Journal {
    /// 打开日志；坏了就照 R12 回复或重建（会写盘）。
    pub fn open(path: PathBuf) -> Journal {
        let now = now_unix();
        let prev = with_suffix(&path, ".prev");
        let mut note = None;
        let data = match read_data(&path) {
            Some(Ok(d)) => d,
            main => {
                let damaged = main.is_some();
                if let Some(Err(e)) = &main {
                    let to = with_suffix(&path, &format!(".corrupt-{now}"));
                    eprintln!(
                        "[sms-journal] {}: {e}: moved to {}",
                        path.display(),
                        to.display()
                    );
                    let _ = std::fs::rename(&path, &to);
                }
                match read_data(&prev) {
                    Some(Ok(mut d)) => {
                        d.generation = new_generation();
                        note = Some("主档坏了或不见了：从 .prev 回复，换了世代".to_owned());
                        d
                    }
                    p => {
                        if damaged || p.is_some() {
                            note = Some(
                                "主档和 .prev 都不能用：重建，原厂库里的短信当基线".to_owned(),
                            );
                        }
                        Data::fresh()
                    }
                }
            }
        };
        let mut j = Journal {
            path,
            data,
            caught_up: false,
        };
        if let Some(what) = note {
            eprintln!("[sms-journal] {what}");
            j.incident(now, what);
            if let Err(e) = j.save() {
                eprintln!("[sms-journal] save after recovery: {e}");
            }
        }
        j
    }

    pub fn generation(&self) -> &str {
        &self.data.generation
    }

    /// 最近的事故（回复、重建、超限丢弃）；doctor 的「电话线路」一节读（E1）。
    #[allow(dead_code)]
    pub fn incidents(&self) -> &[Incident] {
        &self.data.incidents
    }

    fn incident(&mut self, at: u64, what: String) {
        self.data.incidents.push(Incident { at, what });
        let n = self.data.incidents.len();
        if n > MAX_INCIDENTS {
            self.data.incidents.drain(..n - MAX_INCIDENTS);
        }
    }

    /// 收一批固件短信（`sms.list_after` 的 items）。`full` = 这是一次整库扫。
    /// 写盘成功才算收进来；写盘失败整批作废（内存也回到原样），回错误。回新收的序号。
    pub fn ingest(
        &mut self,
        items: &[Value],
        full: bool,
        source: &Source,
    ) -> std::io::Result<Vec<u64>> {
        let now = now_unix();
        let before = self.data.clone();
        let baseline = self.data.baseline_pending;
        let mut known: HashSet<String> = self.data.seen.iter().map(|s| s.key.clone()).collect();
        known.extend(self.data.entries.iter().map(|e| e.key.clone()));
        let mut added = Vec::new();
        for item in items {
            let s = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
            let (sender, date, content) = (s("number"), s("date"), s("content"));
            if sender.is_empty() && content.is_empty() {
                continue;
            }
            // 只收收到的（tag 0 已读、1 未读）：2 是这台设备发出去的副本（插件发短信、告警短信），
            // 3、4 是发送失败和草稿。没有 tag 的照收
            if let Some(tag) = item.get("tag") {
                let t = tag
                    .as_i64()
                    .or_else(|| tag.as_str().and_then(|s| s.trim().parse().ok()));
                if !matches!(t, Some(0 | 1)) {
                    continue;
                }
            }
            let key = key_of(&sender, &date, &content);
            if !known.insert(key.clone()) {
                continue;
            }
            let attributed = !baseline && self.caught_up;
            let seq = self.data.next_seq;
            self.data.next_seq += 1;
            self.data.entries.push(Entry {
                seq,
                key: key.clone(),
                fw_id: item.get("id").and_then(Value::as_u64).unwrap_or(0),
                sender,
                date,
                text: text_of(&content),
                sim: match (attributed, source) {
                    (true, Source::Sim(k)) => Some(k.clone()),
                    _ => None,
                },
                push: !baseline,
                at: now,
            });
            self.data.seen.push(Seen { key, at: now });
            added.push(seq);
        }
        let mut changed = !added.is_empty();
        if full && baseline {
            self.data.baseline_pending = false;
            changed = true;
        }
        if changed {
            self.trim(now);
            if let Err(e) = self.save() {
                self.data = before;
                return Err(e);
            }
        }
        if full {
            self.caught_up = true;
        }
        Ok(added)
    }

    /// 上限：条数、字节、见过的键、事故。
    fn trim(&mut self, now: u64) {
        let mut bytes: usize = self.data.entries.iter().map(Entry::weight).sum();
        while self.data.entries.len() > MAX_ENTRIES || bytes > MAX_BYTES {
            let acked = self.data.acked;
            let i = self
                .data
                .entries
                .iter()
                .position(|e| e.seq <= acked)
                .unwrap_or(0);
            let gone = self.data.entries.remove(i);
            bytes -= gone.weight();
            if gone.seq > acked {
                self.data.dropped_unacked += 1;
                self.incident(
                    now,
                    format!("日志满了：丢掉一条还没确认的短信（第 {} 条）", gone.seq),
                );
            }
        }
        self.data
            .seen
            .retain(|s| now.saturating_sub(s.at) <= SEEN_KEEP_SECS);
        let n = self.data.seen.len();
        if n > MAX_SEEN {
            self.data.seen.drain(..n - MAX_SEEN);
        }
    }

    /// 插件补读：世代不同就从头给（`reset`），否则给 `after` 之后的，最多 `limit` 条。
    pub fn read(&self, generation: Option<&str>, after: u64, limit: usize) -> Value {
        let reset = generation != Some(self.data.generation.as_str());
        let after = if reset { 0 } else { after };
        let limit = limit.clamp(1, READ_MAX);
        let mut more = self.data.entries.iter().filter(|e| e.seq > after);
        let entries: Vec<Value> = more
            .by_ref()
            .take(limit)
            .map(|e| {
                json!({"seq": e.seq, "sender": e.sender, "date": e.date, "text": e.text,
                       "sim": e.sim, "push": e.push, "at": e.at, "fw_id": e.fw_id})
            })
            .collect();
        json!({
            "generation": self.data.generation,
            "reset": reset,
            "entries": entries,
            "has_more": more.next().is_some(),
            "acked": if reset { 0 } else { self.data.acked },
        })
    }

    /// 插件确认收到这一世代的第 `seq` 条（含之前的）。世代不对回 false。
    pub fn ack(&mut self, generation: &str, seq: u64) -> std::io::Result<bool> {
        if generation != self.data.generation {
            return Ok(false);
        }
        let seq = seq.min(self.data.next_seq.saturating_sub(1));
        if seq <= self.data.acked {
            return Ok(true);
        }
        let before = self.data.acked;
        self.data.acked = seq;
        if let Err(e) = self.save() {
            self.data.acked = before;
            return Err(e);
        }
        Ok(true)
    }

    pub fn last_seq(&self) -> u64 {
        self.data.next_seq.saturating_sub(1)
    }

    /// 临时档 + fsync + 旧档改名 `.prev` + rename + 目录 fsync，0600（含短信全文）。
    fn save(&self) -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = with_suffix(&self.path, ".tmp");
        let bytes = serde_json::to_vec(&self.data).map_err(std::io::Error::other)?;
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        if self.path.exists() {
            std::fs::rename(&self.path, with_suffix(&self.path, ".prev"))?;
        }
        std::fs::rename(&tmp, &self.path)?;
        std::fs::File::open(dir)?.sync_all()
    }
}

// ---- 进程里的那一份 ----

static JOURNAL: OnceLock<Mutex<Journal>> = OnceLock::new();
static SALT: OnceLock<Vec<u8>> = OnceLock::new();
/// 扫描用的 core 连线：插件删短信（`sms.remove`）也走它，和扫描、管理网页同一个执行者。
static CONTROL: OnceLock<crate::api::Compat> = OnceLock::new();

/// core 起来时打开日志和设备盐（写之前就开好，sms-forward 和插件都等它）。
pub fn init(journal: PathBuf, salt: PathBuf) {
    let _ = SALT.set(load_salt(&salt));
    let _ = JOURNAL.set(Mutex::new(Journal::open(journal)));
}

pub fn global() -> Option<&'static Mutex<Journal>> {
    JOURNAL.get()
}

pub fn control() -> Option<&'static crate::api::Compat> {
    CONTROL.get()
}

fn load_salt(p: &Path) -> Vec<u8> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Ok(b) = std::fs::read(p)
        && b.len() >= 16
    {
        return b;
    }
    let salt: [u8; 32] = rand::random();
    let wrote = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(p)
        .and_then(|mut f| f.write_all(&salt).and_then(|_| f.sync_all()));
    if let Err(e) = wrote {
        eprintln!(
            "[sms-journal] {}: {e}: SIM keys only stable for this run",
            p.display()
        );
    }
    salt.to_vec()
}

/// 双卡时的电话卡（[`crate::ops::dual_sim::voice_card`]，每轮采集更新）：ICCID、号码。
static VOICE: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

/// 每轮采集：双卡时电话卡的 ICCID 和号码，没有（单卡、电话卡没开）是 `None`。
pub fn note_voice(card: Option<(String, String)>) {
    *VOICE.lock().unwrap_or_else(|e| e.into_inner()) = card;
}

/// 电话卡的号码（双卡、卡里存了才有）。
pub fn voice_number() -> Option<String> {
    VOICE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|(_, n)| n.clone())
        .filter(|n| !n.is_empty())
}

/// 现在这张卡（R14、R20）：身份稳定（online/reloaded）才给键，否则来源未知。双卡时是电话卡：
/// 原厂的短信不带卡槽，上网卡换来换去，线路和短信都跟电话卡（10-29 用户定「固定电话卡」）。
pub fn current_source() -> Source {
    use crate::sim_identity::Phase;
    let Some(salt) = SALT.get() else {
        return Source::Unknown;
    };
    let s = crate::sim_identity::global()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let voice = VOICE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|(iccid, _)| iccid.clone());
    match (s.phase(), s.known()) {
        (Phase::Online | Phase::Reloaded, Some(_)) if voice.is_some() => {
            Source::Sim(sim_key(salt, voice.as_deref().unwrap_or_default()))
        }
        (Phase::Online | Phase::Reloaded, Some(id)) if !id.iccid.is_empty() => {
            Source::Sim(sim_key(salt, &id.iccid))
        }
        _ => Source::Unknown,
    }
}

/// 收一批（sms-forward 的读取、自己的扫描都走这里）。写不进去回错误：调用方不能往下用这批。
/// 日志没开（只有不开写入的 core 和单元测试）就什么都不做。
pub fn ingest(items: &[Value], full: bool) -> Result<Vec<u64>, String> {
    let Some(j) = global() else {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| eprintln!("[sms-journal] not open: SMS pass through unrecorded"));
        return Ok(Vec::new());
    };
    let source = current_source();
    let added = j
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .ingest(items, full, &source)
        .map_err(|e| format!("sms journal: {e}"))?;
    if !added.is_empty() {
        crate::plug::sms::announce();
    }
    Ok(added)
}

// ---- 自己的扫描 ----

/// 没有 `sms` 块发布时多久整库扫一次（保底；平常由 `sms` 块的变化叫醒）。
const RESCAN: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// 一页取多少：`sms.list_after` 收的上限（多了它整次拒绝）。
const LIST_PAGE: u64 = crate::sms::LIST_LIMIT_MAX;

/// 整库扫：经 core 自己的 `/control sms.list_after`（执行者上，和 sms-forward 一样），收进日志。
pub async fn run(c: crate::api::Compat) {
    let _ = CONTROL.set(c.clone());
    let woken = c.src.hub.as_ref().map(|h| h.watch(&["sms"]));
    loop {
        if let Err(e) = scan(&c).await {
            eprintln!("[sms-journal] scan: {e}");
        }
        let look = tokio::time::sleep(RESCAN);
        match &woken {
            Some(n) => tokio::select! {
                _ = n.notified() => {}
                _ = look => {}
            },
            None => look.await,
        }
    }
}

async fn scan(c: &crate::api::Compat) -> Result<(), String> {
    let items = list_all(c).await?;
    tokio::task::spawn_blocking(move || ingest(&items, true))
        .await
        .map_err(|e| e.to_string())?
        .map(|_| ())
}

/// 两个库的全部短信（明文，含发出的副本），一页一页经 `/control sms.list_after`。
pub async fn list_all(c: &crate::api::Compat) -> Result<Vec<Value>, String> {
    let mut items = Vec::new();
    let mut after = 0u64;
    loop {
        let r = c
            .src
            .control
            .send_with(
                "auto",
                None,
                "sms.list_after",
                &json!({"after_id": after, "limit": LIST_PAGE}),
                LIST_TIMEOUT,
            )
            .await
            .into_result()?;
        let page = r["items"].as_array().ok_or("sms.list_after: no items")?;
        let last = page.iter().filter_map(|i| i["id"].as_u64()).max();
        items.extend(page.iter().cloned());
        match (r["has_more"].as_bool(), last) {
            (Some(true), Some(id)) if id > after => after = id,
            _ => return Ok(items),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn dir() -> Dir {
        let d = std::env::temp_dir().join(format!("sms-journal-{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn sms(id: u64, from: &str, date: &str, text: &str) -> Value {
        let hex: String = text.encode_utf16().map(|u| format!("{u:04X}")).collect();
        json!({"id": id, "number": from, "date": date, "content": hex})
    }

    fn card() -> Source {
        Source::Sim(sim_key(b"salt", "8986001"))
    }

    /// 开一份新日志并做完第一次整库扫（空库）。
    fn ready(p: &Path) -> Journal {
        let mut j = Journal::open(p.to_path_buf());
        j.ingest(&[], true, &card()).unwrap();
        j
    }

    #[test]
    fn the_first_scan_is_a_baseline_and_later_ones_are_new_and_attributed() {
        let d = dir();
        let p = d.0.join("sms-journal.json");
        let mut j = Journal::open(p.clone());
        let got = j
            .ingest(
                &[sms(5, "10086", "26,10,27,09,00,00", "旧短信")],
                true,
                &card(),
            )
            .unwrap();
        assert_eq!(got, vec![1]);
        let v = j.read(None, 0, 10);
        assert_eq!(v["entries"][0]["push"], false);
        assert_eq!(v["entries"][0]["sim"], Value::Null);
        assert_eq!(v["entries"][0]["text"], "旧短信");
        let got = j
            .ingest(
                &[sms(6, "10010", "26,10,27,10,00,00", "验证码 123456")],
                false,
                &card(),
            )
            .unwrap();
        assert_eq!(got, vec![2]);
        let v = j.read(Some(j.generation()), 1, 10);
        assert_eq!(v["reset"], false);
        assert_eq!(v["entries"][0]["push"], true);
        assert_eq!(v["entries"][0]["sim"], json!(sim_key(b"salt", "8986001")));
    }

    #[test]
    fn the_sent_copies_the_firmware_keeps_are_not_taken_as_received() {
        let d = dir();
        let mut j = ready(&d.0.join("sms-journal.json"));
        let tagged = |id, tag: Value| {
            let mut v = sms(id, "10086", "26,10,27,21,49,10,+32", "查话费");
            v["tag"] = tag;
            v
        };
        let got = j
            .ingest(
                &[
                    tagged(406, json!("2")),
                    tagged(407, json!(4)),
                    tagged(408, json!("1")),
                    tagged(409, json!(0)),
                ],
                false,
                &card(),
            )
            .unwrap();
        // 408 和 409 内容一样、时间一样：同一条
        assert_eq!(got, vec![1]);
        assert_eq!(j.read(None, 0, 10)["entries"][0]["fw_id"], 408);
    }

    #[test]
    fn renumbered_sms_are_not_taken_twice_and_a_lower_new_id_is_not_missed() {
        let d = dir();
        let p = d.0.join("sms-journal.json");
        let mut j = ready(&p);
        j.ingest(
            &[sms(40, "a", "1", "x"), sms(41, "b", "2", "y")],
            true,
            &card(),
        )
        .unwrap();
        // 固件重读卡：同一批换了 id，还多了一条 id 更小的新短信
        let got = j
            .ingest(
                &[
                    sms(1, "a", "1", "x"),
                    sms(2, "b", "2", "y"),
                    sms(3, "c", "3", "z"),
                ],
                true,
                &card(),
            )
            .unwrap();
        assert_eq!(got.len(), 1);
        let v = j.read(Some(j.generation()), 2, 10);
        assert_eq!(v["entries"][0]["sender"], "c");
    }

    #[test]
    fn before_the_first_full_scan_of_a_run_sms_have_no_source() {
        let d = dir();
        let p = d.0.join("sms-journal.json");
        ready(&p);
        // core 重启：第一次整库扫之前收到的（core 没在看时进来的）不猜来源
        let mut j = Journal::open(p.clone());
        j.ingest(&[sms(7, "a", "1", "while down")], false, &card())
            .unwrap();
        j.ingest(&[], true, &card()).unwrap();
        j.ingest(&[sms(8, "b", "2", "after")], false, &card())
            .unwrap();
        let v = j.read(None, 0, 10);
        assert_eq!(v["entries"][0]["sim"], Value::Null);
        assert_eq!(v["entries"][0]["push"], true);
        assert_ne!(v["entries"][1]["sim"], Value::Null);
        // 换卡中：来源未知
        j.ingest(&[sms(9, "c", "3", "switching")], false, &Source::Unknown)
            .unwrap();
        assert_eq!(j.read(None, 0, 10)["entries"][2]["sim"], Value::Null);
    }

    #[test]
    fn a_broken_main_file_is_read_back_from_prev_with_a_new_generation_and_no_repush() {
        let d = dir();
        let p = d.0.join("sms-journal.json");
        let mut j = ready(&p);
        j.ingest(&[sms(1, "a", "1", "x")], false, &card()).unwrap();
        j.ingest(&[sms(2, "b", "2", "y")], false, &card()).unwrap();
        let g1 = j.generation().to_owned();
        drop(j);
        std::fs::write(&p, b"{not json").unwrap();
        let mut j = Journal::open(p.clone());
        assert_ne!(j.generation(), g1);
        assert!(j.incidents().iter().any(|i| i.what.contains(".prev")));
        let corrupt = std::fs::read_dir(&d.0)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains(".corrupt-"));
        assert!(corrupt);
        // .prev 有第一条；第二条在原厂库里还在，再扫一次也不会重复收第一条
        let got = j
            .ingest(
                &[sms(1, "a", "1", "x"), sms(2, "b", "2", "y")],
                true,
                &card(),
            )
            .unwrap();
        assert_eq!(got.len(), 1);
        // 客户端拿旧世代来读：从头给
        assert_eq!(j.read(Some(&g1), 5, 10)["reset"], true);
    }

    #[test]
    fn both_files_broken_rebuilds_and_takes_what_is_on_the_device_as_baseline() {
        let d = dir();
        let p = d.0.join("sms-journal.json");
        let mut j = ready(&p);
        j.ingest(&[sms(1, "a", "1", "x")], false, &card()).unwrap();
        j.ingest(&[sms(2, "b", "2", "y")], false, &card()).unwrap();
        drop(j);
        std::fs::write(&p, b"broken").unwrap();
        std::fs::write(with_suffix(&p, ".prev"), b"broken too").unwrap();
        let mut j = Journal::open(p.clone());
        assert!(j.incidents().iter().any(|i| i.what.contains("重建")));
        j.ingest(
            &[sms(1, "a", "1", "x"), sms(2, "b", "2", "y")],
            true,
            &card(),
        )
        .unwrap();
        let v = j.read(None, 0, 10);
        assert_eq!(v["entries"].as_array().unwrap().len(), 2);
        assert!(
            v["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["push"] == false && e["sim"].is_null())
        );
    }

    #[test]
    fn a_failed_save_takes_nothing_in() {
        let d = dir();
        let p = d.0.join("sub").join("sms-journal.json");
        let mut j = ready(&p);
        // 目录换成只读：写不进去
        use std::os::unix::fs::PermissionsExt;
        let sub = d.0.join("sub");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o500)).unwrap();
        let r = j.ingest(&[sms(1, "a", "1", "x")], false, &card());
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).unwrap();
        if unsafe { libc::geteuid() } == 0 {
            // root 不受目录权限限制：这种环境下测不出来
            return;
        }
        assert!(r.is_err());
        assert_eq!(j.last_seq(), 0);
        assert_eq!(
            j.ingest(&[sms(1, "a", "1", "x")], false, &card()).unwrap(),
            vec![1]
        );
    }

    #[test]
    fn acks_and_the_cap_drop_confirmed_ones_first() {
        let d = dir();
        let p = d.0.join("sms-journal.json");
        let mut j = ready(&p);
        let batch: Vec<Value> = (0..(MAX_ENTRIES as u64 + 5))
            .map(|i| sms(i, "s", &i.to_string(), "t"))
            .collect();
        let g = j.generation().to_owned();
        j.ingest(&batch[..10], false, &card()).unwrap();
        assert!(j.ack(&g, 10).unwrap());
        assert!(!j.ack("other", 10).unwrap());
        j.ingest(&batch[10..], false, &card()).unwrap();
        assert_eq!(j.data.entries.len(), MAX_ENTRIES);
        // 先清的是已确认的 10 条之中的 5 条：没有丢未确认的
        assert_eq!(j.data.dropped_unacked, 0);
        assert_eq!(j.data.entries[0].seq, 6);
        // 重开之后还在、确认还在
        let j2 = Journal::open(p.clone());
        assert_eq!(j2.data.acked, 10);
        assert_eq!(j2.read(Some(&g), 10, 1)["entries"][0]["seq"], 11);
    }

    #[test]
    fn the_file_is_0600_and_holds_no_iccid() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir();
        let p = d.0.join("sms-journal.json");
        let mut j = ready(&p);
        j.ingest(&[sms(1, "a", "1", "x")], false, &card()).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!std::fs::read_to_string(&p).unwrap().contains("8986001"));
    }

    #[test]
    fn sim_keys_are_stable_and_salted() {
        assert_eq!(sim_key(b"s1", "8986"), sim_key(b"s1", "8986"));
        assert_ne!(sim_key(b"s1", "8986"), sim_key(b"s2", "8986"));
        assert_eq!(sim_key(b"s1", "8986").len(), 32);
        let d = dir();
        let salt = d.0.join("sim-salt");
        let a = load_salt(&salt);
        assert_eq!(load_salt(&salt), a, "the salt survives a restart");
    }

    #[test]
    fn ucs2_text_decodes_with_surrogate_pairs_and_plain_text_stays() {
        assert_eq!(text_of("4F60597D"), "你好");
        assert_eq!(text_of("D83DDE00"), "😀");
        assert_eq!(text_of("hello"), "hello");
    }
}
