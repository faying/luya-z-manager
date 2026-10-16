//! Live Wi-Fi values moved from the agent: actual channel/width and stations per band (only those six, never a key), power save.
//!
//! u60-web.md 8d-1b: the agent's reads luyaz-web still used, served here at the agent's path with
//! the agent's reply shape (`{ok, data}`), so luyaz-web only changes where it asks. On demand,
//! off the collection tick (spawn_blocking / a timeout), never cached across requests unless said.

use std::time::Duration;

use axum::{extract::State, response::Response};
use serde_json::{Value, json};

use super::{Compat, reply};

const DEADLINE: Duration = Duration::from_secs(3);
/// The agent's interface per band: wlan0 = 2.4 GHz, wlan2 = 5 GHz (MU5250).
const IF_2G: &str = "wlan0";
const IF_5G: &str = "wlan2";
const PSM_HOTPLUG: &str = "99-disable-powersave";

fn iw_bin() -> String {
    std::env::var("LUYAZ_IW_BIN").unwrap_or_else(|_| "/usr/sbin/iw".into())
}

/// (channel, width) from `iw <if> info`; empty strings when absent, like the agent.
pub(super) fn parse_info(out: &str) -> (String, String) {
    let channel = out
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("channel "))
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string();
    let bw = out
        .lines()
        .find_map(|l| {
            let l = l.trim();
            let pos = l.find("width:")?;
            let rest = l[pos + 6..].trim();
            let end = rest.find("MHz").map_or(rest.len(), |i| i + 3);
            Some(rest[..end].trim().to_string())
        })
        .unwrap_or_default();
    (channel, bw)
}

/// What the agent's `grep -c Station` counted.
pub(super) fn count_stations(out: &str) -> u64 {
    out.lines().filter(|l| l.contains("Station")).count() as u64
}

async fn iw(args: &[&str]) -> String {
    match crate::command::run(&iw_bin(), args, DEADLINE).await {
        Ok(o) => String::from_utf8_lossy(&o).into_owned(),
        Err(_) => String::new(),
    }
}

/// Exactly six fields: the agent's reply also carries both Wi-Fi keys, which no page here shows.
pub(super) fn status_data(info: [(String, String); 2], clients: [u64; 2]) -> Value {
    json!({
        "actual_channel_2g": info[0].0,
        "actual_bw_2g": info[0].1,
        "actual_channel_5g": info[1].0,
        "actual_bw_5g": info[1].1,
        "clients_2g": clients[0],
        "clients_5g": clients[1],
    })
}

/// GET /api/wifi/status
pub async fn status(State(_c): State<Compat>) -> Response {
    let (i2, i5, s2, s5) = tokio::join!(
        iw(&[IF_2G, "info"]),
        iw(&[IF_5G, "info"]),
        iw(&[IF_2G, "station", "dump"]),
        iw(&[IF_5G, "station", "dump"]),
    );
    let data = status_data(
        [parse_info(&i2), parse_info(&i5)],
        [count_stations(&s2), count_stations(&s5)],
    );
    reply(200, json!({"ok": true, "data": data}))
}

/// `iw dev X get power_save` → on/off; anything else (Wi-Fi off) → None.
pub(super) fn psm_from_iw(out: &str) -> Option<bool> {
    if out.contains("Power save: on") {
        Some(true)
    } else if out.contains("Power save: off") {
        Some(false)
    } else {
        None
    }
}

/// The saved choice in the hotplug script (missing: never set, driver default).
pub(super) fn psm_from_script(text: &str) -> Option<bool> {
    let line = text.lines().find(|l| l.contains("set power_save"))?;
    psm_from_iw(&line.replace("set power_save", "Power save:"))
}

pub(super) fn psm_data(live: Option<bool>, saved: Option<bool>) -> Value {
    json!({"enabled": live.or(saved), "live": live, "saved": saved})
}

/// GET /api/wifi/power-save
pub async fn power_save(State(_c): State<Compat>) -> Response {
    let mut live = None;
    for w in [IF_2G, IF_5G] {
        live = psm_from_iw(&iw(&["dev", w, "get", "power_save"]).await);
        if live.is_some() {
            break;
        }
    }
    // same directory the write (control.rs wifi_power_save) puts the script in
    let dir =
        std::env::var("LUYAZ_HOTPLUG_DIR").unwrap_or_else(|_| "/etc/hotplug.d/iface".into());
    let file = std::path::Path::new(&dir).join(PSM_HOTPLUG);
    let saved = crate::state::blocking(move || std::fs::read_to_string(file).ok())
        .await
        .and_then(|t| psm_from_script(&t));
    reply(200, json!({"ok": true, "data": psm_data(live, saved)}))
}
