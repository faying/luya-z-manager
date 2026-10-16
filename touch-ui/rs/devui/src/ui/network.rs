//! 网络 (src/ui_parts/network.c): zte-agent's /api/netinfo on four pages. 情景 → the 情景 page,
//! 出口 IP → the exit tab, 运营商 + 手动选网 → 运营商选择, 邻小区 → 小区信息; net_reflow lays
//! each page out from its base. Drawn by net_paint (apn.rs, as in C). What touches the modem
//! (scan, register, back to automatic) and pinning a scene take two taps: the first colours the
//! row or button and says what follows, a second within 4 s sends it; every tap is answered at
//! once (9-25).
// SPDX-License-Identifier: MIT

use super::{Sub, Ui, SUB_VIEW};
use crate::kit::{self, CARD_W, MARGIN, PAD, ROW_H, TAB_PAD};
use crate::lang::{n_, tr};
use crate::lv::{self, sys, Obj};
use crate::source::NiAct;
use crate::theme::{t, uf};
use devui_model::netinfo_t;

pub const NET_EXIT_H: i32 = 50;
pub const NET_STAT_H: i32 = 48;
pub const NET_OP_H: i32 = 44;
pub const NET_BTN_H: i32 = 52;
pub const NET_NBR_HDR: i32 = 40;
pub const NET_NBR_H: i32 = 28;
pub const NET_SC_NOTE_H: i32 = 44;
/// a 情景 pin waits this long for the agent to read back
pub const SC_PEND_MS: u32 = 45000;

/// agent caps (netinfo.h NI_MAX_*)
pub const NI_MAX_SCENES: usize = 6;
pub const NI_MAX_OPS: usize = 8;
pub const NI_MAX_CELLS: usize = 8;
/// 自动 + 各情景 + 1 行备用
pub const NET_SCENE_ROWS: usize = 1 + NI_MAX_SCENES + 1;

/// Which page each block hangs on.
pub const NH_SCENE: usize = 0;
pub const NH_EXIT: usize = 1;
pub const NH_OPER: usize = 2;
pub const NH_CELL: usize = 3;
pub const NH_WIFI: usize = 4;
pub const NH_N: usize = 5;
const NET_HOST: [usize; 5] = [NH_SCENE, NH_EXIT, NH_OPER, NH_OPER, NH_CELL];

/// A line that goes away after a few seconds (esim.c net_flash_t).
#[derive(Default, Clone)]
pub struct NetFlash {
    pub txt: Vec<u8>,
    pub col: u32,
    pub until: u32,
}

impl NetFlash {
    pub fn set(&mut self, col: u32, ms: u32, txt: Vec<u8>) {
        self.txt = txt;
        self.txt.truncate(255);
        self.col = col;
        self.until = lv::tick_get().wrapping_add(ms);
        if self.until == 0 {
            self.until = 1;
        }
    }
    pub fn on(&self) -> bool {
        self.until != 0 && self.until.wrapping_sub(lv::tick_get()) as i32 > 0
    }
}

pub struct Net {
    pub scroll: Obj,
    pub err: Obj,
    pub nh_scroll: [Obj; NH_N],
    pub nh_base: [i32; NH_N],
    pub sec: [Obj; 5],
    pub card: [Obj; 5],
    pub sc_row: [Obj; NET_SCENE_ROWS],
    pub sc_name: [Obj; NET_SCENE_ROWS],
    pub sc_tag: [Obj; NET_SCENE_ROWS],
    pub sc_mark: [Obj; NET_SCENE_ROWS],
    pub sc_when: [Obj; NET_SCENE_ROWS],
    pub sc_does: [Obj; NET_SCENE_ROWS],
    pub sc_note: Obj,
    pub ex_row: [Obj; 2],
    pub ex_key: [Obj; 2],
    pub ex_ip: [Obj; 2],
    pub ex_sub: [Obj; 2],
    pub op: [Obj; 4],
    pub status: Obj,
    pub opr: [Obj; NI_MAX_OPS],
    pub opr_name: [Obj; NI_MAX_OPS],
    pub opr_det: [Obj; NI_MAX_OPS],
    pub opr_tag: [Obj; NI_MAX_OPS],
    pub btns: Obj,
    pub scan_btn: Obj,
    pub scan_lbl: Obj,
    pub auto_btn: Obj,
    pub auto_lbl: Obj,
    pub nbr_state: Obj,
    pub nbr_btn: Obj,
    pub nbr_lbl: Obj,
    pub nbr_row: [Obj; NI_MAX_CELLS],
    pub nbr_l: [Obj; NI_MAX_CELLS],
    pub nbr_r: [Obj; NI_MAX_CELLS],
    // two-tap arms
    pub arm_scan: u32,
    pub arm_auto: u32,
    pub arm_op: u32,
    pub arm_nbr: u32,
    pub arm_idx: i32,
    pub arm_sc: u32,
    pub arm_sc_idx: i32,
    pub painted_arm: i32,
    /// 情景切换 sent: 0 = 自动, 1.. = scenes[i-1]; -1 none
    pub sc_pend: i32,
    pub sc_pend_at: u32,
    pub sc_pend_id: String,
    pub sc_pend_name: String,
    pub sc_pend_wifi_off: bool,
    pub sc_flash: NetFlash,
    pub ms_flash: NetFlash,
    /// net_reflow's last arguments (net_relayout)
    pub nr_last: [i32; 5],
    /// the exit tab's › card (moves with 出口 IP)
    pub exit_nav_sec: Obj,
    pub exit_nav_card: Obj,
    /// the agent's answer last drawn (net_paint repaints when it changed)
    pub last_mode: i32,
}

impl Default for Net {
    fn default() -> Self {
        let n = Obj::NULL;
        Net {
            scroll: n,
            err: n,
            nh_scroll: [n; NH_N],
            nh_base: [0; NH_N],
            sec: [n; 5],
            card: [n; 5],
            sc_row: [n; NET_SCENE_ROWS],
            sc_name: [n; NET_SCENE_ROWS],
            sc_tag: [n; NET_SCENE_ROWS],
            sc_mark: [n; NET_SCENE_ROWS],
            sc_when: [n; NET_SCENE_ROWS],
            sc_does: [n; NET_SCENE_ROWS],
            sc_note: n,
            ex_row: [n; 2],
            ex_key: [n; 2],
            ex_ip: [n; 2],
            ex_sub: [n; 2],
            op: [n; 4],
            status: n,
            opr: [n; NI_MAX_OPS],
            opr_name: [n; NI_MAX_OPS],
            opr_det: [n; NI_MAX_OPS],
            opr_tag: [n; NI_MAX_OPS],
            btns: n,
            scan_btn: n,
            scan_lbl: n,
            auto_btn: n,
            auto_lbl: n,
            nbr_state: n,
            nbr_btn: n,
            nbr_lbl: n,
            nbr_row: [n; NI_MAX_CELLS],
            nbr_l: [n; NI_MAX_CELLS],
            nbr_r: [n; NI_MAX_CELLS],
            arm_scan: 0,
            arm_auto: 0,
            arm_op: 0,
            arm_nbr: 0,
            arm_idx: -1,
            arm_sc: 0,
            arm_sc_idx: -1,
            painted_arm: -2,
            sc_pend: -1,
            sc_pend_at: 0,
            sc_pend_id: String::new(),
            sc_pend_name: String::new(),
            sc_pend_wifi_off: false,
            sc_flash: NetFlash::default(),
            ms_flash: NetFlash::default(),
            nr_last: [0, ROW_H, 1, 0, 0],
            exit_nav_sec: n,
            exit_nav_card: n,
            last_mode: -1,
        }
    }
}

pub fn net_scene_name<'a>(n: &'a netinfo_t, id: &'a str) -> &'a str {
    for s in n.scenes.iter().take(n.nscenes.max(0) as usize) {
        if s.id == id {
            return if s.name.is_empty() { id } else { &s.name };
        }
    }
    id
}

pub fn net_armed(at: u32) -> bool {
    at != 0 && lv::tick_get().wrapping_sub(at) < 4000
}

/// The second tap: after a 300 ms debounce, within the 4 s.
pub fn net_confirm(at: u32) -> bool {
    let d = lv::tick_get().wrapping_sub(at);
    at != 0 && d > 300 && d < 4000
}

pub fn net_busy(n: &netinfo_t) -> bool {
    n.scan_state == "scanning" || n.guard_phase == "registering" || n.guard_phase == "reverting"
}

fn net_busy_what(n: &netinfo_t) -> &'static str {
    if n.scan_state == "scanning" {
        tr("正在搜索网络")
    } else if n.guard_phase == "reverting" {
        tr("正在恢复自动选网")
    } else if n.guard_phase == "registering" {
        tr("正在注册")
    } else {
        tr("正在忙")
    }
}

unsafe extern "C" fn net_scan_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.net_scan_tap());
}

unsafe extern "C" fn net_auto_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.net_auto_tap());
}

unsafe extern "C" fn net_op_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.net_op_tap(i));
}

unsafe extern "C" fn net_nbr_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.net_nbr_tap());
}

unsafe extern "C" fn net_sc_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.net_sc_tap(i));
}

/// 搜网结果的 m_rat: the vendor web page's table (mobile_network.js), not 27.007.
pub fn net_rat_name(rat: &str) -> &str {
    match rat {
        "9" | "11" | "12" => "5G",
        "7" | "13" => "4G",
        "2" => "3G",
        "0" => "2G",
        _ => rat,
    }
}

/// 国内 by the agent's country_iso (in English `country` is China); an old agent has none.
fn net_oper_home_country(o: &devui_model::ni_oper_t) -> bool {
    if o.country_iso.is_empty() { o.country == "中国" } else { o.country_iso == "CN" }
}

/// "中国联通 46001"; abroad with the country: "SoftBank（日本）44020".
pub fn net_oper_text(o: &devui_model::ni_oper_t) -> Vec<u8> {
    let name = if o.name.is_empty() { tr("未知") } else { o.name.as_str() };
    if o.mcc.is_empty() && o.name.is_empty() {
        return "—".into();
    }
    if !o.country.is_empty() && !net_oper_home_country(o) {
        crate::cf!(tr("%s（%s）%s%s"), name, &o.country, &o.mcc, &o.mnc)
    } else {
        crate::cf!("%s %s%s", name, &o.mcc, &o.mnc)
    }
}

/// The second line: 归属地 · 运营商 or node; why not when it could not be found.
pub fn net_exit_sub(e: &devui_model::ni_exit_t, extra: &[u8]) -> Vec<u8> {
    if e.ip.is_empty() && !e.err.is_empty() {
        return tr("暂时查不到，稍后自动重试").into();
    }
    crate::cf!(
        "%s%s%s%s",
        &e.geo,
        if !e.geo.is_empty() && !extra.is_empty() { " · " } else { "" },
        extra,
        if e.err.is_empty() { "" } else { tr(" · 刷新失败") }
    )
}

impl Ui {
    /// The blocks of the old 情景 · 网络 page, each on its own page (NET_HOST). Also fills
    /// the 情景 page and hangs cards on 小区信息 / 出口.
    #[allow(clippy::needless_range_loop)]
    pub(super) fn build_sub_net(&mut self, t0: Obj) {
        const SEC: [&str; 5] = [n_("选择"), n_("出口 IP"), n_("运营商"), n_("手动选网"), n_("邻小区")];
        const OP_CAP: [&str; 4] = [n_("原始运营商"), n_("注册运营商"), n_("漫游"), n_("选网")];
        let th = t();
        let f = uf();
        let sc = kit::scroll(t0, 0, SUB_VIEW, 1400);
        let net = &mut self.net;
        net.scroll = sc;
        net.nh_scroll[NH_OPER] = sc;
        net.nh_scroll[NH_SCENE] = kit::scroll(self.sub_page[Sub::Scene as usize], 0, SUB_VIEW, 600);
        net.nh_base[NH_SCENE] = 4;
        net.nh_base[NH_OPER] = 4;
        let host: [Obj; 5] = NET_HOST.map(|h| net.nh_scroll[h]);
        net.err = kit::label_w(sc, f.cj13, th.bad_t, MARGIN + 6, 4, CARD_W - 12, true, "");
        for i in 0..5 {
            net.sec[i] = kit::section(host[i], 0, tr(SEC[i]));
        }

        let c = kit::card(host[0], MARGIN, 0, CARD_W, ROW_H + NET_SC_NOTE_H);
        net.card[0] = c;
        for i in 0..NET_SCENE_ROWS {
            let r = kit::bx(c, 0, i as i32 * ROW_H, CARD_W, ROW_H, th.card, 0);
            r.set_style_bg_opa(lv::OPA_TRANSP, 0);
            if i > 0 {
                kit::sep(r, 0);
            }
            let mark = kit::bx(r, PAD, (ROW_H - 16) / 2, 16, 16, th.card, 8);
            mark.set_style_border_width(2, 0);
            net.sc_mark[i] = mark;
            net.sc_name[i] = kit::label_w(r, f.cj14, th.t1, PAD + 26, 11, 140, false, "");
            net.sc_tag[i] = kit::label_r(r, f.cj13, th.t3, CARD_W - PAD, 12, "");
            net.sc_when[i] = kit::label_w(r, f.cj12, th.t2, PAD + 26, 32, CARD_W - 2 * PAD - 26, true, "");
            net.sc_does[i] = kit::label_w(r, f.cj12, th.t3, PAD + 26, 50, CARD_W - 2 * PAD - 26, false, "");
            for l in [net.sc_when[i], net.sc_does[i]] {
                l.set_height(f.cj12.line_height());
                l.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
            }
            kit::tappable(r, Some(net_sc_cb), i);
            kit::show(r, i == 0);
            net.sc_row[i] = r;
        }
        net.sc_note = kit::label_w(c, f.cj12, th.t3, PAD, ROW_H + 8, CARD_W - 2 * PAD, true, "");

        let c = kit::card(host[1], MARGIN, 0, CARD_W, 2 * NET_EXIT_H);
        net.card[1] = c;
        for i in 0..2 {
            let r = kit::bx(c, 0, i as i32 * NET_EXIT_H, CARD_W, NET_EXIT_H, th.card, 0);
            r.set_style_bg_opa(lv::OPA_TRANSP, 0);
            if i > 0 {
                kit::sep(r, 0);
            }
            net.ex_key[i] = kit::label(r, f.cj14, th.t2, PAD, 7, "");
            net.ex_ip[i] = kit::label_r(r, f.n15, th.t1, CARD_W - PAD, 6, "");
            net.ex_sub[i] = kit::label_w(r, f.cj12, th.t3, PAD, 29, CARD_W - 2 * PAD, false, "");
            net.ex_row[i] = r;
        }

        let c = kit::card(host[2], MARGIN, 0, CARD_W, 4 * ROW_H);
        net.card[2] = c;
        for i in 0..4 {
            net.op[i] = kit::row(c, i as i32 * ROW_H, tr(OP_CAP[i]), i == 0);
            net.op[i].set_style_text_font(f.cj14, 0);
        }

        let c = kit::card(host[3], MARGIN, 0, CARD_W, NET_STAT_H + NET_BTN_H);
        net.card[3] = c;
        net.status = kit::label_w(c, f.cj13, th.t2, PAD, 8, CARD_W - 2 * PAD, true, "");
        for i in 0..NI_MAX_OPS {
            let r = kit::bx(c, 0, NET_STAT_H + i as i32 * NET_OP_H, CARD_W, NET_OP_H, th.card, 0);
            r.set_style_bg_opa(lv::OPA_TRANSP, 0);
            kit::sep(r, 0);
            net.opr_name[i] = kit::label_w(r, f.cj14, th.t1, PAD, 5, CARD_W - 2 * PAD - 70, false, "");
            net.opr_det[i] = kit::label_w(r, f.cj12, th.t3, PAD, 25, CARD_W - 2 * PAD - 70, false, "");
            net.opr_tag[i] = kit::label_r(r, f.cj13, th.t3, CARD_W - PAD, 13, "");
            kit::tappable(r, Some(net_op_cb), i);
            kit::show(r, false);
            net.opr[i] = r;
        }
        let b = kit::bx(c, 0, NET_STAT_H, CARD_W, NET_BTN_H, th.card, 0);
        b.set_style_bg_opa(lv::OPA_TRANSP, 0);
        kit::sep(b, 0);
        net.btns = b;
        let bw = (CARD_W - 2 * PAD - 8) / 2;
        (net.scan_btn, net.scan_lbl) = kit::button(b, PAD, 10, bw, 32, tr("搜索网络"), kit::BtnKind::Plain, Some(net_scan_cb), 0);
        (net.auto_btn, net.auto_lbl) = kit::button(b, PAD + bw + 8, 10, bw, 32, tr("恢复自动"), kit::BtnKind::Plain, Some(net_auto_cb), 0);

        let c = kit::card(host[4], MARGIN, 0, CARD_W, NET_NBR_HDR);
        net.card[4] = c;
        net.nbr_state = kit::label_w(c, f.cj13, th.t2, PAD, 12, CARD_W - 2 * PAD - 80, false, "");
        (net.nbr_btn, net.nbr_lbl) = kit::button(c, CARD_W - PAD - 72, 6, 72, 28, tr("扫描"), kit::BtnKind::Plain, Some(net_nbr_cb), 0);
        for i in 0..NI_MAX_CELLS {
            let r = kit::bx(c, 0, NET_NBR_HDR + i as i32 * NET_NBR_H, CARD_W, NET_NBR_H, th.card, 0);
            r.set_style_bg_opa(lv::OPA_TRANSP, 0);
            if i == 0 {
                kit::sep(r, 0);
            }
            net.nbr_l[i] = kit::label(r, f.n12, th.t1, PAD, 7, "");
            net.nbr_r[i] = kit::label_r(r, f.n12, th.t2, CARD_W - PAD, 7, "");
            kit::show(r, false);
            net.nbr_row[i] = r;
        }

        self.net_reflow(0, ROW_H, 1, 0, 0);
        sc.scroll_to_y(0);
    }

    pub fn net_clear_arms(&mut self) {
        let n = &mut self.net;
        n.arm_scan = 0;
        n.arm_auto = 0;
        n.arm_nbr = 0;
        n.arm_op = 0;
        n.arm_sc = 0;
        n.arm_idx = -1;
        n.arm_sc_idx = -1;
    }

    fn ni(&self) -> netinfo_t {
        self.model.netinfo.clone().unwrap_or_default()
    }

    /// An action that touches the modem: 已发送 drawn first (the screen stalls while it goes
    /// out); refused: the reason goes on this card.
    fn net_send(&mut self, a: NiAct) {
        let th = t();
        self.net.ms_flash.set(th.t2, 2500, tr("已发送，等设备回应…").into());
        self.net_paint(true);
        lv::refr_now();
        self.src.netinfo_act(a);
        self.sync();
        let err = self.model.netinfo_action_error.clone().unwrap_or_default();
        if !err.is_empty() {
            self.net.ms_flash.set(th.bad_t, 8000, crate::cf!(tr("没执行：%s"), &err));
        } else {
            self.src.netinfo_hurry(15);
        }
        self.net_paint(true);
    }

    /// Busy with something else: say what, do nothing.
    fn net_say_busy(&mut self, n: &netinfo_t) -> bool {
        if !net_busy(n) {
            return false;
        }
        self.net.ms_flash.set(t().t2, 3000, crate::cf!(tr("%s，等它做完再操作"), net_busy_what(n)));
        self.net_paint(true);
        true
    }

    fn net_scan_tap(&mut self) {
        let n = self.ni();
        if self.net_say_busy(&n) {
            return;
        }
        if net_confirm(self.net.arm_scan) {
            self.net_clear_arms();
            self.net_send(NiAct::Scan);
            return;
        }
        self.net_clear_arms();
        self.net.arm_scan = lv::tick_get();
        self.net_paint(true);
    }

    fn net_auto_tap(&mut self) {
        let n = self.ni();
        if self.net_say_busy(&n) {
            return;
        }
        if n.selection == "auto" {
            self.net_clear_arms();
            self.net.ms_flash.set(t().t2, 3000, tr("现在已经是自动选网，不用恢复").into());
            self.net_paint(true);
            return;
        }
        if net_confirm(self.net.arm_auto) {
            self.net_clear_arms();
            self.net_send(NiAct::Auto);
            return;
        }
        self.net_clear_arms();
        self.net.arm_auto = lv::tick_get();
        self.net_paint(true);
    }

    fn net_op_tap(&mut self, i: usize) {
        let n = self.ni();
        if self.net_say_busy(&n) {
            return;
        }
        if i >= n.nops.max(0) as usize {
            return;
        }
        let o = n.ops.get(i).cloned().unwrap_or_default();
        // the current row: under automatic selection it can be tapped, pinning the selection
        // to it (the vendor page can too); already manual on it: nothing to register
        if o.status == "2" && n.selection != "auto" {
            self.net_clear_arms();
            let nm = if o.name.is_empty() { &o.plmn } else { &o.name };
            self.net.ms_flash.set(t().t2, 3000, crate::cf!(tr("现在就在 %s 上"), nm));
            self.net_paint(true);
            return;
        }
        if self.net.arm_idx == i as i32 && net_confirm(self.net.arm_op) {
            self.net_clear_arms();
            self.net_send(NiAct::Register(i));
            return;
        }
        self.net_clear_arms();
        self.net.arm_idx = i as i32;
        self.net.arm_op = lv::tick_get();
        self.net_paint(true);
    }

    fn net_nbr_tap(&mut self) {
        if self.ni().nbr_state == "scanning" {
            return;
        }
        if net_confirm(self.net.arm_nbr) {
            self.net_clear_arms();
            self.net_send(NiAct::NbrScan);
            return;
        }
        self.net_clear_arms();
        self.net.arm_nbr = lv::tick_get();
        self.net_paint(true);
    }

    /// A scene row: 0 = 自动, 1..n = pin scenes[i-1], the last row is something else. A
    /// scene can turn Wi-Fi on or off, so two taps; once sent, 切换中 until it really moved.
    fn net_sc_tap(&mut self, i: usize) {
        let th = t();
        let n = self.ni();
        if i == NET_SCENE_ROWS - 1 {
            return;
        }
        let nscenes = n.nscenes.max(0) as usize;
        if i > nscenes {
            return;
        }
        let scene = |k: usize| n.scenes.get(k).cloned().unwrap_or_default();
        if self.net.sc_pend >= 0 {
            let t = crate::cf!(tr("正在切换到「%s」，稍等"), &self.net.sc_pend_name);
            self.net.sc_flash.set(th.t2, 3000, t);
            self.net_paint(true);
            return;
        }
        let already = if i == 0 { n.scene_pin.is_empty() } else { n.scene_pin == scene(i - 1).id };
        if already {
            self.net_clear_arms();
            let t = if i == 0 {
                crate::cf!(
                    tr("现在已经是自动 · 按位置和 SIM 判断为「%s」"),
                    if n.scene_current.is_empty() { tr("判定中") } else { net_scene_name(&n, &n.scene_current) }
                )
            } else {
                crate::cf!(tr("已经固定在「%s」。要恢复自动判断，点「自动」"), net_scene_name(&n, &n.scene_pin))
            };
            self.net.sc_flash.set(th.t2, 4000, t);
            self.net_paint(true);
            return;
        }
        if self.net.arm_sc_idx == i as i32 && net_confirm(self.net.arm_sc) {
            self.net_clear_arms();
            let net = &mut self.net;
            net.sc_pend = i as i32;
            net.sc_pend_at = lv::tick_get();
            net.sc_pend_id = if i > 0 { scene(i - 1).id } else { String::new() };
            net.sc_pend_name = if i > 0 { net_scene_name(&n, &scene(i - 1).id).to_string() } else { tr("自动").to_string() };
            net.sc_pend_wifi_off = i > 0 && scene(i - 1).wifi_off != 0;
            net.sc_flash.until = 0;
            self.net_paint(true);
            lv::refr_now();
            let id = self.net.sc_pend_id.clone();
            self.src.netinfo_pin(&id);
            self.sync();
            let err = self.model.netinfo_action_error.clone().unwrap_or_default();
            if !err.is_empty() {
                self.net.sc_pend = -1;
                self.net.sc_flash.set(th.bad_t, 8000, crate::cf!(tr("没切成：%s"), &err));
            } else {
                self.src.scenario_kick(); // Home's card follows
                self.src.netinfo_hurry((SC_PEND_MS / 1000) as i32);
            }
            self.net_paint(true);
            return;
        }
        self.net_clear_arms();
        self.net.arm_sc_idx = i as i32;
        self.net.arm_sc = lv::tick_get();
        self.net_paint(true);
    }

    /// Cards move with the lists above them: lay each page out from its base.
    #[allow(clippy::needless_range_loop)]
    pub(super) fn net_reflow(&mut self, err_h: i32, scene_rows: i32, exits: i32, ops: i32, cells: i32) {
        let net = &mut self.net;
        net.nr_last = [err_h, scene_rows, exits, ops, cells];
        let mut y = net.nh_base;
        y[NH_OPER] += err_h;
        let h = [
            scene_rows + NET_SC_NOTE_H,
            exits * NET_EXIT_H,
            4 * ROW_H,
            NET_STAT_H + ops * NET_OP_H + NET_BTN_H,
            NET_NBR_HDR + cells * NET_NBR_H + if cells != 0 { 6 } else { 0 },
        ];
        for i in 0..5 {
            if net.card[i].hidden() {
                continue;
            }
            let yy = &mut y[NET_HOST[i]];
            net.sec[i].set_y(*yy);
            net.card[i].set_y(*yy + 20);
            net.card[i].set_height(h[i]);
            *yy += 20 + h[i] + 10;
        }
        net.btns.set_y(NET_STAT_H + ops * NET_OP_H);
        net.sc_note.set_y(scene_rows + 8);
        let net = &self.net;
        net.exit_nav_sec.set_y(y[NH_EXIT]);
        net.exit_nav_card.set_y(y[NH_EXIT] + 20);
        y[NH_EXIT] += 20 + net.exit_nav_card.style_height(0) + 10;
        for k in 0..NH_N {
            kit::scroll_extent(net.nh_scroll[k], y[k] - 10 + if k == NH_EXIT || k == NH_WIFI { TAB_PAD } else { 16 });
        }
    }

    /// Something above a page's blocks changed height (the Wi-Fi device list): lay out again
    /// with the last arguments.
    pub(super) fn net_relayout(&mut self) {
        if self.net.card[0].is_null() {
            return; // not built yet
        }
        let a = self.net.nr_last;
        self.net_reflow(a[0], a[1], a[2], a[3], a[4]);
    }
}
