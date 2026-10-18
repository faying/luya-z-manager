//! src/ui_parts/speedtest.c and refresh.c's 「Speedtest subpage」: 测速 on luyaz-core's own speed
//! test. Also 性能测试 (system.c build_sub_perf, cellular.c bench_cb).
// SPDX-License-Identifier: MIT

use super::{Sub, Ui, SUB_VIEW};
use crate::cf;
use crate::kit::{self, BtnKind, CARD_W, MARGIN, PAD, R_CARD, ROW_H, SYMBOL_OK};
use crate::lang::tr;
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};

/// ST_MAX_SERVERS plus Auto, Ookla and LibreSpeed choices.
const ST_SRV_ROWS: usize = 24 + 3;
const ST_IDLE: i32 = 0;
const ST_DOWNLOAD: i32 = 2;
const ST_UPLOAD: i32 = 3;
pub const ST_COMPLETE: i32 = 4;
const ST_ERROR: i32 = 6;

#[derive(Default)]
pub struct Speed {
    phase: Obj,
    live: Obj,
    unit: Obj,
    detail: Obj,
    result: Obj,
    server: Obj,
    btn: Obj,
    btn_lbl: Obj,
    offline: Obj,
    srv_card: Obj,
    srv_hint: Obj,
    srv_row: Vec<Obj>,
    srv_name: Vec<Obj>,
    srv_ok: Vec<Obj>,
    /// the last start was refused: why stays on the card a while
    refused_at: u32,
    // 性能测试
    fps: Obj,
    touch: Obj,
    tbox: Obj,
    box_x: i32,
    box_dir: i32,
    last_f: u64,
    last_r: u64,
}

unsafe extern "C" fn speedtest_btn_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.speedtest_btn());
}

unsafe extern "C" fn speedtest_srv_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.speedtest_srv(i));
}

/// The benchmark: moves the box and draws the whole page, once per timer call.
pub(super) unsafe extern "C" fn bench_cb(_t: *mut sys::lv_timer_t) {
    super::with(|u| u.bench_step());
}

impl Ui {
    pub(super) fn build_sub_speed(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = kit::scroll(p, 0, SUB_VIEW, 4 + 280 + 10 + 20 + ST_SRV_ROWS as i32 * ROW_H + 86);
        let c = kit::card(sc, MARGIN, 4, CARD_W, 280);
        let s = &mut self.st;
        kit::label(c, f.cj12, th.t3, PAD, 10, tr("网络测速"));
        s.phase = kit::label_r(c, f.cj12, th.t2, CARD_W - PAD, 10, "");
        s.live = kit::label(c, f.n36, th.t1, PAD, 30, "--");
        s.unit = kit::label(c, f.n15, th.t3, 100, 50, "Mbps");
        s.detail = kit::label_w(c, f.cj13, th.t2, PAD, 82, CARD_W - 2 * PAD, false, "");
        s.result = kit::label_w(c, f.n15, th.t1, PAD, 104, CARD_W - 2 * PAD, false, "");
        s.server = kit::label_w(c, f.cj12, th.t3, PAD, 128, CARD_W - 2 * PAD, false, "");
        (s.btn, s.btn_lbl) = kit::button(c, PAD, 160, CARD_W - 2 * PAD, 42, tr("开始测速"), BtnKind::Primary, Some(speedtest_btn_cb), 0);
        s.offline = kit::label_w(c, f.cj13, th.bad_t, PAD, 190, CARD_W - 2 * PAD, true, "");
        kit::show(s.offline, false);

        kit::section(sc, 4 + 280 + 10, tr("服务器"));
        s.srv_hint = kit::label_w(sc, f.cj12, th.t3, MARGIN + PAD, 4 + 280 + 10 + 20, CARD_W - 2 * PAD, true, "");
        s.srv_card = kit::card(sc, MARGIN, 4 + 280 + 10 + 90, CARD_W, ST_SRV_ROWS as i32 * ROW_H);
        for i in 0..ST_SRV_ROWS {
            let row = kit::bx(s.srv_card, 0, i as i32 * ROW_H, CARD_W, ROW_H, th.card, 0);
            row.set_style_bg_opa(lv::OPA_TRANSP, 0);
            kit::tappable(row, Some(speedtest_srv_cb), i);
            if i > 0 {
                kit::sep(row, 0);
            }
            s.srv_ok.push(kit::label(row, lv::montserrat(14), th.acc_t, PAD, 12, SYMBOL_OK));
            s.srv_name.push(kit::label_w(row, f.cj14, th.t1, PAD + 22, 11, CARD_W - 2 * PAD - 22, false, ""));
            if i > 2 {
                kit::show(row, false); // shown once the list arrives
            }
            s.srv_row.push(row);
        }
        s.srv_name[0].set_text(tr("自动（含备用来源）"));
        s.srv_name[1].set_text("Ookla");
        s.srv_name[2].set_text("LibreSpeed");
        sc.scroll_to_y(0);
    }

    pub(super) fn build_sub_perf(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let c = kit::card(p, MARGIN, 8, CARD_W, 120);
        kit::label(c, f.cj12, th.t3, PAD, 10, tr("渲染刷新率"));
        self.st.fps = kit::label(c, f.n20, th.ok_t, PAD, 28, "-- FPS");
        kit::label(c, f.cj12, th.t3, PAD, 62, tr("触控上报率"));
        self.st.touch = kit::label(c, f.n20, th.warn_t, PAD, 80, "-- Hz");
        self.st.tbox = kit::bx(p, MARGIN, 150, CARD_W, 76, th.green, R_CARD);
        self.st.box_dir = 1;
    }

    fn bench_step(&mut self) {
        let s = &mut self.st;
        s.box_x += s.box_dir * 4;
        if s.box_x >= 16 {
            s.box_x = 16;
            s.box_dir = -1;
        }
        if s.box_x <= 0 {
            s.box_x = 0;
            s.box_dir = 1;
        }
        s.tbox.set_pos(MARGIN + s.box_x, 150);
        self.sub_page[Sub::Perf as usize].invalidate(); // a full-page redraw
        lv::refr_now(); // render it now: counts a frame
    }

    /// The perf counters (per-second deltas). M5: the frame and touch counts come from the
    /// device loop; the C test's are 0.
    pub(super) fn perf_paint(&mut self) {
        let (f, r) = (self.src.frames(&self.model), self.src.touch_reports(&self.model));
        let (fps, touch) = (self.st.fps, self.st.touch);
        let (lf, lr) = (self.st.last_f, self.st.last_r);
        self.set(fps, cf!("%lu FPS", f.wrapping_sub(lf)));
        self.set(touch, cf!("%lu Hz", r.wrapping_sub(lr)));
        self.st.last_f = f;
        self.st.last_r = r;
    }

    fn speedtest_btn(&mut self) {
        if self.model.st_running != 0 {
            self.src.speedtest_stop();
            self.sync();
            return;
        }
        let l = self.st.btn_lbl;
        l.set_text(tr("已发送…")); // the request is synchronous: answer first
        lv::refr_now();
        let ok = self.src.speedtest_start();
        self.sync();
        if ok {
            self.st.refused_at = 0;
            return;
        }
        let th = t();
        self.st.refused_at = lv::tick_get().max(1);
        l.set_text(tr("开始测速"));
        let off = self.st.offline;
        off.set_text(self.model.st_start_error.as_deref().unwrap_or(""));
        kit::text_color(off, th.warn_t);
        off.set_pos(PAD, 206);
        kit::show(off, true);
    }

    fn speedtest_srv(&mut self, i: usize) {
        self.src.speedtest_select_server(if i < 3 { -(i as i32) - 1 } else { i as i32 - 3 });
        self.sync();
        // the tick moves at once, not at the next refresh
        for (k, &ok) in self.st.srv_ok.iter().enumerate() {
            kit::show(ok, k == i);
        }
    }

    /// refresh.c 「Speedtest subpage」: the progress when speedtest_poll read it, the server
    /// list when speedtest_servers_poll did, the tick every time.
    pub(super) fn speed_paint(&mut self) {
        let th = t();
        let m = self.model.clone();
        let s = &self.st;
        let (phase_l, live, unit, detail, result, server, btn, btn_lbl, offline) =
            (s.phase, s.live, s.unit, s.detail, s.result, s.server, s.btn, s.btn_lbl, s.offline);
        if !self.polled.speedtest {
        } else if m.st_agent_reachable == 0 {
            for o in [live, detail, result, server, btn] {
                kit::show(o, false);
            }
            kit::show(offline, true);
            offline.set_pos(PAD, 32);
            kit::show(unit, false);
            phase_l.set_text("");
            offline.set_text(tr("设备服务没有回应，请稍后重试。"));
        } else {
            for o in [live, detail, result, server, btn] {
                kit::show(o, true);
            }
            kit::show(unit, true);
            let ph = m.st_phase;
            let dl = m.st_download_mbps.unwrap_or(-1.0);
            let ul = m.st_upload_mbps.unwrap_or(-1.0);
            let ping = m.st_ping_ms.unwrap_or(-1.0);
            let jitter = m.st_jitter_ms.unwrap_or(-1.0);
            self.set(phase_l, m.st_phase_label.as_deref().unwrap_or(""));
            if ph == ST_DOWNLOAD || ph == ST_UPLOAD {
                self.set(live, cf!("%.1f", m.st_live_mbps.unwrap_or(-1.0)));
            } else if ph == ST_COMPLETE {
                self.set(live, cf!("%.1f", if dl >= 0.0 { dl } else { 0.0 }));
            } else {
                self.set(live, "--");
            }
            if ping >= 0.0 {
                self.set(detail, cf!(tr("延迟 %.0fms · 抖动 %.0fms"), ping, if jitter >= 0.0 { jitter } else { 0.0 }));
            } else {
                self.set(detail, if ph == ST_IDLE && dl < 0.0 { tr("还没测过") } else { "" });
            }
            if dl >= 0.0 || ul >= 0.0 {
                self.set(result, cf!("↓ %.1f Mbps  ↑ %.1f Mbps", if dl >= 0.0 { dl } else { 0.0 }, if ul >= 0.0 { ul } else { 0.0 }));
            } else {
                self.set(result, "");
            }
            self.set(server, m.st_server.as_deref().unwrap_or(""));
            let running = m.st_running != 0;
            btn_lbl.set_text(if running {
                tr("停止")
            } else if ph == ST_COMPLETE {
                tr("重新测速")
            } else {
                tr("开始测速")
            });
            kit::bg(btn, if running { th.fill_red } else { th.fill_blue });
            live.update_layout();
            unit.set_x(PAD + live.width() + 4);
            let ra = self.st.refused_at;
            let err = m.st_error.as_deref().unwrap_or("");
            if ra != 0 && lv::tick_get().wrapping_sub(ra) < 6000 && !running {
                // refused at the start (the tap wrote why): leave it up a while
            } else if ph == ST_ERROR && !err.is_empty() {
                self.st.refused_at = 0;
                kit::show(offline, true);
                offline.set_pos(PAD, 206);
                kit::text_color(offline, th.bad_t);
                offline.set_text(err);
            } else {
                self.st.refused_at = 0;
                kit::show(offline, false);
            }
        }

        // the server list (only while the page is up)
        if self.polled.servers {
            let n = m.st_servers.len();
            self.set(self.st.srv_hint, m.st_servers_error.as_deref().unwrap_or(""));
            for i in 3..ST_SRV_ROWS {
                let (row, name) = (self.st.srv_row[i], self.st.srv_name[i]);
                if i - 3 >= n {
                    kit::show(row, false);
                    continue;
                }
                let sv = &m.st_servers[i - 3];
                kit::show(row, true);
                self.set(name, cf!("%s · %s", if sv.provider == "librespeed" { "LibreSpeed" } else { "Ookla" }, &sv.name));
            }
            self.st.srv_card.set_height((n as i32 + 3) * ROW_H);
        }
        let sel = m.st_selected_index;
        for (i, &ok) in self.st.srv_ok.iter().enumerate() {
            let is_sel = (sel < 0 && i as i32 == -sel - 1) || (sel >= 0 && i as i32 == sel + 3);
            kit::show(ok, is_sel);
        }
    }
}
