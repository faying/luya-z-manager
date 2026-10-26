//! The data side on the device as the screen's Source: each poll when the 1 s refresh asks,
//! each action when a page sends it, and the view model built from the getters.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::core_api::CoreApi;
use super::background::{Cached, Task};
use super::alerts::Alerts;
use super::battery::BatteryEst;
use super::landing::Landing;
use super::versions::Versions;
use super::chill::Chill;
use super::core_link::{Cfg, CoreLink};
use super::diagnose::Diagnose;
use super::esim::Esim;
use super::http;
use super::netinfo::Netinfo;
use super::scenario::Scenario;
use super::screen_feed::{ScreenFeed, SfCfg};
use super::speedtest::Speedtest;
use super::tailscale::Tailscale;
use super::util;
use crate::logic::Launch;
use crate::source::{Activity, NiAct, Polled, Source};
use ui_model::Model;
use plat::backlight::Backlight;
use plat::key::Key;
use std::rc::Rc;

/// data_control_notice's age everywhere the screen asks: 4 s
const NOTICE_MS: u32 = 4000;
/// UI_LANG_HOLD_MS: the 「正在切换」 frame stays this long before the exec
const LANG_HOLD_MS: u64 = 400;

pub struct Live {
    pub agent: CoreApi,
    pub core: CoreLink,
    sf: Cached<ScreenFeed>,
    scenario: Cached<Scenario>,
    netinfo: Cached<Netinfo>,
    alerts: Cached<Alerts>,
    esim: Cached<Esim>,
    speed: Cached<Speedtest>,
    ts: Cached<Tailscale>,
    diag: Cached<Diagnose>,
    bat: Cached<BatteryEst>,
    vers: Cached<Versions>,
    landing: Cached<Landing>,
    chill: Cached<Chill>,
    aux: Option<Task<(i32, i32, i32)>>,
    pub bl: Backlight,
    pub key: Key,
}

impl Live {
    /// The data side and the device's backlight and power key (key: None = offscreen).
    pub fn new(key: Key) -> Live {
        Live {
            agent: CoreApi::default(),
            core: CoreLink::background(Cfg::default()),
            sf: Cached::new(ScreenFeed::new(SfCfg::default())),
            scenario: Cached::new(Scenario::new()),
            netinfo: Cached::new(Netinfo::default()),
            alerts: Cached::new(Alerts::default()),
            esim: Cached::new(Esim::default()),
            speed: Cached::new(Speedtest::default()),
            ts: Cached::new(Tailscale::default()),
            diag: Cached::new(Diagnose::default()),
            bat: Cached::new(BatteryEst::default()),
            vers: Cached::new(Versions::default()),
            landing: Cached::new(Landing::default()),
            chill: Cached::new(Chill::default()),
            aux: None,
            bl: Backlight::init(),
            key,
        }
    }

    /// the core's stream: every main-loop pass (data_backend_poll + commit_latest).
    pub fn pump(&mut self, now_ms: u32) -> bool {
        let woke = super::background::take_wake();
        let changed = self.core.poll(now_ms) && self.core.commit_latest();
        woke || changed
    }

    /// The view model from the getters.
    pub fn build(&mut self) -> Model {
        let (data_ok, data) = self.core.refresh();
        let (live_ok, live) = self.core.refresh_live();
        let notice = self.core.notice(NOTICE_MS);
        let st = &self.speed;
        let ni = &self.netinfo.ni;
        Model {
            data_ok: data_ok as i32,
            data,
            live_ok: live_ok as i32,
            live,
            backend_alive_wall: self.core.alive_wall(),
            backend_silent: self.core.silent() as i32,
            notice_ok: notice.is_some() as i32,
            notice: notice.unwrap_or_default(),
            core_version: self.core.version(),
            sms_armed: self.core.sms_armed_index(),

            screen_status: self.sf.status(),
            screen_net: self.sf.net().cloned(),
            screen_exec_age: self.sf.exec_age(),
            screen_stuck: self.sf.stuck() as i32,
            screen_op: Some(self.sf.op().to_string()),
            journal: None,

            chill_core: Some(self.chill.core().into()),
            chill_mode: Some(self.chill.mode().into()),
            chill_exit_raw: Some(self.chill.exit_raw().into()),
            chill_mode_raw: Some(self.chill.mode_raw().into()),
            chill_group: Some(self.chill.group()),
            chill_node: Some(self.chill.node().into()),
            chill_traffic: Some(self.chill.traffic().into()),
            chill_speed: Some(self.chill.speed().into()),
            chill_chain: Some(self.chill.chain().into()),
            chill_pin: self.chill.exit_pin(),
            chill_conns: Some(self.chill.conns().into()),
            chill_online: self.chill.online() as i32,
            chill_conn_split: Some(self.chill.conn_split()),
            chill_groups: self.chill.groups(),
            chill_nodes: self.chill.nodes(),
            chill_pairs: self.chill.top_pairs(),
            chill_profile_raw: Some(self.chill.profile_raw().into()),
            chill_profile_effective_raw: Some(self.chill.profile_effective_raw().into()),
            chill_thermal_eco: self.chill.thermal_eco() as i32,
            chill_delay_pending: self.chill.delay_pending() as i32,
            chill_home: Some(self.chill.home().clone()),
            tailscale: self.ts.status(),
            tailscale_peers: self.ts.peers().to_vec(),

            esim_current: Some(self.esim.current().into()),
            esim_state: Some(self.esim.state()),
            esim_ready: self.esim.ready() as i32,
            esim_profiles: self.esim.profiles(),
            esim_switching: self.esim.switching() as i32,
            esim_loaded: self.esim.loaded() as i32,
            esim_enabled_iccid: Some(self.esim.enabled_iccid().into()),

            st_phase: st.phase(),
            st_phase_label: Some(st.phase_label().into()),
            st_progress_pct: st.progress(),
            st_live_mbps: Some(st.live()),
            st_ping_ms: Some(st.ping()),
            st_jitter_ms: Some(st.jitter()),
            st_download_mbps: Some(st.dl()),
            st_upload_mbps: Some(st.ul()),
            st_server: Some(st.server().into()),
            st_error: Some(st.error().into()),
            st_servers_error: Some(st.servers_error().into()),
            st_running: st.running() as i32,
            st_agent_reachable: st.reachable() as i32,
            st_start_error: Some(st.start_error().into()),
            st_servers: st.servers().to_vec(),
            st_selected_index: st.selected(),
            st_v2: st.v2().cloned(),

            agent_health: self.scenario.health(),
            scenario: self.scenario.status(),

            alerts_unread: self.alerts.unread(),
            alerts: self.alerts.items().to_vec(),
            alerts_error: Some(self.alerts.error().into()),
            health_checked: self.alerts.health_checked(),
            health_n: self.alerts.health_count(),
            health: self.alerts.health().to_vec(),

            netinfo: Some(ni.clone()),
            netinfo_action_error: Some(self.netinfo.action_error().into()),

            diagnose_run: Some(self.diag.run().clone()),
            diagnose_idle: self.diag.idle() as i32,
            diagnose_agent_err: self.diag.agent_err(),
            diagnose_error: Some(self.diag.error().into()),
            diagnose_proposal: self.diag.proposal().cloned(),
            diagnose_slow: self.diag.slow().into(),
            diagnose_restore: self.diag.restore().cloned(),

            battery_est: Some(self.bat.text().into()),
            battery_kind: self.bat.est().kind,
            battery_min: self.bat.est().minutes,
            versions: self.vers.rows().to_vec(),
            versions_checked: Some(self.vers.checked().into()),
            landing: self.landing.get().cloned(),

            bl_panel_lit: self.bl.panel_lit() as i32,
            bl_is_on: self.bl.is_on() as i32,
            bl_is_lit: self.bl.panel_lit() as i32,
            bl_get: self.bl.get(),
            bl_max: self.bl.max(),
            frames: crate::device::frames(),
            touch_reports: plat::touch::report_count(),
            boot_s: util::boot_s(),
            now: util::wall(),
        }
    }

    /// POST /control journal.list, read at once.
    fn journal(&self) -> Option<String> {
        let body = r#"{"action":"journal.list","source":"screen","params":{"limit":30}}"#;
        let req = http::build("POST", "/control", "127.0.0.1", "", Some(body), 256)?;
        let fd = http::connect_tcp(&self.core.cfg.host, self.core.cfg.port, 300)?;
        let r = http::exchange(fd, req.as_bytes(), 57344, 600);
        r.body.filter(|_| r.status == 200).map(|b| util::text(&b))
    }
}

/// system(3): through /bin/sh, waited for; true when it exited 0.
fn system(cmd: &str) -> bool {
    std::process::Command::new("/bin/sh").arg("-c").arg(cmd).status().is_ok_and(|s| s.success())
}

/// /sys/class/net/<if>/operstate starts with "up".
fn oper_up(ifname: &str) -> i32 {
    std::fs::read_to_string(format!("/sys/class/net/{ifname}/operstate")).is_ok_and(|s| s.starts_with("up")) as i32
}

impl Source for Live {
    fn pump(&mut self, now_ms: u32) -> bool {
        Live::pump(self, now_ms)
    }
    fn wake_fd(&self) -> i32 { super::background::wake_fd() }
    fn data_fd(&self) -> i32 {
        self.core.fd()
    }
    fn poll(&mut self, _n: u32, a: &Activity) -> Polled {
        let mut p = Polled::default();
        let a = a.clone();
        self.core.set_pace(a.panel_lit);
        let mut ag = self.agent.clone();
        let home = a.home;
        p.scenario = self.scenario.poll(move |s| s.poll(&mut ag, home) as u32) != 0;
        let mut ag = self.agent.clone();
        let mode = a.ni_mode;
        p.netinfo = self.netinfo.poll(move |s| s.poll(&mut ag, mode) as u32) != 0;
        let mut ag = self.agent.clone();
        let diag = a.diag;
        p.diagnose = self.diag.poll(move |s| s.poll(&mut ag, diag) as u32) != 0;
        // The main loop establishes core in the background. Never retry synchronously here.
        if !self.core.refresh().0 { return p; }
        let version = self.core.version();
        if a.lit && !self.core.silent() {
            p.screen = self.sf.poll(move |s| s.poll(version) as u32) != 0;
        }
        let active = a.tailscale;
        p.tailscale = self.ts.poll(move |s| s.poll(active) as u32) != 0;
        // Only this thread owns the delay-test socket; reads are already nonblocking.
        if self.chill.delay_pending() { self.chill.delay_poll(); }
        let mut ag = self.agent.clone();
        let fast = self.chill.home().busy != 0 || a.hx_sending;
        // Home's Tailscale column says how the home exit goes (direct / relay): read there too
        let (page, active) = (a.chill_page || (a.home && a.lit), a.chill);
        let ch = self.chill.poll(move |s| {
            s.home_poll(&mut ag, page, fast) as u32 | ((s.poll(active) as u32) << 1)
        });
        p.chill_home = ch & 1 != 0;
        p.chill = ch & 2 != 0;
        let mut ag = self.agent.clone();
        let (active, prefetch) = (a.esim, a.esim_prefetch);
        let key = self.core.sim_key();
        p.esim = self.esim.poll(move |s| {
            let changed = s.poll(&mut ag, active);
            (changed | (prefetch && s.prefetch(&mut ag, &key))) as u32
        }) != 0;
        let mut ag = self.agent.clone();
        let (active, page) = (a.speed, a.speed_page);
        let speed = self.speed.poll(move |s| {
            s.poll(&mut ag, active) as u32 | ((s.servers_poll(&mut ag, page) as u32) << 1)
        });
        p.speedtest = speed & 1 != 0;
        p.servers = speed & 2 != 0;
        let mut ag = self.agent.clone();
        let sys = a.sys;
        // Home's battery cell: time left / to full
        let bat_on = sys || (a.home && a.lit);
        p.battery = self.bat.poll(move |s| s.poll(&mut ag, bat_on) as u32) != 0;
        let mut ag = self.agent.clone();
        p.versions = self.vers.poll(move |s| s.poll(&mut ag, sys) as u32) != 0;
        let mut ag = self.agent.clone();
        let active = a.home && a.lit;
        p.landing = self.landing.poll(move |s| s.poll(&mut ag, active) as u32) != 0;
        let mut ag = self.agent.clone();
        let active = a.alerts;
        p.alerts = self.alerts.poll(move |s| s.poll(&mut ag, active) as u32) != 0;
        p
    }

    fn take_model(&mut self) -> Option<Rc<Model>> {
        Some(Rc::new(self.build()))
    }

    fn now(&self, _m: &Model) -> i64 {
        util::wall()
    }
    fn boot_s(&self, _m: &Model) -> i64 {
        util::boot_s()
    }
    fn bl_is_on(&self, _m: &Model) -> bool {
        self.bl.is_on()
    }
    fn bl_panel_lit(&self, _m: &Model) -> bool {
        self.bl.panel_lit()
    }
    fn bl_is_lit(&mut self, _m: &Model) -> bool {
        self.bl.is_lit()
    }
    fn bl_get(&self, _m: &Model) -> i32 {
        self.bl.get()
    }
    fn bl_max(&self, _m: &Model) -> i32 {
        self.bl.max()
    }
    fn frames(&self, _m: &Model) -> u64 {
        crate::device::frames()
    }
    fn touch_reports(&self, _m: &Model) -> u64 {
        plat::touch::report_count()
    }
    fn core_version(&self, _m: &Model) -> u64 {
        self.core.version()
    }
    fn health_count(&self, _m: &Model) -> i32 {
        self.alerts.health_count()
    }
    fn sms_delete_armed(&self, _m: &Model, i: usize) -> bool {
        self.core.sms_delete_armed(i)
    }

    fn bl_on(&mut self) {
        crate::device::unblank();
        self.bl.on();
    }
    fn bl_off(&mut self) {
        self.bl.off();
        crate::device::blank();
    }
    fn bl_set(&mut self, level: i32) {
        if level > 0 {
            crate::device::unblank();
        }
        self.bl.set(level);
        if level <= 0 {
            crate::device::blank();
        }
    }
    fn bl_remember(&mut self, level: i32) {
        self.bl.remember(level);
    }

    fn key_poll(&mut self, now_ms: u32) -> i32 {
        self.key.poll(now_ms)
    }
    fn key_fd(&self) -> i32 {
        self.key.fd()
    }
    fn key_held(&self) -> bool {
        self.key.held()
    }

    fn data_set_pace(&mut self, panel_lit: bool) {
        self.core.set_pace(panel_lit);
    }
    fn data_control_fb(&mut self, action: &str, params: &str, fb_action: &str, fb_args: Option<&str>) -> bool {
        self.core.control_fb(action, params, fb_action, fb_args)
    }
    fn data_control_undo(&mut self, action: &str, params: &str) -> bool {
        self.core.control_send_ex(action, params, true)
    }
    fn journal_list(&mut self, _m: &Model) -> Option<String> {
        self.journal()
    }
    fn sms_mark_read_id(&mut self, id: i64) {
        self.core.sms_mark_read_id(id);
    }
    fn sms_mark_all_read(&mut self) {
        self.core.sms_mark_all_read();
    }
    fn sms_delete_id(&mut self, id: i64) {
        self.core.sms_delete_id(id);
    }
    fn sms_delete_arm(&mut self, i: usize) {
        self.core.sms_delete_arm(i);
    }
    fn sms_delete_tap(&mut self, i: usize) -> bool {
        self.core.sms_delete_tap(i)
    }

    fn alerts_mark_all_read(&mut self) {
        self.alerts.mark_all_read(&mut self.agent);
    }
    fn versions_check(&mut self) {
        self.vers.check(&mut self.agent);
    }
    fn esim_select(&mut self, i: usize) -> i32 {
        self.esim.select(&mut self.agent, i)
    }
    fn speedtest_start(&mut self, chill: bool) -> bool {
        self.speed.start(&mut self.agent, chill)
    }
    fn speedtest_stop(&mut self) -> bool {
        self.speed.stop(&mut self.agent)
    }
    fn speedtest_select_server(&mut self, i: i32) {
        self.speed.select_server(i);
    }
    fn scenario_kick(&mut self) {
        self.scenario.kick();
    }
    fn netinfo_act(&mut self, a: NiAct) {
        let ag = &mut self.agent;
        match a {
            NiAct::Scan => self.netinfo.scan(ag),
            NiAct::Auto => self.netinfo.auto(ag),
            NiAct::Register(i) => self.netinfo.register(ag, i),
            NiAct::NbrScan => self.netinfo.nbr_scan(ag),
        }
    }
    fn netinfo_pin(&mut self, id: &str) {
        self.netinfo.pin(&mut self.agent, id);
    }
    fn netinfo_apn_use(&mut self, id: &str) {
        self.netinfo.apn_use(&mut self.agent, id);
    }
    fn netinfo_hurry(&mut self, secs: i32) {
        self.netinfo.hurry(secs);
    }
    fn diagnose_kick(&mut self) {
        self.diag.kick();
    }
    fn diagnose_poll(&mut self, active: bool) -> bool {
        let mut ag = self.agent.clone();
        self.diag.poll(move |s| s.poll(&mut ag, active) as u32) != 0
    }
    fn diagnose_start(&mut self) -> bool {
        self.diag.start(&mut self.agent)
    }
    fn diagnose_speed(&mut self) -> bool {
        self.diag.speed(&mut self.agent)
    }
    fn diagnose_feedback(&mut self, right: bool) -> bool {
        self.diag.feedback(&mut self.agent, right)
    }
    fn scenario_chill_set(&mut self, on: bool) -> bool {
        self.scenario.chill_set(&mut self.agent, on)
    }
    fn chill_set_exit(&mut self, state: &str) -> bool {
        self.chill.set_exit(&mut self.agent, state)
    }
    fn chill_select_node(&mut self, i: usize) -> bool {
        self.chill.select_node(i)
    }
    fn chill_select_group(&mut self, i: usize) -> bool {
        self.chill.select_group(i)
    }
    fn chill_set_profile(&mut self, p: &str) -> bool {
        self.chill.set_profile(&mut self.agent, p)
    }
    fn chill_test_delay(&mut self) -> i32 {
        self.chill.test_delay()
    }
    fn chill_home_set(&mut self, item: usize, on: bool) -> (i32, String) {
        self.chill.home_set(&mut self.agent, item, on)
    }
    fn chill_home_extend(&mut self, _m: &Model) -> Option<i64> {
        self.chill.home_extend(&mut self.agent)
    }
    fn chill_home_read(&mut self) {
        let mut ag = self.agent.clone();
        self.chill.poll(move |s| s.home_poll(&mut ag, true, true) as u32);
    }

    fn exec(&mut self, l: &Launch) -> i32 {
        let argv0 = std::env::args().next().unwrap_or_default();
        crate::exec::exec_self(&argv0, l)
    }
    /// Opening a FIFO for writing with O_NONBLOCK fails (ENXIO) when nobody reads it: exactly
    /// "the screen's owner is not running".
    /// The sentinel's FIFO (Phase 10 R4).
    fn request_vendor(&mut self) -> bool {
        crate::source::SCREEN_CTLS.iter().any(|p| {
            let Ok(c) = std::ffi::CString::new(*p) else { return false };
            // SAFETY: a path we own; the fd is closed below
            let fd = unsafe { libc::open(c.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
            if fd < 0 {
                return false;
            }
            // SAFETY: 7 bytes from a static string; our fd
            let ok = unsafe { libc::write(fd, b"vendor\n".as_ptr().cast(), 7) } == 7;
            // SAFETY: our fd
            unsafe { libc::close(fd) };
            ok
        })
    }
    /// No owner (older installs): this process must let go of /dev/dri/card0 before the vendor
    /// UI can open it: its start in 2 s, and SIGTERM to ourselves.
    fn vendor_fallback(&mut self) {
        system("( sleep 2; /etc/init.d/zte_topsw_devui start ) >/dev/null 2>&1 &");
        // SAFETY: raise on ourselves; the main loop ends cleanly on it
        unsafe { libc::raise(libc::SIGTERM) };
    }
    /// The vendor call the stock UI makes (zte_topsw_mc winds the services down, then a bare
    /// reboot(2) with no sync: sync first); refused: busybox.
    fn power_fallback(&mut self, reboot: bool) {
        let cmd = format!(
            "( sync; ubus -t 10 call zwrt_mc.device.manager {} '{{\"moduleName\":\"zte_topsw_devui\"}}' >/dev/null 2>&1 || {} ) </dev/null >/dev/null 2>&1 &",
            if reboot { "device_reboot" } else { "device_poweroff" },
            if reboot { "reboot" } else { "poweroff" }
        );
        if !system(&cmd) {
            system(if reboot { "reboot" } else { "poweroff" });
        }
    }
    fn power_last_resort(&mut self, reboot: bool) {
        system(if reboot { "reboot" } else { "poweroff" });
    }
    fn lang_hold(&mut self) {
        std::thread::sleep(std::time::Duration::from_millis(LANG_HOLD_MS));
    }
    fn alert_theme_paused(&mut self) {
        // in the scripts' directory since Luya-Z rename D1 (the old copy is gone)
        let lib = "/data/luyaz/scripts/alert-lib.sh";
        if std::fs::metadata(lib).is_err() {
            return;
        }
        if !system(&format!(
            ". {lib} && alert_add ui-theme-paused 'automatic light/dark switching paused until reboot (3 switches within an hour)' >/dev/null 2>&1 &",
        )) {
            eprintln!("ui: could not record the ui-theme-paused alert");
        }
    }
    /// The radios from /sys, Wi-Fi power save from iw (nl80211, not ubus).
    fn aux_read(&mut self) -> Option<(i32, i32, i32)> {
        if let Some(task) = &self.aux {
            match task.take() {
                Ok(result) => { self.aux = None; return Some(result); }
                Err(std::sync::mpsc::TryRecvError::Empty) => return None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.aux = None,
            }
        }
        self.aux = Task::start_notify(read_aux, |_| true);
        None
    }
}

fn read_aux() -> (i32, i32, i32) {
    let (w24, w5) = (oper_up("wlan0"), oper_up("wlan2"));
    let out = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(
            "ps=$(iw dev wlan0 get power_save 2>/dev/null | grep -o 'o[nf]*' | tail -1);\
             [ -z \"$ps\" ] && ps=$(iw dev wlan2 get power_save 2>/dev/null | grep -o 'o[nf]*' | tail -1);\
             echo PSM=$([ \"$ps\" = on ] && echo 1 || echo 0)",
        )
        .output()
        .ok();
    let psm = out
        .and_then(|o| String::from_utf8_lossy(&o.stdout).lines().find_map(|l| l.strip_prefix("PSM=").map(|v| crate::json::atol(v.as_bytes()) as i32)))
        .unwrap_or(-1);
    (w24, w5, psm)
}
