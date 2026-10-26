//! 测速 on luyaz-core's own speed
//! test. Also 性能测试.
//!
//! The v2 engine (net-performance.md Design Review DR1–DR13): the number, one conclusion line,
//! the 「分层」 rows under the card, step words while it runs, the roaming confirm and the
//! 「也测经 CHILL」 switch. An old core (no method_version) gets the page as it was (DR4).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::{Sub, Ui, SUB_VIEW};
use crate::cf;
use crate::kit::{self, BtnKind, CARD_W, MARGIN, PAD, R_CARD, ROW_H, SYMBOL_OK};
use crate::lang::{self, tr, trn};
use crate::logic::Arm;
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};
use std::collections::HashMap;
use ui_model::speedtest_v2_t;

/// ST_MAX_SERVERS plus Auto, Ookla and LibreSpeed choices.
const ST_SRV_ROWS: usize = 24 + 3;
const ST_IDLE: i32 = 0;
const ST_DOWNLOAD: i32 = 2;
const ST_UPLOAD: i32 = 3;
pub const ST_COMPLETE: i32 = 4;
const ST_ERROR: i32 = 6;
const ST_ACTION_Y: i32 = 160;
const ST_ACTION_H: i32 = 42;
/// Same heading-to-content interval as the other touch pages.
const ST_SECTION_H: i32 = 20;
/// 「分层」: single, proxy, queue, radio, cell_wifi (the contract's fixed five)
const ST_LAYERS: usize = 5;
/// the roaming confirm, as on the diagnosis page
const ST_ARM_MS: u32 = 5000;
/// a layer row with its value on a second line
const ST_ROW2_H: i32 = 54;

#[derive(Default)]
pub struct Speed {
    scroll: Obj,
    result_card: Obj,
    phase: Obj,
    live: Obj,
    unit: Obj,
    detail: Obj,
    result: Obj,
    server: Obj,
    btn: Obj,
    btn_lbl: Obj,
    offline: Obj,
    srv_section: Obj,
    srv_card: Obj,
    srv_hint: Obj,
    srv_row: Vec<Obj>,
    srv_name: Vec<Obj>,
    srv_ok: Vec<Obj>,
    /// the last start was refused: why stays on the card a while
    refused_at: u32,
    // ---- v2 ----
    /// the conclusion line: the mark in its state colour, the words in body colour (DR1)
    concl_mark: Obj,
    concl: Obj,
    /// 「也测经 CHILL」 (DR12): the row, its name, why it is off, the switch
    chill_row: Obj,
    chill_name: Obj,
    chill_why: Obj,
    chill_sw: Obj,
    /// asked for the next run only; back to off once it starts
    chill_want: bool,
    /// the cost line under the button (DR13), or the roaming one (DR6)
    hint: Obj,
    /// roaming: the first tap arms, the second starts (DR6)
    arm: Arm,
    /// the y the button is at now (old core: ST_ACTION_Y)
    action_y: i32,
    layer_sec: Obj,
    layer_row: Vec<Obj>,
    layer_mark: Vec<Obj>,
    layer_name: Vec<Obj>,
    layer_val: Vec<Obj>,
    layer_val2: Vec<Obj>,
    layer_go: Vec<Obj>,
    /// where a row's tap goes (DR9)
    layer_to: [Option<Sub>; ST_LAYERS],
    /// the colours set last, per object: a refresh that changes nothing redraws nothing
    cols: HashMap<usize, u32>,
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

unsafe extern "C" fn speedtest_chill_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.speedtest_chill());
}

unsafe extern "C" fn speedtest_layer_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.speedtest_layer(i));
}

/// The benchmark: moves the box and draws the whole page, once per timer call.
pub(super) unsafe extern "C" fn bench_cb(_t: *mut sys::lv_timer_t) {
    super::with(|u| u.bench_step());
}

/// A layer's row name (the contract's keys).
fn layer_name(key: &str) -> &'static str {
    match key {
        "single" => tr("单连接"),
        "proxy" => tr("代理"),
        "queue" => tr("下载时卡"),
        "radio" => tr("无线"),
        _ => tr("基站、Wi-Fi"),
    }
}

/// The step word's stage (DR7).
fn stage_name(stage: &str) -> &'static str {
    match stage {
        "idle_latency" => tr("空闲延迟"),
        "single_download" => tr("单连接下载"),
        "multi_download" => tr("多连接下载"),
        "chill_download" => tr("经 CHILL 下载"),
        "multi_upload" => tr("多连接上传"),
        _ => "",
    }
}

/// The run's mobile data, MB or GB.
fn used(bytes: i64) -> Vec<u8> {
    let mb = bytes as f64 / 1e6;
    if mb >= 1000.0 { cf!(tr("用掉 %.2f GB"), mb / 1000.0) } else { cf!(tr("用掉 %.0f MB"), mb) }
}

/// The line where the conclusion goes: mark (with its colour) and words. None = no line.
fn conclusion(ph: i32, v: &speedtest_v2_t) -> Option<(&'static str, u32, Vec<u8>)> {
    let th = t();
    if ph == ST_DOWNLOAD || ph == ST_UPLOAD || (ph == 1 && !v.stage.is_empty()) {
        // running: how this stage measures (DR7)
        let n = v.connections.max(1);
        let ramp = |n: i32| {
            if v.mode == "coarse" { cf!(trn("%d 条连接 · 前 0.5 秒不计", n as i64), n) } else { cf!(trn("%d 条连接 · 前 2 秒不计", n as i64), n) }
        };
        return match v.stage.as_str() {
            "single_download" => Some(("", th.t2, ramp(1))),
            "multi_download" | "chill_download" => Some(("", th.t2, ramp(n))),
            "multi_upload" => Some(("", th.t2, cf!(trn("%d 条连接上传", n as i64), n))),
            _ => None,
        };
    }
    if ph != ST_COMPLETE {
        return None;
    }
    if v.sample_short {
        return Some(("▲", th.warn_t, tr("样本不足，数字只供参考").into()));
    }
    if v.roaming_capped {
        return Some(("●", th.t3, tr("粗测 · 漫游中只测 5 秒").into()));
    }
    if v.mode == "coarse" {
        return Some(("●", th.t3, tr("粗测 · 只测 5 秒").into()));
    }
    let vd = v.verdict.as_ref()?;
    let txt = lang::pick(&vd.summary_zh, &vd.summary_en).as_bytes().to_vec();
    Some(if vd.bottlenecks.is_empty() { ("●", th.ok_t, txt) } else { ("▲", th.warn_t, txt) })
}

impl Ui {
    pub(super) fn build_sub_speed(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = kit::scroll(p, 0, SUB_VIEW, SUB_VIEW);
        let c = kit::card(sc, MARGIN, 4, CARD_W, ST_ACTION_Y + ST_ACTION_H + PAD);
        let s = &mut self.st;
        s.scroll = sc;
        s.result_card = c;
        s.action_y = ST_ACTION_Y;
        kit::label(c, f.cj12, th.t3, PAD, 10, tr("网络测速"));
        s.phase = kit::label_r(c, f.cj12, th.t2, CARD_W - PAD, 10, "");
        s.live = kit::label(c, f.n36, th.t1, PAD, 30, "--");
        s.unit = kit::label(c, f.n15, th.t3, 100, 50, "Mbps");
        s.detail = kit::label_w(c, f.cj13, th.t2, PAD, 82, CARD_W - 2 * PAD, false, "");
        s.result = kit::label_w(c, f.n15, th.t1, PAD, 104, CARD_W - 2 * PAD, false, "");
        s.server = kit::label_w(c, f.cj12, th.t3, PAD, 128, CARD_W - 2 * PAD, false, "");
        // v2: the conclusion line, the CHILL switch row and the cost line (hidden for an old core)
        s.concl_mark = kit::label(c, f.cj17b, th.t3, PAD, 80, "");
        s.concl = kit::label_w(c, f.cj17b, th.t1, PAD + 22, 80, CARD_W - 2 * PAD - 22, false, "");
        s.chill_row = kit::bx(c, 0, 0, CARD_W, ROW_H, th.card, 0);
        s.chill_row.set_style_bg_opa(lv::OPA_TRANSP, 0);
        s.chill_name = kit::label(s.chill_row, f.cj14, th.t1, PAD, 11, tr("也测经 CHILL"));
        s.chill_why = kit::label_r(s.chill_row, f.cj12, th.t3, CARD_W - PAD - 44 - 8, 13, "");
        s.chill_sw = kit::toggle(s.chill_row, CARD_W - PAD, 7, Some(speedtest_chill_cb), 0);
        s.hint = kit::label_w(c, f.cj12, th.t2, PAD, 0, CARD_W - 2 * PAD, false, "");
        for o in [s.concl_mark, s.concl, s.chill_row, s.hint] {
            kit::show(o, false);
        }
        (s.btn, s.btn_lbl) = kit::button(c, PAD, ST_ACTION_Y, CARD_W - 2 * PAD, ST_ACTION_H, tr("开始测速"), BtnKind::Primary, Some(speedtest_btn_cb), 0);
        s.offline = kit::label_w(c, f.cj13, th.bad_t, PAD, ST_ACTION_Y + ST_ACTION_H + MARGIN, CARD_W - 2 * PAD, true, "");
        kit::show(s.offline, false);

        // 「分层」 (DR2): a display area, not a card; one row per layer, 40 high
        s.layer_sec = kit::section(sc, 0, tr("分层"));
        kit::show(s.layer_sec, false);
        for i in 0..ST_LAYERS {
            let row = kit::bx(sc, MARGIN, 0, CARD_W, ROW_H, th.bg, 0);
            row.set_style_bg_opa(lv::OPA_TRANSP, 0);
            kit::tappable(row, Some(speedtest_layer_cb), i);
            row.remove_flag(lv::LV_OBJ_FLAG_CLICKABLE); // only the rows that lead somewhere (DR9)
            if i > 0 {
                kit::sep(row, 0);
            }
            s.layer_mark.push(kit::label(row, f.cj14, th.t3, PAD, 11, ""));
            s.layer_name.push(kit::label(row, f.cj14, th.t1, PAD + 18, 11, ""));
            s.layer_val.push(kit::label_r(row, f.cj14, th.t1, CARD_W - PAD, 11, ""));
            s.layer_val2.push(kit::label_w(row, f.cj12, th.t2, PAD + 18, 32, CARD_W - 2 * PAD - 18, false, ""));
            s.layer_go.push(kit::label_r(row, f.cj14, th.t3, CARD_W - PAD, 11, "›"));
            kit::show(s.layer_val2[i], false);
            kit::show(s.layer_go[i], false);
            kit::show(row, false);
            s.layer_row.push(row);
        }

        s.srv_section = kit::section(sc, 0, tr("服务器"));
        s.srv_hint = kit::label_w(sc, f.cj12, th.t3, MARGIN + PAD, 0, CARD_W - 2 * PAD, true, "");
        s.srv_card = kit::card(sc, MARGIN, 0, CARD_W, 3 * ROW_H);
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
        self.speed_reflow();
        sc.scroll_to_y(0);
    }

    /// A colour only when it changed (a style write redraws even when it is the same).
    fn st_col(&mut self, o: Obj, col: u32) {
        if self.st.cols.insert(o.0 as usize, col) != Some(col) {
            kit::text_color(o, col);
        }
    }

    fn st_bg(&mut self, o: Obj, col: u32) {
        // keyed apart from the text colour of the same object
        if self.st.cols.insert(o.0 as usize ^ 1, col) != Some(col) {
            kit::bg(o, col);
        }
    }

    /// Result → action/recovery → 「分层」 → server choices. Empty hints reserve no space;
    /// wrapped recovery text and the actual list height determine the scroll end.
    fn speed_reflow(&self) {
        let s = &self.st;
        let mut bottom = if s.btn.hidden() { 32 } else { s.action_y + ST_ACTION_H };
        if !s.btn.hidden() && !s.hint.hidden() {
            s.hint.set_y(bottom + 6);
            bottom += 6 + uf().cj12.line_height();
        }
        if !s.offline.hidden() && !s.offline.text().is_empty() {
            let y = if s.btn.hidden() { 32 } else { bottom + MARGIN };
            s.offline.set_y(y);
            s.offline.update_layout();
            bottom = y + s.offline.height();
        }
        s.result_card.set_height(bottom + PAD);
        let mut y = 4 + bottom + PAD + MARGIN;
        if !s.layer_sec.hidden() {
            s.layer_sec.set_y(y);
            y += ST_SECTION_H;
            for &row in &s.layer_row {
                if row.hidden() {
                    continue;
                }
                row.set_y(y);
                y += row.style_height(0);
            }
            y += MARGIN;
        }
        s.srv_section.set_y(y);
        y += ST_SECTION_H;
        let hint = !s.srv_hint.text().is_empty();
        kit::show(s.srv_hint, hint);
        if hint {
            s.srv_hint.set_y(y);
            s.srv_hint.update_layout();
            y += s.srv_hint.height() + MARGIN;
        }
        s.srv_card.set_y(y);
        kit::scroll_extent(s.scroll, y + s.srv_card.style_height(0) + PAD);
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

    /// The switch can be turned on now: v2, idle, CHILL running, not roaming (DR12).
    fn speed_chill_ok(&self) -> bool {
        self.model.st_v2.is_some() && self.model.st_running == 0 && self.model.chill_online != 0 && !self.dg.net_roam
    }

    fn speedtest_chill(&mut self) {
        let on = self.st.chill_sw.has_state(lv::LV_STATE_CHECKED);
        self.st.chill_want = on && self.speed_chill_ok();
        self.speed_paint();
    }

    fn speedtest_layer(&mut self, i: usize) {
        if let Some(to) = self.st.layer_to.get(i).copied().flatten() {
            self.sub_open_child(to, Sub::Speed);
        }
    }

    fn speedtest_btn(&mut self) {
        if self.model.st_running != 0 {
            self.src.speedtest_stop();
            self.sync();
            return;
        }
        // roaming (v2 only: an old core does not cap the run): first tap arms (DR6)
        if self.model.st_v2.is_some() && self.dg.net_roam && !self.st.arm.tap(0, lv::tick_get(), ST_ARM_MS) {
            self.speed_paint();
            return;
        }
        self.st.arm.clear();
        let l = self.st.btn_lbl;
        l.set_text(tr("已发送…")); // the request is synchronous: answer first
        self.label_cache.remove(&(l.0 as usize));
        lv::refr_now();
        let chill = self.st.chill_want && self.speed_chill_ok();
        let ok = self.src.speedtest_start(chill);
        self.sync();
        if ok {
            self.st.refused_at = 0;
            self.st.chill_want = false; // one run only (DR12)
            return;
        }
        let th = t();
        self.st.refused_at = lv::tick_get().max(1);
        l.set_text(tr("开始测速"));
        let off = self.st.offline;
        off.set_text(self.model.st_start_error.as_deref().unwrap_or(""));
        self.st_col(off, th.warn_t);
        kit::show(off, true);
        self.speed_reflow();
    }

    fn speedtest_srv(&mut self, i: usize) {
        self.src.speedtest_select_server(if i < 3 { -(i as i32) - 1 } else { i as i32 - 3 });
        self.sync();
        // the tick moves at once, not at the next refresh
        for (k, &ok) in self.st.srv_ok.iter().enumerate() {
            kit::show(ok, k == i);
        }
    }

    /// The card's v2 parts: the conclusion line, the lines under it, the CHILL row and the cost
    /// line. Returns the button's y. Hidden for an old core, whose card stays as it was (DR4).
    fn speed_paint_v2(&mut self, running: bool) -> i32 {
        let th = t();
        let f = uf();
        let m = self.model.clone();
        let s = &self.st;
        let (live, detail, result, server, mark_l, concl_l) = (s.live, s.detail, s.result, s.server, s.concl_mark, s.concl);
        let (row, name, why, sw, hint) = (s.chill_row, s.chill_name, s.chill_why, s.chill_sw, s.hint);
        let Some(v) = m.st_v2.as_ref() else {
            for o in [mark_l, concl_l, row, hint] {
                kit::show(o, false);
            }
            kit::show(result, true);
            detail.set_y(82);
            server.set_y(128);
            self.st_col(live, th.t1);
            return ST_ACTION_Y;
        };
        let ph = m.st_phase;
        let complete = ph == ST_COMPLETE;
        // the number: grey when the stable part was too short (DR5)
        self.st_col(live, if complete && v.sample_short { th.t3 } else { th.t1 });
        let mut y = 80;
        match conclusion(ph, v) {
            Some((mark, col, txt)) => {
                kit::show(mark_l, !mark.is_empty());
                kit::show(concl_l, true);
                self.set(mark_l, mark);
                self.st_col(mark_l, col);
                self.st_col(concl_l, if mark.is_empty() { th.t2 } else { th.t1 });
                concl_l.set_x(if mark.is_empty() { PAD } else { PAD + 22 });
                self.set(concl_l, &txt);
                y += f.cj17b.line_height() + 4;
            }
            None => {
                kit::show(mark_l, false);
                kit::show(concl_l, false);
            }
        }
        // ↑ and latency under it (complete), else the old latency line
        kit::show(result, false);
        let (ping, ul) = (m.st_ping_ms.unwrap_or(-1.0), m.st_upload_mbps.unwrap_or(-1.0));
        let mut d: Vec<u8> = Vec::new();
        if complete {
            if ul >= 0.0 {
                d = cf!("↑ %.1f Mbps", ul);
            }
            if ping >= 0.0 {
                if !d.is_empty() {
                    d.extend_from_slice(" · ".as_bytes());
                }
                d.extend(cf!(tr("延迟 %.0f ms"), ping));
                if let Some(ld) = v.loaded_down_ms {
                    d.extend(cf!(tr("，下载时 %.0f ms"), ld));
                }
            }
        } else if ping >= 0.0 {
            d = cf!(tr("延迟 %.0fms · 抖动 %.0fms"), ping, m.st_jitter_ms.unwrap_or(-1.0).max(0.0));
        } else if ph == ST_IDLE && m.st_download_mbps.unwrap_or(-1.0) < 0.0 {
            d = tr("还没测过").into();
        }
        detail.set_y(y);
        self.set(detail, &d);
        if !d.is_empty() {
            y += 22;
        }
        // where it was measured, how, and what it cost (DR1, DR13)
        let srv = m.st_server.as_deref().unwrap_or("");
        let mut sl: Vec<u8> = Vec::new();
        if complete && !srv.is_empty() {
            let prov = match v.provider.as_str() {
                "librespeed" => "LibreSpeed · ",
                "ookla" => "Ookla · ",
                _ => "",
            };
            // the usage must show (DR13): shorter names first, then no provider
            let sponsor = if v.server_sponsor.is_empty() { srv } else { v.server_sponsor.as_str() };
            for (p, n) in [(prov, srv), (prov, sponsor), ("", sponsor)] {
                sl = cf!("%s%s · ", p, n);
                sl.extend(cf!(trn("%d 条连接", v.connections as i64), v.connections));
                sl.extend_from_slice(" · ".as_bytes());
                sl.extend(used(v.download_bytes + v.upload_bytes));
                if lv::text_w(&sl, f.cj12) <= CARD_W - 2 * PAD {
                    break;
                }
            }
        } else {
            sl.extend_from_slice(srv.as_bytes());
        }
        server.set_y(y);
        self.set(server, &sl);
        if !sl.is_empty() {
            y += 20;
        }
        // 「也测经 CHILL」 (DR12)
        y = y.max(104) + 4;
        kit::show(row, true);
        row.set_y(y);
        let roam = self.dg.net_roam;
        let ok = self.speed_chill_ok();
        if !ok && !running {
            self.st.chill_want = false;
        }
        let w: &str = if roam {
            tr("漫游中只粗测")
        } else if m.chill_online == 0 {
            tr("CHILL 没在运行")
        } else {
            ""
        };
        self.set(why, w);
        kit::toggle_set(sw, if running { v.path == "chill" } else { self.st.chill_want });
        let dis = sw.has_state(lv::LV_STATE_DISABLED);
        if !ok && !dis {
            sw.add_state(lv::LV_STATE_DISABLED);
        } else if ok && dis {
            sw.remove_state(lv::LV_STATE_DISABLED);
        }
        self.st_col(name, if ok || (running && v.path == "chill") { th.t1 } else { th.t3 });
        y += ROW_H + 4;
        // the cost before a run (DR13), the roaming line instead (DR6)
        let h: &str = if running {
            ""
        } else if roam {
            tr("漫游中：只粗测 5 秒、最多 30 MB")
        } else if self.st.chill_want {
            tr("约 40 秒 · 最多 1.15 GB")
        } else {
            tr("约 30 秒 · 最多 750 MB")
        };
        kit::show(hint, !h.is_empty());
        self.set(hint, h);
        y
    }

    /// 「分层」 (DR2): complete, a verdict, not short, not coarse. Each row: mark, name, value;
    /// untested rows all grey; proxy ▲ → CHILL nodes, radio ▲ → placement (DR9).
    fn speed_paint_layers(&mut self, show: bool) {
        let th = t();
        let f = uf();
        let m = self.model.clone();
        let vd = m.st_v2.as_ref().and_then(|v| v.verdict.as_ref());
        let show = show && vd.is_some();
        kit::show(self.st.layer_sec, show);
        for i in 0..ST_LAYERS {
            let l = vd.and_then(|vd| vd.layers.get(i)).filter(|_| show);
            let row = self.st.layer_row[i];
            kit::show(row, l.is_some());
            let Some(l) = l else {
                self.st.layer_to[i] = None;
                continue;
            };
            let (mark_l, name_l, val_l, val2_l, go_l) =
                (self.st.layer_mark[i], self.st.layer_name[i], self.st.layer_val[i], self.st.layer_val2[i], self.st.layer_go[i]);
            let untested = l.state != "ok" && l.state != "warn";
            let warn = l.state == "warn";
            let to = match l.key.as_str() {
                "proxy" if warn => Some(Sub::ChillNodes),
                "radio" if warn => Some(Sub::Place),
                _ => None,
            };
            self.st.layer_to[i] = to;
            if self.st.cols.insert(row.0 as usize ^ 2, to.is_some() as u32) != Some(to.is_some() as u32) {
                if to.is_some() {
                    row.add_flag(lv::LV_OBJ_FLAG_CLICKABLE);
                } else {
                    row.remove_flag(lv::LV_OBJ_FLAG_CLICKABLE);
                }
            }
            kit::show(go_l, to.is_some());
            self.set(mark_l, if warn { "▲" } else { "●" });
            self.st_col(mark_l, if untested { th.t3 } else if warn { th.warn_t } else { th.ok_t });
            self.set(name_l, layer_name(&l.key));
            self.st_col(name_l, if untested { th.t3 } else { th.t1 });
            let val = lang::pick(&l.value_zh, &l.value_en).to_string();
            name_l.update_layout();
            let right = CARD_W - PAD - if to.is_some() { 16 } else { 0 };
            let avail = right - (PAD + 18) - name_l.width() - 12;
            let two = lv::text_w(val.as_bytes(), f.cj14) > avail;
            kit::show(val_l, !two);
            kit::show(val2_l, two);
            let col = if untested { th.t3 } else { th.t1 };
            if two {
                self.set(val2_l, &val);
                self.st_col(val2_l, if untested { th.t3 } else { th.t2 });
            } else {
                self.set(val_l, &val);
                self.st_col(val_l, col);
                // the right edge moves only when the 「›」 comes or goes
                if self.st.cols.insert(val_l.0 as usize ^ 2, right as u32) != Some(right as u32) {
                    val_l.align(lv::LV_ALIGN_TOP_RIGHT, right - CARD_W, 11);
                }
            }
            let h = if two { ST_ROW2_H } else { ROW_H };
            if row.style_height(0) != h {
                row.set_height(h);
            }
        }
    }

    /// the progress when speedtest_poll read it, the server
    /// list when speedtest_servers_poll did, the tick every time.
    pub(super) fn speed_paint(&mut self) {
        let th = t();
        let m = self.model.clone();
        let s = &self.st;
        let (phase_l, live, unit, detail, result, server, btn, btn_lbl, offline) =
            (s.phase, s.live, s.unit, s.detail, s.result, s.server, s.btn, s.btn_lbl, s.offline);
        let mut layers = false;
        if !self.polled.speedtest {
        } else if m.st_agent_reachable == 0 {
            for o in [live, detail, result, server, btn, self.st.concl_mark, self.st.concl, self.st.chill_row, self.st.hint] {
                kit::show(o, false);
            }
            kit::show(offline, true);
            kit::show(unit, false);
            phase_l.set_text("");
            self.label_cache.remove(&(phase_l.0 as usize));
            offline.set_text(tr("设备服务没有回应，请稍后重试。"));
        } else {
            for o in [live, detail, server, btn] {
                kit::show(o, true);
            }
            kit::show(unit, true);
            let ph = m.st_phase;
            let dl = m.st_download_mbps.unwrap_or(-1.0);
            let ul = m.st_upload_mbps.unwrap_or(-1.0);
            let ping = m.st_ping_ms.unwrap_or(-1.0);
            let jitter = m.st_jitter_ms.unwrap_or(-1.0);
            let running = m.st_running != 0;
            // the step word (DR7): 「3/5 · 多连接下载」; finding a node has no step
            let stage = m.st_v2.as_ref().filter(|v| running && v.step > 0 && v.steps > 0 && !stage_name(&v.stage).is_empty());
            match stage {
                Some(v) => self.set(phase_l, cf!("%d/%d · %s", v.step, v.steps, stage_name(&v.stage))),
                None => self.set(phase_l, m.st_phase_label.as_deref().unwrap_or("")),
            }
            if ph == ST_DOWNLOAD || ph == ST_UPLOAD {
                self.set(live, cf!("%.1f", m.st_live_mbps.unwrap_or(-1.0)));
            } else if ph == ST_COMPLETE {
                self.set(live, cf!("%.1f", if dl >= 0.0 { dl } else { 0.0 }));
            } else {
                self.set(live, "--");
            }
            if m.st_v2.is_none() {
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
            }
            let ay = self.speed_paint_v2(running);
            if btn.y() != ay || self.st.action_y != ay {
                btn.set_y(ay);
                self.st.action_y = ay;
            }
            if let Some(v) = m.st_v2.as_ref() {
                layers = ph == ST_COMPLETE && !v.sample_short && v.mode != "coarse";
            }
            let armed = !running && self.st.arm.live(0, lv::tick_get(), ST_ARM_MS);
            self.set(btn_lbl, if running {
                tr("停止")
            } else if armed {
                tr("走漫游流量，再按一次")
            } else if ph == ST_COMPLETE {
                tr("重新测速")
            } else {
                tr("开始测速")
            });
            self.st_bg(btn, if running { th.fill_red } else if armed { th.fill_orange } else { th.fill_blue });
            live.update_layout();
            let ux = PAD + live.width() + 4;
            if unit.style_x(0) != ux {
                unit.set_x(ux);
            }
            let ra = self.st.refused_at;
            let err = m.st_error.as_deref().unwrap_or("");
            if ra != 0 && lv::tick_get().wrapping_sub(ra) < 6000 && !running {
                // refused at the start (the tap wrote why): leave it up a while
            } else if ph == ST_ERROR && !err.is_empty() {
                self.st.refused_at = 0;
                kit::show(offline, true);
                self.st_col(offline, th.bad_t);
                if offline.text() != err.as_bytes() {
                    offline.set_text(err);
                }
            } else {
                self.st.refused_at = 0;
                kit::show(offline, false);
            }
        }
        self.speed_paint_layers(layers);

        // the server list (only while the page is up)
        if self.polled.servers {
            let n = m.st_servers.len().min(ST_SRV_ROWS - 3);
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
        self.speed_reflow();
    }
}
