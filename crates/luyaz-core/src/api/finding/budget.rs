//! Persist only a claimed probe budget, before packets. No per-sample disk writes.
use super::RECHECK_SECS;
use fs2::FileExt;
use luyaz_api::clock::BootTime;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    last: BootTime,
}

pub struct Budget {
    path: PathBuf,
    last: Option<BootTime>,
    quarantined: Option<Record>,
}

impl Budget {
    pub fn load(path: PathBuf, now: Option<&BootTime>) -> Self {
        let saved = Self::read(&path);
        let last = now.map(|now| {
            match saved {
                Some(ref r) if r.version == 1 && now.elapsed_since(&r.last).is_some() => {
                    r.last.clone()
                }
                // Across device boots, wait until this boot is ten minutes old.
                Some(ref r)
                    if r.version == 1
                        && !r.last.boot_id.is_empty()
                        && r.last.boot_id != now.boot_id =>
                {
                    BootTime {
                        boot_id: now.boot_id.clone(),
                        seconds: 0,
                    }
                }
                // Missing/legacy/broken/future state cannot prove that no probe ran before a crash.
                _ => now.clone(),
            }
        });
        let quarantined = saved.filter(|r| {
            r.version == 1
                && now.is_some_and(|now| {
                    now.boot_id == r.last.boot_id && now.elapsed_since(&r.last).is_none()
                })
        });
        Self {
            path,
            last,
            quarantined,
        }
    }

    fn read(path: &std::path::Path) -> Option<Record> {
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }

    pub fn due(&self, now: Option<&BootTime>) -> bool {
        now.zip(self.last.as_ref())
            .and_then(|(n, l)| n.elapsed_since(l))
            .is_some_and(|age| age >= RECHECK_SECS)
    }

    pub fn reserve(&mut self, now: Option<&BootTime>) -> bool {
        let Some(now) = now.filter(|_| self.due(now)) else {
            return false;
        };
        // Bound retries even if the filesystem is broken. A restart remains conservative.
        self.last = Some(now.clone());
        self.save(now).is_ok()
    }

    fn save(&mut self, now: &BootTime) -> io::Result<()> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("no budget directory"))?;
        fs::create_dir_all(dir)?;
        let lock_path = self.path.with_extension("lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock.try_lock_exclusive()?;
        // Another core may have claimed since this instance started. Never overwrite its budget.
        if let Some(r) = Self::read(&self.path).filter(|r| r.version == 1)
            && r.last.boot_id == now.boot_id
            && match now.elapsed_since(&r.last) {
                Some(age) => age < RECHECK_SECS,
                None => self.quarantined.as_ref() != Some(&r),
            }
        {
            if now.elapsed_since(&r.last).is_some() {
                self.last = Some(r.last);
            }
            return Err(io::Error::other("probe budget already claimed"));
        }
        let record = Record {
            version: 1,
            last: now.clone(),
        };
        let bytes = serde_json::to_vec(&record)?;
        let tmp = self.path.with_extension("tmp");
        let mut file = File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(tmp, &self.path)?;
        // Unlike best-effort telemetry, a budget must be durable before sending packets.
        File::open(dir)?.sync_all()?;
        self.quarantined = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
