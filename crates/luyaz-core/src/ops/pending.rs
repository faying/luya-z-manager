//! 进行中事务的落盘（write-op-layer.md「重启续退」、D31）：`<目录>/pending.json`，原子写
//! （临时文件 → fsync → rename → fsync 目录）。另有触屏/agent 应急直写前留下的 `takeover` 标记（D18）。
//! 目录写不了时只记一行日志，datad 照常服务（不落盘就没有重启续退）。

use super::txn::Txn;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

const PENDING: &str = "pending.json";
const LAST: &str = "last.json";
const TAKEOVER: &str = "takeover";
const NOTICE: &str = "notice.json";

/// 点过「知道了」的提示（DD18）：`{"acked":["rollback_on"]}`。
#[derive(Debug, Default, Serialize, Deserialize)]
struct Notices {
    #[serde(default)]
    acked: Vec<String>,
}

/// 最近结束的事务和有没有人点过「知道了」（E4 T13，V2-37）：datad 重启后结果还在。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Last {
    pub txn: Txn,
    pub acked: bool,
}

pub struct Store {
    dir: Option<PathBuf>,
}

impl Store {
    /// `None` = 不落盘（测试或目录建不了）。
    pub fn open(dir: Option<PathBuf>) -> Self {
        let dir = dir.and_then(|d| match fs::create_dir_all(&d) {
            Ok(()) => Some(d),
            Err(e) => {
                eprintln!(
                    "ops: cannot use {}: {e}; changes will not survive a restart",
                    d.display()
                );
                None
            }
        });
        Self { dir }
    }

    pub fn save(&self, t: &Txn) {
        let Some(dir) = &self.dir else { return };
        if let Err(e) = write_atomic(
            dir,
            PENDING,
            &serde_json::to_vec(t).expect("txn serializes"),
        ) {
            eprintln!("ops: cannot save {PENDING}: {e}");
        }
    }

    pub fn clear(&self) {
        let Some(dir) = &self.dir else { return };
        match fs::remove_file(dir.join(PENDING)) {
            Ok(()) => sync_dir(dir),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("ops: cannot remove {PENDING}: {e}"),
        }
    }

    /// 读落盘的事务；读不懂的改名成 `pending.json.bad` 留着查，当作没有。
    pub fn load(&self) -> Option<Txn> {
        let dir = self.dir.as_ref()?;
        let path = dir.join(PENDING);
        let bytes = fs::read(&path).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("ops: unreadable {PENDING} ({e}), set aside");
                let _ = fs::rename(&path, dir.join("pending.json.bad"));
                None
            }
        }
    }

    pub fn save_last(&self, last: &Last) {
        let Some(dir) = &self.dir else { return };
        if let Err(e) = write_atomic(
            dir,
            LAST,
            &serde_json::to_vec(last).expect("last serializes"),
        ) {
            eprintln!("ops: cannot save {LAST}: {e}");
        }
    }

    /// 读不懂就当没有（只是界面上的一行结果）。
    pub fn load_last(&self) -> Option<Last> {
        let bytes = fs::read(self.dir.as_ref()?.join(LAST)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// 这个提示有没有人点过「知道了」。不落盘或读不懂时当没点过。
    pub fn notice_acked(&self, notice: &str) -> bool {
        self.load_notices().acked.iter().any(|n| n == notice)
    }

    fn load_notices(&self) -> Notices {
        self.dir
            .as_ref()
            .and_then(|d| fs::read(d.join(NOTICE)).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save_notice_ack(&self, notice: &str) {
        let Some(dir) = &self.dir else { return };
        let mut n = self.load_notices();
        if !n.acked.iter().any(|a| a == notice) {
            n.acked.push(notice.to_owned());
        }
        if let Err(e) = write_atomic(
            dir,
            NOTICE,
            &serde_json::to_vec(&n).expect("notices serialize"),
        ) {
            eprintln!("ops: cannot save {NOTICE}: {e}");
        }
    }

    /// 有 `takeover` 标记就删掉并返回 true（D18：应急直写过，落盘的事务要放弃）。
    pub fn take_takeover(&self) -> bool {
        let Some(dir) = &self.dir else { return false };
        match fs::remove_file(dir.join(TAKEOVER)) {
            Ok(()) => {
                sync_dir(dir);
                true
            }
            Err(_) => false,
        }
    }
}

pub(super) fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!(".{name}.tmp"));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dir.join(name))?;
    sync_dir(dir);
    Ok(())
}

fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}
