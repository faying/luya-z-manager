//! dfs — notice when radar (or anything else) takes the 5 GHz AP off its channel, and bring 5 GHz
//! back on a channel radar cannot take (design: manager docs/designs/wifi-dfs-guard.md).
//!
//! What we know about the device (2026-10-30, both U60s): `wifi1` is `channel=0` (auto) with
//! `htmode=EHT160` and a channel list that includes 52–64. Auto on 36 at 160 MHz spans 36–64, so
//! the AP sits on DFS channels most of the time even though it reads "36". The driver has radar
//! detection (`dfs_bangradar`, NOL) and shortens CAC to 3 s; what it does on a real radar hit —
//! move (CSA), narrow, or just stop the AP (`disableDfsCS 1`) — has never been seen.
//!
//! So nothing here matches driver log lines for the decision. The truth is the outcome:
//!
//!  * 5 GHz is wanted on (uci) but wlan2's hostapd does not answer ENABLED, or wlan2 is gone;
//!  * or the channel / width it serves on changed since the last look.
//!
//! Kernel lines that mention radar/DFS/CSA/NOL only make the core look sooner ([`kmsg_hint`]).
//!
//! Down for [`GRACE_S`] with auto on: the **fallback** writes `wifi1.channel=36` and the same
//! htmode family at 80 MHz (36–48: non-DFS in every region; 149–165 is not Wi-Fi everywhere and
//! cannot do 160), reloads, and keeps an [`Override`] with what was there before. After
//! [`RESTORE_S`] (the radar block period, NOL, is 30 min) and only with no Wi-Fi station and no
//! call, the original is put back. The override is on flash, so a restarted core still restores.
//! Still down [`ALERT_S`] after it went down: one alert.
//!
//! Pure decisions in [`Guard::step`]; the IO (observe, write under the Wi-Fi lock) below it.

use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use crate::{output_within, WifiLock, CMD_TIMEOUT, HOSTAPD_CTRL, IFACE_5G};

/// Down this long before the fallback: hostapd restarting after a reload, or the driver's own
/// channel switch, settles well inside it.
pub const GRACE_S: u64 = 90;
/// Down this long (fallback or not) before one alert.
pub const ALERT_S: u64 = 300;
/// The fallback is tried again at most this often while 5 GHz stays down.
pub const RETRY_S: u64 = 600;
/// Radar blocks a channel for 30 min (NOL); put the original back no earlier.
pub const RESTORE_S: u64 = 1800;
/// The channel the fallback uses: 36–48 is non-DFS everywhere.
pub const FALLBACK_CHANNEL: &str = "36";
pub const CHANNEL_KEY: &str = "wireless.wifi1.channel";
pub const HTMODE_KEY: &str = "wireless.wifi1.htmode";

/// What one look found. `chan`/`width` are only meaningful when `up`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Obs {
    /// uci says the main 5 GHz AP should broadcast
    pub want_5g: bool,
    /// wlan2's hostapd answers ENABLED
    pub up: bool,
    pub chan: Option<u32>,
    /// MHz
    pub width: Option<u32>,
    /// stations on every Wi-Fi AP interface; `None` unreadable (counts as somebody there)
    pub stations: Option<u32>,
    pub in_call: bool,
}

/// What was there before the fallback, and when the fallback was made (unix seconds).
#[derive(Clone, Debug, PartialEq)]
pub struct Override {
    pub channel: String,
    pub htmode: String,
    pub at: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// serving on another channel than last seen (a channel switch)
    Moved { from: u32, to: u32 },
    /// same channel, narrower
    Narrowed { from: u32, to: u32 },
    /// wanted on, not serving
    Down,
    /// serving again after `Down`
    Back,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// write the fallback channel; `htmode` is what to write
    Fallback { htmode: String },
    /// put the override back; `reload` only while 5 GHz is wanted on (else uci only)
    Restore { ov: Override, reload: bool },
}

#[derive(Debug, Default, PartialEq)]
pub struct Step {
    pub events: Vec<Event>,
    pub action: Option<Action>,
    /// raise the "5 GHz down" alert now (once per outage)
    pub alert: bool,
}

/// The guard's memory. Only `ov` is persisted ([`Guard::to_json`]); the rest starts over.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Guard {
    pub down_since: Option<u64>,
    /// (channel, width) last served on
    pub last: Option<(u32, u32)>,
    pub ov: Option<Override>,
    pub alerted: bool,
    pub tried_at: Option<u64>,
}

/// The 20 MHz channels a 5 GHz channel at `width` covers (the standard 40/80/160 blocks).
pub fn span(chan: u32, width: u32) -> Vec<u32> {
    let block = match width {
        w if w >= 160 => 8,
        w if w >= 80 => 4,
        w if w >= 40 => 2,
        _ => 1,
    };
    // blocks are aligned from 36 (UNII-1..2C) and 149 (UNII-3)
    let base = if chan >= 149 { 149 } else { 36 };
    if chan < base {
        return vec![chan];
    }
    let first = base + (chan - base) / (4 * block) * (4 * block);
    (0..block).map(|i| first + 4 * i).collect()
}

/// DFS channels (UNII-2A, UNII-2C): 52–144.
pub fn is_dfs(chan: u32) -> bool {
    (52..=144).contains(&chan)
}

/// Does serving on `chan` at `width` touch a DFS channel?
pub fn on_dfs(chan: u32, width: u32) -> bool {
    span(chan, width).into_iter().any(is_dfs)
}

/// The same htmode family at 80 MHz: `EHT160` → `EHT80`, `HE160` → `HE80`. Already ≤ 80 stays.
pub fn htmode_80(htmode: &str) -> String {
    let digits = htmode.trim_start_matches(|c: char| !c.is_ascii_digit());
    let prefix = &htmode[..htmode.len() - digits.len()];
    match digits.parse::<u32>() {
        Ok(w) if w > 80 => format!("{prefix}80"),
        Ok(_) => htmode.to_string(),
        Err(_) => "EHT80".into(),
    }
}

impl Guard {
    /// One look. `htmode` is uci's `wifi1.htmode` now; `auto` the owner's switch.
    pub fn step(&mut self, o: &Obs, htmode: &str, now: u64, auto: bool) -> Step {
        let mut s = Step::default();
        let quiet = o.stations == Some(0) && !o.in_call;
        let due = |ov: &Override| now >= ov.at.saturating_add(RESTORE_S);
        if !o.want_5g {
            // switched off (scenario, owner): not an outage. An override still due goes back to uci
            // only; the next "on" reload picks it up.
            self.down_since = None;
            self.alerted = false;
            self.tried_at = None;
            if let Some(ov) = self.ov.clone().filter(|ov| due(ov)) {
                s.action = Some(Action::Restore { ov, reload: false });
            }
            return s;
        }
        if o.up {
            if self.down_since.take().is_some() {
                s.events.push(Event::Back);
            }
            self.alerted = false;
            self.tried_at = None;
            if let (Some(c), Some(w)) = (o.chan, o.width) {
                match self.last {
                    Some((lc, _)) if lc != c => s.events.push(Event::Moved { from: lc, to: c }),
                    Some((_, lw)) if w < lw => s.events.push(Event::Narrowed { from: lw, to: w }),
                    _ => {}
                }
                self.last = Some((c, w));
            }
            if let Some(ov) = self.ov.clone().filter(|ov| quiet && due(ov)) {
                s.action = Some(Action::Restore { ov, reload: true });
            }
            return s;
        }
        let since = *self.down_since.get_or_insert_with(|| {
            s.events.push(Event::Down);
            now
        });
        let down_for = now.saturating_sub(since);
        // the fallback only helps when the last channel could have been hit by radar (or is unknown)
        let could_be_radar = self.last.is_none_or(|(c, w)| on_dfs(c, w));
        let may_try = self.tried_at.is_none_or(|t| now >= t.saturating_add(RETRY_S));
        if auto && self.ov.is_none() && could_be_radar && down_for >= GRACE_S && may_try {
            self.tried_at = Some(now);
            s.action = Some(Action::Fallback { htmode: htmode_80(htmode) });
        }
        if down_for >= ALERT_S && !self.alerted {
            self.alerted = true;
            s.alert = true;
        }
        s
    }

    /// The fallback was written: remember what to put back.
    pub fn fell_back(&mut self, ov: Override) {
        self.ov = Some(ov);
        // the new channel is non-DFS: what was last seen no longer says anything
        self.last = None;
    }

    /// The action could not run (the Wi-Fi lock was busy, the write failed): try again next look.
    pub fn deferred(&mut self) {
        self.tried_at = None;
    }

    pub fn restored(&mut self) {
        self.ov = None;
        self.last = None;
    }

    pub fn to_json(&self) -> Value {
        match &self.ov {
            Some(ov) => json!({"override": {"channel": ov.channel, "htmode": ov.htmode, "at": ov.at}}),
            None => json!({}),
        }
    }

    pub fn from_json(v: &Value) -> Guard {
        let o = &v["override"];
        let ov = match (o["channel"].as_str(), o["htmode"].as_str(), o["at"].as_u64()) {
            (Some(c), Some(h), Some(at)) => Some(Override { channel: c.into(), htmode: h.into(), at }),
            _ => None,
        };
        Guard { ov, ..Default::default() }
    }
}

/// A kernel line worth an early look (never a decision on its own).
pub fn kmsg_hint(text: &str) -> bool {
    let l = text.to_ascii_lowercase();
    ["radar", "dfs", "csa", "nol_", "chan_switch", "channel switch"]
        .iter()
        .any(|p| l.contains(p))
        // the CAC timeout line at every Wi-Fi start is not news
        && !l.contains("cac_timeout")
        && !l.contains("allowing suspend")
}

// ── IO ──────────────────────────────────────────────────────────────────────

/// The writes the fallback needs, sent the caller's way (the core: its own `/control`
/// `wifi.apply`, which also checks the AP layout). Only called under the Wi-Fi lock.
pub trait RadioHost {
    fn uci_get(&self, key: &str) -> Option<String>;
    /// Set `wifi1.channel` and `wifi1.htmode`; `reload` as `wifi.apply`'s. `Ok(Some(e))` written but
    /// the reload reported `e`.
    fn write_radio(&self, channel: &str, htmode: &str, reload: bool) -> Result<Option<String>, String>;
}

/// `iw dev <iface> info` → (channel, width MHz): `channel 36 (5180 MHz), width: 160 MHz, …`.
pub fn parse_iw_info(text: &str) -> (Option<u32>, Option<u32>) {
    let Some(l) = text.lines().map(str::trim).find(|l| l.starts_with("channel ")) else {
        return (None, None);
    };
    let chan = l["channel ".len()..].split_whitespace().next().and_then(|n| n.parse().ok());
    let width = l
        .split_once("width: ")
        .and_then(|(_, r)| r.split_whitespace().next())
        .and_then(|n| n.parse().ok());
    (chan, width)
}

/// `iw dev` → the AP interfaces' names.
pub fn parse_ap_ifaces(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for l in text.lines().map(str::trim) {
        if let Some(n) = l.strip_prefix("Interface ") {
            cur = Some(n.trim().to_string());
        } else if l == "type AP" {
            out.extend(cur.take());
        }
    }
    out
}

fn run(args: &[&str]) -> Option<String> {
    output_within(Command::new(args[0]).args(&args[1..]), CMD_TIMEOUT)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// wlan2: (ENABLED, channel, width). Read-only and cheap: one hostapd_cli, one iw.
pub fn observe_5g() -> (bool, Option<u32>, Option<u32>) {
    let up = run(&["hostapd_cli", "-i", IFACE_5G, "-p", HOSTAPD_CTRL, "status"])
        .is_some_and(|t| t.contains("state=ENABLED"));
    if !up {
        return (false, None, None);
    }
    let (c, w) = run(&["iw", "dev", IFACE_5G, "info"]).map(|t| parse_iw_info(&t)).unwrap_or_default();
    (true, c, w)
}

/// Stations on every AP interface; `None` when any could not be read.
pub fn stations() -> Option<u32> {
    let ifs = parse_ap_ifaces(&run(&["iw", "dev"])?);
    let mut n = 0;
    for i in ifs {
        n += run(&["iw", "dev", &i, "station", "dump"])?.lines().filter(|l| l.starts_with("Station ")).count() as u32;
    }
    Some(n)
}

/// Write the fallback and wait for wlan2 to serve (≤ [`crate::ON_DEADLINE`]). The override is
/// returned once written, even when the AP is not up by the deadline (it is still what to put
/// back).
pub fn fall_back(h: &dyn RadioHost, _lk: &WifiLock, htmode: &str, now: u64) -> Result<(Override, bool), String> {
    let ov = Override {
        channel: h.uci_get(CHANNEL_KEY).unwrap_or_else(|| "0".into()),
        htmode: h.uci_get(HTMODE_KEY).ok_or("wifi1.htmode unreadable")?,
        at: now,
    };
    if let Some(e) = h.write_radio(FALLBACK_CHANNEL, htmode, true)? {
        eprintln!("[wifi-dfs] fallback reload: {e}");
    }
    let up = wait_up(crate::ON_DEADLINE);
    Ok((ov, up))
}

/// Put the override back.
pub fn restore(h: &dyn RadioHost, _lk: &WifiLock, ov: &Override, reload: bool) -> Result<bool, String> {
    if let Some(e) = h.write_radio(&ov.channel, &ov.htmode, reload)? {
        eprintln!("[wifi-dfs] restore reload: {e}");
    }
    Ok(!reload || wait_up(crate::ON_DEADLINE))
}

fn wait_up(deadline: Duration) -> bool {
    crate::poll_until(deadline, || observe_5g().0)
}

#[cfg(test)]
mod tests;
