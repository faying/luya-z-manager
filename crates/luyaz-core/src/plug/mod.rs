//! 进程插件的连线（luyaz-phone 设计 R1、R3、R26–R33）。
//!
//! core 开一个 Unix socket，只让插件的专用用户连进来（SO_PEERCRED 核对 uid），一行一个 JSON。
//! 插件是客户端：连上先送 `hello`（契约版本 + 要用的操作）；core 要读写插件设置时，从这条线往下送
//! 带 id 和截止时间的请求帧，插件回同一个 id。这里不认识任何插件的业务，只管连线、上限和设置转送。
//!
//! ```text
//! luyaz-web ─/control plugin.settings.*─▶ write.rs（device_gate、agent_gate）─▶ settings ─▶ link
//!                                                                                  │ 请求帧，等 3 s
//!                        插件（专用用户）──hello──▶ core.sock（SO_PEERCRED）◀──────┘
//! ```

pub mod display;
pub mod frame;
pub mod link;
pub mod settings;
pub mod sms;

use std::time::Duration;

/// 契约版本（v0 随 S1；v1 起照收 v0 的插件，v0 的操作照旧）。
pub const CONTRACT_V: u64 = 1;
/// 还接受的最旧版本：hello 的版本在这两个之间就接。
pub const CONTRACT_MIN: u64 = 0;
/// 单帧上限（R31）：读到这么多还没换行就断开。
pub const FRAME_MAX: usize = 64 * 1024;
/// core 发往插件的待送队列上限（R31）：插件不读、队列满就断开。
pub const OUT_MAX: usize = 64 * 1024;
/// core 发往插件、还没回的请求最多几笔（R31）；多的回 busy。
pub const IN_FLIGHT_MAX: usize = 4;
/// 等插件回复的时限（R26）；也是写入的截止时间（R30），写 socket 的时间算在里面。
pub const REPLY_WAIT: Duration = Duration::from_secs(3);
/// 连上后多久内要送 hello。
pub const HELLO_WAIT: Duration = Duration::from_secs(5);

const SOCK_DEFAULT: &str = "/var/run/luyaz/core.sock";
const USER_DEFAULT: &str = "phone";

/// 起 socket 服务。插件的专用用户不存在（没装插件、公开版）就不开，设置页也不显示。
pub fn start() {
    let path = std::env::var("LUYAZ_PLUG_SOCK").unwrap_or_else(|_| SOCK_DEFAULT.into());
    let user = std::env::var("LUYAZ_PLUG_USER").unwrap_or_else(|_| USER_DEFAULT.into());
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    let Some((uid, gid)) = lookup_user(&passwd, &user) else {
        eprintln!("[plug] no user {user}: plugin socket off");
        return;
    };
    let hub = link::hub();
    hub.set_installed(true);
    tokio::spawn(async move {
        let listener = match link::bind(&path, gid) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[plug] {path}: {e}: plugin socket off");
                return;
            }
        };
        eprintln!("[plug] listening on {path} for uid {uid}");
        link::serve(hub, listener, uid).await;
    });
}

/// 插件发来的调用（契约 v0）。错误回（码, 说明），说明里不放短信内容。
pub async fn dispatch(
    op: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, (String, String)> {
    match op {
        "sms.read" => sms::read(params),
        "sms.ack" => sms::ack(params).await,
        "sms.send" => sms::send(params).await,
        "sms.remove" => sms::remove(params).await,
        "sms.reports" => sms::reports(params).await,
        "sim.identity" => Ok(sms::identity()),
        "display.show" => display::show(params, link::hub().current().map(|c| c.generation)),
        "display.dismiss" => display::dismiss(params),
        "clock.utc" => clock_utc().await,
        "net.now" => Ok(net_now(crate::usage_log::latest("net").as_ref())),
        "power.now" => Ok(crate::plugin::battery_eta::now()),
        _ => Err(("unknown_op".into(), format!("no such op: {op}"))),
    }
}

/// `net.now`：现在的制式、频段、格数，取 core 上一轮采集的快照（不读基带，随时可叫）。
/// `{"type":"SA","band":"n78","bars":4}`；还没采集过、或没注册上（没有制式）回 `null`。
/// SA 的频段用 `nr_band`（`wan_active_band` 那时是锚点的 LTE 频段或空），其余用 `band`。
fn net_now(signal: Option<&serde_json::Value>) -> serde_json::Value {
    let Some(s) = signal else {
        return serde_json::Value::Null;
    };
    let text = |k: &str| s[k].as_str().unwrap_or("").trim().to_owned();
    let kind = text("type");
    if kind.is_empty() || kind.to_ascii_lowercase().contains("service") {
        return serde_json::Value::Null;
    }
    let band = match (kind.as_str(), text("nr_band")) {
        ("SA", nr) if !nr.is_empty() => nr,
        _ => text("band"),
    };
    let bars = s["bars"].as_i64().unwrap_or(0).clamp(0, 5);
    serde_json::json!({"type": kind, "band": band, "bars": bars})
}

/// `clock.utc`：设备时钟比真实 UTC 快几秒（设备时钟是当地时间标成 UTC）。时钟显然没对过
/// （早于 2026 年）就回错：插件拿它签 APNs 令牌，宁可推送先排着。
async fn clock_utc() -> Result<serde_json::Value, (String, String)> {
    const PLAUSIBLE: u64 = 1_767_225_600; // 2026-01-01
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now < PLAUSIBLE {
        return Err((
            "clock_unset".into(),
            "the device clock is not set yet".into(),
        ));
    }
    Ok(serde_json::json!({"offset": crate::api::utc_offset().await}))
}

/// `/etc/passwd` 里某个用户的 uid 和 gid。
fn lookup_user(passwd: &str, name: &str) -> Option<(u32, u32)> {
    passwd.lines().find_map(|line| {
        let mut f = line.split(':');
        if f.next()? != name {
            return None;
        }
        let _password = f.next()?;
        Some((f.next()?.parse().ok()?, f.next()?.parse().ok()?))
    })
}

/// 开机以来的毫秒数（CLOCK_MONOTONIC）。插件在同一台设备上用同一个时钟比截止时间（R30）。
pub fn mono_ms() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts 是有效的可写 timespec；CLOCK_MONOTONIC 在 Linux 上总是可用。
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as u64) * 1000 + (ts.tv_nsec as u64) / 1_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_now_reads_the_cached_signal_block() {
        use serde_json::json;
        assert_eq!(net_now(None), serde_json::Value::Null);
        let sa = json!({"type":"SA","band":"","nr_band":"n78","bars":4});
        assert_eq!(net_now(Some(&sa)), json!({"type":"SA","band":"n78","bars":4}));
        let lte = json!({"type":"LTE","band":"3","nr_band":"","bars":9});
        assert_eq!(net_now(Some(&lte)), json!({"type":"LTE","band":"3","bars":5}));
        let nsa = json!({"type":"NSA","band":"3","nr_band":"n78","bars":2});
        assert_eq!(net_now(Some(&nsa))["band"], "3");
        assert_eq!(net_now(Some(&json!({"type":"","bars":0}))), serde_json::Value::Null);
        assert_eq!(net_now(Some(&json!({"type":"No Service"}))), serde_json::Value::Null);
    }

    #[test]
    fn user_lookup_reads_uid_and_gid() {
        let passwd = "root:x:0:0:root:/root:/bin/ash\nphone:x:921:921:phone:/var:/bin/false\n";
        assert_eq!(lookup_user(passwd, "phone"), Some((921, 921)));
        assert_eq!(lookup_user(passwd, "root"), Some((0, 0)));
        assert_eq!(lookup_user(passwd, "pho"), None);
        assert_eq!(lookup_user("phone:x:abc:1\n", "phone"), None);
    }

    #[test]
    fn monotonic_clock_moves_forward() {
        let a = mono_ms();
        std::thread::sleep(Duration::from_millis(5));
        assert!(mono_ms() >= a + 4);
    }
}
