//! Where the screen's data comes from and where its actions go: the C getters, polls and
//! actions the pages call, one method each (src/data.c, esim.c, netinfo.c …). Two of them:
//! the data side on the device (data::Live) and the offscreen harness's replay of the C render
//! test (render::Replay), which answers as the test's fixtures do (tests/render/fixtures.c).
//!
//! Values that C reads live (the backlight, the clock, the frame and touch counters) are asked
//! with the model at hand: the replay reads them from it, the device from the device.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0


/// The screen owner's control FIFO: the sentinel's.
pub const SCREEN_CTLS: [&str; 1] = ["/tmp/luyaz-screen.ctl"];
use crate::logic::Launch;
use ui_model::Model;
use std::rc::Rc;

/// netinfo_poll's modes (include/netinfo.h): how much of /api/netinfo the page on screen needs.
pub const NI_OFF: i32 = 0;
pub const NI_PAGE: i32 = 1;
pub const NI_HOME: i32 = 2;
pub const NI_LITE: i32 = 3;
pub const NI_APN: i32 = 4;
pub const NI_CLIENTS: i32 = 5;

/// What is on screen at a refresh: the `active` argument of each poll, as refresh.c computes it.
#[derive(Clone, Debug, Default)]
pub struct Activity {
    /// backlight_panel_lit(): the core's pace
    pub panel_lit: bool,
    /// backlight_is_on(): /v2/screen is read only while lit
    pub lit: bool,
    /// tab_visible(TAB_HOME): scenario_poll
    pub home: bool,
    /// netinfo_poll's mode (NI_*)
    pub ni_mode: i32,
    /// Home, 出口 or the Tailscale page
    pub tailscale: bool,
    /// the CHILL page itself (家里出口)
    pub chill_page: bool,
    /// a 家里出口 switch is being sent from the screen
    pub hx_sending: bool,
    /// Home, 出口 or a CHILL page
    pub chill: bool,
    /// the eSIM page
    pub esim: bool,
    /// read the eSIM list once for this card (蜂窝 up, the finger idle 1.5 s)
    pub esim_prefetch: bool,
    /// the 测速 page or 出口
    pub speed: bool,
    /// the 测速 page (its server list)
    pub speed_page: bool,
    /// 系统 (the battery estimate)
    pub sys: bool,
    /// 健康与告警
    pub alerts: bool,
    /// 网络诊断
    pub diag: bool,
}

/// What each poll said this refresh (1 = what it shows changed; refresh.c repaints on it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Polled {
    pub scenario: bool,
    pub netinfo: bool,
    pub screen: bool,
    pub tailscale: bool,
    pub chill: bool,
    pub chill_home: bool,
    pub esim: bool,
    pub speedtest: bool,
    pub servers: bool,
    pub battery: bool,
    pub versions: bool,
    /// 落地 changed
    pub landing: bool,
    pub alerts: bool,
    pub diagnose: bool,
}

/// The netinfo actions sent with 已发送 first (network.c net_send).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NiAct {
    Scan,
    Auto,
    Register(usize),
    NbrScan,
}

/// esim_select's answers (include/esim.h ESIM_SEL_*).
pub const ESIM_SEL_FAIL: i32 = -1;
pub const ESIM_SEL_ARMED: i32 = 0;
pub const ESIM_SEL_STARTED: i32 = 1;
pub const ESIM_SEL_CURRENT: i32 = 2;
pub const ESIM_SEL_BUSY: i32 = 3;
pub const ESIM_SEL_COOLDOWN: i32 = 4;

/// The power key (include/key_input.h KEY_EV_*).
pub const KEY_EV_NONE: i32 = 0;
pub const KEY_EV_SHORT: i32 = 1;
pub const KEY_EV_LONG: i32 = 2;

pub trait Source {
    // ---- the main loop ----
    /// the core's stream, every pass (data_backend_poll + commit); true = a new snapshot
    fn pump(&mut self, now_ms: u32) -> bool;
    /// the core's socket, -1 none (the quiet and dark loops wait on it)
    fn data_fd(&self) -> i32;

    // ---- the refresh ----
    /// The polls refresh n runs, in refresh.c's order; what each said.
    fn poll(&mut self, n: u32, a: &Activity) -> Polled;
    /// The model as the getters see it now (after a poll or an action); None = unchanged.
    fn take_model(&mut self) -> Option<Rc<Model>>;

    // ---- read live in C ----
    fn now(&self, m: &Model) -> i64;
    fn boot_s(&self, m: &Model) -> i64;
    fn bl_is_on(&self, m: &Model) -> bool;
    /// sysfs brightness > 0 (backlight_panel_lit)
    fn bl_panel_lit(&self, m: &Model) -> bool;
    /// sysfs brightness > 0, and the on/off flag follows it (backlight_is_lit)
    fn bl_is_lit(&mut self, m: &Model) -> bool;
    /// the remembered on-level
    fn bl_get(&self, m: &Model) -> i32;
    fn bl_max(&self, m: &Model) -> i32;
    /// flushes pushed to the panel; touch reports seen
    fn frames(&self, m: &Model) -> u64;
    fn touch_reports(&self, m: &Model) -> u64;
    /// the core's committed snapshot version
    fn core_version(&self, m: &Model) -> u64;
    /// health_count(): -1 not read yet, -2 read but nothing came
    fn health_count(&self, m: &Model) -> i32;
    fn sms_delete_armed(&self, m: &Model, i: usize) -> bool;

    // ---- the backlight ----
    fn bl_on(&mut self);
    fn bl_off(&mut self);
    fn bl_set(&mut self, level: i32);
    fn bl_remember(&mut self, level: i32);

    // ---- the power key ----
    fn key_poll(&mut self, now_ms: u32) -> i32;
    fn key_fd(&self) -> i32;
    fn key_held(&self) -> bool;

    // ---- core ----
    fn data_set_pace(&mut self, panel_lit: bool);
    /// /control; fb_args None = no emergency write (fb_action: the script's name for it)
    fn data_control_fb(&mut self, action: &str, params: &str, fb_action: &str, fb_args: Option<&str>) -> bool;
    fn data_control_undo(&mut self, action: &str, params: &str) -> bool;
    /// journal.list's answer (op.c op_log_fetch)
    fn journal_list(&mut self, m: &Model) -> Option<String>;
    fn sms_mark_read_id(&mut self, id: i64);
    fn sms_mark_all_read(&mut self);
    fn sms_delete_id(&mut self, id: i64);
    fn sms_delete_arm(&mut self, i: usize);
    fn sms_delete_tap(&mut self, i: usize) -> bool;

    // ---- luyaz-core ----
    fn alerts_mark_all_read(&mut self);
    /// 「检查更新」 (component versions)
    fn versions_check(&mut self);
    fn esim_select(&mut self, i: usize) -> i32;
    fn speedtest_start(&mut self) -> bool;
    fn speedtest_stop(&mut self) -> bool;
    fn speedtest_select_server(&mut self, i: i32);
    fn scenario_kick(&mut self);
    fn netinfo_act(&mut self, a: NiAct);
    /// "" = back to automatic
    fn netinfo_pin(&mut self, id: &str);
    fn netinfo_apn_use(&mut self, id: &str);
    fn netinfo_hurry(&mut self, secs: i32);
    fn diagnose_kick(&mut self);
    fn diagnose_poll(&mut self, active: bool) -> bool;
    fn diagnose_start(&mut self) -> bool;
    fn diagnose_speed(&mut self) -> bool;
    fn diagnose_feedback(&mut self, right: bool) -> bool;
    fn scenario_chill_set(&mut self, on: bool) -> bool;
    fn chill_set_exit(&mut self, state: &str) -> bool;
    fn chill_select_node(&mut self, i: usize) -> bool;
    fn chill_select_group(&mut self, i: usize) -> bool;
    fn chill_set_profile(&mut self, p: &str) -> bool;
    fn chill_test_delay(&mut self) -> i32;
    /// (HTTP status, the agent's code on 409)
    fn chill_home_set(&mut self, item: usize, on: bool) -> (i32, String);
    /// the new end of 其余, None = not extended
    fn chill_home_extend(&mut self, m: &Model) -> Option<i64>;
    /// chill_home_poll(1, 1) right after a switch
    fn chill_home_read(&mut self);

    // ---- the process ----
    /// exec a fresh copy with these arguments; returns only on failure (-1)
    fn exec(&mut self, l: &Launch) -> i32;
    /// ask the screen's owner ([`SCREEN_CTLS`], first that listens) for the vendor UI; false =
    /// nobody listens on either
    fn request_vendor(&mut self) -> bool;
    /// no screen owner: start the vendor UI in 2 s and stop this process
    fn vendor_fallback(&mut self);
    /// core could not be asked to power down: the vendor call directly
    fn power_fallback(&mut self, reboot: bool);
    /// 40 s and still up: busybox reboot / poweroff
    fn power_last_resort(&mut self, reboot: bool);
    /// hold the 「正在切换」 frame before the exec (UI_LANG_HOLD_MS)
    fn lang_hold(&mut self);
    /// the alert for automatic appearance switching paused
    fn alert_theme_paused(&mut self);
    /// the Wi-Fi radios up (wlan0, wlan2) and Wi-Fi power save, None = not read here
    fn aux_read(&mut self) -> Option<(i32, i32, i32)>;
}

#[cfg(test)]
mod r4_r5_tests {
    #[test]
    fn new_fifo_first_then_old() {
        assert_eq!(super::SCREEN_CTLS, ["/tmp/luyaz-screen.ctl"]);
    }
}
