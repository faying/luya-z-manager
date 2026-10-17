//! Turning Wi-Fi back on when the scenario engine is gone (G4; the guard's `restore_wifi`,
//! `vendor_wifi_on`, `via_datad`, `set_autosleep`).
//!
//! Through the core's `/control` first (source `guard`, as the guard: u60d journals it and knows the
//! source); written directly only when nothing listens on the core's port, or the core is too old to
//! know the action (`unknown_action`). Any other answer — a refusal, no answer in time — means no
//! direct write either (D18: a core that is alive but stuck is restarted by its own watchdog).
//!
//! Locks (D3, the one deliberate change from the guard):
//!  * through the core: the Wi-Fi lock only, held across the vendor switch, the write and the
//!    hostapd poll — u60d sees the pid in the lock file is its caller and does not wait for it;
//!  * directly: the write lock (`/var/run/luyaz-write.lock`) first, then the Wi-Fi lock, each waited
//!    for at most 150 s; either not got → this round fails (the caller backs off), and nobody is
//!    killed. Never the write lock while holding the Wi-Fi lock: when the core turns out not to be
//!    there, everything is let go before the direct path starts.
//!
//! What is written is the guard's, not `luyaz_wifi::apply_locked`'s: radios AND AP interfaces (the
//! admin UI can switch a whole radio off), and success is "some hostapd within 45 s" (the guard's
//! judge). The vendor master switch first: with the engine gone there is no telling a stock-UI "Wi-Fi
//! off" from any other, and the takeover overrides them all.

use std::time::Duration;

pub const LOCK_WAIT: Duration = Duration::from_secs(150);
pub const ON_DEADLINE_S: u64 = 45;
pub const POLL_S: u64 = 3;

pub const SET_MODULE: &str =
    r#"{"action":"wifi.set_module","source":"guard","params":{"enabled":1}}"#;
pub const APPLY: &str = r#"{"action":"wifi.apply","source":"guard","params":{"set":{"wireless.wifi0.disabled":"0","wireless.wifi1.disabled":"0","wireless.main_2g.disabled":"0","wireless.main_5g.disabled":"0","wireless.guest_2g.disabled":"1","wireless.guest_5g.disabled":"1"},"reload":true,"best_effort":true}}"#;
/// Guest APs are turned off by a rescue: with one band's guest on, the two main APs make exactly three,
/// which the firmware cannot run (luyaz_wifi::layout) — the core refuses that write, and directly it
/// would leave Wi-Fi down. `best_effort` because a firmware may lack a guest section.
pub const GUESTS: [&str; 2] = ["guest_2g", "guest_5g"];
pub const KEYS: [&str; 4] = [
    "wireless.wifi0.disabled",
    "wireless.wifi1.disabled",
    "wireless.main_2g.disabled",
    "wireless.main_5g.disabled",
];

/// How a call to the core's `/control` went (the guard's `via_datad` return codes).
#[derive(Clone, Debug, PartialEq)]
pub enum Core {
    /// `"ok":true`.
    Done,
    /// Answered no, or did not answer in time: no direct write.
    No(String),
    /// Nothing listening, or a core too old to know the action: write directly.
    Direct,
}

/// Classify a reply body (`None` = connection refused).
pub fn classify(reply: Result<String, String>, refused: bool) -> Core {
    if refused {
        return Core::Direct;
    }
    match reply {
        Ok(body) if body.contains(r#""ok":true"#) => Core::Done,
        Ok(body) if body.contains(r#""unknown_action""#) => Core::Direct,
        Ok(body) => Core::No(body.chars().take(160).collect()),
        Err(e) => Core::No(e),
    }
}

pub fn autosleep_body(on: bool) -> String {
    format!(
        r#"{{"action":"vendor.call","source":"guard","params":{{"object":"zwrt_zte_sleep_faw.wakelock","method":"enableAutoSleep","args":{{"switch":{on}}}}}}}"#
    )
}

/// The vendor switch on, directly: the stock body, with band steering sent along as it stands (left
/// out unless it reads exactly 0 or 1).
pub fn vendor_on_body(lbd: Option<&str>) -> String {
    match lbd {
        Some(v @ ("0" | "1")) => format!(r#"{{"zte_mbb":{{"wifi_onoff":"1","lbd":"{v}"}}}}"#),
        _ => r#"{"zte_mbb":{"wifi_onoff":"1"}}"#.to_string(),
    }
}

/// What the procedure needs from the device.
pub trait WifiOps {
    /// Is anything listening on the core's port? (A connect, nothing sent.)
    fn core_listening(&mut self) -> bool;
    fn core(&mut self, body: &str) -> Core;
    fn lock_wifi(&mut self, wait: Duration) -> Result<(), String>;
    fn unlock_wifi(&mut self);
    fn lock_write(&mut self, wait: Duration) -> Result<(), String>;
    fn unlock_write(&mut self);
    fn uci_get(&mut self, key: &str) -> Option<String>;
    /// `uci set` each, then `uci commit wireless`.
    fn uci_set_commit(&mut self, pairs: &[(&str, &str)]) -> Result<(), String>;
    fn ubus(&mut self, object: &str, method: &str, args: &str) -> Result<(), String>;
    fn beaconing(&mut self) -> bool;
    fn sleep_s(&mut self, s: u64);
    fn log(&mut self, msg: &str);
}

fn poll_on(ops: &mut dyn WifiOps) -> bool {
    // Poll, never assume: the same reload has taken 8 s once and done nothing for a minute another time.
    let mut t = 0;
    loop {
        if ops.beaconing() {
            return true;
        }
        if t >= ON_DEADLINE_S {
            return false;
        }
        ops.sleep_s(POLL_S);
        t += POLL_S;
    }
}

enum Via {
    Done(Result<(), String>),
    Direct,
}

fn through_core(ops: &mut dyn WifiOps) -> Via {
    if let Err(e) = ops.lock_wifi(LOCK_WAIT) {
        return Via::Done(Err(format!("Wi-Fi lock: {e}")));
    }
    if ops.uci_get(luyaz_wifi::VENDOR_ONOFF).as_deref() == Some("0") {
        ops.log("vendor Wi-Fi switch (wireless.zte_mbb.wifi_onoff) is off; turning it on");
        match ops.core(SET_MODULE) {
            Core::Done => ops.log("vendor Wi-Fi switch on asked through the core"),
            Core::No(_) => ops.log("the core refused or did not answer the vendor Wi-Fi switch; restoring the APs anyway"),
            Core::Direct => {
                ops.unlock_wifi();
                return Via::Direct;
            }
        }
    }
    let r = match ops.core(APPLY) {
        Core::Done => {
            ops.log("Wi-Fi on asked through the core");
            if poll_on(ops) {
                Ok(())
            } else {
                Err(format!("no hostapd within {ON_DEADLINE_S}s"))
            }
        }
        Core::No(e) => {
            ops.log("the core refused or did not answer the Wi-Fi restore; next round");
            Err(format!("core: {e}"))
        }
        Core::Direct => {
            ops.unlock_wifi();
            return Via::Direct;
        }
    };
    ops.unlock_wifi();
    Via::Done(r)
}

fn direct(ops: &mut dyn WifiOps) -> Result<(), String> {
    ops.lock_write(LOCK_WAIT)
        .map_err(|e| format!("write lock: {e}; giving up this round"))?;
    if let Err(e) = ops.lock_wifi(LOCK_WAIT) {
        ops.unlock_write();
        return Err(format!("Wi-Fi lock: {e}; giving up this round"));
    }
    ops.log("the core is not there: writing Wi-Fi directly");
    if ops.uci_get(luyaz_wifi::VENDOR_ONOFF).as_deref() == Some("0") {
        ops.log("vendor Wi-Fi switch (wireless.zte_mbb.wifi_onoff) is off; turning it on");
        let lbd = ops.uci_get("wireless.zte_mbb.lbd");
        if let Err(e) = ops.ubus("zwrt_wlan", "set", &vendor_on_body(lbd.as_deref())) {
            ops.log(&format!(
                "vendor Wi-Fi switch: {e}; restoring the APs anyway"
            ));
        }
    }
    let mut pairs: Vec<(&str, &str)> = KEYS.iter().map(|k| (*k, "0")).collect();
    let guest_keys: Vec<String> = GUESTS
        .iter()
        .filter(|g| {
            ops.uci_get(&format!("wireless.{g}")).is_some()
                && ops.uci_get(&format!("wireless.{g}.disabled")).as_deref() != Some("1")
        })
        .map(|g| format!("wireless.{g}.disabled"))
        .collect();
    pairs.extend(guest_keys.iter().map(|k| (k.as_str(), "1")));
    let r = match ops.uci_set_commit(&pairs) {
        Err(e) => Err(format!("uci: {e}")),
        Ok(()) => {
            let _ = ops.ubus("zwrt_wlan", "reload", "{}");
            if poll_on(ops) {
                Ok(())
            } else {
                Err(format!("no hostapd within {ON_DEADLINE_S}s"))
            }
        }
    };
    ops.unlock_wifi();
    ops.unlock_write();
    r
}

/// The whole procedure: `Ok` once something beacons.
pub fn restore(ops: &mut dyn WifiOps) -> Result<(), String> {
    if ops.core_listening() {
        match through_core(ops) {
            Via::Done(r) => return r,
            Via::Direct => {}
        }
    }
    direct(ops)
}

/// Auto-sleep on/off: through the core, directly only when it is not there.
pub fn set_autosleep(ops: &mut dyn WifiOps, on: bool) {
    if ops.core(&autosleep_body(on)) == Core::Direct {
        let _ = ops.ubus(
            "zwrt_zte_sleep_faw.wakelock",
            "enableAutoSleep",
            &format!(r#"{{"switch":{on}}}"#),
        );
    }
}
