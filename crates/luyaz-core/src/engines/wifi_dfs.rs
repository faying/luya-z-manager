//! The 5 GHz AP's channel guard (design docs/designs/wifi-dfs-guard.md): radar or anything else
//! taking 5 GHz down is noticed, and with the owner's switch on (default) 5 GHz comes back on a
//! non-DFS channel; the original goes back after the radar block period. The decisions are
//! [`luyaz_wifi::dfs::Guard`]; this is when to look, the writes, the record on flash, the
//! `wifi_dfs` block and the alerts.
//!
//! Looks: every [`LOOK_S`] while 5 GHz is down or overridden, every [`IDLE_S`] otherwise (uci and
//! one `hostapd_cli` + one `iw`; with 5 GHz off, uci only), and at once when a kernel line
//! mentions radar/DFS/CSA ([`poke`], from the crash watcher's kmsg reader).
//!
//! Writes: `wifi.apply` on the core's own `/control` (which checks the AP layout too), under the
//! Wi-Fi lock like every writer of `wireless`. A busy lock defers to the next look.

use luyaz_wifi::dfs::{self, Action, Event, Guard, Obs};
use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const RECORD: &str = "/data/luyaz/core/wifi-dfs.json";
/// Present = the owner turned the automatic fallback off (report and alert only).
pub const OFF_FLAG: &str = "/data/luyaz/core/wifi-dfs-off";
/// The call bridge's file while a call rings or runs (`plug::call::CALLS_FILE`).
const CALLS: &str = "/tmp/luyaz/calls";
const LOOK_S: u64 = 30;
const IDLE_S: u64 = 300;
const LOCK_WAIT: Duration = Duration::from_secs(30);
/// Never texted: the fallback worked, the owner reads it in the inbox.
pub const FALLBACK_KIND: &str = "wifi-5g-fallback";
/// Texted: 5 GHz is still down five minutes later.
pub const DOWN_KIND: &str = "wifi-5g-down";

#[derive(Default)]
struct State {
    guard: Guard,
    obs: Obs,
    auto: bool,
    /// (what, wall) last thing that happened, for the screen and the web
    last: Option<(&'static str, i64)>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| {
        let guard = std::fs::read_to_string(crate::state::host_path(RECORD))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .map(|v| Guard::from_json(&v))
            .unwrap_or_default();
        Mutex::new(State { guard, auto: true, ..Default::default() })
    })
}

fn wake() -> &'static tokio::sync::Notify {
    static N: OnceLock<tokio::sync::Notify> = OnceLock::new();
    N.get_or_init(tokio::sync::Notify::new)
}

/// A kernel record from the crash watcher's reader: look now when it may be about the channel.
pub fn poke(text: &str) {
    if dfs::kmsg_hint(text) {
        wake().notify_one();
    }
}

/// The `wifi_dfs` block.
pub fn report() -> Value {
    let Ok(s) = state().lock() else { return Value::Null };
    let o = &s.obs;
    let ov = s.guard.ov.as_ref();
    let on_dfs = match (o.up, o.chan, o.width) {
        (true, Some(c), Some(w)) => Some(dfs::on_dfs(c, w)),
        _ => None,
    };
    json!({
        "want_5g": o.want_5g,
        "up": o.up,
        "channel": o.up.then_some(o.chan).flatten(),
        "width": o.up.then_some(o.width).flatten(),
        "on_dfs": on_dfs,
        "auto": s.auto,
        "down_since": s.guard.down_since,
        "override_channel": ov.map(|v| v.channel.clone()),
        "override_htmode": ov.map(|v| v.htmode.clone()),
        "override_at": ov.map(|v| v.at),
        "restore_at": ov.map(|v| v.at + dfs::RESTORE_S),
        "last_event": s.last.map_or("", |l| l.0),
        "last_event_at": s.last.map(|l| l.1),
    })
}

/// `/control`'s address and token, set once by [`spawn`].
static CONTROL: OnceLock<(String, Option<String>)> = OnceLock::new();

pub fn spawn(c: crate::api::Compat) {
    let _ = CONTROL.set(c.src.control.target());
    let hub = c.src.hub.clone();
    tokio::spawn(async move {
        let publish = || {
            if let Some(h) = &hub {
                h.record("wifi_dfs", Ok(report()), tokio::time::Instant::now());
            }
        };
        // the first look right away: the block is there from the start
        let _ = tokio::task::spawn_blocking(look).await;
        publish();
        loop {
            let busy = state()
                .lock()
                .is_ok_and(|s| s.guard.down_since.is_some() || s.guard.ov.is_some());
            let every = Duration::from_secs(if busy { LOOK_S } else { IDLE_S });
            tokio::select! {
                _ = tokio::time::sleep(every) => {}
                _ = wake().notified() => {
                    // a burst of driver lines is one look
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
            let _ = tokio::task::spawn_blocking(look).await;
            publish();
        }
    });
}

fn uci(key: &str) -> Option<String> {
    let out = luyaz_wifi::output_within(
        std::process::Command::new("uci").args(["-q", "get", key]),
        Duration::from_secs(5),
    )
    .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn exists(p: &str) -> bool {
    std::path::Path::new(&crate::state::host_path(p)).exists()
}

fn look() {
    let get = |k: &str| uci(k).unwrap_or_default();
    let want_5g = get(luyaz_wifi::AP_5G) != "1"
        && get("wireless.wifi1.disabled") != "1"
        && get(luyaz_wifi::VENDOR_ONOFF) != "0";
    let (up, chan, width) = if want_5g { dfs::observe_5g() } else { (false, None, None) };
    let overridden = state().lock().is_ok_and(|s| s.guard.ov.is_some());
    let obs = Obs {
        want_5g,
        up,
        chan,
        width,
        // only needed to put an override back; `iw station dump` per AP otherwise is waste
        stations: if overridden { dfs::stations() } else { None },
        in_call: crate::at::in_call() || exists(CALLS),
    };
    let auto = !exists(OFF_FLAG);
    let htmode = get(dfs::HTMODE_KEY);
    let now = luyaz_api::alerts::now().max(0) as u64;
    let step = {
        let Ok(mut s) = state().lock() else { return };
        s.obs = obs;
        s.auto = auto;
        let o = s.obs.clone();
        s.guard.step(&o, &htmode, now, auto)
    };
    for e in &step.events {
        eprintln!("[wifi-dfs] {e:?}");
        let what = match e {
            Event::Moved { .. } => "moved",
            Event::Narrowed { .. } => "narrowed",
            Event::Down => "down",
            Event::Back => "back",
        };
        note(what, now);
    }
    if step.alert {
        alert(DOWN_KIND, "5 GHz Wi-Fi is still down 5 minutes later", now);
    }
    if let Some(a) = step.action {
        act(a, now);
    }
}

fn note(what: &'static str, now: u64) {
    if let Ok(mut s) = state().lock() {
        s.last = Some((what, now as i64));
    }
}

fn act(a: Action, now: u64) {
    let host = Radio;
    let lk = match luyaz_wifi::lock_at(&crate::state::host_path(luyaz_wifi::LOCK_FILE), LOCK_WAIT) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[wifi-dfs] {a:?} deferred: {e}");
            if let Ok(mut s) = state().lock() {
                s.guard.deferred();
            }
            return;
        }
    };
    match a {
        Action::Fallback { htmode } => match dfs::fall_back(&host, &lk, &htmode, now) {
            Ok((ov, up)) => {
                eprintln!("[wifi-dfs] fallback to ch{} {htmode}: up={up}, was {ov:?}", dfs::FALLBACK_CHANNEL);
                if let Ok(mut s) = state().lock() {
                    s.guard.fell_back(ov);
                    save(&s.guard);
                }
                note("fallback", now);
                alert(
                    FALLBACK_KIND,
                    &format!("5 GHz Wi-Fi stopped; moved to channel {} at 80 MHz", dfs::FALLBACK_CHANNEL),
                    now,
                );
            }
            Err(e) => {
                eprintln!("[wifi-dfs] fallback: {e}");
                if let Ok(mut s) = state().lock() {
                    s.guard.deferred();
                }
            }
        },
        Action::Restore { ov, reload } => match dfs::restore(&host, &lk, &ov, reload) {
            Ok(up) => {
                eprintln!("[wifi-dfs] restored {ov:?} (reload {reload}, up {up})");
                if let Ok(mut s) = state().lock() {
                    s.guard.restored();
                    save(&s.guard);
                }
                note("restored", now);
            }
            Err(e) => eprintln!("[wifi-dfs] restore: {e}"),
        },
    }
}

fn save(g: &Guard) {
    let p = crate::state::host_path(RECORD);
    let tmp = format!("{p}.tmp");
    if std::fs::write(&tmp, g.to_json().to_string()).is_err() || std::fs::rename(&tmp, &p).is_err() {
        eprintln!("[wifi-dfs] could not save {p}");
    }
}

fn alert(kind: &str, text: &str, now: u64) {
    let boot = crate::longterm::boot::proc_boot_id();
    let Some(b) = boot.get(..8) else { return };
    let up = std::fs::read_to_string(crate::state::host_path("/proc/uptime"))
        .ok()
        .and_then(|s| s.split('.').next()?.parse::<u64>().ok())
        .unwrap_or(0);
    // `<boot>-<n>` as the queue wants it; ×100 + 72/73 keeps clear of heat's 71 and sim_alerts
    let n = if kind == DOWN_KIND { 72 } else { 73 };
    let id = format!("{}-{}", b.to_ascii_lowercase(), up * 100 + n);
    let dir = crate::state::host_path(luyaz_api::alerts::DIR);
    let a = luyaz_api::alerts::Alerts::at(std::path::Path::new(&dir));
    if let Err(e) = a.add_once(&id, kind, text, now as i64, up) {
        eprintln!("[wifi-dfs] alert: {e}");
    }
}

/// The radio writes through the core's own `/control`.
struct Radio;

impl dfs::RadioHost for Radio {
    fn uci_get(&self, key: &str) -> Option<String> {
        uci(key)
    }

    fn write_radio(&self, channel: &str, htmode: &str, reload: bool) -> Result<Option<String>, String> {
        let params = json!({"set": {dfs::CHANNEL_KEY: channel, dfs::HTMODE_KEY: htmode}, "reload": reload});
        let (url, token) = CONTROL.get().ok_or("no /control address")?;
        // the lock's wait is outside /control's queue (we hold it already); this covers the reload
        match super::scenario::control_as("auto", url, token.as_deref(), "wifi.apply", &params, 240) {
            luyaz_api::control::Reply::Done { result, .. } => {
                Ok(result["reload_error"].as_str().map(str::to_string))
            }
            r => Err(r.into_result().err().unwrap_or_default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_has_the_contract_shape() {
        let v = report();
        assert!(crate::contract::shape_error("wifi_dfs", &v).is_none(), "{v}");
    }
}
