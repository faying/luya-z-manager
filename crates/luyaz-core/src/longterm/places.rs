//! The experience library (manager docs/designs/u60-radio.md §7, D6, D12; review A3, A4, O4):
//! per serving network (MCC-MNC), per radio-mode preference, how long the device was there, how
//! often the modem crashed and the link dropped, and per band how often it crashed — so that in a
//! country where 5G kept crashing last time, the device can say so and suggest 4G + 3G.
//!
//! - Folded online by the long-term thread (its events are kept 30 days; this is kept a year):
//!   each check adds the boot seconds since the previous one to (network, preference); a `link`
//!   down counts a drop there.
//! - A crash is the next boot's reason code (1134, 1132: the modem took the device down;
//!   LEDGER.md's classification, as doctor's); it belongs to where the previous boot last was, so
//!   that key is on flash within one check of changing. 1182 can be a crash or a `reboot`: kept
//!   as a raw count that no rule reads. Each boot is counted once (the backfill, a restarted core).
//! - Preference aliases: the automatic ones (`WL_AND_5G`, `TCHGWL_5G`, `NETWORK_auto`) are one
//!   bucket `auto`; none known is `unknown`, counted but never part of the rule.
//! - Backfill once: the guard's ledger (`/data/ledger`, 9-28 until the sentinel took over), its
//!   `hour`, `boot`, `net_change`, `ssr` and `link` lines, the preference carried across boots.
//! - No cell id, no PCI: bands only (D6). The file stays on the device.
//! - Advice only, never a write: the `advice` block for where the device is now.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

pub const FILE: &str = "places.json";
/// The guard's ledger, read once for the backfill.
pub const LEDGER: &str = "/data/ledger";
/// Reason codes of a modem crash that took the device down.
pub const CRASH_CODES: [i64; 2] = [1134, 1132];
/// The default reason code: a crash or a plain `reboot`, not told apart.
pub const UNCLEAR_CODE: i64 = 1182;
/// D12: the preference crashed at least this often over at least this long …
pub const CRASHES_MIN: u32 = 2;
pub const EXPOSED_MIN_S: i64 = 2 * 3600;
/// … and 4G + 3G ran at least this long on the same network without a crash.
pub const SAFE_MIN_S: i64 = 3 * 3600;
pub const SAFE: &str = "WCDMA_AND_LTE";
/// Exposure is written at most this often (a key change or a crash at once).
const SAVE_EVERY_S: i64 = 600;
/// A place not seen for a year goes.
const KEEP_S: i64 = 365 * 86_400;
const BOOTS_KEEP: usize = 200;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stat {
    pub secs: i64,
    pub crashes: u32,
    pub drops: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Place {
    /// preference bucket → counts
    pub prefs: BTreeMap<String, Stat>,
    /// band → crashes
    pub bands: BTreeMap<String, u32>,
    /// reboots with the default code (crash or `reboot`, unknown)
    pub unclear: u32,
    /// wall clock of the last time here (when trusted)
    pub last_t: Option<i64>,
}

/// Where the device is: network, preference bucket, band (of one boot).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Key {
    pub boot: String,
    pub net: String,
    pub pref: String,
    pub band: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Places {
    pub v: u32,
    pub backfilled: bool,
    pub places: BTreeMap<String, Place>,
    /// This boot (8 characters) and where it last was.
    pub boot: String,
    pub last: Option<Key>,
    /// Where the previous boot last was: its crash, when the reason code comes in.
    pub prev: Option<Key>,
    /// Boots whose reason code was taken.
    pub counted: Vec<String>,
    /// (boot, boot seconds) of the last exposure step.
    at: Option<(String, i64)>,
    #[serde(skip)]
    dirty_now: bool,
    #[serde(skip)]
    dirty: bool,
    #[serde(skip)]
    saved_at: Option<i64>,
}

/// What the long-term fold knows now.
#[derive(Clone, Debug, Default)]
pub struct Now {
    pub net: Option<String>,
    pub net_select: Option<String>,
    pub band: Option<String>,
}

/// The preference bucket: the automatic modes are one, none known is `unknown`.
pub fn bucket(sel: Option<&str>) -> String {
    match sel.map(str::trim) {
        None | Some("") | Some("-") => "unknown".into(),
        Some("WL_AND_5G" | "TCHGWL_5G" | "NETWORK_auto") => "auto".into(),
        Some(s) => s.chars().take(24).collect(),
    }
}

/// A band as kept: letters and digits only, short (`n78`, `B3`); anything else is not a band.
pub fn band(b: Option<&str>) -> Option<String> {
    let b = b?.trim();
    (!b.is_empty() && b != "-" && b.len() <= 8 && b.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| b.to_string())
}

impl Places {
    pub fn load(dir: &Path) -> Places {
        std::fs::read_to_string(dir.join(FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn stat(&mut self, net: &str, pref: &str) -> &mut Stat {
        self.places
            .entry(net.to_string())
            .or_default()
            .prefs
            .entry(pref.to_string())
            .or_default()
    }

    /// A reboot's reason code, for the boot it starts (charged to where the one before was).
    fn reason(&mut self, boot: &str, code: i64, prev: Option<&Key>) {
        if self.counted.iter().any(|b| b == boot) {
            return;
        }
        self.counted.push(boot.to_string());
        if self.counted.len() > BOOTS_KEEP {
            let n = self.counted.len() - BOOTS_KEEP;
            self.counted.drain(..n);
        }
        let Some(k) = prev.cloned() else { return };
        if CRASH_CODES.contains(&code) {
            self.stat(&k.net, &k.pref).crashes += 1;
            if let Some(b) = &k.band {
                *self
                    .places
                    .entry(k.net.clone())
                    .or_default()
                    .bands
                    .entry(b.clone())
                    .or_default() += 1;
            }
        } else if code == UNCLEAR_CODE {
            self.places.entry(k.net.clone()).or_default().unclear += 1;
        }
        self.dirty_now = true;
    }

    /// One round of the long-term thread: `boot` (8 characters), boot seconds, wall clock when
    /// trusted, what the fold knows now, the events it produced this round, a check or not.
    pub fn step(
        &mut self,
        boot: &str,
        boottime: i64,
        wall: Option<i64>,
        now: &Now,
        events: &[Value],
        due: bool,
    ) {
        if self.boot != boot {
            if let Some(k) = self.last.take()
                && k.boot != boot
            {
                self.prev = Some(k);
            }
            self.boot = boot.to_string();
            self.at = None;
            self.dirty_now = true;
        }
        let key = now.net.as_ref().map(|net| Key {
            boot: boot.to_string(),
            net: net.clone(),
            pref: bucket(now.net_select.as_deref()),
            band: band(now.band.as_deref()),
        });
        for e in events {
            match e["k"].as_str() {
                Some("boot") => {
                    let b = super::boot8(e["boot_id"].as_str().unwrap_or_default());
                    if b == boot
                        && let Some(code) = e["code"].as_i64()
                    {
                        let prev = self.prev.clone();
                        self.reason(boot, code, prev.as_ref());
                    }
                }
                Some("link") if e["state"] == "down" => {
                    if let Some(k) = &key {
                        self.stat(&k.net, &k.pref).drops += 1;
                        self.dirty = true;
                    }
                }
                _ => {}
            }
        }
        if !due {
            return;
        }
        if let (Some((b, t)), Some(k)) = (&self.at, &key)
            && b == boot
        {
            let dt = boottime - t;
            if dt > 0 && dt < 3600 {
                self.stat(&k.net, &k.pref).secs += dt;
                if let Some(w) = wall {
                    self.places.entry(k.net.clone()).or_default().last_t = Some(w);
                }
                self.dirty = true;
            }
        }
        self.at = Some((boot.to_string(), boottime));
        if let Some(k) = key
            && self.last.as_ref() != Some(&k)
        {
            self.last = Some(k);
            self.dirty_now = true;
        }
        if let Some(w) = wall {
            self.places
                .retain(|_, p| p.last_t.is_none_or(|t| w - t < KEEP_S));
        }
    }

    /// Whether to write now: a key or a crash at once, exposure every 10 minutes.
    pub fn save_due(&self, boottime: i64) -> bool {
        self.dirty_now || (self.dirty && self.saved_at.is_none_or(|t| boottime - t >= SAVE_EVERY_S))
    }

    /// Written whole (tmp + fsync + rename).
    pub fn save(&mut self, dir: &Path, boottime: i64) -> std::io::Result<()> {
        use std::io::Write;
        self.v = 1;
        let text = serde_json::to_string(self).map_err(std::io::Error::other)?;
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE}.tmp"));
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, dir.join(FILE))?;
        self.dirty_now = false;
        self.dirty = false;
        self.saved_at = Some(boottime);
        Ok(())
    }

    /// The backfill from the guard's ledger: its segments in order, each a boot's
    /// (`boot-<seq>-<boot8>-<part>.jsonl`). Builds the counts whole, then takes them in one go.
    pub fn backfill(&mut self, ledger: &Path) {
        let mut b = Places::default();
        let (mut net, mut pref, mut bnd): (Option<String>, String, Option<String>) =
            (None, "unknown".into(), None);
        let mut cur = String::new();
        let mut prev: Option<Key> = None;
        for f in crate::scorecard::replay::segments(ledger) {
            let name = f
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let Some(boot8) = name.split('-').nth(2).map(str::to_string) else {
                continue;
            };
            if boot8 != cur {
                prev = net.clone().map(|n| Key {
                    boot: cur.clone(),
                    net: n,
                    pref: pref.clone(),
                    band: bnd.clone(),
                });
                cur = boot8.clone();
                bnd = None;
            }
            let Ok(text) = std::fs::read_to_string(&f) else {
                continue;
            };
            for l in text.lines() {
                let Ok(e) = serde_json::from_str::<Value>(l) else {
                    continue;
                };
                match e["k"].as_str() {
                    Some("boot") => {
                        if let Some(s) = e["net_select"].as_str().filter(|s| !s.is_empty()) {
                            pref = bucket(Some(s));
                        }
                        if let Some(code) = e["code"].as_i64() {
                            b.reason(&boot8, code, prev.as_ref());
                        }
                    }
                    Some("net_change") => {
                        if let Some(n) = e["net"].as_str().filter(|s| !s.is_empty() && *s != "-") {
                            net = Some(n.to_string());
                        }
                        if let Some(s) = e["net_select"].as_str().filter(|s| !s.is_empty()) {
                            pref = bucket(Some(s));
                        }
                    }
                    Some("hour") => {
                        if let Some(n) = e["net"].as_str().filter(|s| !s.is_empty() && *s != "-") {
                            net = Some(n.to_string());
                        }
                        let secs =
                            e["to"].as_f64().unwrap_or(0.0) - e["from"].as_f64().unwrap_or(0.0);
                        if let Some(n) = &net
                            && secs > 0.0
                        {
                            b.stat(n, &pref).secs += secs as i64;
                            if let Some(t) = e["tt"].as_i64() {
                                b.places.entry(n.clone()).or_default().last_t = Some(t);
                            }
                        }
                    }
                    Some("ssr") => {
                        if let Some(x) =
                            band(e["nrband"].as_str()).or_else(|| band(e["band"].as_str()))
                        {
                            bnd = Some(x);
                        }
                    }
                    Some("link") if e["state"] == "down" => {
                        if let Some(n) = &net {
                            b.stat(n, &pref).drops += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
        // the live fold's own boots stay counted; the ledger's join them
        for x in std::mem::take(&mut self.counted) {
            if !b.counted.contains(&x) {
                b.counted.push(x);
            }
        }
        for (n, p) in b.places {
            let q = self.places.entry(n).or_default();
            for (k, s) in p.prefs {
                let t = q.prefs.entry(k).or_default();
                t.secs += s.secs;
                t.crashes += s.crashes;
                t.drops += s.drops;
            }
            for (k, c) in p.bands {
                *q.bands.entry(k).or_default() += c;
            }
            q.unclear += p.unclear;
            q.last_t = q.last_t.max(p.last_t);
        }
        self.counted = b.counted;
        if self.last.is_none() {
            self.last = net.map(|n| Key {
                boot: cur,
                net: n,
                pref,
                band: bnd,
            });
        }
        self.backfilled = true;
        self.dirty_now = true;
    }

    /// The `advice` block for where the device is now.
    pub fn advice(&self, now: &Now) -> Value {
        let word = |p: &str| match p {
            "auto" => ("自动".to_string(), "Auto".to_string()),
            "unknown" => ("不明".to_string(), "Unknown".to_string()),
            p => crate::project::screen::net_select_word(p),
        };
        let hours = |s: i64| format!("{}.{}", s / 3600, s % 3600 / 360);
        let pref = bucket(now.net_select.as_deref());
        let mut v = json!({
            "net": now.net.clone().unwrap_or_default(),
            "pref": pref,
            "verdict": "unknown",
            "text": "还不知道在哪个网络",
            "text_en": "Network not known yet",
            "prefs": {},
            "bands": {},
            "unclear": 0,
            "backfilled": self.backfilled,
        });
        let Some(net) = &now.net else { return v };
        let p = self.places.get(net).cloned().unwrap_or_default();
        v["prefs"] = json!(p.prefs);
        v["bands"] = json!(p.bands);
        v["unclear"] = json!(p.unclear);
        let crashes: u32 = p.prefs.values().map(|s| s.crashes).sum();
        let get = |k: &str| p.prefs.get(k).cloned().unwrap_or_default();
        let (cur, safe) = (get(&pref), get(SAFE));
        let (w, w_en) = word(&pref);
        if crashes == 0 {
            v["verdict"] = json!("quiet");
            v["text"] = json!("这里没崩过");
            v["text_en"] = json!("No modem crash here so far");
        } else if pref != SAFE
            && pref != "unknown"
            && cur.crashes >= CRASHES_MIN
            && cur.secs >= EXPOSED_MIN_S
            && safe.secs >= SAFE_MIN_S
            && safe.crashes == 0
        {
            v["verdict"] = json!("suggest");
            v["text"] = json!(format!(
                "这里上次用「{w}」崩了 {} 次（{} 小时）；4G + 3G 用了 {} 小时没崩，要不要切 4G + 3G？",
                cur.crashes,
                hours(cur.secs),
                hours(safe.secs)
            ));
            v["text_en"] = json!(format!(
                "Here \"{w_en}\" crashed {} times in {} h; 4G + 3G ran {} h without a crash. Switch to 4G + 3G?",
                cur.crashes,
                hours(cur.secs),
                hours(safe.secs)
            ));
        } else {
            v["verdict"] = json!("insufficient");
            v["text"] = json!(format!(
                "证据不足：这里崩过 {crashes} 次；「{w}」{} 次 / {} 小时，4G + 3G {} 次 / {} 小时",
                cur.crashes,
                hours(cur.secs),
                safe.crashes,
                hours(safe.secs)
            ));
            v["text_en"] = json!(format!(
                "Not enough evidence: {crashes} crashes here; \"{w_en}\" {} in {} h, 4G + 3G {} in {} h",
                cur.crashes,
                hours(cur.secs),
                safe.crashes,
                hours(safe.secs)
            ));
        }
        v
    }
}

static ADVICE: Mutex<Option<Value>> = Mutex::new(None);

/// The long-term thread's latest `advice` block.
pub fn set_advice(v: Value) {
    *ADVICE.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
}

/// The `advice` block to publish this round (`off` while the long-term layer is off).
pub fn advice_now() -> Value {
    ADVICE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| {
            json!({"net": "", "pref": "unknown", "verdict": "off", "text": "经验库没开",
                "text_en": "Experience library off", "prefs": {}, "bands": {}, "unclear": 0,
                "backfilled": false})
        })
}

#[cfg(test)]
#[path = "places_tests.rs"]
mod tests;
