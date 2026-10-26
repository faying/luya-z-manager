//! The guard's light duties, once the guard hands them over (u60-sentinel.md §10 step 5c, T4).
//!
//! The sentinel writes the handover list to [`FILE`]; u60d runs a duty only while its writes are
//! on and that file names it (the old guard that shared it is retired, 10c).
//!
//! - `sms`: the alert SMS, [`luyaz_api::alerts::sms`] with u60d as the sender `core`; the send and
//!   the deletion of the sent copy go through u60d's own `/control` as source `guard` (the duty's
//!   source, as the guard sent them). The claim file keeps it apart from the sentinel's fallback.
//! - `lanv6`: while `/data/luyaz/scripts/lan-ipv6-off` exists, keep br-lan's `disable_ipv6` at 1
//!   (user's decision 9-24; zte_router turns it back on). The reconciler's third user-authorised
//!   auto-restore item (§12): put back, logged as an outside change.
//! - `core` (`core` before F7): read by nobody now — it only stopped the retired guard's
//!   degraded-alert round (§3 G5: the health protocol replaced it). The sentinel still lists it.

use luyaz_api::alerts::{Alerts, sms};
use serde_json::{Value, json};
use std::time::Duration;

use crate::api::Compat;

/// The duties the guard skipped this round, one per line (written by the guard, in `/tmp`: after a
/// reboot nothing is handed until the guard's first round says so).
pub const FILE: &str = "/tmp/luyaz/handover";
pub const SMS: &str = "sms";
pub const LANV6: &str = "lanv6";
/// The guard ran these every 60 s; alert SMS do not need finer (an idle look reads two files).
const LOOK_SECS: u64 = 30;
const LAN_V6_FLAG: &str = "/data/luyaz/scripts/lan-ipv6-off";
const LAN_V6_SYSCTL: &str = "/proc/sys/net/ipv6/conf/br-lan/disable_ipv6";
const CLOCK_OK: &str = "/tmp/luyaz/clock-ok";
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";
/// The sender's name in the claim file.
pub const SENDER: &str = "core";

pub(crate) fn owns(c: &Compat, part: &str) -> bool {
    c.writes
        && std::fs::read_to_string(c.path(FILE)).is_ok_and(|t| luyaz_api::handover::names(&t, part))
}

pub async fn run(c: Compat) {
    let mut look = tokio::time::interval(c.clock.secs(LOOK_SECS));
    let mut had = (false, false);
    loop {
        look.tick().await;
        let now = (owns(&c, SMS), owns(&c, LANV6));
        if now != had {
            eprintln!(
                "[compat] guard duties: u60d now runs{}{}{}",
                if now.0 { " sms" } else { "" },
                if now.1 { " lanv6" } else { "" },
                if now == (false, false) { " none" } else { "" }
            );
            had = now;
        }
        if now.1 {
            lanv6(&c);
        }
        if now.0 {
            let host = Host {
                c: c.clone(),
                handle: tokio::runtime::Handle::current(),
            };
            let _ = tokio::task::spawn_blocking(move || {
                let mut host = host;
                let a = Alerts::at(&host.c.alerts_dir());
                let r = a.sms_round(SENDER, std::process::id(), &mut host);
                if !matches!(r, sms::Round::Idle) {
                    eprintln!("[compat] alert sms: {r:?}");
                }
            })
            .await;
        }
    }
}

/// guard `lanv6_round`: an empty or unreadable value is left alone (br-lan not up yet).
pub(super) fn lanv6(c: &Compat) {
    if !c.path(LAN_V6_FLAG).exists() {
        return;
    }
    let v = std::fs::read_to_string(c.path(LAN_V6_SYSCTL)).unwrap_or_default();
    let v = v.trim();
    if v.is_empty() || v == "1" {
        return;
    }
    match std::fs::write(c.path(LAN_V6_SYSCTL), "1\n") {
        Ok(()) => eprintln!(
            "[compat] lanv6: LAN IPv6 was on again (outside change): disabled it on br-lan"
        ),
        Err(e) => eprintln!("[compat] lanv6: could not disable LAN IPv6: {e}"),
    }
}

/// u60d as an alert SMS sender (blocking: runs on the blocking pool).
struct Host {
    c: Compat,
    handle: tokio::runtime::Handle,
}

impl Host {
    fn control(&self, action: &str, params: &Value) -> Result<Value, String> {
        self.handle
            .block_on(self.c.src.control.send_with(
                "guard",
                None,
                action,
                params,
                crate::api::control::TIMEOUT,
            ))
            .into_result()
    }
    fn read(&self, p: &str) -> String {
        std::fs::read_to_string(self.c.path(p)).unwrap_or_default()
    }
}

impl sms::SmsIo for Host {
    fn wall(&mut self) -> i64 {
        luyaz_api::alerts::now()
    }
    fn uptime(&mut self) -> u64 {
        self.read("/proc/uptime")
            .split('.')
            .next()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }
    fn boot_id(&mut self) -> String {
        self.read(BOOT_ID).trim().to_string()
    }
    fn pid_alive(&mut self, pid: u32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        let r = unsafe { libc::kill(pid as i32, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    fn clock_ok(&mut self) -> bool {
        let at = luyaz_api::clock::BootTime::from_uptime(
            &self.read(BOOT_ID),
            &self.read("/proc/uptime"),
        );
        luyaz_api::clock::evidence_status(
            &self.read(CLOCK_OK),
            at.as_ref(),
            luyaz_api::alerts::now(),
        )
        .wall_usable()
    }
    fn wall_settled(&mut self) -> bool {
        let at = luyaz_api::clock::BootTime::from_uptime(
            &self.read(BOOT_ID),
            &self.read("/proc/uptime"),
        );
        luyaz_api::clock::evidence_settled(
            &self.read(CLOCK_OK),
            at.as_ref(),
            luyaz_api::alerts::now(),
        )
    }
    fn lang_en(&mut self) -> bool {
        sms::lang_en(&self.read(sms::UI_CONF))
    }
    fn tz_hours(&mut self) -> i32 {
        let v = self.handle.block_on(crate::state::uci_value(
            "zwrt_zte_sntp.settings.time_from_utc",
        ));
        sms::tz_hours(&v)
    }
    fn send(&mut self, args: &Value) -> Result<(), sms::SendError> {
        use luyaz_api::control::Reply;
        use sms::SendError::{NotSent, Rejected};
        let reply = self.handle.block_on(self.c.src.control.send_with(
            "guard",
            None,
            "vendor.call",
            &json!({"object": "zwrt_wms", "method": "zte_libwms_send_sms", "args": args}),
            crate::api::control::TIMEOUT,
        ));
        match reply {
            Reply::Done { result, .. } if sms::send_result_ok(result.get("result")) => Ok(()),
            Reply::Done { result, .. } => Err(Rejected(format!(
                "device rejected (result={})",
                result["result"]
            ))),
            Reply::Failed { error, .. } => Err(Rejected(format!("ubus: {error}"))),
            Reply::Invalid(e) => Err(Rejected(format!("refused by /control: {e}"))),
            Reply::Fallback => Err(NotSent("/control fell back".into())),
            Reply::Busy { .. } => Err(NotSent("/control busy".into())),
            Reply::OpBusy { message, .. } => Err(NotSent(format!("op busy: {message}"))),
            Reply::Unreachable { error, .. } => Err(NotSent(format!("/control: {error}"))),
        }
    }
    fn delete_sent_copy(&mut self, number: &str) {
        std::thread::sleep(Duration::from_secs(1)); // let the firmware store it
        for store in [1, 0] {
            let args = json!({"tags": 2, "page": 0, "data_per_page": 5, "mem_store": store,
                "order_by": "order by id desc"});
            let Ok(list) = self.handle.block_on((self.c.src.ubus)(
                "zwrt_wms",
                "zte_libwms_get_sms_data",
                args,
            )) else {
                continue;
            };
            let Some(id) = sms::newest_to(&list, number) else {
                continue;
            };
            match self.control("sms.delete", &json!({"ids": id})) {
                Ok(_) => eprintln!("[compat] alert sms: deleted the sent copy (id {id})"),
                Err(e) => eprintln!("[compat] alert sms: deleting the sent copy {id}: {e}"),
            }
            return;
        }
        eprintln!("[compat] alert sms: sent copy not found to delete");
    }
    fn log(&mut self, msg: &str) {
        eprintln!("[compat] alert {msg}");
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    use luyaz_api::clock::{BootTime, ClockEvidence, ClockSource};
    use sms::SmsIo;

    #[tokio::test]
    async fn sms_host_checks_the_same_freshness_boot_and_step_rules_as_the_ledger() {
        let root = std::env::temp_dir().join(format!("sms-clock-evidence-{}", std::process::id()));
        let c = Compat::new(root.to_str().unwrap(), crate::api::Sources::none());
        let mut h = Host {
            c,
            handle: tokio::runtime::Handle::current(),
        };
        let put = |p: &str, value: &str| {
            let path = root.join(p.trim_start_matches('/'));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, value).unwrap();
        };
        let wall = luyaz_api::alerts::now();
        put(BOOT_ID, "a\n");
        put("/proc/uptime", "1000.00 1.00\n");
        let e = ClockEvidence::observation(
            wall,
            BootTime {
                boot_id: "a".into(),
                seconds: 1000,
            },
            Some(1000),
            ClockSource::Year,
        );
        put(CLOCK_OK, &e.encode());
        assert!(h.clock_ok());
        for step in [-28_800, 28_800] {
            let mut stepped = e.clone();
            stepped.raw_wall += step;
            put(CLOCK_OK, &stepped.encode());
            assert!(!h.clock_ok());
        }
        put(CLOCK_OK, &e.encode());
        put(BOOT_ID, "b\n");
        assert!(!h.clock_ok());
        put(BOOT_ID, "a\n");
        put("/proc/uptime", "1181.00 1.00\n");
        assert!(!h.clock_ok());
        put("/proc/uptime", "1000.00 1.00\n");
        put(CLOCK_OK, "1 1790000000 1000 year\n");
        assert!(!h.clock_ok());
        std::fs::remove_file(root.join("proc/uptime")).unwrap();
        put(CLOCK_OK, &e.encode());
        assert!(!h.clock_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}
