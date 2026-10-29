//! 蜂窝: the carrier card on top, 排查 / SIM 卡 › rows, 移动数据 and
//! 数据漫游 (two taps each), 网络模式 (lock.rs), then 网络 / 消息 › rows. Also, as in C: the
//! exit tab's frame (build_exit) and the Wi-Fi tab's device list (refresh_wifi).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::network::{NH_EXIT, NH_WIFI};
use super::{Sub, Ui, VIEW_H};
use crate::cf;
use crate::kit::{self, CARD_W, MARGIN, PAD, ROW_H, TAB_PAD, W};
use crate::lang::{n_, tr, trn};
use crate::logic::Arm;
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};
use ui_model::{core_snapshot_t, ni_client_t};

pub const MD_DATA: usize = 0;
pub const MD_ROAM: usize = 1;
const MD_ARM_MS: u32 = 5000;
pub const WIFI_MAX_CLI: usize = 5;
pub const WIFI_CLI_H: i32 = 50;

/// A lan-client-id kind token in words (core's list: contract.rs Client.kind); unknown ones as sent.
fn client_kind_word(k: &str) -> &str {
    match k {
        "phone" => tr("手机"),
        "tablet" => tr("平板"),
        "laptop" => tr("笔记本电脑"),
        "computer" => tr("电脑"),
        "tv" => tr("电视"),
        "speaker" => tr("音箱"),
        "watch" => tr("手表"),
        "printer" => tr("打印机"),
        "iot" => tr("智能设备"),
        _ => k,
    }
}

/// 「Apple · 手机 · 随机地址」: what lanid knows of a device, "" when nothing.
fn client_desc(c: &ui_model::lan_client_t) -> Vec<u8> {
    let mut parts: Vec<&str> = Vec::new();
    if !c.vendor.is_empty() {
        parts.push(&c.vendor);
    }
    if !c.kind.is_empty() {
        parts.push(client_kind_word(&c.kind));
    }
    if c.private {
        parts.push(tr("随机地址"));
    }
    parts.join(" · ").into_bytes()
}

#[derive(Default, Clone)]
pub struct Cell {
    pub scroll: Obj,
    pub rest: Obj,
    pub md_sw: [Obj; 2],
    md_st: [Obj; 2],
    md_note: Obj,
    md_hold: u32,
    /// armed id = switch * 2 + the way it is going
    md_arm: Arm,
    // 网络模式 card and its 「上次改动」 row (E4 T15); the cards below move with it
    mode_card: Obj,
    own_row: Obj,
    pub own_val: Obj,
    tail: Obj,
    tail_y: i32,
    /// 「测速 ›」「网络诊断 ›」 on this tab (the exit tab's 测速 row is built later)
    pub speed_sub: Obj,
    pub diag_sub: Obj,
    /// dual standby: 「双卡」 above 排查, one row per card (data card first)
    dual_box: Obj,
    dual_img: [Obj; 2],
    dual_name: [Obj; 2],
    dual_role: [Obj; 2],
    dual_sig: [Obj; 2],
    /// what each row's image shows: (slug, w); w 0 = none
    dual_logo: [(Option<String>, i32); 2],
}

/// 「双卡」: section title + two 56 px rows
const DUAL_ROW: i32 = 56;
const DUAL_BOX_H: i32 = 20 + 2 * DUAL_ROW;

/// 0 = none: a tick kept as a time stamp is never 0.
pub fn nz(t: u32) -> u32 {
    if t != 0 { t } else { 1 }
}

pub fn sw_apply(sw: Obj, on: bool) {
    if on {
        sw.add_state(lv::LV_STATE_CHECKED);
    } else {
        sw.remove_state(lv::LV_STATE_CHECKED);
    }
}

unsafe extern "C" fn md_sw_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let (id, sw) = unsafe { (lv::event_user(e), lv::event_target(e)) };
    super::with(|u| u.md_tap(id, sw));
}

/// A card row with a label, a state word and a switch.
pub fn toggle_row(c: Obj, y: i32, name: &str, first: bool, state: &mut Obj, cb: Option<lv::EventCb>, user: usize) -> Obj {
    if !first {
        kit::sep(c, y);
    }
    kit::label(c, uf().cj14, t().t1, PAD, y + 11, name);
    *state = kit::label_r(c, uf().cj12, t().t3, CARD_W - PAD - 52, y + 13, "");
    kit::toggle(c, CARD_W - PAD, y + 7, cb, user)
}

impl Ui {
    fn md_note(&mut self, text: &str, col: u32) {
        self.cell.md_note.set_text(text);
        kit::text_color(self.cell.md_note, col);
    }

    fn md_armed_row(&self) -> i32 {
        if self.cell.md_arm.at != 0 { self.cell.md_arm.id / 2 } else { -1 }
    }

    /// Both switches cut the data or cost money: the first tap only arms (the row says what
    /// the second does), a second within 5 s sends. 8 s after sending, readings do not move
    /// the switch (the firmware redials).
    fn md_tap(&mut self, id: usize, sw: Obj) {
        let on = sw.has_state(lv::LV_STATE_CHECKED);
        let now = lv::tick_get();
        let th = t();
        if self.cell.md_arm.tap((id * 2) as i32 + on as i32, now, MD_ARM_MS) {
            // core merges into the whole get_wwaniface object: only the switch pressed
            let k = if id == MD_DATA { "enabled" } else { "roaming" };
            self.data_control("cellular.set", &format!("{{\"{k}\":{}}}", on as i32), Some(&format!("{k}={}", on as i32)));
            if id == MD_DATA {
                self.aux.data = on as i32;
                self.aux.hold_data = nz(now);
            } else {
                self.aux.roam = on as i32;
                self.aux.hold_roam = nz(now);
            }
            self.cell.md_hold = nz(now);
            self.md_note(if on { tr("已下发，正在拨号…") } else { tr("已下发，正在断开…") }, th.t2);
            return;
        }
        // first tap: the switch goes back, the row says what the second tap does
        sw_apply(sw, !on);
        let msg = if id == MD_DATA {
            if on { tr("再按一次：打开移动数据") } else { tr("再按一次：关掉移动数据，所有设备断网") }
        } else if on {
            tr("再按一次：打开数据漫游，按漫游计费")
        } else {
            tr("再按一次：关掉数据漫游，漫游时会断网")
        };
        self.md_note(msg, th.warn_t);
    }

    pub(super) fn md_refresh(&mut self) {
        let now = lv::tick_get();
        if self.cell.md_note.is_null() {
            return;
        }
        let th = t();
        if self.cell.md_arm.expire(now, MD_ARM_MS) {
            // no second tap: dropped
            self.md_note(tr("会断网或按漫游计费，切换要按两次确认"), th.t3);
        }
        if self.cell.md_hold != 0 && now.wrapping_sub(self.cell.md_hold) >= 8000 {
            self.cell.md_hold = 0;
            self.md_note(tr("会断网或按漫游计费，切换要按两次确认"), th.t3);
        }
        let v = [self.aux.data, self.aux.roam];
        for (i, &vi) in v.iter().enumerate() {
            if self.md_armed_row() != i as i32 && self.cell.md_hold == 0 {
                sw_apply(self.cell.md_sw[i], vi == 1);
            }
            let st = self.cell.md_st[i];
            self.set(st, if vi < 0 { "—" } else if vi != 0 { tr("已开启") } else { tr("已关闭") });
        }
    }

    pub(super) fn build_cellular(&mut self, tile: Obj) {
        // 排查: under the carrier card; 测速 is also a row on the exit tab (the same page)
        const IDSD: [Sub; 3] = [Sub::Diag, Sub::Speed, Sub::Place];
        const NAMESD: [&str; 3] = [n_("网络诊断"), n_("测速"), n_("摆放模式")];
        const IDS1: [Sub; 2] = [Sub::Esim, Sub::Apn];
        const NAMES1: [&str; 2] = [n_("SIM 与 eSIM"), "APN"];
        const IDS2: &[Sub] = &[
            Sub::Net,
            Sub::Lock,
            Sub::Cell,
        ];
        const NAMES2: &[&str] = &[
            n_("运营商选择"),
            n_("锁频"),
            n_("小区信息"),
        ];
        let th = t();
        let f = uf();
        let sc = kit::scroll(tile, 0, VIEW_H, 1000);
        self.cell.scroll = sc;
        // 当前连接: every carrier (built with Home)
        self.home.ca_card.set_parent(sc);
        self.home.ca_card.set_pos(MARGIN, 4);
        // 双卡 (dual standby only): between the carrier card and the rest
        let db = Obj::create(sc);
        db.remove_style_all();
        db.remove_flag(lv::LV_OBJ_FLAG_SCROLLABLE);
        db.set_size(W, DUAL_BOX_H);
        kit::section(db, 0, tr("双卡"));
        let dc = kit::card(db, MARGIN, 20, CARD_W, 2 * DUAL_ROW);
        for i in 0..2 {
            let y = i as i32 * DUAL_ROW;
            if i > 0 {
                kit::sep(dc, y);
            }
            self.cell.dual_img[i] = Obj::image(dc);
            self.cell.dual_img[i].remove_flag(lv::LV_OBJ_FLAG_CLICKABLE);
            kit::show(self.cell.dual_img[i], false);
            self.cell.dual_name[i] = kit::label_w(dc, f.cj14, th.t1, PAD, y + 9, 170, false, "");
            self.cell.dual_name[i].set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
            self.cell.dual_role[i] = kit::label_r(dc, f.cj13, th.t2, CARD_W - PAD, y + 10, "");
            self.cell.dual_sig[i] = kit::label_w(dc, f.cj12, th.t2, PAD, y + 32, CARD_W - 2 * PAD, false, "");
        }
        kit::show(db, false);
        self.cell.dual_box = db;
        let rest = Obj::create(sc);
        rest.remove_style_all();
        rest.remove_flag(lv::LV_OBJ_FLAG_SCROLLABLE);
        rest.set_width(W);
        self.cell.rest = rest;
        let mut y = self.nav_card(rest, 4, n_("排查"), &IDSD, &NAMESD);
        self.cell.speed_sub = self.tile_sub[Sub::Speed as usize];
        self.cell.diag_sub = self.tile_sub[Sub::Diag as usize];
        y = self.nav_card(rest, y, n_("SIM 卡"), &IDS1, &NAMES1);

        kit::section(rest, y, tr("移动数据"));
        let mc = kit::card(rest, MARGIN, y + 20, CARD_W, 2 * ROW_H + 30);
        let mut st = [Obj::NULL; 2];
        self.cell.md_sw[MD_DATA] = toggle_row(mc, 0, tr("移动数据"), true, &mut st[MD_DATA], Some(md_sw_cb), MD_DATA);
        self.cell.md_sw[MD_ROAM] = toggle_row(mc, ROW_H, tr("数据漫游"), false, &mut st[MD_ROAM], Some(md_sw_cb), MD_ROAM);
        self.cell.md_st = st;
        self.cell.md_note =
            kit::label_w(mc, f.cj12, th.t3, PAD, 2 * ROW_H + 6, CARD_W - 2 * PAD, false, tr("会断网或按漫游计费，切换要按两次确认"));
        y += 20 + 2 * ROW_H + 30 + 10;

        kit::section(rest, y, tr("网络模式"));
        // 100 high: a failed switch needs two lines (try again, else reboot and switch at once)
        let md = kit::card(rest, MARGIN, y + 20, CARD_W, 100);
        self.lk.seg = kit::seg(md, PAD, 12, CARD_W - 2 * PAD, &super::lock::LK_MODE_N, Some(super::lock::lk_mode_cb));
        self.lk.mode_lbl = kit::label_w(md, f.cj12, th.t3, PAD, 54, CARD_W - 2 * PAD, true, tr("切换会短暂断网，需要按两次确认"));
        // 「上次改动 10-03 14:32 · 网页 ›」 (DD5): only while core knows who last changed it
        self.cell.mode_card = md;
        let own = kit::bx(md, 0, 100, CARD_W, ROW_H, th.card, 0);
        own.set_style_bg_opa(lv::OPA_TRANSP, 0);
        self.cell.own_val = kit::row_nav(own, 0, tr("上次改动"), false, Some(super::op::OP_OWNER_CB), 0);
        kit::show(own, false);
        self.cell.own_row = own;
        y += 20 + 100 + 10;

        // the cards after it in one layer, moved down as a whole when 「上次改动」 shows
        self.cell.tail_y = y;
        let tail = Obj::create(rest);
        tail.remove_style_all();
        tail.remove_flag(lv::LV_OBJ_FLAG_SCROLLABLE);
        tail.set_pos(0, y);
        tail.set_width(W);
        self.cell.tail = tail;
        let mut ty = self.nav_card(tail, 0, n_("网络"), IDS2, NAMES2);
        ty = self.nav_card(tail, ty, n_("消息"), &[Sub::Sms], &[n_("短信")]);
        tail.set_height(ty);
        rest.set_height(y + ty);
        self.cell_reflow();
    }

    /// The 双卡 card: shown in dual standby (`x` Some), hidden otherwise.
    pub(super) fn cell_dual(&mut self, x: Option<&ui_model::sim_dual_t>) {
        let th = t();
        let on = x.is_some();
        if self.cell.dual_box.is_null() {
            return;
        }
        if self.cell.dual_box.hidden() == on {
            kit::show(self.cell.dual_box, on);
            self.cell_reflow();
        }
        let Some(x) = x else { return };
        let mut order: Vec<&ui_model::sim_slot_t> = x.slots.iter().collect();
        order.sort_by_key(|s| s.role != "data");
        for i in 0..2 {
            let (img, name, role, sig) = (self.cell.dual_img[i], self.cell.dual_name[i], self.cell.dual_role[i], self.cell.dual_sig[i]);
            let Some(s) = order.get(i).copied() else {
                kit::show(img, false);
                self.set(name, "");
                self.set(role, "");
                self.set(sig, "");
                continue;
            };
            let (mcc, mnc) = (s.mcc.parse().unwrap_or(0), s.mnc.parse().unwrap_or(-1));
            let slug = Some(s.logo.as_str())
                .filter(|g| !g.is_empty())
                .or_else(|| crate::data::core_link::mainland_logo(mcc, mnc, &s.operator));
            if self.cell.dual_logo[i].0.as_deref() != slug {
                let w = slug.and_then(|g| Self::logo_load(img, g)).map_or(0, |(w, h)| {
                    img.set_pos(PAD, i as i32 * DUAL_ROW + 9 + (uf().cj14.line_height() - h) / 2);
                    w
                });
                self.cell.dual_logo[i] = (slug.map(String::from), w);
            }
            let lw = self.cell.dual_logo[i].1;
            kit::show(img, lw > 0);
            let cn = crate::data::core_link::mainland_operator_cn(mcc, mnc, &s.operator).unwrap_or(s.operator.as_str());
            // with a logo the name is not repeated: 卡 number only
            let text: Vec<u8> = if lw > 0 { cf!(tr("卡槽 %d"), s.slot) } else { cf!("%s · %s", cn, &cf!(tr("卡槽 %d"), s.slot)) };
            name.set_x(if lw > 0 { PAD + lw + 8 } else { PAD });
            self.set(name, &text);
            let data = s.role == "data";
            let r: &str = match s.state.as_str() {
                "" | "ready" => {
                    if data && x.switching != 0 {
                        tr("切换中")
                    } else if data {
                        tr("上网中")
                    } else {
                        tr("待机")
                    }
                }
                "locked" => tr("PIN 锁"),
                "absent" => tr("无卡"),
                "off" => tr("未启用"),
                _ => tr("未就绪"),
            };
            let place = crate::data::core_link::slot_place(s);
            let mut r: Vec<u8> = if place.is_empty() { r.into() } else { cf!("%s · %s", r, place) };
            // too wide beside the logo and 卡槽 N: the place alone (the order says which is data)
            let left = name.x() + lv::text_w(&text, uf().cj14) + 10;
            if !place.is_empty() && lv::text_w(&r, uf().cj13) > CARD_W - PAD - left {
                r = place.into();
            }
            self.set(role, r);
            let line: Vec<u8> = if s.has_sig != 0 {
                cf!(tr("信号强度 %d dBm · 信号质量 %s dB"), s.sig_rsrp, &s.sig_snr)
            } else {
                tr("信号强度 — · 信号质量 —").into()
            };
            self.set(sig, line);
            kit::text_color(name, if data { th.t1 } else { th.t2 });
        }
    }

    /// The 网络模式 card with or without 「上次改动」: 100 / 140 high, the cards after it move.
    pub(super) fn cell_own_layout(&mut self, show: bool) {
        let c = &self.cell;
        let extra = if show { ROW_H } else { 0 };
        if c.mode_card.is_null() {
            return;
        }
        kit::show(c.own_row, show);
        c.mode_card.set_height(100 + extra);
        c.tail.set_y(c.tail_y + extra);
        c.rest.set_height(c.tail_y + extra + c.tail.style_height(0));
        self.cell_reflow();
    }

    pub(super) fn cell_reflow(&mut self) {
        let c = &self.cell;
        if c.rest.is_null() {
            return;
        }
        let ca = self.home.ca_card;
        let h = ca.style_height(0);
        let mut y = if ca.hidden() { 0 } else { 4 + h + MARGIN - 4 };
        if !c.dual_box.hidden() {
            c.dual_box.set_y(y + 4);
            y += DUAL_BOX_H + MARGIN;
        }
        c.rest.set_y(y);
        kit::scroll_extent(c.scroll, y + c.rest.style_height(0) - 10 + TAB_PAD);
    }

    /// 出口: where the traffic goes out (CHILL, Tailscale), and how fast.
    pub(super) fn build_exit(&mut self, tile: Obj) {
        let sc = kit::scroll(tile, 0, VIEW_H, 1000);
        self.net.nh_scroll[NH_EXIT] = sc;
        // Home's CHILL card (Tailscale went back to Home 9-25)
        self.ex.chill_card.set_parent(sc);
        // 出口 IP (build_sub_net) on top, this card after it (net_reflow)
        self.net.nh_base[NH_EXIT] = 4;
        self.nav_card(sc, 4, n_("代理与测速"), &[Sub::Chill, Sub::Ts, Sub::Speed], &["CHILL", "Tailscale", n_("测速")]);
        self.net.exit_nav_sec = self.nav_sec;
        self.net.exit_nav_card = self.nav_card;
    }

    /// netinfo's Wi-Fi reading of this device (by MAC, else by IP).
    fn wifi_station_for(&self, mac: &str, ip: &str) -> Option<ni_client_t> {
        let n = self.model.netinfo.as_ref()?;
        let cl = &n.clients[..(n.nclients.max(0) as usize).min(n.clients.len())];
        cl.iter()
            .find(|c| !mac.is_empty() && !c.mac.is_empty() && mac.eq_ignore_ascii_case(&c.mac))
            .or_else(|| cl.iter().find(|c| !ip.is_empty() && ip == c.ip))
            .cloned()
    }

    /// 「5 GHz · ↓12K/s ↑3K/s · 信号很好」: the band first, the signal in words.
    fn ni_client_line(&self, cl: &ni_client_t) -> Vec<u8> {
        let bits = self.conf.speed_bits;
        let rate = |r: i64| if r >= 0 { super::refresh::fmt_rate_top(r, bits, false) } else { b"-".to_vec() };
        let (ra, rb) = (rate(cl.down_rate), rate(cl.up_rate));
        let band = if cl.band.is_empty() { tr("频段读不到 · ").as_bytes().to_vec() } else { cf!("%s · ", &cl.band) };
        let sig = super::home::wifi_sig_word(&cl.signal_tier).map_or(Vec::new(), |w| cf!(" · %s", w));
        let mut out = if cl.down_rate < 0 && cl.up_rate < 0 {
            cf!(tr("%s速率稍后显示%s"), &band, &sig)
        } else {
            cf!("%s↓%s/s ↑%s/s%s", &band, &ra, &rb, &sig)
        };
        out.truncate(95);
        out
    }

    /// 「协商 ↓2402 ↑1201 Mbps · Wi-Fi 6 · -39 dBm」: what the link negotiated; core sends null
    /// (parsed as 0) for what iw didn't print, and that field says 读不到 instead of a guess.
    fn ni_link_line(cl: &ni_client_t) -> Vec<u8> {
        let rate = |m: i32| if m > 0 { cf!("%d Mbps", m) } else { tr("读不到").as_bytes().to_vec() };
        let mut out = cf!(tr("协商 ↓%s ↑%s"), &rate(cl.link_down), &rate(cl.link_up));
        out.extend(if cl.wifi_gen > 0 { cf!(" · Wi-Fi %d", cl.wifi_gen) } else { cf!(" · %s", tr("协议读不到")) });
        out.extend(if cl.signal < 0 { cf!(" · %d dBm", cl.signal) } else { cf!(" · %s", tr("信号读不到")) });
        out.truncate(95);
        out
    }

    pub(super) fn refresh_wifi(&mut self, d: &core_snapshot_t) {
        let th = t();
        let w = self.wifi.clone();
        // "Open" is what the encryption mode says: core never sends the passphrase, so an
        // empty key is not an open network
        let open = d.ap_auth.is_empty() || d.ap_auth.contains("none");
        self.set(w.ssid, if d.ap_ssid.is_empty() { "—" } else { d.ap_ssid.as_str() });
        self.set(
            w.pass,
            if !d.ap_psk.is_empty() {
                d.ap_psk.as_str()
            } else if open {
                tr("无密码")
            } else {
                tr("密码未公开")
            },
        );
        // the mode as the backend says it: psk2 / sae / sae-mixed differ
        self.set(w.enc, if open { tr("开放") } else { d.ap_auth.as_str() });
        self.set(w.state, if d.ap_on != 0 { tr("已开启") } else { tr("已关闭") });
        kit::text_color(w.state, if d.ap_on != 0 { th.ok_t } else { th.t3 });

        // the client list: fixed slots, hidden/shown
        let n = (d.lan_n.max(0) as usize).min(WIFI_MAX_CLI);
        if d.lan_n as usize > WIFI_MAX_CLI {
            self.set(w.cli_n, cf!(tr("%d/%d 台"), n, d.lan_n));
        } else {
            self.set(w.cli_n, cf!(trn("%d 台", d.lan_n as i64), d.lan_n));
        }
        let mut cli_y = 0;
        for i in 0..WIFI_MAX_CLI {
            if i >= n {
                kit::show(w.cli[i], false);
                continue;
            }
            w.cli[i].remove_flag(lv::LV_OBJ_FLAG_HIDDEN);
            let c = d.lan.get(i).cloned().unwrap_or_default();
            // the user's alias first (core already prefers it in name; an older core may not)
            let name = [c.alias.as_str(), c.name.as_str()].into_iter().find(|v| !v.is_empty()).unwrap_or("?");
            self.set(w.cli_name[i], name);
            let desc = client_desc(&c);
            self.set(w.cli_ip[i], &c.ip);
            let extra = kit::name_address_reflow(w.cli_name[i], w.cli_ip[i], CARD_W - PAD);
            w.cli_mac[i].set_y(29 + extra);
            w.cli_link[i].set_y(47 + extra);
            w.cli_tot[i].set_y(65 + extra);
            w.cli_desc[i].set_y(83 + extra);
            // a Wi-Fi device: 「5 GHz · ↓… ↑… · 信号很好」, then the totals, then what lanid knows
            // on its own line; cable / USB devices keep the MAC
            let st = self.wifi_station_for(&c.mac, &c.ip);
            let mut rh = WIFI_CLI_H + extra;
            if let Some(s) = &st {
                let line = self.ni_client_line(s);
                self.set(w.cli_mac[i], &line);
                self.set(w.cli_link[i], Self::ni_link_line(s));
                rh += 18;
                let (a, b) = (super::home::fmt_bytes_total(s.down), super::home::fmt_bytes_total(s.up));
                self.set(w.cli_tot[i], cf!(tr("连上以来 ↓%s ↑%s"), &a, &b));
                rh += 18;
                if desc.is_empty() {
                    self.set(w.cli_desc[i], "");
                } else {
                    self.set(w.cli_desc[i], &desc);
                    rh += 18;
                }
            } else {
                let line = if desc.is_empty() { c.mac.as_bytes().to_vec() } else { [&desc[..], " · ".as_bytes(), c.mac.as_bytes()].concat() };
                self.set(w.cli_mac[i], &line);
                self.set(w.cli_link[i], "");
                self.set(w.cli_tot[i], "");
                self.set(w.cli_desc[i], "");
            }
            w.cli[i].set_y(cli_y);
            w.cli[i].set_height(rh);
            cli_y += rh;
        }
        // the card shrinks to the rows shown, DHCP follows
        kit::show(w.cli_empty, n == 0);
        let cli_h = if n > 0 { cli_y } else { ROW_H };
        w.cli_card.set_height(cli_h);
        let dy = w.cli_card.style_y(0) + cli_h + 10;
        w.dhcp_sec.set_y(dy);
        w.dhcp_card.set_y(dy + 20);
        let base = dy + 20 + 3 * ROW_H + 10;
        if base != self.net.nh_base[NH_WIFI] {
            self.net.nh_base[NH_WIFI] = base;
            self.net_relayout();
        }

        // DHCP
        self.set(w.gw, if d.lan_ip.is_empty() { "-" } else { d.lan_ip.as_str() });
        let pool = cf!(
            tr("%s · 共 %s"),
            if d.dhcp_first.is_empty() { "-" } else { d.dhcp_first.as_str() },
            if d.dhcp_count.is_empty() { "-" } else { d.dhcp_count.as_str() }
        );
        self.set_at(w.pool, 0, pool);
        let lt = super::atol(&d.dhcp_lease);
        let lease = if lt >= 3600 {
            cf!(tr("%ld 小时"), lt / 3600)
        } else if lt >= 60 {
            cf!(tr("%ld 分钟"), lt / 60)
        } else if lt > 0 {
            cf!(tr("%ld 秒"), lt)
        } else {
            b"-".to_vec()
        };
        self.set(w.lease, lease);
    }
}
