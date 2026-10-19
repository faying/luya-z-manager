//! The 1 s refresh (src/ui_parts/refresh.c refresh_cb): the view model drawn onto every page,
//! in the C order (the status bar reads the RAT label the signal card wrote a second earlier).
// SPDX-License-Identifier: MIT

use super::lock::LK_MODE_V;
use super::{Sub, Ui, TAB_CELL, TAB_HOME, TAB_WIFI};
use crate::lv;
use crate::cf;
use crate::clock;
use crate::kit;
use crate::lang::{pick, tr};
use crate::logic::{self, SimKind, CTL_BUSY, CTL_FULL};
use crate::theme::{t, uf};
use crate::trc;

/// The card in the modem, as the 「SIM 与 eSIM」 row says it (refresh.c s_sim).
#[derive(Clone, PartialEq)]
pub struct Sim {
    pub kind: SimKind,
    pub oper: String,
    pub iccid: String,
    pub imsi: String,
    pub msisdn: String,
}

impl Default for Sim {
    fn default() -> Self {
        Sim { kind: SimKind::None, oper: String::new(), iccid: String::new(), imsi: String::new(), msisdn: String::new() }
    }
}

use super::speed::ST_COMPLETE;
use crate::source::{Activity, NI_APN, NI_CLIENTS, NI_HOME, NI_LITE, NI_OFF, NI_PAGE};

/// Status bar rate: "12.4MB" / "380KB" (bits: "12.4Mb"), or the short form "12M" / "380K"
/// (bits: "12Mb") when space runs out. The unit letter stays in both.
pub fn fmt_rate_top(bps: i64, bits: bool, shortf: bool) -> Vec<u8> {
    let v = if bits { bps as f64 * 8.0 } else { bps as f64 };
    let k = if bits { 1000.0 } else { 1024.0 };
    let u = if bits { "b" } else { "B" };
    if v >= k * k {
        if shortf { cf!("%.0fM%s", v / (k * k), if bits { "b" } else { "" }) } else { cf!("%.1fM%s", v / (k * k), u) }
    } else if v >= k {
        cf!("%.0fK%s", v / k, if shortf && !bits { "" } else { u })
    } else {
        cf!("%.0f%s", v, u)
    }
}

impl Ui {
    /// What is on screen, as each poll's `active` (refresh.c).
    fn activity(&self) -> Activity {
        use super::{TAB_EXIT as EXIT, TAB_SYS};
        // only 运营商选择 reads everything (the agent runs AT+COPS?); others what they show
        let ni_mode = if self.sub_visible(Sub::Net) {
            NI_PAGE
        } else if self.sub_visible(Sub::Apn) || self.tab_visible(TAB_CELL) {
            NI_APN
        } else if self.tab_visible(TAB_WIFI) {
            NI_CLIENTS
        } else if self.sub_visible(Sub::Scene) || self.sub_visible(Sub::Cell) || self.tab_visible(EXIT) {
            NI_LITE
        } else if self.tab_visible(TAB_HOME) {
            NI_HOME
        } else {
            NI_OFF
        };
        Activity {
            panel_lit: self.src.bl_panel_lit(&self.model),
            lit: self.bl_is_on(),
            home: self.tab_visible(TAB_HOME),
            ni_mode,
            tailscale: self.tab_visible(TAB_HOME) || self.tab_visible(EXIT) || self.sub_visible(Sub::Ts),
            chill_page: self.sub_visible(Sub::Chill),
            hx_sending: self.cp.hx_sending >= 0,
            chill: self.tab_visible(TAB_HOME)
                || self.tab_visible(EXIT)
                || self.sub_visible(Sub::Chill)
                || self.sub_visible(Sub::ChillNodes)
                || self.sub_visible(Sub::ChillPairs),
            esim: self.sub_visible(Sub::Esim),
            // the list once per card while 蜂窝 is up and the finger idle (lpac stalls a moment)
            esim_prefetch: self.tab_visible(TAB_CELL) && !self.sub_visible(Sub::Esim) && lv::inactive_ms() > 1500,
            speed: self.sub_visible(Sub::Speed) || self.tab_visible(EXIT),
            speed_page: self.sub_visible(Sub::Speed),
            sys: self.tab_visible(TAB_SYS),
            alerts: self.sub_visible(Sub::Alerts),
            diag: self.sub_visible(Sub::Diag),
        }
    }

    pub fn refresh(&mut self) {
        self.appearance_tick();
        self.refreshes += 1;
        let a = self.activity();
        self.polled = self.src.poll(self.refreshes, &a);
        self.sync();
        let th = t();
        let m = self.model.clone();

        // perf counters (per-second deltas)
        self.perf_paint();

        // wall clock, independent of the backend
        {
            let tm = clock::local(self.now());
            let l = self.top_time;
            self.set(l, cf!("%02d:%02d", tm.hour, tm.min));
        }
        let sc_changed = self.home_scene_changed();
        if self.ex.xm_off_arm.expire(crate::lv::tick_get(), super::exit::XM_ARM_MS) {
            self.exit_menu_refresh();
        }
        self.vendor_arm_tick();
        let ah = &m.agent_health;
        self.alert_st = if ah.lost_secs != 0 {
            2
        } else if ah.unread > 0 {
            1
        } else {
            0
        };
        self.system_health_paint();

        // the network pages only need the /api side, not /v2: before datad's early return
        {
            let mode = a.ni_mode;
            let paint = mode != NI_OFF && mode != NI_HOME;
            let changed = self.polled.netinfo || (paint && self.net.last_mode != mode);
            self.net.last_mode = mode;
            if paint {
                self.net_paint(changed);
            }
        }

        if m.data_ok == 0 {
            // backend down: a device-wide condition, said once in the shared banner
            self.banner_set(Some(tr("后台数据服务不可用").as_bytes()));
            self.home_signal_down();
            self.lk_mode_sync(-1, false); // a tapped 网络模式 still gets its ending
            self.op_refresh();
            if self.sub_visible(Sub::Place) {
                self.place_paint(false);
            }
            self.diag_refresh();
            return;
        }
        // answered before, silent now: keep the last numbers, dimmed, say since when
        let datad_silent = m.backend_silent != 0;
        let v = self.screen_view();
        let d = &m.data;
        if !datad_silent {
            self.usbmode_tick(d); // a phone plugged in: ask (or do the remembered)
        }
        self.dg.net_roam = v.roam != 0;
        self.op_refresh(); // E4: 事务行 / Home's notice line
        if self.sub_visible(Sub::Place) {
            self.place_paint(!datad_silent);
        }
        self.diag_refresh();
        if datad_silent {
            self.banner_set(Some(tr("后台数据服务不可用").as_bytes()));
            self.home_signal_down();
        } else if m.screen_stuck != 0 {
            // E4: answers, but its executor has not moved for 20 s (V2-40)
            self.banner_set(Some(tr("数据服务没响应 · 暂时不能改设置").as_bytes()));
        } else if m.notice_ok != 0 {
            // a write datad turned down (the data side keeps it for 4 s)
            let n = &m.notice;
            let txt = if !n.say_zh.is_empty() {
                pick(&n.say_zh, &n.say_en)
            } else if n.kind == CTL_BUSY {
                tr("正在改别的设置，稍等")
            } else if n.kind == CTL_FULL {
                tr("数据服务忙 · 没改，稍后再试")
            } else {
                tr("没改成 · 数据服务回了错误")
            };
            self.banner_set(Some(txt.as_bytes()));
        } else if let Some(b) = self.usbmode_banner() {
            self.banner_set(Some(&b)); // 已按记住的方式：…
        } else if ah.lost_secs != 0 {
            self.banner_set(Some(&cf!(tr("管理后台失联 %ld 分钟"), ah.lost_secs / 60)));
        } else if d.bat_temp >= 50 {
            self.banner_set(Some(tr("电池过热，拔掉充电器放到阴凉处").as_bytes()));
        } else if d.bat_percent <= 10 && !(d.charger_connect != 0 && d.chg_uv > 1000000) {
            self.banner_set(Some(tr("电量低，尽快充电").as_bytes()));
        } else {
            self.banner_set(None);
        }

        // status bar: the same tiers as Home's 信号强/中/弱 (4–5 green, 3 orange, 1–2 red)
        {
            let tier = v.bars_tier;
            // tier -1 with bars > 0 = datad has not graded it: grey, not red
            let sig_col = match tier {
                2 => th.green,
                1 => th.orange,
                0 => th.red,
                _ => th.track,
            };
            for i in 0..5 {
                let col = if (i as i32) < d.bars { sig_col } else { th.track };
                if col != self.c_sig[i] {
                    self.c_sig[i] = col;
                    kit::bg(self.top_sig[i], col);
                }
            }
            // the phone-style label needs the carrier counts: the signal card writes it
            let label = self.top_label.clone();
            let net = self.top_net;
            self.set(net, &label);
            // 「无服务」 needs the CJK font: the numeral font has no Chinese
            net.set_style_text_font(if label.as_bytes().first().is_some_and(|&c| c >= 0x80) { uf().cj12 } else { uf().n12 }, 0);
            let bits = self.conf.speed_bits;
            let (dn, up) = (fmt_rate_top(d.rx_speed, bits, false), fmt_rate_top(d.tx_speed, bits, false));
            let (dn2, up2) = (fmt_rate_top(d.rx_speed, bits, true), fmt_rate_top(d.tx_speed, bits, true));
            let full = cf!("↓%s ↑%s", &dn, &up);
            let shrt = cf!("↓%s ↑%s", &dn2, &up2);
            let down = cf!("↓%s", &dn2);
            // charging = charger_connect only: d.charging is a status code, truthy almost always
            let chg = d.charger_connect != 0 && d.chg_uv > 1000000;
            if d.bat_percent != self.c_pct || chg as i32 != self.c_chg {
                self.c_pct = d.bat_percent;
                self.c_chg = chg as i32;
                kit::battery_set(&self.top_batt, d.bat_percent, chg);
                kit::show(self.top_batt.body, true);
                kit::show(self.top_batt.nub, true);
            }
            let alert_col = if self.alert_st == 2 { th.bad_t } else { th.warn_t };
            self.statusbar_layout(&full, &shrt, &down, d.bat_percent, chg, self.alert_st != 0, alert_col);
        }

        self.home_signal_paint(&v, datad_silent);
        self.home_exit_paint();
        self.home_scene_paint(sc_changed);
        if self.tab_visible(TAB_HOME) || self.tab_visible(super::TAB_EXIT) || self.sub_visible(super::Sub::Ts) {
            self.home_ts_paint();
        }
        // CHILL: the card (Home's, on the exit tab) with the notice on its top, and the pages
        self.chill_refresh();
        self.home_reflow();
        let sim_changed = self.sim_refresh();
        self.esim_refresh(sim_changed);
        self.speed_paint();
        self.system_paint();
        self.charts_paint(datad_silent);
        self.sms_paint();
        if self.polled.alerts {
            self.alerts_paint();
        }
        self.sig_paint(&v.mode_word);

        // 锁频 and the 网络模式 row: what the device is on (TCHGWL_5G and NETWORK_auto are
        // automatic too: datad says which are)
        {
            let mut sel = LK_MODE_V.iter().rposition(|&x| x == d.net_select).map_or(-1, |i| i as i32);
            if v.mode_auto != 0 {
                sel = 0;
            }
            self.lock_refresh(&d.sa_sup, &d.sa_bands, &d.nsa_sup, &d.nsa_bands, &d.lte_sup, &d.lte_bands, sel);
        }
        self.tile_rows_paint();

        // Wi-Fi (the radios and power save are read while Wi-Fi, 系统 or 蜂窝 is up: M5)
        let aux_on = self.tab_visible(TAB_WIFI) || self.tab_visible(super::TAB_SYS) || self.tab_visible(TAB_CELL);
        self.aux_refresh(aux_on, d);
        self.md_refresh();
        self.refresh_wifi(d);
        {
            let a = &self.aux;
            let st = [a.w24 == 1 || a.w5 == 1, a.w24 == 1, a.w5 == 1, a.psm == 1, d.nfc_switch != 0];
            let pool = a.pool.clone();
            let dps = a.dps;
            let w = self.wifi.clone();
            for (i, &on) in st.iter().enumerate() {
                super::cellular::sw_apply(w.sw[i], on);
                self.set(w.sw_st[i], if on { tr("已开启") } else { tr("已关闭") });
            }
            if !pool.is_empty() {
                self.set_at(w.pool, 1, &pool);
            }
            let s = self.sys.clone();
            super::cellular::sw_apply(s.dps_sw, dps == 1);
            self.set(s.dps_st, if dps < 0 { "—" } else if dps != 0 { tr("已开启") } else { tr("已关闭") });
            // whichever of {this switch, the status-bar tap} was touched last
            super::cellular::sw_apply(s.speedunit_sw, self.conf.speed_bits);
            s.speedunit_st.set_text(if self.conf.speed_bits { "Mbps" } else { "MB/s" });
        }
    }

    /// The card in the modem (the eSIM section's reading); true when it changed.
    fn sim_refresh(&mut self) -> bool {
        let m = self.model.clone();
        let d = &m.data;
        // the agent's home operator only while it is this card's: 网络 pages poll it, the eSIM
        // page does not, so after a switch it is the previous card's (2026-10-11)
        let n_home = m
            .netinfo
            .as_ref()
            .filter(|n| !n.home.mcc.is_empty() && d.sim_imsi.starts_with(&format!("{}{}", n.home.mcc, n.home.mnc)))
            .map(|n| n.home.name.clone())
            .unwrap_or_default();
        let enabled = m.esim_enabled_iccid.as_deref().unwrap_or("");
        let k = logic::sim_kind(&d.sim_state, &d.sim_iccid, enabled);
        // then the card's own name (SPN), and the network's name last (roaming: not the card's)
        let op = [n_home, d.sim_spn.clone(), d.operator_name.clone()].into_iter().find(|s| !s.is_empty()).unwrap_or_default();
        // no ICCID from the modem: an eSIM's is the enabled profile's
        let iccid = if d.sim_iccid.is_empty() && k == logic::SimKind::Esim { enabled.to_string() } else { d.sim_iccid.clone() };
        let sim = Sim { kind: k, oper: op, iccid, imsi: d.sim_imsi.clone(), msisdn: d.sim_msisdn.clone() };
        let changed = sim != self.sim;
        self.sim = sim;
        changed
    }

    /// The small words right of the 「›」 rows on 蜂窝 / 出口.
    fn tile_rows_paint(&mut self) {
        let th = t();
        let m = self.model.clone();
        let d = &m.data;
        let ts = self.tile_sub.clone();
        // 开着 (CHILL running) = green; unread SMS = accent (the old tiles' dot and badge)
        let chill_on = m.chill_online != 0;
        if self.c_tile_on[0] != chill_on as i32 {
            self.c_tile_on[0] = chill_on as i32;
            kit::text_color(ts[Sub::Chill as usize], if chill_on { th.ok_t } else { th.t2 });
        }
        let unread = d.sms_unread > 0;
        if self.c_tile_on[1] != unread as i32 {
            self.c_tile_on[1] = unread as i32;
            kit::text_color(ts[Sub::Sms as usize], if unread { th.acc_t } else { th.t2 });
        }
        if d.sms_unread != 0 {
            self.set(ts[Sub::Sms as usize], cf!(tr("%d 条 · %d 未读"), d.sms_n, d.sms_unread));
        } else {
            self.set(ts[Sub::Sms as usize], cf!(trc!("短信", "%d 条"), d.sms_n));
        }
        self.set(
            ts[Sub::Chill as usize],
            cf!("%s · %s", m.chill_core.as_deref().unwrap_or(""), m.chill_mode.as_deref().unwrap_or("")),
        );
        // a physical SIM says 「SIM 卡 · operator」, an eSIM 「eSIM · profile」
        let sim = self.sim.clone();
        let t6 = match sim.kind {
            SimKind::Esim => cf!("eSIM · %s", m.esim_current.as_deref().unwrap_or("")),
            SimKind::Plain => cf!(tr("SIM 卡 · %s"), if sim.oper.is_empty() { tr("已插入") } else { sim.oper.as_str() }),
            SimKind::None => tr("没插卡").into(),
        };
        self.set(ts[Sub::Esim as usize], t6);
        let lw = self.lock_word();
        self.set(ts[Sub::Lock as usize], lw);
        let mut t4: Vec<u8> = if m.st_agent_reachable == 0 {
            tr("服务不可用").into()
        } else if m.st_running != 0 {
            m.st_phase_label.clone().unwrap_or_default().into()
        } else if m.st_phase == ST_COMPLETE {
            let (dn, up) = (m.st_download_mbps.unwrap_or(-1.0), m.st_upload_mbps.unwrap_or(-1.0));
            cf!("↓ %.0f · ↑ %.0f Mbps", if dn >= 0.0 { dn } else { 0.0 }, if up >= 0.0 { up } else { 0.0 })
        } else {
            tr("点击测速").into()
        };
        self.set(ts[Sub::Speed as usize], &t4);
        // the same row on 蜂窝 (from the 40-byte cache, as C)
        t4.truncate(39);
        let cs = self.cell.speed_sub;
        if !cs.is_null() && cs.text() != t4 {
            cs.set_text(&t4);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bar_rates() {
        // the good scene: 1532000 / 188000 B/s, bits
        assert_eq!(fmt_rate_top(1532000, true, false), b"12.3Mb");
        assert_eq!(fmt_rate_top(188000, true, false), b"1.5Mb");
        assert_eq!(fmt_rate_top(1532000, true, true), b"12Mb");
        assert_eq!(fmt_rate_top(1532000, false, false), b"1.5MB");
        assert_eq!(fmt_rate_top(188000, false, true), b"184K");
        assert_eq!(fmt_rate_top(900, false, false), b"900B");
    }
}
