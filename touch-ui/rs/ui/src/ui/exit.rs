//! the 小区信息 page's frame (its values: refresh.rs). Also the CHILL card (built with Home,
//! shown on the exit tab) with the 手动优先 notice on its top, the 「在国外 · CHILL 怎么走」
//! sheet, and the CHILL pages.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::network::NH_CELL;
use super::{Ui, SUB_VIEW};
use crate::kit::{self, CARD_W, MARGIN, ROW_H};
use crate::lang::{n_, tr};
use crate::lv::Obj;
use crate::theme::uf;

/// Traffic values stay complete; names use the remaining width in both CHILL lists.
fn pair_name_fit(name: Obj, value: Obj) {
    value.update_layout();
    name.set_width((CARD_W - 2 * kit::PAD - value.width() - 12).max(0));
}

/// 小区信息's values (the old 信令读取 page).
#[derive(Default, Clone)]
pub struct SigPage {
    nr: [Obj; 6],
    lt: [Obj; 6],
    net: [Obj; 4],
    nr_sec: Obj,
    lte_sec: Obj,
}

impl Ui {
    /// 小区信息: 5G / LTE serving cell, 网络; 邻小区 hangs under them (build_sub_net).
    pub(super) fn build_sub_cell(&mut self, p: Obj) {
        const NR_CAP: [&str; 6] = [n_("频段"), "ARFCN", "PCI", "Cell ID", "PLMN", "RSRP / RSRQ / SINR"];
        const LT_CAP: [&str; 6] = [n_("频段"), "EARFCN", "PCI", "Cell ID", "RSRP / RSRQ / SINR", "RSSI"];
        const NET_CAP: [&str; 4] = [n_("网络模式"), "WAN", n_("制式"), n_("高铁模式")];
        let mut y = 4;
        let sc = kit::scroll(p, 0, SUB_VIEW, 1000);
        let sg = &mut self.sg;
        sg.nr_sec = kit::section(sc, y, tr("5G 服务小区"));
        y += 20;
        let nr = kit::card(sc, MARGIN, y, CARD_W, 6 * ROW_H);
        for (i, cap) in NR_CAP.iter().enumerate() {
            sg.nr[i] = kit::row(nr, i as i32 * ROW_H, tr(cap), i == 0);
        }
        y += 6 * ROW_H + 10;
        // LTE row by row like 5G (9-25: squeezed into one line it read as nothing)
        sg.lte_sec = kit::section(sc, y, tr("LTE 服务小区"));
        y += 20;
        let lte = kit::card(sc, MARGIN, y, CARD_W, 6 * ROW_H);
        for (i, cap) in LT_CAP.iter().enumerate() {
            sg.lt[i] = kit::row(lte, i as i32 * ROW_H, tr(cap), i == 0);
        }
        y += 6 * ROW_H + 10;
        kit::section(sc, y, tr("网络"));
        y += 20;
        let net = kit::card(sc, MARGIN, y, CARD_W, 4 * ROW_H);
        for (i, cap) in NET_CAP.iter().enumerate() {
            sg.net[i] = kit::row(net, i as i32 * ROW_H, tr(cap), i == 0);
        }
        sg.net[3].set_style_text_font(uf().cj14, 0);
        y += 4 * ROW_H + 10;
        // the supported bands are not listed here (9-25): the 锁频 chips are the same list
        // 邻小区 hangs below (build_sub_net; net_reflow sets its place and the scroll range)
        self.net.nh_scroll[NH_CELL] = sc;
        self.net.nh_base[NH_CELL] = y;
        sc.scroll_to_y(0);
    }

    /// 小区信息's values, every refresh.
    pub(super) fn sig_paint(&mut self, mode_word: &str) {
        use crate::logic::{self, Rat};
        let m = self.model.clone();
        let d = &m.data;
        let sg = self.sg.clone();
        let or_dash = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
        self.set(sg.nr[0], crate::cf!("%s  %s MHz", &or_dash(&d.nr_band_text), &or_dash(&d.nr_width)));
        self.set(sg.nr[1], crate::cf!("%ld", d.nr_arfcn));
        self.set(sg.nr[2], crate::cf!("%d", d.nr_pci));
        self.set(sg.nr[3], crate::cf!("%ld", d.nr_cid));
        self.set(sg.nr[4], crate::cf!("%d-%02d %s", d.mcc, d.mnc, &d.carrier));
        self.set(sg.nr[5], crate::cf!("%d / %d / %s", d.nr_rsrp, d.nr_rsrq, &or_dash(&d.nr_snr)));
        // the serving cell: lteca's first entry (the primary) when there is one, else net.*'s lte_*
        let pc = crate::net_view::parse_ca(&d.lte_ca, 1, b'B');
        let have = !pc.is_empty();
        let mut bs = String::new();
        let bw = if have { pc[0].bw } else { super::atol(&d.bandwidth) as i32 };
        let earfcn = if have { pc[0].arfcn } else { d.channel };
        let pci = if have { pc[0].pci } else { d.lte_pci };
        if have {
            bs = format!("B{}", pc[0].band);
        } else if !d.band_text.is_empty() && d.band_text.contains("LTE") {
            bs = logic::band_short(&d.band_text, false);
        }
        let rat = logic::rat(&d.radio);
        let in_use = rat == Rat::G4 || rat == Rat::G5Nsa;
        sg.lte_sec.set_text(if in_use { tr("LTE 服务小区") } else { tr("LTE（现在不用，下面是测量值）") });
        sg.nr_sec.set_text(if rat == Rat::G4 { tr("5G（现在不用，下面是测量值）") } else { tr("5G 服务小区") });
        if !bs.is_empty() && bw > 0 {
            self.set(sg.lt[0], crate::cf!("%s  %d MHz", &bs, bw));
        } else {
            self.set(sg.lt[0], or_dash(&bs));
        }
        self.set(sg.lt[1], if earfcn > 0 { crate::cf!("%ld", earfcn) } else { "-".into() });
        self.set(sg.lt[2], if pci > 0 { crate::cf!("%d", pci) } else { "-".into() });
        self.set(sg.lt[3], if in_use && d.lte_cell_id > 0 { crate::cf!("%ld", d.lte_cell_id) } else { "-".into() });
        self.set(
            sg.lt[4],
            if d.lte_rsrp != 0 { crate::cf!("%d / %d / %s", d.lte_rsrp, d.lte_rsrq, &or_dash(&d.lte_snr)) } else { "-".into() },
        );
        self.set(sg.lt[5], if d.lte_rssi != 0 { crate::cf!("%d", d.lte_rssi) } else { "-".into() });
        self.set(sg.net[0], or_dash(mode_word));
        self.set(sg.net[1], or_dash(&d.wan_state));
        self.set(sg.net[2], &d.radio);
        self.set(sg.net[3], if d.rail_mode != 0 { tr("开启") } else { tr("关闭") });
    }
}

use super::home::{mf_view, mf_wrap};
use super::network::NH_EXIT;
use super::{Sub, TAB_EXIT, TAB_HOME};
use crate::cf;
use crate::kit::{Sheet, PAD, R_CARD, W};
use crate::logic::Arm;
use crate::lv::{self, sys};
use crate::theme::t;

pub const CHILL_HOME_ROWS: usize = 5;
pub const CHILL_HOME_ROW_H: i32 = 36;
/// pair rows start here
pub const CHILL_HOME_TOP: i32 = 100;

const XM_RULE: usize = 0;
const XM_GLOBAL: usize = 1;
const XM_DIRECT: usize = 2;
const XM_OFF: usize = 3;
const XM_N: usize = 4;
pub const XM_ARM_MS: u32 = 4000;
const XM_TOP: i32 = 62;

#[derive(Default, Clone)]
pub struct Exit {
    pub chill_card: Obj,
    state: Obj,
    rate: Obj,
    split: Obj,
    line: Obj,
    total: Obj,
    pair_name: [Obj; CHILL_HOME_ROWS],
    pair_val: [Obj; CHILL_HOME_ROWS],
    pair_sep: [Obj; CHILL_HOME_ROWS],
    /// the card without the notice
    pub base_h: i32,
    // 手动优先 notice: a whole line on top; the rest moves down by mf_dy
    mf_box: Obj,
    mf_box2: Obj,
    mf_mark: Obj,
    mf_text: Obj,
    mf_go: Obj,
    pub mf_dy: i32,
    mf_col: Option<u32>,
    mf_wash: Option<u32>,
    /// what the card was last drawn from
    last: Option<ChillSnap>,
    // 「在国外 · CHILL 怎么走」
    pub xm: Sheet,
    xm_btn: [Obj; XM_N],
    xm_lbl: [Obj; XM_N],
    xm_ok: [Obj; XM_N],
    xm_foot: Obj,
    /// 「关闭」 tapped once (two taps)
    pub xm_off_arm: Arm,
}

/// The parts of the model the card shows.
#[derive(Clone, PartialEq)]
struct ChillSnap {
    core: Option<String>,
    mode: Option<String>,
    node: Option<String>,
    traffic: Option<String>,
    speed: Option<String>,
    split: Option<String>,
    online: i32,
    nodes: Vec<ui_model::chill_node_info_t>,
    pairs: Vec<ui_model::chill_traffic_item_t>,
    chill_on: i32,
    chill_back: i32,
    absent: i32,
}

unsafe extern "C" fn exit_pick_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.exit_pick(i));
}

unsafe extern "C" fn exit_cancel_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.exit_menu_set(false));
}

impl Ui {
    /// CHILL: status, live rate, exit · node · delay, connections, traffic, and the real top
    /// rule → node pairs (one list keyed by the pair). Built on Home's scroll, moved to the
    /// exit tab by build_exit.
    pub(super) fn build_chill_card(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let e = &mut self.ex;
        e.base_h = CHILL_HOME_TOP + CHILL_HOME_ROW_H;
        let h = kit::card(p, MARGIN, 300, CARD_W, e.base_h);
        e.chill_card = h;
        kit::label(h, f.cj12, th.t3, PAD, 10, "CHILL");
        e.state = kit::label_r(h, f.cj12, th.ok_t, CARD_W - PAD - 16, 10, "");
        kit::label_r(h, f.cj15, th.t3, CARD_W - PAD, 6, "›");
        e.rate = kit::label(h, f.n17, th.t1, PAD, 27, "");
        e.split = kit::label_r(h, f.cj12, th.t2, CARD_W - PAD, 31, "");
        e.line = kit::label_w(h, f.cj12, th.t2, PAD, 52, 170, false, "");
        e.total = kit::label_r(h, f.n12, th.t3, CARD_W - PAD, 51, "");
        kit::bx(h, 0, 74, CARD_W, 1, th.sep, 0);
        kit::label(h, f.cj12, th.t3, PAD, 81, tr("规则 → 节点"));
        for i in 0..CHILL_HOME_ROWS {
            let y = CHILL_HOME_TOP + i as i32 * CHILL_HOME_ROW_H;
            e.pair_sep[i] = if i > 0 { kit::sep(h, y) } else { Obj::NULL };
            e.pair_name[i] = kit::label_w(h, f.cj13, th.t1, PAD, y + 10, 180, false, "");
            e.pair_val[i] = kit::label_r(h, f.n12, th.t2, CARD_W - PAD, y + 11, "");
        }
        kit::tappable(h, Some(super::tile_click_cb), Sub::Chill as usize);
        // 手动优先: a whole line on top of the card (the same words as Home's 出口 row); with it
        // the rest moves down (mf_exit_paint). Rounded like the status block: a rounded box and
        // a square one under its lower half.
        e.mf_box = kit::bx(h, 0, 0, CARD_W, 40, th.wash_w, R_CARD);
        e.mf_box2 = kit::bx(h, 0, 20, CARD_W, 20, th.wash_w, 0);
        e.mf_mark = kit::label(h, f.cj13, th.warn_t, PAD, 11, "");
        e.mf_text = kit::label_w(h, f.cj13, th.warn_t, PAD + 18, 11, CARD_W - 2 * PAD - 18, true, "");
        e.mf_go = kit::label(h, f.cj13, th.acc_t, 0, 0, tr("去选节点 ›"));
        kit::tappable(e.mf_go, Some(super::tile_click_cb), Sub::ChillNodes as usize);
        e.mf_go.set_ext_click_area(12);
        for o in [e.mf_box, e.mf_box2, e.mf_mark, e.mf_text, e.mf_go] {
            kit::show(o, false);
        }
        kit::show(h, false);
    }

    /// The CHILL card on Home: repainted
    /// when what it shows changed.
    pub(super) fn chill_card_paint(&mut self) {
        let th = t();
        let m = self.model.clone();
        let snap = ChillSnap {
            core: m.chill_core.clone(),
            mode: m.chill_mode.clone(),
            node: m.chill_node.clone(),
            traffic: m.chill_traffic.clone(),
            speed: m.chill_speed.clone(),
            split: m.chill_conn_split.clone(),
            online: m.chill_online,
            nodes: m.chill_nodes.clone(),
            pairs: m.chill_pairs.clone(),
            chill_on: m.scenario.chill_on,
            chill_back: m.scenario.chill_back,
            absent: m.chill_absent,
        };
        if self.ex.last.as_ref() == Some(&snap) {
            return;
        }
        self.ex.last = Some(snap);
        let s = |o: &Option<String>| o.clone().unwrap_or_default();
        let online = m.chill_online != 0;
        let csc = &m.scenario;
        let e = self.ex.clone();
        // hidden when CHILL is not installed or there is no CHILL at all; stopped still shows,
        // with the way back
        kit::show(e.chill_card, m.chill_absent == 0 && (online || csc.chill_on == 0));
        let rows = if !online {
            self.set(e.state, tr("已关闭"));
            kit::text_color(e.state, th.t3);
            self.set(e.rate, "—");
            self.set(e.split, "");
            self.set(e.line, if csc.chill_back != 0 { tr("在国外关掉了 · 回国自动打开") } else { tr("点这里打开") });
            self.set(e.total, "");
            0
        } else {
            self.set(e.state, s(&m.chill_core));
            kit::text_color(e.state, th.ok_t);
            self.set(e.rate, s(&m.chill_speed));
            self.set(e.split, s(&m.chill_conn_split));
            self.set(e.total, s(&m.chill_traffic));
            // 出口 · 节点 · 延迟: the main selector's node, and its last measured delay
            let node = s(&m.chill_node);
            let mut dl: Vec<u8> = Vec::new();
            if let Some(ni) = m.chill_nodes.iter().find(|ni| ni.selected != 0 && ni.name == node) {
                if ni.delay > 0 {
                    dl = cf!(" · %dms", ni.delay);
                } else if ni.delay == 0 {
                    dl = tr(" · 超时").into();
                }
            }
            self.set(e.line, cf!("%s%s%s%s", &s(&m.chill_mode), if node.is_empty() { "" } else { " · " }, &node, &dl));
            m.chill_pairs.len()
        };
        let vis_rows = rows.clamp(1, CHILL_HOME_ROWS);
        for i in 0..CHILL_HOME_ROWS {
            let vis = i < vis_rows;
            kit::show(e.pair_name[i], vis);
            kit::show(e.pair_val[i], vis);
            kit::show(e.pair_sep[i], vis);
            if !vis {
                continue;
            }
            if i >= rows {
                // up but idle: one placeholder row
                self.set(e.pair_name[i], if online { tr("暂无活跃连接") } else { "—" });
                self.set(e.pair_val[i], "");
                kit::text_color(e.pair_name[i], th.t3);
            } else {
                let it = &m.chill_pairs[i];
                self.set(e.pair_name[i], &it.name);
                self.set(e.pair_val[i], &it.traffic);
                kit::text_color(e.pair_name[i], th.t1);
            }
            pair_name_fit(e.pair_name[i], e.pair_val[i]);
        }
        self.ex.base_h = CHILL_HOME_TOP + vis_rows as i32 * CHILL_HOME_ROW_H + 2;
        e.chill_card.set_height(self.ex.base_h + self.ex.mf_dy);
    }

    /// The line on top of the exit tab's CHILL card; the rest of the card moves with it.
    /// Returns its height.
    pub(super) fn mf_exit_paint(&mut self) -> i32 {
        let th = t();
        let m = mf_view(&self.model.scenario, self.model.chill_chain.as_deref().unwrap_or(""), self.model.chill_pin.as_deref());
        let e = self.ex.clone();
        let o = [e.mf_box, e.mf_box2, e.mf_mark, e.mf_text, e.mf_go];
        for (i, &x) in o.iter().enumerate() {
            kit::show(x, m.on && (i != 4 || m.alldown));
        }
        let mut h = 0;
        if m.on {
            let x = PAD + if m.mark.is_empty() { 0 } else { 18 };
            self.set(e.mf_mark, m.mark);
            e.mf_text.set_x(x);
            e.mf_text.set_width(CARD_W - PAD - x);
            let mut line = m.line.clone();
            line.truncate(255);
            let wrapped = mf_wrap(&line, uf().cj13, CARD_W - PAD - x);
            self.set(e.mf_text, &wrapped);
            if self.ex.mf_col != Some(m.col) {
                self.ex.mf_col = Some(m.col);
                kit::text_color(e.mf_text, m.col);
                kit::text_color(e.mf_mark, m.mark_col);
            }
            let wash = if m.wash != 0 { m.wash } else { th.card };
            if self.ex.mf_wash != Some(wash) {
                self.ex.mf_wash = Some(wash);
                kit::bg(e.mf_box, wash);
                kit::bg(e.mf_box2, wash);
            }
            e.mf_text.update_layout();
            h = 10 + e.mf_text.height() + 10;
            if m.alldown {
                e.mf_go.update_layout();
                e.mf_go.set_pos(CARD_W - PAD - e.mf_go.width(), h - 4);
                h += e.mf_go.height() + 6;
            }
            e.mf_box.set_height(h);
            e.mf_box2.set_height(h.min(20));
            e.mf_box2.set_y(h - h.min(20));
        }
        if h != self.ex.mf_dy {
            let d = h - self.ex.mf_dy;
            self.ex.mf_dy = h;
            let card = e.chill_card;
            for i in 0..card.child_count() as i32 {
                let c = card.child(i);
                if !o.contains(&c) {
                    c.set_y(c.style_y(0) + d);
                }
            }
        }
        h
    }

    /// Home's 出口 row: to the exit tab; with a 手动优先 notice stop at the CHILL card (the
    /// notice is on its top).
    pub(super) fn exit_row_go(&mut self) {
        let m = mf_view(&self.model.scenario, self.model.chill_chain.as_deref().unwrap_or(""), self.model.chill_pin.as_deref());
        self.tab_go(TAB_EXIT);
        if !m.on || self.ex.chill_card.hidden() {
            return;
        }
        let sc = self.net.nh_scroll[NH_EXIT];
        sc.update_layout();
        sc.scroll_to_y(self.ex.chill_card.style_y(0) - 4);
    }

    /// The CHILL part of the refresh: the card when Home or the exit tab may show it, the
    /// notice on top while one of them is up.
    pub(super) fn chill_refresh(&mut self) {
        // the card and the pages when chill_poll read
        if self.polled.chill {
            self.chill_card_paint();
            self.chill_page_paint();
        }
        if self.tab_visible(TAB_EXIT) || self.tab_visible(TAB_HOME) {
            self.mf_exit_paint();
            self.ex.chill_card.set_height(self.ex.base_h + self.ex.mf_dy);
        }
    }

    // ---- 国外时的 CHILL 出口面板 ----
    // The sheet is a compact canonical mode picker plus the independent CHILL switch.
    #[allow(clippy::needless_range_loop)]
    pub(super) fn exit_menu_refresh(&mut self) {
        const LBL: [&str; XM_N] = [n_("规则"), n_("全局"), n_("直连"), n_("关闭 CHILL（最省电）")];
        let th = t();
        let sc = self.model.scenario.clone();
        let x = self.model.chill_mode_raw.clone().unwrap_or_default();
        let on = sc.chill_on != 0;
        let e = self.ex.clone();
        for i in 0..XM_N {
            let cur = on
                && ((i == XM_RULE && x == "rule")
                    || (i == XM_GLOBAL && x == "global")
                    || (i == XM_DIRECT && x == "direct"));
            let mut text = tr(LBL[i]);
            // CHILL off: the first four mean nothing
            let mut col = if on { th.acc_t } else { th.t3 };
            let mut bg = 0;
            if i == XM_OFF {
                if !on {
                    text = tr("打开 CHILL");
                    col = th.ok_t;
                } else if self.ex.xm_off_arm.at != 0 {
                    text = tr("再点一下：关闭 CHILL，全部流量直连");
                    col = 0xffffff;
                    bg = th.fill_orange;
                } else {
                    col = th.bad_t;
                }
            }
            e.xm_lbl[i].set_text(text);
            kit::text_color(e.xm_lbl[i], col);
            e.xm_lbl[i].set_style_text_font(if cur { uf().cj17b } else { uf().cj15 }, 0);
            e.xm_btn[i].set_style_bg_opa(if bg != 0 { lv::OPA_COVER } else { lv::OPA_TRANSP }, 0);
            if bg != 0 {
                kit::bg(e.xm_btn[i], bg);
            }
            // ✓ from the symbol font: the device CJK font has none
            kit::show(e.xm_ok[i], cur);
            if cur {
                e.xm_lbl[i].update_layout();
                e.xm_ok[i].set_pos(e.xm_lbl[i].x() - 22, 11);
            }
        }
        e.xm_foot.set_text(if sc.auto_direct != 0 {
            tr("情景模式会根据所在地自动调整出口")
        } else {
            tr("情景模式可能自动调整出口")
        });
        kit::text_color(e.xm_foot, th.t3);
    }

    pub fn exit_menu_set(&mut self, v: bool) {
        self.ex.xm_off_arm.clear();
        if v {
            self.exit_menu_refresh();
        }
        kit::sheet_show(&self.ex.xm, v);
        self.update_tabs();
    }

    fn exit_menu_fail(&mut self) {
        let f = self.ex.xm_foot;
        f.set_text(tr("没成功（后台没回应或 CHILL 没在运行），稍后再试"));
        kit::text_color(f, t().warn_t);
    }

    fn exit_pick(&mut self, i: usize) {
        const STATE: [&str; 3] = ["rule", "global", "direct"];
        let sc = self.model.scenario.clone();
        if i == XM_OFF {
            if sc.chill_on == 0 {
                // 打开
                if self.scenario_chill_set(true) {
                    self.exit_menu_set(false);
                } else {
                    self.exit_menu_fail();
                }
                self.home_sc_force();
                return;
            }
            if self.ex.xm_off_arm.tap(XM_OFF as i32, lv::tick_get(), XM_ARM_MS) {
                if self.scenario_chill_set(false) {
                    self.exit_menu_set(false);
                } else {
                    self.exit_menu_refresh();
                    self.exit_menu_fail();
                }
                self.home_sc_force();
            } else {
                self.exit_menu_refresh();
            }
            return;
        }
        if sc.chill_on == 0 {
            return;
        }
        if i >= STATE.len() { return; }
        if self.chill_set_exit(STATE[i]) {
            self.exit_menu_set(false);
        } else {
            self.exit_menu_fail();
        }
        self.home_sc_force();
    }

    pub(super) fn build_exit_menu(&mut self) {
        let th = t();
        let s = kit::sheet(XM_TOP + XM_N as i32 * 40 + 4, Some(exit_cancel_cb));
        let p = s.panel;
        let title = kit::label(p, uf().cj13, th.t3, 0, 0, tr("在国外 · CHILL 怎么走"));
        title.align(lv::LV_ALIGN_TOP_MID, 0, 8);
        let foot = kit::label_w(p, uf().cj12, th.t3, 12, 27, W - 16 - 24, true, "");
        foot.set_style_text_align(lv::LV_TEXT_ALIGN_CENTER, 0);
        self.ex.xm_foot = foot;
        for i in 0..XM_N {
            let (l, b) = kit::sheet_item(&s, XM_TOP + i as i32 * 40, "", th.acc_t, Some(exit_pick_cb), i);
            self.ex.xm_lbl[i] = l;
            self.ex.xm_btn[i] = b;
            let ok = kit::label(b, lv::montserrat(16), th.acc_t, 0, 11, crate::kit::SYMBOL_OK);
            kit::show(ok, false);
            self.ex.xm_ok[i] = ok;
        }
        self.ex.xm = s;
    }
}
// ---- the CHILL pages ----
const EXIT_STATE: [&str; 3] = ["rule", "global", "direct"];
const PROFILE: [&str; 3] = ["eco", "standard", "perf"];
/// SC_MAX_NODE (a manual group can have 168 nodes)
pub const CHILL_MAX_NODES: usize = 200;
pub const CHILL_MAX_GROUPS: usize = 32;
/// SC_TOP_SHOW
const CHILL_TRAF_ROWS: usize = 6;
const CHILL_NAV_ROWS: usize = 2;
const CHILL_NAV_NODES: usize = 0;
const CHILL_NAV_PAIRS: usize = 1;

#[derive(Clone)]
pub struct ChillPage {
    scroll: Obj,
    hero: kit::Hero,
    core: Obj,
    pub sw: Obj,
    conns: Obj,
    traffic: Obj,
    exit_note: Obj,
    opt: [kit::Opt; 3],
    prof: [kit::Opt; 3],
    prof_sec: Obj,
    prof_note: Obj,
    prof_card: Obj,
    prof_y: i32,
    configs_card: Obj,
    configs_note: Obj,
    configs_rows: Vec<Obj>,
    configs_names: Vec<Obj>,
    configs_values: Vec<Obj>,
    configs_refresh: Obj,
    configs_refresh_text: Obj,
    configs_refresh_arm: Arm,
    nav_card: Obj,
    nav_val: [Obj; CHILL_NAV_ROWS],
    content_h: i32,
    on_shown: i32,
    // 节点
    scroll_nodes: Obj,
    grp_card: Obj,
    grp_empty: Obj,
    grp_btn: [Obj; CHILL_MAX_GROUPS],
    grp_lbl: [Obj; CHILL_MAX_GROUPS],
    grp_sig: Option<(usize, String)>,
    node_sec: Obj,
    delay_lbl: Obj,
    node_card: Obj,
    node_empty: Obj,
    node_row: Vec<Obj>,
    node_ok: Vec<Obj>,
    node_name: Vec<Obj>,
    node_dl: Vec<Obj>,
    // 规则 → 节点
    pair_card: Obj,
    pair_empty: Obj,
    pair_name: [Obj; CHILL_TRAF_ROWS],
    pair_val: [Obj; CHILL_TRAF_ROWS],
    pair_sep: [Obj; CHILL_TRAF_ROWS],
}

impl Default for ChillPage {
    fn default() -> Self {
        let n = Obj::NULL;
        ChillPage {
            scroll: n,
            hero: kit::Hero::default(),
            core: n,
            sw: n,
            conns: n,
            traffic: n,
            exit_note: n,
            opt: [kit::Opt::default(); 3],
            prof: [kit::Opt::default(); 3],
            prof_sec: n,
            prof_note: n,
            prof_card: n,
            prof_y: 0,
            configs_card: n,
            configs_note: n,
            configs_rows: Vec::new(),
            configs_names: Vec::new(),
            configs_values: Vec::new(),
            configs_refresh: n,
            configs_refresh_text: n,
            configs_refresh_arm: Arm::default(),
            nav_card: n,
            nav_val: [n; CHILL_NAV_ROWS],
            content_h: 0,
            on_shown: -1,
            scroll_nodes: n,
            grp_card: n,
            grp_empty: n,
            grp_btn: [n; CHILL_MAX_GROUPS],
            grp_lbl: [n; CHILL_MAX_GROUPS],
            grp_sig: None,
            node_sec: n,
            delay_lbl: n,
            node_card: n,
            node_empty: n,
            node_row: Vec::new(),
            node_ok: Vec::new(),
            node_name: Vec::new(),
            node_dl: Vec::new(),
            pair_card: n,
            pair_empty: n,
            pair_name: [n; CHILL_TRAF_ROWS],
            pair_val: [n; CHILL_TRAF_ROWS],
            pair_sep: [n; CHILL_TRAF_ROWS],
        }
    }
}

unsafe extern "C" fn chill_mode_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| {
        if i < EXIT_STATE.len() {
            u.chill_set_exit(EXIT_STATE[i]);
        }
    });
}

unsafe extern "C" fn chill_node_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.chill_node_tap(i));
}

unsafe extern "C" fn chill_group_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.chill_group_tap(i));
}

unsafe extern "C" fn chill_delay_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| {
        if u.model.chill_online == 0 || u.model.chill_nodes.is_empty() || u.model.chill_delay_pending != 0 {
            return;
        }
        u.src.chill_test_delay();
        u.sync();
        u.chill_page_paint();
    });
}

unsafe extern "C" fn chill_profile_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.chill_profile_tap(i));
}

unsafe extern "C" fn chill_config_cb(e: *mut sys::lv_event_t) {
    let i = unsafe { lv::event_user(e) };
    super::with(|u| {
        let Some(profile) = u.model.chill_config_profiles.get(i) else { return };
        if profile.active || matches!(u.model.chill_config_status.as_deref(), Some("running" | "refresh_running")) { return; }
        let action = format!("profile:{}", profile.id);
        u.src.chill_set_exit(&action);
        u.sync();
        u.chill_page_paint();
    });
}

unsafe extern "C" fn chill_config_refresh_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| {
        if matches!(u.model.chill_config_status.as_deref(), Some("running" | "refresh_running")) { return; }
        let Some(profile) = u.model.chill_config_profiles.iter().find(|p| p.active) else { return };
        let action = format!("refresh_profile:{}", profile.id);
        if u.cp.configs_refresh_arm.tap(0, lv::tick_get(), XM_ARM_MS) {
            u.src.chill_set_exit(&action);
            u.sync();
        }
        u.chill_page_paint();
    });
}

/// The CHILL page's master switch: the agent says no, or does not answer: flip it back.
unsafe extern "C" fn chill_master_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let sw = unsafe { lv::event_target(e) };
    super::with(|u| {
        let on = sw.has_state(lv::LV_STATE_CHECKED);
        if !u.scenario_chill_set(on) {
            super::cellular::sw_apply(sw, !on);
        }
    });
}

/// The CHILL page's 节点 / 规则 → 节点 rows: one level down, 返回 comes back here.
unsafe extern "C" fn chill_nav_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.sub_open_child(super::SUBS[i], Sub::Chill));
}

impl Ui {
    /// The switch follows the agent (an old agent says nothing: no switch).
    pub(super) fn chill_master_sync(&mut self) {
        let sw = self.cp.sw;
        if sw.is_null() {
            return;
        }
        let on = self.model.scenario.chill_on;
        if on < 0 {
            sw.add_flag(lv::LV_OBJ_FLAG_HIDDEN);
        } else {
            sw.remove_flag(lv::LV_OBJ_FLAG_HIDDEN);
            super::cellular::sw_apply(sw, on != 0);
        }
    }

    fn chill_node_tap(&mut self, i: usize) {
        // the tick moves at once; the next refresh draws what mihomo says (back if it failed)
        let ok = self.src.chill_select_node(i);
        self.sync();
        if ok {
            for (k, &ok) in self.cp.node_ok.iter().enumerate() {
                kit::show(ok, k == i);
            }
            // on the fallback and another node picked: the agent switches back and says so
            if !self.model.scenario.mf_notice.is_empty() {
                self.src.scenario_kick();
            }
        }
    }

    fn chill_group_tap(&mut self, i: usize) {
        // the group lights at once; its nodes at the next read
        let ok = self.src.chill_select_group(i);
        self.sync();
        if ok {
            let cp = &self.cp;
            for k in 0..CHILL_MAX_GROUPS {
                kit::chip_set(cp.grp_btn[k], cp.grp_lbl[k], k == i);
            }
        }
    }

    fn chill_profile_tap(&mut self, i: usize) {
        if i >= PROFILE.len() || self.model.chill_profile_raw.as_deref().unwrap_or("") == PROFILE[i] {
            return;
        }
        let ok = self.src.chill_set_profile(PROFILE[i]);
        self.sync();
        if ok {
            for k in 0..3 {
                kit::opt_set(&self.cp.prof[k], k == i, false);
            }
        }
    }

    pub(super) fn build_sub_chill(&mut self, p: Obj) {
        const LABELS: [&str; 3] = [n_("规则"), n_("全局"), n_("直连")];
        const SUBS: [&str; 3] = [n_("按规则分流"), n_("全部走代理"), n_("全部直连")];
        const PLABELS: [&str; 3] = [n_("省电"), n_("标准"), n_("性能")];
        const PSUBS: [&str; 3] = [n_("更凉更省"), n_("默认"), n_("切换更快")];
        const NAV_CAP: [&str; CHILL_NAV_ROWS] = [n_("节点"), n_("规则 → 节点")];
        const NAV_CHILD: [Sub; CHILL_NAV_ROWS] = [Sub::ChillNodes, Sub::ChillPairs];
        let th = t();
        let f = uf();
        let content_h = 4 + kit::HERO_H + 80 + 10 + 20 + 100 + 10 + 20 + 70 + 10 + 80 + 16;
        let sc = kit::scroll(p, 0, SUB_VIEW, content_h);
        self.cp.content_h = content_h;
        self.cp.scroll = sc;
        // status block: node (big), delay, group; the master switch top right
        let st = kit::card(sc, MARGIN, 4, CARD_W, kit::HERO_H + 2 * ROW_H);
        let hero = kit::hero(st, f.cj22b);
        hero.big.set_width(180);
        hero.big.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
        hero.big.set_height(f.cj22b.line_height());
        self.cp.hero = hero;
        self.cp.core = hero.st;
        // the switch sits over the delay (hero r1): 2 px up, the delay and its caption 2 px
        // down, so they no longer touch (10-02)
        self.cp.sw = kit::toggle(st, CARD_W - PAD, 4, Some(chill_master_cb), 0);
        hero.r1.set_y(32);
        hero.r2.set_y(54);
        kit::show(self.cp.sw, false); // shown once its state is known
        self.home_sc_force(); // the next refresh syncs it
        kit::show(hero.rtop, false);
        self.cp.conns = kit::row(st, kit::HERO_H, tr("连接"), true);
        self.cp.traffic = kit::row(st, kit::HERO_H + ROW_H, tr("流量"), false);
        let mut y = 4 + kit::HERO_H + 2 * ROW_H + 10;

        kit::section(sc, y, tr("出口"));
        y += 20;
        let md = kit::card(sc, MARGIN, y, CARD_W, 100);
        for i in 0..3 {
            self.cp.opt[i] = kit::opt(md, 12 + i as i32 * 94, 12, tr(LABELS[i]), tr(SUBS[i]), Some(chill_mode_cb), i);
            self.cp.opt[i].obj.set_width(88);
            kit::opt_set(&self.cp.opt[i], false, false);
        }
        self.cp.exit_note = kit::label_w(md, f.cj12, th.t3, 12, 68, CARD_W - 24, false, "");
        y += 100 + 10;

        self.cp.prof_y = y;
        self.cp.prof_sec = kit::section(sc, y, tr("档位"));
        self.cp.prof_note = kit::label_r(sc, f.cj12, th.t3, W - MARGIN - 6, y, "");
        y += 20;
        let pf = kit::card(sc, MARGIN, y, CARD_W, 70);
        self.cp.prof_card = pf;
        for i in 0..3 {
            self.cp.prof[i] = kit::opt(pf, 12 + i as i32 * 94, 12, tr(PLABELS[i]), tr(PSUBS[i]), Some(chill_profile_cb), i);
            self.cp.prof[i].obj.set_width(88);
            kit::opt_set(&self.cp.prof[i], false, false);
        }
        y += 70 + 10;

        kit::section(sc, y, tr("配置档案"));
        self.cp.configs_note = kit::label_r(sc, f.cj12, th.t3, W - MARGIN - 6, y, "");
        y += 20;
        self.cp.configs_card = kit::card(sc, MARGIN, y, CARD_W, ROW_H);
        self.cp.configs_refresh = kit::bx(self.cp.configs_card, 0, 0, CARD_W, ROW_H, th.card, 0);
        self.cp.configs_refresh.set_style_bg_opa(lv::OPA_TRANSP, 0);
        kit::tappable(self.cp.configs_refresh, Some(chill_config_refresh_cb), 0);
        self.cp.configs_refresh_text = kit::label_w(self.cp.configs_refresh, f.cj14, th.acc_t, PAD, 11, CARD_W - 2 * PAD, false, "");
        y += ROW_H + 10;

        let nav = kit::card(sc, MARGIN, y, CARD_W, CHILL_NAV_ROWS as i32 * ROW_H);
        self.cp.nav_card = nav;
        for i in 0..CHILL_NAV_ROWS {
            self.cp.nav_val[i] = kit::row_nav(nav, i as i32 * ROW_H, tr(NAV_CAP[i]), i == 0, Some(chill_nav_cb), NAV_CHILD[i] as usize);
        }
        sc.scroll_to_y(0);
    }

    /// 策略组 chips flow like band chips; the node list follows them.
    fn chill_nodes_layout(&mut self, ng: usize) {
        let cp = &self.cp;
        let (mut x, mut y, maxx) = (PAD, 12, CARD_W - PAD);
        for i in 0..ng {
            // Measure the new name naturally, then constrain it to the card before wrapping.
            cp.grp_lbl[i].set_width(lv::SIZE_CONTENT);
            cp.grp_lbl[i].update_layout();
            let w = (cp.grp_lbl[i].width() + 22).min(maxx - PAD);
            cp.grp_lbl[i].set_width(w - 22);
            cp.grp_btn[i].set_width(w);
            if x > PAD && x + w > maxx {
                x = PAD;
                y += 36;
            }
            cp.grp_btn[i].set_pos(x, y);
            x += w + 6;
        }
        let gh = y + 28 + 12;
        cp.grp_card.set_height(gh);
        let ny = 24 + gh + 10;
        // The action owns a full row: its expanded target must not hit the first node.
        cp.node_sec.set_y(ny + (ROW_H - uf().cj12.line_height()) / 2);
        cp.delay_lbl.align(lv::LV_ALIGN_TOP_RIGHT, -(MARGIN + 6), ny + (ROW_H - uf().cj13.line_height()) / 2);
        cp.node_card.set_y(ny + ROW_H);
    }

    pub(super) fn build_sub_chill_nodes(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = kit::scroll(p, 0, SUB_VIEW, 200 + CHILL_MAX_NODES as i32 * ROW_H);
        let cp = &mut self.cp;
        cp.scroll_nodes = sc;
        kit::section(sc, 4, tr("策略组"));
        cp.grp_card = kit::card(sc, MARGIN, 24, CARD_W, 52);
        cp.grp_empty = kit::label_w(cp.grp_card, f.cj14, th.t3, PAD, 11, CARD_W - 2 * PAD, true, tr("还没读到策略组"));
        for i in 0..CHILL_MAX_GROUPS {
            let (b, l) = kit::chip(cp.grp_card, PAD, 12, "");
            l.set_style_text_font(f.cj13, 0);
            l.set_height(f.cj13.line_height());
            l.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
            l.set_y(6);
            kit::tappable(b, Some(chill_group_cb), i);
            kit::show(b, false);
            cp.grp_btn[i] = b;
            cp.grp_lbl[i] = l;
        }
        cp.node_sec = kit::section(sc, 90, tr("节点"));
        cp.delay_lbl = kit::label(sc, f.cj13, th.acc_t, 0, 90, tr("测延迟"));
        cp.delay_lbl.set_height(f.cj13.line_height());
        kit::tappable(cp.delay_lbl, Some(chill_delay_cb), 0);
        cp.node_card = kit::card(sc, MARGIN, 110, CARD_W, ROW_H);
        cp.node_empty = kit::label_w(cp.node_card, f.cj14, th.t3, PAD, 11, CARD_W - 2 * PAD, true, "");
        for i in 0..CHILL_MAX_NODES {
            let row = kit::bx(cp.node_card, 0, i as i32 * ROW_H, CARD_W, ROW_H, th.card, 0);
            row.set_style_bg_opa(lv::OPA_TRANSP, 0);
            kit::tappable(row, Some(chill_node_cb), i);
            if i > 0 {
                kit::sep(row, 0);
            }
            cp.node_ok.push(kit::label(row, lv::montserrat(14), th.acc_t, PAD, 12, kit::SYMBOL_OK));
            cp.node_name.push(kit::label_w(row, f.cj14, th.t1, PAD + 22, 11, CARD_W - 2 * PAD - 22 - 70, false, ""));
            cp.node_dl.push(kit::label_r(row, f.n15, th.t3, CARD_W - PAD, 10, ""));
            kit::show(row, false);
            cp.node_row.push(row);
        }
        self.chill_nodes_layout(0);
        sc.scroll_to_y(0);
    }

    pub(super) fn build_sub_chill_pairs(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = kit::scroll(p, 0, SUB_VIEW, 24 + CHILL_TRAF_ROWS as i32 * ROW_H + 16);
        kit::section(sc, 4, tr("规则 → 节点流量"));
        let cp = &mut self.cp;
        cp.pair_card = kit::card(sc, MARGIN, 24, CARD_W, CHILL_TRAF_ROWS as i32 * ROW_H);
        cp.pair_empty = kit::label_w(cp.pair_card, f.cj14, th.t3, PAD, 11, CARD_W - 2 * PAD, true, "");
        for i in 0..CHILL_TRAF_ROWS {
            cp.pair_sep[i] = if i > 0 { kit::sep(cp.pair_card, i as i32 * ROW_H) } else { Obj::NULL };
            cp.pair_name[i] = kit::label_w(cp.pair_card, f.cj13, th.t1, PAD, i as i32 * ROW_H + 12, 190, false, "");
            cp.pair_val[i] = kit::label_r(cp.pair_card, f.n12, th.t2, CARD_W - PAD, i as i32 * ROW_H + 13, "");
            kit::show(cp.pair_name[i], false);
            kit::show(cp.pair_val[i], false);
        }
        sc.scroll_to_y(0);
    }

    /// The CHILL pages (M5: on chill_poll, every second).
    #[allow(clippy::needless_range_loop)]
    pub(super) fn chill_page_paint(&mut self) {
        let th = t();
        let m = self.model.clone();
        let s = |o: &Option<String>| o.clone().unwrap_or_default();
        let online = m.chill_online != 0;
        let node = s(&m.chill_node);
        let cp = self.cp.clone();
        self.set(cp.core, s(&m.chill_core));
        if online as i32 != self.cp.on_shown {
            self.cp.on_shown = online as i32;
            kit::hero_tone(&cp.hero, if online { 0 } else { 3 });
        }
        let mut dl: Vec<u8> = "-".into();
        if let Some(ni) = m.chill_nodes.iter().find(|ni| ni.selected != 0 && ni.name == node) {
            if ni.delay > 0 {
                dl = cf!("%d ms", ni.delay);
            } else if ni.delay == 0 {
                dl = tr("超时").into();
            }
        }
        self.set(cp.hero.big, if online && !node.is_empty() { node.as_str() } else { "—" });
        self.set(cp.hero.r1, if online { dl.as_slice() } else { b"" });
        let grp = s(&m.chill_group);
        self.set(cp.hero.r2, if online { grp.as_str() } else { tr("打开右上角开关启动") });
        self.set(cp.conns, format!("{} · {}", s(&m.chill_conns), s(&m.chill_conn_split)));
        self.set(cp.traffic, format!("{} · {}", s(&m.chill_speed), s(&m.chill_traffic)));
        let xraw = s(&m.chill_mode_raw);
        for i in 0..3 {
            kit::opt_set(&cp.opt[i], xraw == EXIT_STATE[i], false);
        }
        let note = m.chill_tun.as_ref().map(|t| format!("TUN {} · {} · {}{}",
            if t.enable { tr("已开启") } else { tr("已关闭") }, t.stack, t.device,
            if t.auto_route { " · 自动路由" } else { "" })).unwrap_or_default();
        self.set(cp.exit_note, &note);
        {
            let pr = s(&m.chill_profile_raw);
            for i in 0..3 {
                kit::opt_set(&cp.prof[i], pr == PROFILE[i], false);
            }
            let hot = m.chill_thermal_eco != 0 && s(&m.chill_profile_effective_raw) != pr;
            self.set(cp.prof_note, if hot { tr("太热 · 暂按省电") } else { tr("进出省电断网约 10 秒") });
            kit::text_color(cp.prof_note, if hot { th.warn_t } else { th.t3 });
        }
        let count = m.chill_config_profiles.len();
        while self.cp.configs_rows.len() < count {
            let i = self.cp.configs_rows.len();
            let row = kit::bx(cp.configs_card, 0, i as i32 * ROW_H, CARD_W, ROW_H, th.card, 0);
            row.set_style_bg_opa(lv::OPA_TRANSP, 0);
            kit::tappable(row, Some(chill_config_cb), i);
            if i > 0 { kit::sep(row, 0); }
            let name = kit::label_w(row, uf().cj14, th.t1, PAD, 11, 150, false, "");
            let value = kit::label_r(row, uf().cj12, th.t2, CARD_W - PAD, 12, "");
            self.cp.configs_rows.push(row);
            self.cp.configs_names.push(name);
            self.cp.configs_values.push(value);
        }
        for i in 0..self.cp.configs_rows.len() {
            kit::show(self.cp.configs_rows[i], i < count);
            if let Some(p) = m.chill_config_profiles.get(i) {
                let name = self.cp.configs_names[i];
                let value = self.cp.configs_values[i];
                self.set(name, &p.name);
                self.set(value, format!("{}{}", p.kind, if p.active { " · 当前" } else { " ›" }));
            }
        }
        let can_refresh = m.chill_config_profiles.iter().any(|p| p.active);
        kit::show(cp.configs_refresh, can_refresh);
        cp.configs_refresh.set_y(count as i32 * ROW_H);
        let refresh_armed = self.cp.configs_refresh_arm.live(0, lv::tick_get(), XM_ARM_MS);
        self.set(cp.configs_refresh_text, if refresh_armed {
            tr("再点一次：刷新当前档案（可能短暂断网）")
        } else { tr("刷新当前档案 ›") });
        let configs_h = (count + usize::from(can_refresh)).max(1) as i32 * ROW_H;
        cp.configs_card.set_height(configs_h);
        cp.nav_card.set_y(cp.configs_card.style_y(0) + configs_h + 10);
        kit::scroll_extent(cp.scroll, cp.nav_card.style_y(0) + CHILL_NAV_ROWS as i32 * ROW_H + 16);
        self.set(cp.configs_note, match m.chill_config_status.as_deref() {
            Some("running") => tr("正在切换…"),
            Some("refresh_running") => tr("正在刷新…"),
            Some("error") | Some("timeout") => tr("操作失败，请重试"),
            Some("done") => tr("已切换"),
            Some("refresh_done") => tr("已刷新"),
            _ if count == 0 => tr("未配置档案"),
            _ => "",
        });
        let ng = m.chill_groups.len().min(CHILL_MAX_GROUPS);
        let mut names: Vec<String> = Vec::new();
        for i in 0..CHILL_MAX_GROUPS {
            if i >= ng {
                kit::show(cp.grp_btn[i], false);
                continue;
            }
            let gi = &m.chill_groups[i];
            cp.grp_btn[i].remove_flag(lv::LV_OBJ_FLAG_HIDDEN);
            self.set(cp.grp_lbl[i], &gi.name);
            // the layout's signature: the 48-byte name cache
            let mut nm = gi.name.as_bytes().to_vec();
            nm.truncate(47);
            names.push(String::from_utf8_lossy(&nm).into_owned());
            if gi.auto_pick != 0 && gi.selected == 0 {
                // one colour per refresh: chip_set's t2 then t3 redrew the chip every second
                kit::bg(cp.grp_btn[i], th.track);
                kit::text_color(cp.grp_lbl[i], th.t3);
            } else {
                kit::chip_set(cp.grp_btn[i], cp.grp_lbl[i], gi.selected != 0);
            }
        }
        {
            let sig = (ng, names.concat());
            if self.cp.grp_sig.as_ref() != Some(&sig) {
                self.cp.grp_sig = Some(sig);
                self.chill_nodes_layout(ng);
            }
        }
        kit::show(cp.grp_empty, ng == 0);
        let pending = m.chill_delay_pending != 0;
        let nn = m.chill_nodes.len().min(CHILL_MAX_NODES);
        cp.delay_lbl.set_text(if !online { tr("服务不可用") } else if pending { tr("测试中…") } else { tr("测延迟") });
        kit::text_color(cp.delay_lbl, if !online || pending { th.t3 } else { th.acc_t });
        kit::show(cp.delay_lbl, nn > 0 || (online && pending));
        if online && nn > 0 && !pending {
            cp.delay_lbl.add_flag(lv::LV_OBJ_FLAG_CLICKABLE);
        } else {
            cp.delay_lbl.remove_flag(lv::LV_OBJ_FLAG_CLICKABLE);
        }
        kit::show(cp.node_empty, nn == 0);
        self.set(cp.node_empty, if online { tr("还没读到节点") } else { tr("服务不可用") });
        for i in 0..CHILL_MAX_NODES {
            if i >= nn {
                kit::show(cp.node_row[i], false);
                continue;
            }
            let ni = &m.chill_nodes[i];
            cp.node_row[i].remove_flag(lv::LV_OBJ_FLAG_HIDDEN);
            self.set(cp.node_name[i], &ni.name);
            // >0 = ms, 0 = timed out, -1 = not measured yet; the 150/400 ms tiers
            if ni.delay > 0 {
                self.set(cp.node_dl[i], cf!("%d ms", ni.delay));
            } else {
                self.set(cp.node_dl[i], if ni.delay == 0 { tr("超时") } else { "-" });
            }
            kit::text_color(
                cp.node_dl[i],
                if !online {
                    th.t3
                } else if ni.delay == 0 {
                    th.bad_t
                } else if ni.delay < 0 {
                    th.t3
                } else if ni.delay < 150 {
                    th.ok_t
                } else if ni.delay < 400 {
                    th.warn_t
                } else {
                    th.bad_t
                },
            );
            kit::show(cp.node_ok[i], ni.selected != 0);
        }
        cp.node_card.set_height(nn.max(1) as i32 * ROW_H);
        kit::scroll_extent(cp.scroll_nodes, cp.node_card.style_y(0) + nn.max(1) as i32 * ROW_H + 16);

        // 规则 → 节点: from the /connections chill_poll already read; one row per (rule, node)
        let tpn = m.chill_pairs.len();
        kit::show(cp.pair_empty, tpn == 0);
        self.set(cp.pair_empty, if online { tr("暂无活跃连接") } else { tr("服务不可用") });
        for i in 0..CHILL_TRAF_ROWS {
            if i >= tpn {
                cp.pair_name[i].add_flag(lv::LV_OBJ_FLAG_HIDDEN);
                cp.pair_val[i].add_flag(lv::LV_OBJ_FLAG_HIDDEN);
                continue;
            }
            let it = &m.chill_pairs[i];
            cp.pair_name[i].remove_flag(lv::LV_OBJ_FLAG_HIDDEN);
            cp.pair_val[i].remove_flag(lv::LV_OBJ_FLAG_HIDDEN);
            self.set(cp.pair_name[i], &it.name);
            self.set(cp.pair_val[i], &it.traffic);
            pair_name_fit(cp.pair_name[i], cp.pair_val[i]);
        }
        for i in 1..CHILL_TRAF_ROWS {
            kit::show(cp.pair_sep[i], i < tpn);
        }
        cp.pair_card.set_height(tpn.max(1) as i32 * ROW_H);

        // the page's two summary rows: plain counts
        self.set(cp.nav_val[CHILL_NAV_NODES], cf!(tr("%d 组 · %d 个"), ng, nn));
        self.set(cp.nav_val[CHILL_NAV_PAIRS], cf!(crate::trc!("规则节点", "%d 条"), tpn));
    }
}
