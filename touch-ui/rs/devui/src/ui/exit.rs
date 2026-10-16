//! src/ui_parts/exit.c: the 小区信息 page's frame (its values: refresh.rs) and the Tailscale page.
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
