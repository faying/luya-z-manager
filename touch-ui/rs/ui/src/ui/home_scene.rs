//! 首页 under the status card (DESIGN.md §4 Home hierarchy): the scenario card (header, then
//! the CHILL column) and the bottom row (Wi-Fi · 今日 · 电池, or a notice over the first two
//! cells). Both sit in fixed slots: what changes inside never moves what is below.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::home::{fmt_bytes_total, mf_view, midcut, CELL_H, ROW_Y, SCENE_H, SCENE_Y};
use super::{Sub, Ui, TAB_SYS, TAB_WIFI};
use crate::cf;
use crate::clock;
use crate::data::battery::{EST_CHARGING, EST_DISCHARGING, EST_REACHED};
use crate::kit::{self, CARD_W, MARGIN};
use crate::lang::{self, tr, trn};
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};
use ui_model::Model;

/// the header band (scenario, mode pill, why)
const BAND_H: i32 = 58;
/// the CHILL column takes the card's full width
const CH_W: i32 = CARD_W;
const CELL_GAP: i32 = 8;
const CELL_R: i32 = 18;
/// a battery this warm gets 「▲ 偏热」 in its cell (the banner says more from 50 °C)
const BATT_WARM_C: i32 = 45;

// Montserrat's symbol range (the device CJK font has none)
const SYM_HOME: &str = "\u{F015}";
const SYM_GPS: &str = "\u{F124}";
const SYM_SHUFFLE: &str = "\u{F074}";

#[derive(Default, Clone, Copy)]
struct Cell {
    bx: Obj,
    k: Obj,
    v: Obj,
    s: Obj,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoticeKind {
    /// DD18 自动退回已开 (acked with 知道了)
    Rollback,
    /// 手动优先 (to the CHILL area)
    Chill,
}

#[derive(Default, Clone)]
pub struct Scene {
    pub card: Obj,
    band: Obj,
    icon: Obj,
    icon_l: Obj,
    name: Obj,
    pill: Obj,
    pill_l: Obj,
    why: Obj,
    ch_box: Obj,
    ch_mode: Obj,
    ch_mode_l: Obj,
    ch_run: Obj,
    ch_profile: Obj,
    ch_node: Obj,
    ch_speed: Obj,
    ch_traffic: Obj,
    cells: [Cell; 3],
    ntc: Obj,
    ntc_t: Obj,
    ntc_k: Obj,
    ntc_ok_l: Obj,
    /// which notice is up, of those painted last
    ntc_i: usize,
    ntc_kinds: Vec<NoticeKind>,
    /// the notice as wrapped last (wrap_cjk measures: only when it changes)
    ntc_src: Vec<u8>,
    /// the pill's and the tags' fills as last set (bg colours are not compared by LVGL)
    pill_fill: u32,
}

/// Compact, readback-only presentation for the home card.
struct MihomoCard<'a> {
    status: &'static str,
    mode: &'static str,
    profile: &'a str,
    node: &'a str,
    speed: &'a str,
    traffic: &'a str,
}

fn card_value(value: &Option<String>) -> &str {
    value.as_deref().filter(|s| !s.is_empty() && *s != "-").unwrap_or("—")
}

fn mihomo_card(m: &Model) -> MihomoCard<'_> {
    let online = m.chill_online != 0;
    let mode = if online {
        match m.chill_mode_raw.as_deref() {
            Some("rule") => tr("规则"),
            Some("global") => tr("全局"),
            Some("direct") => tr("直连"),
            _ => "",
        }
    } else { "" };
    let status = if online && m.chill_mode_raw.as_deref() == Some("direct") {
        tr("直连")
    } else if online {
        tr("运行中")
    } else if m.chill_profile.as_deref() == Some("") {
        if lang::is_en() { "Not configured" } else { "未配置" }
    } else if m.scenario.available != 0 && m.scenario.chill_on == 0 {
        tr("直连")
    } else {
        tr("服务不可用")
    };
    let node = card_value(&m.chill_node);
    let group = m.chill_groups.iter().find(|g| g.selected != 0)
        .map(|g| g.name.as_str()).filter(|s| !s.is_empty()).unwrap_or("—");
    MihomoCard {
        status,
        mode,
        profile: card_value(&m.chill_profile),
        node: if online { if node == "—" { group } else { node } } else { "—" },
        speed: if online { card_value(&m.chill_speed) } else { "—" },
        traffic: if online { card_value(&m.chill_traffic) } else { "—" },
    }
}

/// minutes as 「14h 51m」「50m」「2h」, to the minute (the battery cell is 72 px wide)
pub fn dur_short(m: i64) -> String {
    if m < 60 {
        format!("{m}m")
    } else if m % 60 == 0 {
        format!("{}h", m / 60)
    } else {
        format!("{}h {}m", m / 60, m % 60)
    }
}

/// how long it has been here, in the largest whole unit: 「40 分钟」「2 小时」「3 天」
fn stay(secs: i64) -> String {
    let m = secs / 60;
    let (n, unit) = if m < 60 { (m.max(1), tr("%d 分钟")) } else if m < 48 * 60 { (m / 60, tr("%d 小时")) } else { (m / 1440, tr("%d 天")) };
    String::from_utf8_lossy(&cf!(unit, n as i32)).into_owned()
}

/// a #rrggbb recolour run (label_set_recolor): `w` in `col`
fn hl(col: u32, w: &str) -> String {
    format!("#{col:06x} {w}#")
}

/// 「判断中 ●●○」 (core 1.50's GET /api/scenario): the scans point at another scenario than the
/// one in force; a filled dot per scan so far, out of the scans it takes. Empty = not judging.
pub fn judge_dots(sc: &ui_model::scenario_status_t) -> String {
    if sc.detail == 0 || sc.candidate.is_empty() || sc.candidate == sc.current || sc.enter_hits <= 0 || sc.enter_hits > 6 {
        return String::new();
    }
    let n = sc.enter_hits as usize;
    let k = (sc.hits.max(0) as usize).min(n);
    format!("{} {}{}", tr("判断中"), "●".repeat(k), "○".repeat(n - k))
}

/// The Wi-Fi that put the device in the scenario in force: (SSID, since) when the last scan
/// matched one for it (core 1.50); None = an older core, or not by Wi-Fi.
pub fn matched_wifi(sc: &ui_model::scenario_status_t) -> Option<(String, i64)> {
    (sc.detail != 0 && !sc.matched_ssid.is_empty() && sc.matched_id == sc.current && sc.matched_since > 0)
        // a # would start a recolour run in the label
        .then(|| (sc.matched_ssid.replace('#', "＃"), sc.matched_since))
}

/// The scans miss the Wi-Fi that put the device in the scenario in force: (the scenario they
/// point at, misses so far). None = not leaving, not Wi-Fi-detected, or an older core.
pub fn leaving(sc: &ui_model::scenario_status_t) -> Option<(String, i32)> {
    if sc.detail == 0 || sc.misses <= 0 || sc.cur_ssid.is_empty() || sc.candidate.is_empty() || sc.candidate == sc.current {
        return None;
    }
    let to = if sc.cand_name.is_empty() { tr("别的情景").to_string() } else { sc.cand_name.clone() };
    Some((to, sc.misses))
}

/// 「08:41」 today, else the date (as the switch time is written).
fn hhmm_or_date(at: i64, now: i64) -> String {
    let (tm, tn) = (clock::local(at), clock::local(now));
    let today = tm.year == tn.year && tm.yday == tn.yday;
    clock::strftime(if today { "%H:%M" } else if lang::is_en() { "%d %b" } else { "%m-%d" }, &tm)
}

/// `x` as it is when it fits in `room` px, else cut in the middle (midcut) to the longest that
/// does; never under 7 characters.
pub fn fit_midcut(x: &str, room: i32, w: impl Fn(&str) -> i32) -> String {
    if w(x) <= room {
        return x.into();
    }
    let n = x.chars().count();
    (7..n).rev().map(|k| midcut(x, k)).find(|c| w(c) <= room).unwrap_or_else(|| midcut(x, 7))
}

unsafe extern "C" fn sc_band_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.sub_open(Sub::Scene));
}

unsafe extern "C" fn ch_col_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.sub_open(Sub::Chill));
}

unsafe extern "C" fn ntc_text_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.home_notice_next());
}

unsafe extern "C" fn ntc_ok_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.home_notice_ok());
}

/// A small coloured disc with a white symbol: the scenario and CHILL marks.
fn disc(p: Obj, x: i32, y: i32, d: i32, fill: u32, sym: &str, font: lv::Font) -> (Obj, Obj) {
    let o = kit::bx(p, x, y, d, d, fill, d / 2 + 1);
    let l = kit::label(o, font, 0xffffff, 0, 0, sym);
    l.align(lv::LV_ALIGN_CENTER, 0, 0);
    (o, l)
}

impl Ui {
    pub(super) fn build_home_scene(&mut self, sc: Obj) {
        let th = t();
        let f = uf();
        let mut s = Scene::default();
        let c = kit::card(sc, MARGIN, SCENE_Y, CARD_W, SCENE_H);
        c.set_style_clip_corner(true, 0);
        c.set_style_border_width(0, 0); // dark cards' 1 px top highlight read as a stray line here
        s.card = c;

        // header: icon · name · mode pill / why
        s.band = kit::bx(c, 0, 0, CARD_W, BAND_H, th.ind_w, 0);
        (s.icon, s.icon_l) = disc(s.band, 12, 6, 28, th.fill_ind, SYM_HOME, lv::montserrat(14));
        s.name = kit::label_w(s.band, f.cj20b, th.ind, 50, 8, 150, false, "");
        s.pill = kit::bx(s.band, CARD_W - 12 - 60, 9, 60, 22, th.fill_ind, 11);
        s.pill_l = kit::label(s.pill, f.cj12, 0xffffff, 0, 0, "");
        s.pill_l.align(lv::LV_ALIGN_CENTER, 0, 0);
        s.pill_fill = th.fill_ind;
        s.why = kit::label_w(s.band, f.cj12, th.t2, 50, 36, CARD_W - 50 - 12, false, "");
        s.why.label_set_recolor(true);
        kit::tappable(s.band, Some(sc_band_cb), 0);

        // CHILL column
        s.ch_box = kit::bx(c, 0, BAND_H, CH_W, SCENE_H - BAND_H, th.card, 0);
        s.ch_box.set_style_bg_opa(lv::OPA_TRANSP, 0);
        let hy = 8;
        disc(s.ch_box, 12, hy, 22, th.fill_blue, SYM_SHUFFLE, lv::montserrat(12));
        let chl = kit::label(s.ch_box, f.cj12, th.t2, 40, hy + 3, "Mihomo");
        chl.update_layout();
        let mx = 40 + chl.width() + 6;
        s.ch_mode = kit::bx(s.ch_box, mx, hy + 3, 40, 16, th.bg, 8);
        s.ch_mode_l = kit::label(s.ch_mode, f.cj11, th.t2, 6, 1, "");
        s.ch_run = kit::label_r(s.ch_box, f.cj12, th.t2, CH_W - 10, hy + 3, "");
        s.ch_profile = kit::label_w(s.ch_box, f.cj14, th.t1, 12, 34, CH_W - 24, false, "");
        s.ch_node = kit::label_w(s.ch_box, f.cj12, th.t2, 12, 56, CH_W - 24, false, "");
        s.ch_speed = kit::label_w(s.ch_box, f.cj12, th.t2, 12, 78, (CH_W - 30) / 2, false, "");
        s.ch_traffic = kit::label_w(s.ch_box, f.cj12, th.t2, 18 + (CH_W - 30) / 2, 78, (CH_W - 30) / 2, false, "");
        kit::tappable(s.ch_box, Some(ch_col_cb), 0);

        // bottom row: three cells; a notice over the first two
        let cw = (CARD_W - 2 * CELL_GAP) / 3;
        let mut x = MARGIN;
        let targets: [(Option<lv::EventCb>, usize); 3] =
            [(Some(super::tab_go_cb), TAB_WIFI), (Some(super::tile_click_cb), Sub::Usage as usize), (Some(super::tab_go_cb), TAB_SYS)];
        for (i, (cb, user)) in targets.into_iter().enumerate() {
            let w = if i == 1 { CARD_W - 2 * CELL_GAP - 2 * cw } else { cw };
            let bx = kit::card(sc, x, ROW_Y, w, CELL_H);
            bx.set_style_radius(CELL_R, 0);
            bx.set_style_border_width(0, 0);
            let cell = Cell {
                bx,
                k: kit::label(bx, f.cj11, th.t3, 14, 5, ""),
                v: kit::label_w(bx, f.n17, th.t1, 14, 19, w - 22, false, ""),
                s: kit::label_w(bx, f.cj11, th.t2, 14, 38, w - 22, false, ""),
            };
            kit::tappable(bx, cb, user);
            s.cells[i] = cell;
            x += w + CELL_GAP;
        }
        let nw = s.cells[1].bx.style_x(0) + s.cells[1].bx.style_width(0) - MARGIN;
        s.ntc = kit::card(sc, MARGIN, ROW_Y, nw, CELL_H);
        s.ntc.set_style_radius(CELL_R, 0);
        s.ntc.set_style_border_width(0, 0);
        kit::dot(s.ntc, 12, (CELL_H - 7) / 2, 7, th.blue);
        s.ntc_t = kit::label_w(s.ntc, f.cj12, th.t1, 27, 0, nw - 27 - 48, true, "");
        s.ntc_t.set_height(2 * f.cj12.line_height());
        s.ntc_t.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
        s.ntc_t.set_y((CELL_H - 2 * f.cj12.line_height()) / 2);
        kit::tappable(s.ntc_t, Some(ntc_text_cb), 0);
        s.ntc_t.remove_flag(lv::LV_OBJ_FLAG_CLICKABLE); // only with a next one (home_notice_paint)
        s.ntc_k = kit::label_r(s.ntc, f.n11, th.t3, nw - 48, 4, "");
        let ok = kit::bx(s.ntc, nw - 4 - 44, (CELL_H - 44) / 2, 44, 44, th.card, 12);
        ok.set_style_bg_opa(lv::OPA_TRANSP, 0);
        s.ntc_ok_l = kit::label(ok, f.cj13, th.acc_t, 0, 0, tr("知道了"));
        s.ntc_ok_l.align(lv::LV_ALIGN_CENTER, 0, 0);
        kit::tappable(ok, Some(ntc_ok_cb), 0);
        kit::show(s.ntc, false);
        self.home.sn = s;
    }

    /// The scenario card: header, CHILL. Every label is compared before it is
    /// written, so a refresh that changed nothing draws nothing.
    pub(super) fn home_scene_paint(&mut self, changed: bool) {
        if changed {
            self.chill_master_sync(); // the exit tab's CHILL switch follows the scenario
        }
        let th = t();
        let f = uf();
        let m = self.model.clone();
        let sc = &m.scenario;
        let now = self.now();
        let s = self.home.sn.clone();
        kit::show(s.card, true);

        // ---- header
        let when = if sc.last_switch > 0 {
            let (tm, tn) = (clock::local(sc.last_switch), clock::local(now));
            let today = tm.year == tn.year && tm.yday == tn.yday;
            clock::strftime(
                if today {
                    "%H:%M"
                } else if lang::is_en() {
                    "%d %b"
                } else {
                    "%m-%d"
                },
                &tm,
            )
        } else {
            String::new()
        };
        let home_like = sc.wifi_off != 0 || sc.name.contains('家') || sc.name.eq_ignore_ascii_case("home");
        self.set(s.icon_l, if home_like && sc.abroad == 0 { SYM_HOME } else { SYM_GPS });
        let name: &str = if sc.available == 0 || sc.enabled == 0 || sc.name.is_empty() { tr("情景") } else { &sc.name };
        self.set(s.name, name);
        let (pill, fill): (&str, u32) = if sc.available == 0 {
            (tr("没有情景"), th.fill_grey)
        } else if sc.enabled == 0 {
            (tr("已停用"), th.fill_grey)
        } else if !sc.pin.is_empty() {
            (tr("恢复自动 ›"), th.fill_blue)
        } else if sc.name.is_empty() {
            (tr("判断中"), th.fill_ind)
        } else {
            (tr("● 自动"), th.fill_ind)
        };
        // core 1.50: the scans point elsewhere: 「判断中 ●●○」, a dot per scan it needs
        let judging = judge_dots(sc);
        let pill: &str = if sc.available != 0 && sc.enabled != 0 && sc.pin.is_empty() && !judging.is_empty() { &judging } else { pill };
        self.set(s.pill_l, pill);
        let pw = lv::text_w(pill.as_bytes(), f.cj12) + 18;
        s.pill.set_width(pw);
        s.pill.set_x(CARD_W - 12 - pw);
        s.pill_l.align(lv::LV_ALIGN_CENTER, 0, 0);
        if self.home.sn.pill_fill != fill {
            self.home.sn.pill_fill = fill;
            kit::bg(s.pill, fill);
        }
        s.name.set_width((CARD_W - 12 - pw - 8 - 50).max(40));
        let k = |w: &str| hl(th.ind, w);
        let since = |w: &str| -> String {
            if w.is_empty() { String::new() } else { String::from_utf8_lossy(&cf!(tr(" · %s 起"), w)).into_owned() }
        };
        let why: String = if sc.available == 0 {
            tr("情景引擎没开").into()
        } else if sc.enabled == 0 {
            tr("不会自动切 Wi-Fi 和 CHILL").into()
        } else if sc.name.is_empty() {
            tr("开机后要连续两次扫描确认位置").into()
        } else if !sc.pin.is_empty() {
            let by = if when.is_empty() { String::new() } else { String::from_utf8_lossy(&cf!(tr(" · 你 %s 手动选的"), &when)).into_owned() };
            format!("{}{}", k(tr("已固定")), by)
        } else if sc.abroad != 0 {
            let ni = m.netinfo.clone().unwrap_or_default();
            let here = if ni.serving.country.is_empty() { tr("国外").to_string() } else { ni.serving.country.clone() };
            let mut w = String::from_utf8_lossy(&cf!(tr("因为在%s"), &k(&here))).into_owned();
            if !ni.home.country.is_empty() && ni.home.country != here {
                w.push_str(&String::from_utf8_lossy(&cf!(tr("（卡是%s的）"), &ni.home.country)));
            }
            if sc.last_switch > 0 && now >= sc.last_switch {
                w.push_str(&String::from_utf8_lossy(&cf!(tr(" · 到了 %s"), &stay(now - sc.last_switch))));
            }
            w
        } else if let Some((to, n)) = leaving(sc) {
            // core 1.50: the Wi-Fi that brought it here has not been seen N scans in a row
            let wifi: String = if home_like {
                tr("家里 Wi-Fi").into()
            } else {
                let room = CARD_W - 50 - 12 - lv::text_w(cf!(tr("可能切到%s：%s %d 次没看到"), &to, "Wi-Fi ", n).as_slice(), f.cj12);
                format!("Wi-Fi {}", fit_midcut(&sc.cur_ssid.replace('#', "＃"), room, |x| lv::text_w(x.as_bytes(), f.cj12)))
            };
            String::from_utf8_lossy(&cf!(tr("可能切到%s：%s %d 次没看到"), &k(&to), &wifi, n)).into_owned()
        } else if let Some((ssid, at)) = matched_wifi(sc) {
            // core 1.50: the network the scans matched, and since when
            let wifi = if home_like { tr("家里 Wi-Fi") } else { "Wi-Fi" };
            let at = hhmm_or_date(at, now);
            let head = String::from_utf8_lossy(&cf!(tr("因为看到%s"), wifi)).into_owned();
            // the SSID gets what the line has left (cj12), both ends kept
            let room = CARD_W - 50 - 12 - lv::text_w(format!("{head} {}", since(&at)).as_bytes(), f.cj12);
            let ssid = fit_midcut(&ssid, room, |x| lv::text_w(x.as_bytes(), f.cj12));
            String::from_utf8_lossy(&cf!(tr("因为看到%s"), &k(&format!("{wifi} {ssid}")))).into_owned() + &since(&at)
        } else if sc.wifi_off != 0 {
            String::from_utf8_lossy(&cf!(tr("因为看到%s"), &k(tr("家里 Wi-Fi")))).into_owned() + &since(&when)
        } else if when.is_empty() {
            String::new()
        } else {
            format!("{}{}", tr("切换于 "), when)
        };
        self.set(s.why, &why);

        // ---- CHILL: not installed, no column
        if m.chill_absent != 0 {
            kit::show(s.ch_box, false);
            return;
        }
        kit::show(s.ch_box, true);
        let card = mihomo_card(&m);
        self.set(s.ch_run, card.status);
        kit::text_color(s.ch_run, if m.chill_online != 0 { th.ok_t } else { th.t2 });
        self.set(s.ch_mode_l, card.mode);
        kit::show(s.ch_mode, !card.mode.is_empty());
        s.ch_mode.set_width(lv::text_w(card.mode.as_bytes(), f.cj11) + 12);
        self.set(s.ch_profile, card.profile);
        self.set(s.ch_node, card.node);
        self.set(s.ch_speed, card.speed);
        self.set(s.ch_traffic, card.traffic);
    }

    /// The bottom row: Wi-Fi · 今日 · 电池; a notice takes the first two cells.
    pub(super) fn home_row_paint(&mut self) {
        let th = t();
        let m = self.model.clone();
        let d = &m.data;
        let s = self.home.sn.clone();
        let [cw, ct, cb] = s.cells;

        // Wi-Fi
        self.set(cw.k, "Wi-Fi");
        if d.ap_on == 0 {
            self.set(cw.v, tr("关"));
            cw.v.set_style_text_font(uf().cj15b, 0);
            self.set(cw.s, if m.scenario.wifi_off != 0 { tr("情景关的") } else { tr("已关闭") });
        } else {
            self.set(cw.v, cf!(trn("%d 台", d.lan_total as i64), d.lan_total));
            cw.v.set_style_text_font(uf().n17, 0);
            let ssid = if d.ap_ssid.is_empty() { "-".to_string() } else { midcut(&d.ap_ssid, 11) };
            self.set(cw.s, &ssid);
        }
        kit::text_color(cw.v, if d.ap_on != 0 { th.t1 } else { th.t3 });

        // 今日
        self.set(ct.k, tr("今日"));
        self.set(ct.v, fmt_bytes_total(d.day_rx_bytes + d.day_tx_bytes));
        self.set(ct.s, cf!(tr("月 %s"), &fmt_bytes_total(d.month_rx_bytes + d.month_tx_bytes)));

        // 电池
        self.set(cb.k, tr("电池"));
        let pct = d.batt_pct;
        // as System and the status bar say it: a charger with voltage (batt_charging can stay set on battery)
        let charging = d.charger_in != 0 && d.chg_volt_uv > 1000000;
        let (kind, mins) = (m.battery_kind, m.battery_min);
        let (v, vcol, sub, scol): (String, u32, String, u32) = if charging {
            let sub = match kind {
                EST_CHARGING if mins > 0 => String::from_utf8_lossy(&cf!(tr("充电 %s满"), &dur_short(mins))).into_owned(),
                EST_REACHED => tr("已充满").into(),
                _ => tr("充电中").into(),
            };
            (format!("{pct}%"), th.t1, sub, th.ok_t)
        } else if pct < 20 {
            let sub = if kind == EST_DISCHARGING && mins > 0 {
                String::from_utf8_lossy(&cf!(tr("约 %s"), &dur_short(mins))).into_owned()
            } else {
                tr("尽快充电").into()
            };
            (format!("▲{pct}%"), th.bad_t, sub, th.t2)
        } else if kind == EST_DISCHARGING && mins > 0 {
            (dur_short(mins), th.t1, format!("{pct}%"), th.t2)
        } else {
            (format!("{pct}%"), th.t1, String::new(), th.t2)
        };
        let (sub, scol) = if d.batt_temp >= BATT_WARM_C {
            (String::from_utf8_lossy(&cf!(tr("▲ 偏热 %d°C"), d.batt_temp)).into_owned(), th.warn_t)
        } else {
            (sub, scol)
        };
        self.set(cb.v, &v);
        kit::text_color(cb.v, vcol);
        self.set(cb.s, &sub);
        kit::text_color(cb.s, scol);

        self.home_notice_paint();
    }

    /// The notices waiting, most urgent first.
    fn home_notices(&self) -> Vec<(NoticeKind, Vec<u8>)> {
        let m = &self.model;
        let mut v = Vec::new();
        if self.op.view.notice == "rollback_on" && !self.op.notice_acked {
            v.push((NoticeKind::Rollback, tr("自动退回已开：切模式没通会退回").as_bytes().to_vec()));
        }
        let mf = mf_view(&m.scenario, m.chill_chain.as_deref().unwrap_or(""), m.chill_pin.as_deref());
        if mf.on {
            let mut l: Vec<u8> = if mf.mark.is_empty() { Vec::new() } else { cf!("%s ", mf.mark) };
            l.extend_from_slice(&mf.line);
            v.push((NoticeKind::Chill, l));
        }
        v
    }

    pub(super) fn home_notice_paint(&mut self) {
        let list = self.home_notices();
        let s = self.home.sn.clone();
        let on = !list.is_empty();
        kit::show(s.ntc, on);
        kit::show(s.cells[0].bx, !on);
        kit::show(s.cells[1].bx, !on);
        if !on {
            self.home.sn.ntc_kinds.clear();
            return;
        }
        let i = self.home.sn.ntc_i.min(list.len() - 1);
        self.home.sn.ntc_i = i;
        self.home.sn.ntc_kinds = list.iter().map(|x| x.0).collect();
        if self.home.sn.ntc_src != list[i].1 {
            let w = s.ntc_t.style_width(0);
            self.set(s.ntc_t, kit::wrap_cjk(&list[i].1, uf().cj12, w));
            self.home.sn.ntc_src = list[i].1.clone();
        }
        let many = list.len() > 1;
        if many != s.ntc_t.has_flag(lv::LV_OBJ_FLAG_CLICKABLE) {
            if many {
                s.ntc_t.add_flag(lv::LV_OBJ_FLAG_CLICKABLE);
            } else {
                s.ntc_t.remove_flag(lv::LV_OBJ_FLAG_CLICKABLE);
            }
        }
        self.set(s.ntc_k, if list.len() > 1 { format!("{}/{}", i + 1, list.len()) } else { String::new() });
        self.set(s.ntc_ok_l, if list[i].0 == NoticeKind::Rollback { tr("知道了") } else { tr("去看") });
        s.ntc_ok_l.align(lv::LV_ALIGN_CENTER, 0, 0);
    }

    /// A tap on the notice's text: the next one, if there are several.
    pub(super) fn home_notice_next(&mut self) {
        let n = self.home.sn.ntc_kinds.len();
        if n > 1 {
            self.home.sn.ntc_i = (self.home.sn.ntc_i + 1) % n;
            self.home_notice_paint();
        }
    }

    /// 知道了 / 去看
    pub(super) fn home_notice_ok(&mut self) {
        let Some(&k) = self.home.sn.ntc_kinds.get(self.home.sn.ntc_i) else { return };
        match k {
            NoticeKind::Rollback => {
                self.data_control("op.notice_ack", "{}", None);
                self.op.notice_acked = true;
                self.home.sn.ntc_i = 0;
                self.home_notice_paint(); // answered at once (DESIGN.md §4)
            }
            NoticeKind::Chill => self.exit_row_go(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matched_wifi_and_judging() {
        let mut sc = ui_model::scenario_status_t { detail: 1, current: "home".into(), enter_hits: 2, ..Default::default() };
        assert!(judge_dots(&sc).is_empty());
        sc.candidate = "away".into();
        sc.hits = 1;
        assert_eq!(judge_dots(&sc), "判断中 ●○");
        sc.hits = 5;
        assert_eq!(judge_dots(&sc), "判断中 ●●");
        // an older core: never
        sc.detail = 0;
        assert!(judge_dots(&sc).is_empty());
        let mut sc = ui_model::scenario_status_t { detail: 1, current: "home".into(), matched_id: "home".into(), matched_ssid: "Home#5G".into(), matched_since: 100, ..Default::default() };
        assert_eq!(matched_wifi(&sc), Some(("Home＃5G".into(), 100)));
        let mut lv_ = sc.clone();
        lv_.cur_ssid = "Home-5G".into();
        assert!(leaving(&lv_).is_none());
        (lv_.candidate, lv_.cand_name, lv_.misses) = ("away".into(), "外出".into(), 2);
        assert_eq!(leaving(&lv_), Some(("外出".into(), 2)));
        lv_.cand_name.clear();
        assert_eq!(leaving(&lv_).unwrap().0, "别的情景");
        // the match is for a scenario not (yet) in force
        sc.matched_id = "office".into();
        assert!(matched_wifi(&sc).is_none());
        // one px per char: both ends kept, as long as fits
        let w = |x: &str| x.chars().count() as i32;
        assert_eq!(fit_midcut("Home-5G", 20, w), "Home-5G");
        assert_eq!(fit_midcut("MyVeryLongHomeNetwork-5G", 12, w), "MyVery…rk-5G");
        assert_eq!(fit_midcut("MyVeryLongHomeNetwork-5G", 12, w).chars().count(), 12);
    }

    #[test]
    fn mihomo_card_states_and_long_profile() {
        lang::set_en(false);
        let mut m = Model::default();
        assert_eq!(mihomo_card(&m).status, "服务不可用");
        m.chill_profile = Some(String::new());
        assert_eq!(mihomo_card(&m).status, "未配置");
        m.chill_online = 1;
        m.chill_profile = Some("很长的家庭配置名称包含很多文字和节点信息".into());
        m.chill_mode_raw = Some("rule".into());
        m.chill_node = Some("SG2HOME".into());
        m.chill_speed = Some("↓1M/s ↑2K/s".into());
        m.chill_traffic = Some("↓3G ↑4M".into());
        let card = mihomo_card(&m);
        assert_eq!((card.status, card.mode, card.node), ("运行中", "规则", "SG2HOME"));
        assert_eq!(card.profile, m.chill_profile.as_deref().unwrap());
        assert_eq!((card.speed, card.traffic), ("↓1M/s ↑2K/s", "↓3G ↑4M"));
        m.chill_mode_raw = Some("direct".into());
        assert_eq!((mihomo_card(&m).status, mihomo_card(&m).mode), ("直连", "直连"));
        m.chill_mode_raw = Some("global".into());
        assert_eq!(mihomo_card(&m).mode, "全局");
        m.chill_online = 0;
        assert_eq!((mihomo_card(&m).status, mihomo_card(&m).node), ("服务不可用", "—"));
        lang::set_en(true);
        assert_eq!(mihomo_card(&m).status, "Unavailable");
        m.chill_profile = Some(String::new());
        assert_eq!(mihomo_card(&m).status, "Not configured");
        lang::set_en(false);
        assert!(SCENE_Y + SCENE_H < ROW_Y);
        assert!(ROW_Y + CELL_H < super::super::home::FOLD_Y);
        assert!(BAND_H + 78 + 18 <= SCENE_H);
    }

    #[test]
    fn short_durations() {
        assert_eq!(dur_short(50), "50m");
        assert_eq!(dur_short(100), "1h 40m");
        assert_eq!(dur_short(120), "2h");
        assert_eq!(dur_short(891), "14h 51m");
    }

    #[test]
    fn stay_in_the_largest_whole_unit() {
        assert_eq!(stay(20), "1 分钟");
        assert_eq!(stay(40 * 60), "40 分钟");
        assert_eq!(stay(2 * 3600 + 59 * 60), "2 小时");
        assert_eq!(stay(47 * 3600), "47 小时");
        assert_eq!(stay(3 * 86400), "3 天");
    }
}
