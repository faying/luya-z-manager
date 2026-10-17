//! src/ui_parts/exit.c: the 小区信息 page's frame (its values: refresh.rs) and the Tailscale page.
//! Also the CHILL card (built with Home, shown on the exit tab) with the 手动优先 notice on its
//! top, the 「在国外 · CHILL 怎么走」 sheet, and the CHILL pages with 家里出口.
// SPDX-License-Identifier: MIT

use super::home::fmt_bytes_total;
use super::network::NH_CELL;
use super::{Ui, SUB_VIEW};
use crate::kit::{self, CARD_W, MARGIN, ROW_H};
use crate::lang::{n_, tr};
use crate::lv::Obj;
use crate::theme::uf;

/// 小区信息's values (the old 信令读取 page).
#[derive(Default, Clone)]
pub struct SigPage {
    nr: [Obj; 6],
    lt: [Obj; 6],
    net: [Obj; 4],
    nr_sec: Obj,
    lte_sec: Obj,
}

/// tailscale.h TS_PEER_MAX
pub const TS_PEER_MAX: usize = 12;
const TS_SELF_ROWS: usize = 8;
const TS_PEER_H: i32 = 70;

/// The Tailscale page: this device, then each node in three lines (name and how it is
/// connected; IP and system; the link to this device: direct address or which DERP, the
/// last handshake, how much went each way).
#[derive(Default)]
pub struct TsPage {
    self_v: [Obj; TS_SELF_ROWS],
    card: Obj,
    row: [Obj; TS_PEER_MAX],
    name: [Obj; TS_PEER_MAX],
    ip: [Obj; TS_PEER_MAX],
    tag: [Obj; TS_PEER_MAX],
    link: [Obj; TS_PEER_MAX],
}

/// "42 秒前" / "5 分钟前" / "3 小时前" / "2 天前"
pub fn fmt_ago(s: i64) -> Vec<u8> {
    if s < 60 {
        crate::cf!(tr("%ld 秒前"), s)
    } else if s < 3600 {
        crate::cf!(tr("%ld 分钟前"), s / 60)
    } else if s < 86400 {
        crate::cf!(tr("%ld 小时前"), s / 3600)
    } else {
        crate::cf!(tr("%ld 天前"), s / 86400)
    }
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

    /// 小区信息's values (refresh.c 「信令读取 subpage」), every refresh.
    pub(super) fn sig_paint(&mut self, mode_word: &str) {
        use crate::logic::{self, Rat};
        let m = self.model.clone();
        let d = &m.data;
        let sg = self.sg.clone();
        let or_dash = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
        self.set(sg.nr[0], crate::cf!("%s  %s MHz", &or_dash(&d.nr_band), &or_dash(&d.nr_bw)));
        self.set(sg.nr[1], crate::cf!("%ld", d.nr_channel));
        self.set(sg.nr[2], crate::cf!("%d", d.nr_pci));
        self.set(sg.nr[3], crate::cf!("%ld", d.nr_cell_id));
        self.set(sg.nr[4], crate::cf!("%d-%02d %s", d.mcc, d.mnc, &d.operator_name));
        self.set(sg.nr[5], crate::cf!("%d / %d / %s", d.nr_rsrp, d.nr_rsrq, &or_dash(&d.nr_snr)));
        // the serving cell: lteca's first entry (the primary) when there is one, else net.*'s lte_*
        let pc = crate::net_view::parse_ca(&d.lteca, 1, b'B');
        let have = !pc.is_empty();
        let mut bs = String::new();
        let bw = if have { pc[0].bw } else { super::atol(&d.bandwidth) as i32 };
        let earfcn = if have { pc[0].arfcn } else { d.channel };
        let pci = if have { pc[0].pci } else { d.lte_pci };
        if have {
            bs = format!("B{}", pc[0].band);
        } else if !d.band.is_empty() && d.band.contains("LTE") {
            bs = logic::band_short(&d.band, false);
        }
        let rat = logic::rat(&d.net_type);
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
        self.set(sg.net[1], or_dash(&d.wan_status));
        self.set(sg.net[2], &d.net_type);
        self.set(sg.net[3], if d.hsr != 0 { tr("开启") } else { tr("关闭") });
    }

    pub(super) fn build_sub_ts(&mut self, p: Obj) {
        const SELF_CAP: [&str; TS_SELF_ROWS] =
            [n_("主机名"), "IP", "IPv6", n_("DERP 中继"), n_("子网路由"), "Tailnet", n_("版本"), n_("密钥到期")];
        let th = crate::theme::t();
        let f = uf();
        let pad = crate::kit::PAD;
        let sc = kit::scroll(p, 0, SUB_VIEW, 24 + TS_SELF_ROWS as i32 * ROW_H + 10 + 20 + TS_PEER_MAX as i32 * TS_PEER_H + 16);
        kit::section(sc, 4, tr("本机"));
        let me = kit::card(sc, MARGIN, 24, CARD_W, TS_SELF_ROWS as i32 * ROW_H);
        let tp = &mut self.ts;
        for (i, cap) in SELF_CAP.iter().enumerate() {
            tp.self_v[i] = kit::row(me, i as i32 * ROW_H, tr(cap), i == 0);
        }
        for i in [0, 5, 7] {
            tp.self_v[i].set_style_text_font(f.cj14, 0);
        }
        let y = 24 + TS_SELF_ROWS as i32 * ROW_H + 10;
        kit::section(sc, y, tr("节点（和本机之间）"));
        tp.card = kit::card(sc, MARGIN, y + 20, CARD_W, TS_PEER_MAX as i32 * TS_PEER_H);
        for i in 0..TS_PEER_MAX {
            let r = kit::bx(tp.card, 0, i as i32 * TS_PEER_H, CARD_W, TS_PEER_H, th.card, 0);
            r.set_style_bg_opa(crate::lv::OPA_TRANSP, 0);
            if i > 0 {
                kit::sep(r, 0);
            }
            tp.name[i] = kit::label_w(r, f.cj14, th.t1, pad, 8, 170, false, "");
            tp.name[i].set_long_mode(crate::lv::LV_LABEL_LONG_MODE_DOTS);
            tp.name[i].set_height(f.cj14.line_height());
            tp.ip[i] = kit::label(r, f.n11, th.t3, pad, 30, "");
            tp.tag[i] = kit::label_r(r, f.cj13, th.t3, CARD_W - pad, 9, "");
            tp.link[i] = kit::label_w(r, f.cj12, th.t2, pad, 47, CARD_W - 2 * pad, false, "");
            tp.link[i].set_long_mode(crate::lv::LV_LABEL_LONG_MODE_DOTS);
            tp.link[i].set_height(f.cj12.line_height());
            kit::show(r, false);
            tp.row[i] = r;
        }
        sc.scroll_to_y(0);
    }

    /// The Tailscale page (refresh.c 「Tailscale」, the subpage part): own identity and the
    /// peer list. Drawn with Home's card, when Tailscale is there.
    pub(super) fn ts_page_paint(&mut self) {
        let th = crate::theme::t();
        let m = self.model.clone();
        let ts = &m.tailscale;
        let dash = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
        let tp = &self.ts;
        tp.self_v[0].set_text(dash(&ts.name));
        tp.self_v[1].set_text(dash(&ts.ip));
        tp.self_v[2].set_text(dash(&ts.ip6));
        tp.self_v[3].set_text(dash(&ts.relay));
        tp.self_v[4].set_text(dash(&ts.routes));
        tp.self_v[5].set_text(dash(&ts.tailnet));
        tp.self_v[6].set_text(crate::cf!(
            "%s%s%s",
            if ts.version.is_empty() { "-" } else { ts.version.as_str() },
            if ts.os.is_empty() { "" } else { " · " },
            &ts.os
        ));
        tp.self_v[7].set_text(if ts.key_expiry.is_empty() { tr("不过期") } else { ts.key_expiry.as_str() });
        let pn = m.tailscale_peers.len().min(TS_PEER_MAX);
        for i in 0..TS_PEER_MAX {
            if i >= pn {
                kit::show(tp.row[i], false);
                continue;
            }
            let pe = &m.tailscale_peers[i];
            kit::show(tp.row[i], true);
            tp.name[i].set_text(dash(&pe.name));
            let mut trf: Vec<u8> = Vec::new();
            if pe.rx != 0 || pe.tx != 0 {
                trf = crate::cf!(" · ↓%s ↑%s", &fmt_bytes_total(pe.rx), &fmt_bytes_total(pe.tx));
                trf.truncate(47);
            }
            tp.ip[i].set_text(crate::cf!(
                "%s%s%s%s%s",
                &pe.ip,
                if pe.os.is_empty() { "" } else { " · " },
                &pe.os,
                if pe.exit_node != 0 { tr(" · 出口节点") } else { "" },
                &trf
            ));
            // the third line: how it is linked with this device, the last handshake
            let mut how: Vec<u8> = if pe.active != 0 && pe.direct != 0 {
                crate::cf!(tr("直连 %s"), &pe.cur_addr)
            } else if pe.active != 0 {
                crate::cf!(tr("经 DERP %s 中继"), if pe.relay.is_empty() { "?" } else { pe.relay.as_str() })
            } else if pe.online != 0 {
                tr("在线，现在没在传数据").into()
            } else {
                // not 「last seen」: LastSeen is real UTC, the device clock is local time
                // labelled UTC
                tr("离线").into()
            };
            how.truncate(79);
            let mut ago: Vec<u8> = Vec::new();
            if pe.hs_ago >= 0 && (pe.active != 0 || pe.online != 0) {
                ago = fmt_ago(pe.hs_ago);
                ago.truncate(23);
            }
            let mut line = crate::cf!("%s%s%s", &how, if ago.is_empty() { "" } else { tr(" · 握手 ") }, &ago);
            line.truncate(159);
            tp.link[i].set_text(&line);
            kit::text_color(tp.link[i], if pe.online != 0 { th.t2 } else { th.t3 });
            // one tag, the most specific first: how it is connected beats online/offline
            let tag = if pe.active != 0 {
                if pe.direct != 0 { tr("直连") } else { tr("中继") }
            } else if pe.online != 0 {
                tr("在线")
            } else {
                tr("离线")
            };
            tp.tag[i].set_text(tag);
            kit::text_color(tp.tag[i], if pe.active != 0 { th.ok_t } else if pe.online != 0 { th.t2 } else { th.t3 });
            kit::text_color(tp.name[i], if pe.online != 0 { th.t1 } else { th.t3 });
        }
        tp.card.set_height(pn.max(1) as i32 * TS_PEER_H);
    }
}

use super::home::{mf_view, mf_wrap};
use super::network::NH_EXIT;
use super::{Sub, TAB_EXIT, TAB_HOME};
use crate::cf;
use crate::clock;
use crate::kit::{Sheet, PAD, R_CARD, W};
use crate::logic::Arm;
use crate::lv::{self, sys};
use crate::theme::t;

pub const CHILL_HOME_ROWS: usize = 5;
pub const CHILL_HOME_ROW_H: i32 = 36;
/// pair rows start here
pub const CHILL_HOME_TOP: i32 = 100;

const XM_PROXY: usize = 0;
const XM_GLOBAL: usize = 1;
const XM_KEEP_AI: usize = 2;
const XM_ALL: usize = 3;
const XM_OFF: usize = 4;
const XM_N: usize = 5;
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
    nodes: Vec<devui_model::chill_node_info_t>,
    pairs: Vec<devui_model::chill_traffic_item_t>,
    chill_on: i32,
    chill_back: i32,
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

    /// The CHILL card (refresh.c 「CHILL (Home card + subpage)」, the card part): repainted
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
        };
        if self.ex.last.as_ref() == Some(&snap) {
            return;
        }
        self.ex.last = Some(snap);
        let s = |o: &Option<String>| o.clone().unwrap_or_default();
        let online = m.chill_online != 0;
        let csc = &m.scenario;
        let e = self.ex.clone();
        // hidden only when there is no CHILL at all; stopped still shows, with the way back
        kit::show(e.chill_card, online || csc.chill_on == 0);
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
        }
        self.ex.base_h = CHILL_HOME_TOP + vis_rows as i32 * CHILL_HOME_ROW_H + 2;
        e.chill_card.set_height(self.ex.base_h + self.ex.mf_dy);
    }

    /// The line on top of the exit tab's CHILL card; the rest of the card moves with it.
    /// Returns its height.
    pub(super) fn mf_exit_paint(&mut self) -> i32 {
        let th = t();
        let m = mf_view(&self.model.scenario);
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
    /// notice is on its top); 家里不通: the CHILL page's 家里出口.
    pub(super) fn exit_row_go(&mut self) {
        let m = mf_view(&self.model.scenario);
        if m.home {
            self.sub_open(Sub::Chill);
            return;
        }
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
        // 家里出口: read only on the CHILL page; repainted every round there (countdowns, an
        // arm running out); labels that did not change are not touched
        if self.sub_visible(Sub::Chill) {
            self.hx_paint();
        }
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
    // Abroad with a local SIM most traffic can go direct, but AI (regional limits, risk checks
    // on a sudden IP change) and VoWiFi (operators only take their own country's IP) keep the
    // old node. Four choices are the states people use; 「关闭」 sends AI and VoWiFi direct too,
    // so it takes two taps and says the cost.
    #[allow(clippy::needless_range_loop)]
    pub(super) fn exit_menu_refresh(&mut self) {
        const LBL: [&str; XM_N] =
            [n_("代理（和在国内一样）"), n_("全局"), n_("直连 · AI 不动"), n_("全部直连（AI 也直连）"), n_("关闭 CHILL（最省电）")];
        let th = t();
        let sc = self.model.scenario.clone();
        let x = self.model.chill_exit_raw.clone().unwrap_or_default();
        let on = sc.chill_on != 0;
        let e = self.ex.clone();
        for i in 0..XM_N {
            let cur = on
                && ((i == XM_PROXY && x == "proxy")
                    || (i == XM_GLOBAL && x == "global")
                    || (i == XM_KEEP_AI && x == "direct_keep_ai")
                    || (i == XM_ALL && x == "direct_all"));
            let mut text = tr(LBL[i]);
            // CHILL off: the first four mean nothing
            let mut col = if on { th.acc_t } else { th.t3 };
            let mut bg = 0;
            if i == XM_OFF {
                if !on {
                    text = tr("打开 CHILL");
                    col = th.ok_t;
                } else if self.ex.xm_off_arm.at != 0 {
                    text = tr("再点一下：关闭（AI、VoWiFi 也直连）");
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
            tr("换国外卡已自动切到「直连 · AI 不动」，回国自动回到「代理」")
        } else {
            tr("换回国内卡后自动回到「代理」")
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
        const STATE: [&str; 4] = ["proxy", "global", "direct_keep_ai", "direct_all"];
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
const EXIT_STATE: [&str; 4] = ["proxy", "global", "direct_keep_ai", "direct_all"];
const PROFILE: [&str; 3] = ["eco", "standard", "perf"];
/// chill.c SC_MAX_NODE (a manual group can have 168 nodes)
pub const CHILL_MAX_NODES: usize = 200;
pub const CHILL_MAX_GROUPS: usize = 12;
/// chill.c SC_TOP_SHOW
const CHILL_TRAF_ROWS: usize = 6;
const CHILL_NAV_ROWS: usize = 2;
const CHILL_NAV_NODES: usize = 0;
const CHILL_NAV_PAIRS: usize = 1;
// 家里出口 (E3, manager docs/designs/home-exit.md §10)
const CH_MEDIA: usize = 0;
const CH_IP: usize = 1;
const CH_REST: usize = 2;
const CH_N: usize = 3;
const HX_ARM_MS: u32 = 5000;
const HX_NOTE_MS: u32 = 4000;

#[derive(Clone)]
pub struct ChillPage {
    scroll: Obj,
    hero: kit::Hero,
    core: Obj,
    pub sw: Obj,
    conns: Obj,
    traffic: Obj,
    exit_note: Obj,
    opt: [kit::Opt; 4],
    prof: [kit::Opt; 3],
    prof_sec: Obj,
    prof_note: Obj,
    prof_card: Obj,
    prof_y: i32,
    nav_card: Obj,
    nav_val: [Obj; CHILL_NAV_ROWS],
    content_h: i32,
    on_shown: i32,
    // 节点
    scroll_nodes: Obj,
    grp_card: Obj,
    grp_btn: [Obj; CHILL_MAX_GROUPS],
    grp_lbl: [Obj; CHILL_MAX_GROUPS],
    grp_sig: Option<(usize, String)>,
    node_sec: Obj,
    delay_lbl: Obj,
    node_card: Obj,
    node_row: Vec<Obj>,
    node_ok: Vec<Obj>,
    node_name: Vec<Obj>,
    node_dl: Vec<Obj>,
    // 规则 → 节点
    pair_card: Obj,
    pair_name: [Obj; CHILL_TRAF_ROWS],
    pair_val: [Obj; CHILL_TRAF_ROWS],
    pair_sep: [Obj; CHILL_TRAF_ROWS],
    // 家里出口: a section under 出口; without one configured it is not drawn
    hx_sec: Obj,
    hx_card: Obj,
    hx_foot: Obj,
    hx_row: [Obj; CH_N],
    hx_name: [Obj; CH_N],
    hx_st: [Obj; CH_N],
    hx_sw: [Obj; CH_N],
    hx_again: [Obj; CH_N],
    hx_sep: [Obj; CH_N],
    hx_y: i32,
    hx_dy: i32,
    /// id = item * 2 + the state wanted
    pub hx_arm: Arm,
    /// what a tap just said (refused, switching …)
    hx_note: Vec<u8>,
    hx_note_col: u32,
    pub hx_note_at: u32,
    /// the second tap went out, no answer yet: this item says 「正在…」
    pub hx_sending: i32,
    hx_stcol: [u32; CH_N],
    hx_footcol: u32,
    hx_wash: [u32; CH_N],
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
            opt: [kit::Opt::default(); 4],
            prof: [kit::Opt::default(); 3],
            prof_sec: n,
            prof_note: n,
            prof_card: n,
            prof_y: 0,
            nav_card: n,
            nav_val: [n; CHILL_NAV_ROWS],
            content_h: 0,
            on_shown: -1,
            scroll_nodes: n,
            grp_card: n,
            grp_btn: [n; CHILL_MAX_GROUPS],
            grp_lbl: [n; CHILL_MAX_GROUPS],
            grp_sig: None,
            node_sec: n,
            delay_lbl: n,
            node_card: n,
            node_row: Vec::new(),
            node_ok: Vec::new(),
            node_name: Vec::new(),
            node_dl: Vec::new(),
            pair_card: n,
            pair_name: [n; CHILL_TRAF_ROWS],
            pair_val: [n; CHILL_TRAF_ROWS],
            pair_sep: [n; CHILL_TRAF_ROWS],
            hx_sec: n,
            hx_card: n,
            hx_foot: n,
            hx_row: [n; CH_N],
            hx_name: [n; CH_N],
            hx_st: [n; CH_N],
            hx_sw: [n; CH_N],
            hx_again: [n; CH_N],
            hx_sep: [n; CH_N],
            hx_y: 0,
            hx_dy: -1,
            hx_arm: Arm::default(),
            hx_note: Vec::new(),
            hx_note_col: 0,
            hx_note_at: 0,
            hx_sending: -1,
            hx_stcol: [0; CH_N],
            hx_footcol: 0,
            hx_wash: [0; CH_N],
        }
    }
}

unsafe extern "C" fn chill_mode_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| {
        u.chill_set_exit(EXIT_STATE[i]);
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
        u.src.chill_test_delay();
        u.sync();
    });
}

unsafe extern "C" fn chill_profile_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.chill_profile_tap(i));
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

unsafe extern "C" fn hx_row_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.hx_row_tap(i));
}

unsafe extern "C" fn hx_extend_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.hx_extend());
}

fn hx_block_text(b: &str) -> &'static str {
    match b {
        "chill_off" => tr("CHILL 已关"),
        "hot" => tr("过热暂停中，降温后自动恢复"),
        "mode_global" => tr("全局模式下不生效 · 换成代理或直连 · AI 不动"),
        "mode_direct" => tr("全部直连下不生效"),
        _ => "",
    }
}

fn hx_code_text(c: &str) -> &'static str {
    match c {
        "busy" => tr("正在切换，稍等"),
        "list_empty" => tr("表里还没有网站 · 见 /data/chill/home_ip.list"),
        "not_configured" => tr("家里出口没配置"),
        _ => {
            let b = hx_block_text(c);
            if b.is_empty() { tr("没成功，稍后再试") } else { b }
        }
    }
}

/// Whether this item shows now (版权 and 其余 only abroad, or while on).
fn hx_shown(h: &devui_model::chill_home_t, i: usize) -> bool {
    i == CH_IP || h.on.get(i).copied().unwrap_or(0) != 0 || h.in_china == 0
}

fn hx_on(h: &devui_model::chill_home_t, i: usize) -> bool {
    h.on.get(i).copied().unwrap_or(0) != 0
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
        if self.model.chill_profile_raw.as_deref().unwrap_or("") == PROFILE[i] {
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

    fn hx_say(&mut self, t: &str, col: u32) {
        self.cp.hx_note = t.as_bytes().to_vec();
        self.cp.hx_note.truncate(199);
        self.cp.hx_note_col = col;
        self.cp.hx_note_at = lv::tick_get().max(1);
    }

    fn hx_row_tap(&mut self, i: usize) {
        let th = t();
        let h = self.model.chill_home.clone().unwrap_or_default();
        let now = lv::tick_get();
        if h.known == 0 || h.updating != 0 {
            self.hx_say(tr("正在读取，稍等"), th.t2);
            self.hx_paint();
            return;
        }
        if h.busy != 0 || self.cp.hx_sending >= 0 {
            self.cp.hx_arm.clear();
            self.hx_say(tr("正在切换，稍等"), th.t2);
            self.hx_paint();
            return;
        }
        let want = !hx_on(&h, i);
        if want && !h.blocked.is_empty() {
            self.cp.hx_arm.clear();
            self.hx_say(hx_block_text(&h.blocked), th.warn_t);
            self.hx_paint();
            return;
        }
        if want && i == CH_IP && h.sites == 0 {
            self.cp.hx_arm.clear();
            self.hx_say(tr("表里还没有网站 · 见 /data/chill/home_ip.list"), th.warn_t);
            self.hx_paint();
            return;
        }
        if !self.cp.hx_arm.tap(i as i32 * 2 + want as i32, now, HX_ARM_MS) {
            // first tap: armed, drawn at once
            self.cp.hx_note_at = 0;
            self.hx_paint();
            return;
        }
        // second tap: 「正在…」 first, then send (the request is synchronous)
        self.cp.hx_sending = i as i32;
        self.cp.hx_note_at = 0;
        self.hx_paint();
        lv::refr_now();
        let (rc, code) = self.src.chill_home_set(i, want);
        self.sync();
        self.cp.hx_sending = -1;
        if rc == 0 {
            self.hx_say(tr("后台没回应"), th.bad_t);
        } else if rc == 409 {
            self.hx_say(hx_code_text(&code), th.warn_t);
        } else if !(200..300).contains(&rc) {
            self.hx_say(tr("没成功，稍后再试"), th.bad_t);
        }
        self.src.chill_home_read();
        self.sync();
        self.hx_paint();
    }

    fn hx_extend(&mut self) {
        let th = t();
        let until = self.src.chill_home_extend(&self.model).unwrap_or(0);
        self.sync();
        if until > 0 {
            let hm = clock::strftime("%H:%M", &clock::local(until));
            let s = cf!(tr("已延长到 %s"), &hm);
            self.hx_say(&String::from_utf8_lossy(&s), th.ok_t);
        } else {
            self.hx_say(tr("没延长成，稍后再试"), th.bad_t);
        }
        self.src.chill_home_read();
        self.sync();
        self.hx_paint();
    }

    /// An item's second line (DD6). Returns its colour.
    fn hx_status(&self, h: &devui_model::chill_home_t, i: usize, now: i64) -> (Vec<u8>, u32) {
        const IDLE: [&str; CH_N] = [n_("爱奇艺、优酷、B 站等"), "", n_("AI、VoWiFi、国内直连照旧")];
        let th = t();
        if self.cp.hx_sending == i as i32 || (h.busy != 0 && h.busy_item == i as i32) {
            let on = if self.cp.hx_sending == i as i32 { !hx_on(h, i) } else { h.busy_on != 0 };
            let out: Vec<u8> = if h.busy != 0 && h.busy_phase == "trial" {
                let left = (h.busy_until - now).clamp(0, 59);
                cf!(tr("试运行 0:%02ld"), left)
            } else if h.busy != 0 && h.busy_phase == "checking" {
                tr("正在测家里…").into()
            } else if on {
                tr("正在打开…").into()
            } else {
                tr("正在关闭…").into()
            };
            return (out, th.t2);
        }
        if h.nitem == i as i32 && !h.nkind.is_empty() && now - h.nat < 600 {
            if h.nkind == "failed" {
                return (cf!("■ %s", &h.ntext), th.bad_t);
            }
            if h.nkind == "opened" && now - h.nat < 3 {
                return (cf!("● %s", &h.ntext), th.ok_t);
            }
            if h.nkind != "opened" {
                return (h.ntext.clone().into(), th.t2);
            }
        }
        if hx_on(h, i) && h.home_ok == 0 {
            if i == CH_MEDIA || h.fallback_to.is_empty() {
                return (cf!("▲ %s", tr("家里不通，暂走直连")), th.warn_t);
            }
            return (cf!(tr("▲ 家里不通，暂走 %s"), &h.fallback_to), th.warn_t);
        }
        if i == CH_IP {
            if h.sites == 0 {
                return (tr("表里还没有网站 · 见 /data/chill/home_ip.list").into(), th.t3);
            }
            return (cf!(crate::lang::trn("%d 个网站", h.sites as i64), h.sites), if hx_on(h, i) { th.t2 } else { th.t3 });
        }
        if i == CH_REST && hx_on(h, i) {
            let left = (h.until - now).max(0);
            return (cf!(tr("剩 %ld:%02ld"), left / 3600, (left % 3600) / 60), th.t2);
        }
        (tr(IDLE[i]).into(), if hx_on(h, i) { th.t2 } else { th.t3 })
    }

    /// The line under the card: the armed tap's cost > what a tap said > a notice for no item >
    /// extend > why it is blocked > no country > the Tailscale path.
    fn hx_foot(&self, h: &devui_model::chill_home_t, now: i64) -> (Vec<u8>, u32, bool) {
        let th = t();
        let tick = lv::tick_get();
        if self.cp.hx_arm.live(-1, tick, HX_ARM_MS) {
            return ((if h.roaming != 0 { tr("会断一下，走漫游流量") } else { tr("会断一下（约 5 秒）") }).into(), th.warn_t, false);
        }
        let na = self.cp.hx_note_at;
        if na != 0 && tick.wrapping_sub(na) < HX_NOTE_MS {
            return (self.cp.hx_note.clone(), self.cp.hx_note_col, false);
        }
        if h.nitem < 0 && !h.nkind.is_empty() && now - h.nat < 600 {
            return (h.ntext.clone().into(), th.t2, false);
        }
        if h.can_extend != 0 && h.busy == 0 {
            return (tr("10 分钟后关 · 再开 2 小时 ›").into(), th.acc_t, true);
        }
        if !h.blocked.is_empty() {
            return (hx_block_text(&h.blocked).into(), th.t3, false);
        }
        if h.in_china < 0 && !hx_on(h, CH_MEDIA) && !hx_on(h, CH_REST) {
            return (tr("读不到所在国家，版权和其余暂不显示").into(), th.t3, false);
        }
        match h.ts.as_str() {
            "offline" => (cf!("■ %s", tr("连不上家里")), th.bad_t, false),
            "relay" => (cf!("▲ %s", tr("经中继，视频可能卡")), th.warn_t, false),
            "direct" => {
                if h.ms > 0 {
                    (cf!(tr("● 经 Tailscale · 直连 %d ms"), h.ms), th.ok_t, false)
                } else {
                    (cf!("● %s", tr("经 Tailscale · 直连")), th.ok_t, false)
                }
            }
            _ => (Vec::new(), th.t3, false),
        }
    }

    /// 档位 and the › card move with the height of 家里出口.
    fn hx_reflow(&mut self, dy: i32) {
        let cp = &mut self.cp;
        if dy == cp.hx_dy || cp.prof_sec.is_null() {
            return;
        }
        cp.hx_dy = dy;
        let y = cp.prof_y + dy;
        cp.prof_sec.set_y(y);
        cp.prof_note.set_y(y);
        cp.prof_card.set_y(y + 20);
        cp.nav_card.set_y(y + 20 + 70 + 10);
        kit::scroll_extent(cp.scroll, cp.content_h + dy);
    }

    pub(super) fn hx_paint(&mut self) {
        let th = t();
        if self.cp.hx_card.is_null() {
            return;
        }
        let h = self.model.chill_home.clone().unwrap_or_default();
        let show = h.known != 0 && h.configured != 0 && h.updating == 0;
        let cp = self.cp.clone();
        kit::show(cp.hx_sec, show);
        kit::show(cp.hx_card, show);
        kit::show(cp.hx_foot, show);
        if !show {
            self.hx_reflow(0);
            return;
        }
        let now = self.now();
        let tick = lv::tick_get();
        self.cp.hx_arm.expire(tick, HX_ARM_MS); // lapsed: plain rows again
        let dim = !h.blocked.is_empty() || h.stale != 0;
        let (mut yy, mut first) = (0, true);
        for i in 0..CH_N {
            let vis = hx_shown(&h, i);
            kit::show(cp.hx_row[i], vis);
            if !vis {
                continue;
            }
            cp.hx_row[i].set_y(yy);
            if !cp.hx_sep[i].is_null() {
                kit::show(cp.hx_sep[i], !first);
            }
            first = false;
            yy += kit::ROW2_H;
            let armed = self.cp.hx_arm.live(-1, tick, HX_ARM_MS) && self.cp.hx_arm.id / 2 == i as i32;
            let wash = if armed { th.wash_w } else { 0 };
            if wash != self.cp.hx_wash[i] {
                self.cp.hx_wash[i] = wash;
                if wash != 0 {
                    kit::bg(cp.hx_row[i], wash);
                }
                cp.hx_row[i].set_style_bg_opa(if wash != 0 { lv::OPA_COVER } else { lv::OPA_TRANSP }, 0);
            }
            kit::show(cp.hx_again[i], armed);
            kit::show(cp.hx_sw[i], !armed);
            kit::toggle_set(cp.hx_sw[i], hx_on(&h, i));
            if dim && !hx_on(&h, i) {
                cp.hx_sw[i].add_state(lv::LV_STATE_DISABLED);
            } else {
                cp.hx_sw[i].remove_state(lv::LV_STATE_DISABLED);
            }
            kit::text_color(cp.hx_name[i], if dim && !hx_on(&h, i) { th.t3 } else { th.t1 });
            let (mut st, mut col) = self.hx_status(&h, i, now);
            st.truncate(223);
            if h.stale != 0 {
                col = th.t3;
            }
            self.set(cp.hx_st[i], &st);
            if col != self.cp.hx_stcol[i] {
                self.cp.hx_stcol[i] = col;
                kit::text_color(cp.hx_st[i], col);
            }
        }
        cp.hx_card.set_height(yy);
        let (mut ft, mut fcol, extend) = self.hx_foot(&h, now);
        ft.truncate(223);
        if h.stale != 0 {
            let hm = clock::strftime("%H:%M", &clock::local(now));
            ft.extend_from_slice(&cf!(tr(" · 停在 %s"), &hm));
            ft.truncate(223);
            fcol = th.t3;
        }
        self.set(cp.hx_foot, &ft);
        if fcol != self.cp.hx_footcol {
            self.cp.hx_footcol = fcol;
            kit::text_color(cp.hx_foot, fcol);
        }
        if extend {
            cp.hx_foot.add_flag(lv::LV_OBJ_FLAG_CLICKABLE);
        } else {
            cp.hx_foot.remove_flag(lv::LV_OBJ_FLAG_CLICKABLE);
        }
        cp.hx_foot.set_y(cp.hx_y + 20 + yy + 6);
        cp.hx_foot.update_layout();
        let fh = if ft.is_empty() { 0 } else { cp.hx_foot.height() };
        self.hx_reflow(20 + yy + 6 + fh + 10);
    }

    #[allow(clippy::needless_range_loop)]
    fn build_home_exit(&mut self, p: Obj, y: i32) {
        const NAME: [&str; CH_N] = [n_("国内版权走家里"), n_("家里 IP 网站"), n_("其余都走家里 · 2 小时")];
        let th = t();
        let f = uf();
        let cp = &mut self.cp;
        cp.hx_y = y;
        cp.hx_sec = kit::section(p, y, tr("家里出口"));
        cp.hx_card = kit::card(p, MARGIN, y + 20, CARD_W, CH_N as i32 * kit::ROW2_H);
        for i in 0..CH_N {
            let row = kit::bx(cp.hx_card, 0, i as i32 * kit::ROW2_H, CARD_W, kit::ROW2_H, th.card, 0);
            row.set_style_bg_opa(lv::OPA_TRANSP, 0);
            kit::tappable(row, Some(hx_row_cb), i);
            cp.hx_row[i] = row;
            cp.hx_sep[i] = if i > 0 { kit::sep(row, 0) } else { Obj::NULL };
            cp.hx_name[i] = kit::label_w(row, f.cj15, th.t1, PAD, 7, CARD_W - 2 * PAD - 52, false, tr(NAME[i]));
            cp.hx_st[i] = kit::label_w(row, f.cj12, th.t3, PAD, 29, CARD_W - 2 * PAD - 52, false, "");
            cp.hx_sw[i] = kit::toggle(row, CARD_W - PAD, 12, None, 0);
            cp.hx_sw[i].remove_flag(lv::LV_OBJ_FLAG_CLICKABLE); // the row is the button, the switch only shows
            cp.hx_again[i] = kit::label_r(row, f.cj13, th.warn_t, CARD_W - PAD, 16, tr("再点一次确认"));
            kit::show(cp.hx_again[i], false);
        }
        cp.hx_foot = kit::label_w(p, f.cj12, th.t3, MARGIN + 6, y + 20 + CH_N as i32 * kit::ROW2_H + 6, CARD_W - 12, true, "");
        cp.hx_foot.add_event_cb(hx_extend_cb, lv::LV_EVENT_CLICKED, 0);
        cp.hx_foot.set_ext_click_area(12);
        kit::show(cp.hx_sec, false);
        kit::show(cp.hx_card, false);
        kit::show(cp.hx_foot, false);
    }

    pub(super) fn build_sub_chill(&mut self, p: Obj) {
        const LABELS: [&str; 4] = [n_("代理"), n_("全局"), n_("直连 · AI 不动"), n_("全部直连")];
        const SUBS: [&str; 4] = [n_("规则分流"), n_("全部走代理"), n_("国外卡用"), n_("AI 也直连")];
        const PLABELS: [&str; 3] = [n_("省电"), n_("标准"), n_("性能")];
        const PSUBS: [&str; 3] = [n_("更凉更省"), n_("默认"), n_("切换更快")];
        const NAV_CAP: [&str; CHILL_NAV_ROWS] = [n_("节点"), n_("规则 → 节点")];
        const NAV_CHILD: [Sub; CHILL_NAV_ROWS] = [Sub::ChillNodes, Sub::ChillPairs];
        let th = t();
        let f = uf();
        let content_h = 4 + kit::HERO_H + 80 + 10 + 20 + 124 + 10 + 20 + 70 + 10 + 80 + 16;
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
        self.cp.exit_note = kit::label_r(sc, f.cj12, th.t3, W - MARGIN - 6, y, "");
        y += 20;
        let md = kit::card(sc, MARGIN, y, CARD_W, 124);
        for i in 0..4 {
            self.cp.opt[i] = kit::opt(md, 12 + (i as i32 % 2) * 142, 12 + (i as i32 / 2) * 54, tr(LABELS[i]), tr(SUBS[i]), Some(chill_mode_cb), i);
            kit::opt_set(&self.cp.opt[i], false, false);
        }
        y += 124 + 10;

        // 家里出口: hidden when not configured; what is below moves with it (hx_paint)
        self.build_home_exit(sc, y);

        // 档位: three in a row (opt's 134 width is for two columns: 88 here)
        self.cp.prof_y = y;
        self.cp.prof_sec = kit::section(sc, y, tr("档位"));
        self.cp.prof_note = kit::label_r(sc, f.cj12, th.t3, W - MARGIN - 6, y, "");
        y += 20;
        let pf = kit::card(sc, MARGIN, y, CARD_W, 70);
        self.cp.prof_card = pf;
        for i in 0..3 {
            self.cp.prof[i] = kit::opt(pf, 12 + i as i32 * 94, 12, tr(PLABELS[i]), tr(PSUBS[i]), Some(chill_profile_cb), i);
            self.cp.prof[i].obj.set_width(88);
            kit::opt_set(&self.cp.prof[i], i == 1, false);
        }
        y += 70 + 10;

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
            cp.grp_lbl[i].update_layout();
            let w = cp.grp_lbl[i].width() + 22;
            cp.grp_btn[i].set_width(w);
            if x + w > maxx {
                x = PAD;
                y += 36;
            }
            cp.grp_btn[i].set_pos(x, y);
            x += w + 6;
        }
        let gh = y + 28 + 12;
        cp.grp_card.set_height(gh);
        let ny = 24 + gh + 10;
        cp.node_sec.set_y(ny);
        cp.delay_lbl.align(lv::LV_ALIGN_TOP_RIGHT, -(MARGIN + 6), ny);
        cp.node_card.set_y(ny + 20);
    }

    pub(super) fn build_sub_chill_nodes(&mut self, p: Obj) {
        let th = t();
        let f = uf();
        let sc = kit::scroll(p, 0, SUB_VIEW, 200 + CHILL_MAX_NODES as i32 * ROW_H);
        let cp = &mut self.cp;
        cp.scroll_nodes = sc;
        kit::section(sc, 4, tr("策略组"));
        cp.grp_card = kit::card(sc, MARGIN, 24, CARD_W, 52);
        for i in 0..CHILL_MAX_GROUPS {
            let (b, l) = kit::chip(cp.grp_card, PAD, 12, "");
            l.set_style_text_font(f.cj13, 0);
            l.set_y(6);
            kit::tappable(b, Some(chill_group_cb), i);
            kit::show(b, false);
            cp.grp_btn[i] = b;
            cp.grp_lbl[i] = l;
        }
        cp.node_sec = kit::section(sc, 90, tr("节点"));
        cp.delay_lbl = kit::label(sc, f.cj13, th.acc_t, 0, 90, tr("测延迟"));
        cp.delay_lbl.add_flag(lv::LV_OBJ_FLAG_CLICKABLE);
        cp.delay_lbl.set_ext_click_area(14);
        cp.delay_lbl.add_event_cb(chill_delay_cb, lv::LV_EVENT_CLICKED, 0);
        cp.node_card = kit::card(sc, MARGIN, 110, CARD_W, ROW_H);
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
        for i in 0..CHILL_TRAF_ROWS {
            cp.pair_sep[i] = if i > 0 { kit::sep(cp.pair_card, i as i32 * ROW_H) } else { Obj::NULL };
            cp.pair_name[i] = kit::label_w(cp.pair_card, f.cj13, th.t1, PAD, i as i32 * ROW_H + 12, 190, false, "");
            cp.pair_val[i] = kit::label_r(cp.pair_card, f.n12, th.t2, CARD_W - PAD, i as i32 * ROW_H + 13, "");
            kit::show(cp.pair_name[i], false);
            kit::show(cp.pair_val[i], false);
        }
        sc.scroll_to_y(0);
    }

    /// The CHILL pages (refresh.c 「CHILL (Home card + subpage)」, the page part; M5: on
    /// chill_poll, the C test's every second).
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
        self.set(cp.conns, s(&m.chill_conn_split));
        self.set(cp.traffic, s(&m.chill_traffic));
        let xraw = s(&m.chill_exit_raw);
        for i in 0..4 {
            kit::opt_set(&cp.opt[i], xraw == EXIT_STATE[i], false);
        }
        self.set(cp.exit_note, if xraw == "direct_all" { tr("AI、VoWiFi 也直连") } else { "" });
        {
            let pr = s(&m.chill_profile_raw);
            for i in 0..3 {
                kit::opt_set(&cp.prof[i], pr == PROFILE[i], false);
            }
            let hot = m.chill_thermal_eco != 0 && s(&m.chill_profile_effective_raw) != pr;
            self.set(cp.prof_note, if hot { tr("太热 · 暂按省电") } else { tr("进出省电断网约 10 秒") });
            kit::text_color(cp.prof_note, if hot { th.warn_t } else { th.t3 });
        }
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
            kit::chip_set(cp.grp_btn[i], cp.grp_lbl[i], gi.selected != 0);
            if gi.auto_pick != 0 && gi.selected == 0 {
                kit::text_color(cp.grp_lbl[i], th.t3);
            }
        }
        {
            let sig = (ng, names.concat());
            if self.cp.grp_sig.as_ref() != Some(&sig) {
                self.cp.grp_sig = Some(sig);
                self.chill_nodes_layout(ng);
            }
        }
        let pending = m.chill_delay_pending != 0;
        cp.delay_lbl.set_text(if pending { tr("测试中…") } else { tr("测延迟") });
        kit::text_color(cp.delay_lbl, if pending { th.t3 } else { th.acc_t });

        let nn = m.chill_nodes.len().min(CHILL_MAX_NODES);
        for i in 0..CHILL_MAX_NODES {
            if i >= nn {
                kit::show(cp.node_row[i], false);
                continue;
            }
            let ni = &m.chill_nodes[i];
            cp.node_row[i].remove_flag(lv::LV_OBJ_FLAG_HIDDEN);
            self.set(cp.node_name[i], &ni.name);
            // chill.c: >0 = ms, 0 = timed out, -1 = not measured yet; the 150/400 ms tiers
            if ni.delay > 0 {
                self.set(cp.node_dl[i], cf!("%d ms", ni.delay));
            } else {
                self.set(cp.node_dl[i], if ni.delay == 0 { tr("超时") } else { "-" });
            }
            kit::text_color(
                cp.node_dl[i],
                if ni.delay == 0 {
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

#[cfg(test)]
mod tests {
    use super::fmt_ago;

    #[test]
    fn ago_in_the_largest_whole_unit() {
        let _l = crate::lang::TEST_LANG.lock().unwrap_or_else(|e| e.into_inner());
        crate::lang::set_en(false);
        assert_eq!(fmt_ago(42), "42 秒前".as_bytes());
        assert_eq!(fmt_ago(300), "5 分钟前".as_bytes());
        assert_eq!(fmt_ago(3 * 3600 + 5), "3 小时前".as_bytes());
        assert_eq!(fmt_ago(2 * 86400), "2 天前".as_bytes());
    }
}
