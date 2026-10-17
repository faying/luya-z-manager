//! 首页 (src/ui_parts/home.c + the Home parts of src/ui_parts/refresh.c): the status card (a
//! conclusion, then 载波 · Wi-Fi · 出口 · 流量), 情景, Tailscale, and the net-speed chart
//! (ui/system.rs, built with the charts). Cards are stacked by home_reflow under whatever the
//! status card shows.
// SPDX-License-Identifier: MIT

use super::{Sub, Ui, SF_OLD_DATAD, TAB_CELL, TAB_HOME, TAB_WIFI, VIEW_H};
use crate::cf;
use crate::clock;
use crate::kit::{self, Hero, CARD_W, HERO_H, MARGIN, PAD, ROW_H, TAB_PAD, W};
use crate::lang::{self, tr, trn};
use crate::logic::{self, NetHold, CAUSE_NARROW, NET_BAD, NET_OK, NET_WARN};
use crate::lv::{self, sys, EventCb, Obj};
use crate::net_view as nv;
use crate::theme::{t, uf};
use crate::trc;
use devui_model::net_view_t;
use devui_model::scenario_status_t;

/// 5 carrier slots: 3 NR + 2 LTE covers EN-DC on this modem with headroom.
pub const CA_SLOTS: usize = 5;
pub const CA_CARD_TOP: i32 = 34;
pub const SC_CARD_H: i32 = 72;
/// Tailscale · 本机 · 节点 · 子网 · 出口/提示
pub const TS_HOME_ROWS: usize = 5;

#[derive(Default, Clone, Copy)]
pub struct HomeRow {
    pub bx: Obj,
    /// moved right of the mark before 出口 (private build); unread in the public build
    #[allow(dead_code)]
    pub key: Obj,
    pub val: Obj,
}

/// One carrier: active (40 px) or configured but inactive (32 px).
#[derive(Default, Clone, Copy)]
pub struct CaSlot {
    pub bx: Obj,
    pub band: Obj,
    pub bw: Obj,
    pub rsrp: Obj,
    pub rsrp_c: Obj,
    pub sinr: Obj,
    pub sinr_c: Obj,
    pub pci: Obj,
    pub arfcn: Obj,
    pub ina: Obj,
    pub ina_tag: Obj,
    pub ina_info: Obj,
    pub sep: Obj,
}

#[derive(Clone)]
pub struct Home {
    pub scroll: Obj,
    // 自动退回已打开 · 知道了 (DD18)
    pub hn_box: Obj,
    hn_h: i32,
    // status card
    pub cell_card: Obj,
    pub cc_hero: Hero,
    pub cc_hint_box: Obj,
    cc_hint: Obj,
    cc_diag: Obj,
    cc_hint_sub: Sub,
    cc_logo: Obj,
    logo_slug: Option<String>,
    logo_w: i32,
    logo_h: i32,
    hr_ca: HomeRow,
    hr_wifi: HomeRow,
    hr_exit: HomeRow,
    hr_traf: HomeRow,
    hr_ca_sub: Obj,
    hr_exit_sub: Obj,
    hr_exit_mark: Obj,
    c_wash: Option<u32>,
    c_subcol: Option<u32>,
    last_exit: String,
    // 情景
    sc_card: Obj,
    sc_state: Obj,
    sc_note: Obj,
    pub sc_force: bool,
    // 载波明细 (lives on 蜂窝)
    pub ca_card: Obj,
    ca_qos: Obj,
    ca: [CaSlot; CA_SLOTS],
    // Tailscale
    ts_card: Obj,
    ts_dot: Obj,
    ts_val: [Obj; TS_HOME_ROWS],
    ts_key: [Obj; TS_HOME_ROWS],
    ts_sep: [Obj; TS_HOME_ROWS],
    // refresh state
    ever_valid: bool,
    pub last_valid_wall: i64,
    cc_tone: i32,
    shown_tone: i32,
    shown_cause: i32,
    shown_headline: String,
    shown_hint: String,
    shown_state: i32,
    hold: NetHold,
    nosig_since: u32,
    c_diag: i32,
}

impl Default for Home {
    fn default() -> Self {
        Home {
            scroll: Obj::NULL,
            hn_box: Obj::NULL,
            hn_h: ROW_H,
            cell_card: Obj::NULL,
            cc_hero: Hero::default(),
            cc_hint_box: Obj::NULL,
            cc_hint: Obj::NULL,
            cc_diag: Obj::NULL,
            cc_hint_sub: Sub::Diag,
            cc_logo: Obj::NULL,
            logo_slug: None,
            logo_w: 0,
            logo_h: 0,
            hr_ca: HomeRow::default(),
            hr_wifi: HomeRow::default(),
            hr_exit: HomeRow::default(),
            hr_traf: HomeRow::default(),
            hr_ca_sub: Obj::NULL,
            hr_exit_sub: Obj::NULL,
            hr_exit_mark: Obj::NULL,
            c_wash: None,
            c_subcol: None,
            last_exit: String::new(),
            sc_card: Obj::NULL,
            sc_state: Obj::NULL,
            sc_note: Obj::NULL,
            sc_force: false,
            ca_card: Obj::NULL,
            ca_qos: Obj::NULL,
            ca: [CaSlot::default(); CA_SLOTS],
            ts_card: Obj::NULL,
            ts_dot: Obj::NULL,
            ts_val: [Obj::NULL; TS_HOME_ROWS],
            ts_key: [Obj::NULL; TS_HOME_ROWS],
            ts_sep: [Obj::NULL; TS_HOME_ROWS],
            ever_valid: false,
            last_valid_wall: 0,
            cc_tone: -1,
            shown_tone: 0,
            shown_cause: 0,
            shown_headline: String::new(),
            shown_hint: String::new(),
            shown_state: 0,
            hold: NetHold::default(),
            nosig_since: 0,
            c_diag: -1,
        }
    }
}

/// One 40 px row inside the status card; the box carries its labels so the whole row moves
/// with one set_y. cb = tappable with a chevron.
fn home_row(c: Obj, key: &str, cb: Option<EventCb>, user: usize) -> HomeRow {
    let th = t();
    let bx = kit::bx(c, 0, 0, CARD_W, ROW_H, th.card, 0);
    bx.set_style_bg_opa(lv::OPA_TRANSP, 0);
    kit::sep(bx, 0);
    let key = kit::label(bx, uf().cj14, th.t2, PAD, 11, key);
    let val = kit::label_r(bx, uf().n15, th.t1, CARD_W - PAD - if cb.is_some() { 16 } else { 0 }, 10, "");
    if cb.is_some() {
        kit::chevron(bx, 0);
        kit::tappable(bx, cb, user);
    }
    HomeRow { bx, key, val }
}

/// 「国家 省 市」keeps the country and the last part (the city); two parts or fewer as they
/// are. English (agent geo_en) is split at ", " (place names have spaces: New Taipei).
pub fn geo_short(geo: &str) -> String {
    if let Some(first) = geo.find(", ") {
        let last = geo.rfind(", ").unwrap_or(first);
        if last == first {
            return geo.into();
        }
        return format!("{}{}", &geo[..first], &geo[last..]);
    }
    match (geo.find(' '), geo.rfind(' ')) {
        (Some(f), Some(l)) if f != l => format!("{}{}", &geo[..f], &geo[l..]),
        _ => geo.into(),
    }
}

/// bytes/s as a total-quantity text (no "/s"): the day and month counters.
pub fn fmt_bytes_total(b: i64) -> Vec<u8> {
    let bf = b as f64;
    if b >= 1024 * 1024 * 1024 {
        cf!("%.1fGB", bf / (1024.0 * 1024.0 * 1024.0))
    } else if b >= 1024 * 1024 {
        cf!("%.0fMB", bf / (1024.0 * 1024.0))
    } else if b >= 1024 {
        cf!("%.0fKB", bf / 1024.0)
    } else {
        cf!("%ldB", b)
    }
}

/// A key for the headline hold: datad's verdict code, or the headline's djb hash (a datad from
/// before 10-01 sends no code).
fn hold_key(v: &net_view_t) -> u32 {
    if v.state != nv::ST_UNKNOWN {
        return 0x8000_0000 | v.state as u32;
    }
    v.story.headline.bytes().fold(5381u32, |k, c| k.wrapping_mul(33).wrapping_add(c as u32))
}

/// CHILL 手动优先的提示 (src/ui_parts/exit.c mf_view): what zte-agent says, with a mark and a
/// colour by its kind; a stale one dimmed with the time it stopped.
pub struct MfView {
    pub on: bool,
    pub alldown: bool,
    /// 家里出口 is on its fallback (shown before 手动优先, DD11)
    pub home: bool,
    pub mark: &'static str,
    pub col: u32,
    pub mark_col: u32,
    /// 0 = no wash
    pub wash: u32,
    pub line: Vec<u8>,
}

pub fn mf_view(sc: &scenario_status_t) -> MfView {
    let th = t();
    let mut m = MfView { on: false, alldown: false, home: false, mark: "", col: 0, mark_col: 0, wash: 0, line: Vec::new() };
    if sc.home_on != 0 && sc.home_fallback != 0 {
        m.on = true;
        m.home = true;
        m.mark = "▲";
        m.col = th.warn_t;
        m.mark_col = th.warn_t;
        m.wash = th.wash_w;
        m.line = cf!(
            "%s%s",
            if sc.home_on == 1 { tr("家里不通，暂走直连") } else { tr("家里不通，暂走原出口") },
            if sc.mf_notice.is_empty() { "" } else { " +1" }
        );
        return m;
    }
    if sc.mf_notice.is_empty() {
        return m;
    }
    m.on = true;
    match sc.mf_kind.as_str() {
        "backup" | "unstable" => (m.mark, m.col, m.wash) = ("▲", th.warn_t, th.wash_w),
        "back" | "repick" => (m.mark, m.col, m.wash) = ("●", th.ok_t, th.wash),
        "alldown" => {
            (m.mark, m.col, m.wash) = ("■", th.bad_t, th.wash_b);
            m.alldown = true;
        }
        _ => m.col = th.t2,
    }
    m.mark_col = m.col;
    m.line = sc.mf_notice.as_bytes().to_vec();
    if sc.mf_stale != 0 {
        let hm = if sc.mf_seen != 0 { clock::strftime("%H:%M", &clock::local(sc.mf_seen)) } else { "--:--".into() };
        m.line.extend(cf!(tr(" · 停在 %s"), &hm));
        m.col = th.t3;
        m.mark_col = th.t3;
    }
    m
}

/// Two lines when one is not enough: break after the last 「，」/「; 」 that leaves a first
/// line that fits, so no lone character drops to the second line. No such place: as it is.
pub fn mf_wrap(input: &[u8], f: lv::Font, w: i32) -> Vec<u8> {
    if lv::text_w(input, f) <= w {
        return input.to_vec();
    }
    let seps: [&[u8]; 2] = ["，".as_bytes(), b"; "];
    let mut blen = 0usize;
    for (k, sep) in seps.iter().enumerate() {
        let mut from = 0;
        while let Some(p) = input[from..].windows(sep.len()).position(|x| x == *sep).map(|p| p + from) {
            let cut = p + if k == 1 { 1 } else { sep.len() }; // "; " keeps the ";"
            if lv::text_w(&input[..cut], f) <= w && cut > blen {
                blen = cut;
            }
            from = p + 1;
        }
    }
    if blen == 0 {
        return input.to_vec();
    }
    let mut rest = &input[blen..];
    while rest.first() == Some(&b' ') {
        rest = &rest[1..];
    }
    let mut out = input[..blen].to_vec();
    out.push(b'\n');
    out.extend_from_slice(rest);
    out
}

unsafe extern "C" fn cc_hint_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.sub_open(u.home.cc_hint_sub));
}

unsafe extern "C" fn sc_card_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.sub_open(Sub::Scene));
}

unsafe extern "C" fn op_notice_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| {
        u.data_control("op.notice_ack", "{}", None);
        u.op.notice_acked = true;
        u.op_notice_paint(); // answered at once (DESIGN.md §4)
    });
}

/// 首页「出口」行: with a 手动优先 notice to the top of the exit tab's CHILL area; 家里不通:
/// the CHILL page (exit.rs exit_row_go).
unsafe extern "C" fn exit_row_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.exit_row_go());
}

fn visible_h(o: Obj) -> i32 {
    if o.hidden() { -1 } else { o.style_height(0) }
}

impl Ui {
    pub(super) fn build_home(&mut self, tile: Obj) {
        let th = t();
        let f = uf();
        // worst case (5 active carriers, 5 Tailscale rows, …) ~1010; home_reflow moves the
        // spacer to the real height
        let sc = kit::scroll(tile, 0, VIEW_H, 1100);
        let h = &mut self.home;
        h.scroll = sc;

        // 自动退回已打开 · 知道了: text left (wraps), 知道了 a ≥ 40 px target right
        {
            const DOT_X: i32 = PAD + MARGIN;
            const LBL_X: i32 = PAD + MARGIN + 16;
            const ACK_W: i32 = 72;
            let hb = kit::bx(sc, 0, 0, W, ROW_H, th.card, 0);
            hb.set_style_border_side(lv::LV_BORDER_SIDE_BOTTOM, 0);
            hb.set_style_border_width(1, 0);
            hb.set_style_border_color(th.sep, 0);
            let dot = kit::dot(hb, DOT_X, 0, 8, th.acc_t);
            let lbl = kit::label_w(hb, f.cj13, th.t1, LBL_X, 0, W - LBL_X - ACK_W, true, tr("自动退回已开：切模式没通会退回"));
            lbl.update_layout();
            let lh = lbl.height();
            h.hn_h = if lh + 18 > ROW_H { lh + 18 } else { ROW_H };
            hb.set_height(h.hn_h);
            lbl.set_y((h.hn_h - lh) / 2);
            dot.set_y((h.hn_h - 8) / 2);
            let ack = kit::bx(hb, W - ACK_W, 0, ACK_W, h.hn_h - 1, th.card, 0);
            ack.set_style_bg_opa(lv::OPA_TRANSP, 0);
            let al = kit::label(ack, f.cj14, th.acc_t, 0, 0, tr("知道了"));
            al.align(lv::LV_ALIGN_CENTER, 0, 0);
            kit::tappable(ack, Some(op_notice_cb), 0);
            kit::show(hb, false);
            h.hn_box = hb;
        }

        // status card: the conclusion, then Wi-Fi · traffic · carrier summary
        let c = kit::card(sc, MARGIN, 4, CARD_W, HERO_H + 4 * ROW_H);
        h.cell_card = c;
        h.cc_hero = kit::hero(c, f.cj24b);
        kit::show(h.cc_hero.unit, false);
        h.cc_hero.big.set_x(PAD); // same margin left and right
        // the hint line: while the verdict is abnormal the whole row opens 网络诊断 (查原因 ›)
        h.cc_hint_box = kit::bx(c, 0, HERO_H, CARD_W, ROW_H, th.card, 0);
        h.cc_hint_box.set_style_bg_opa(lv::OPA_TRANSP, 0);
        h.cc_hint = kit::label_w(h.cc_hint_box, f.cj13, th.t2, PAD, 8, CARD_W - 2 * PAD, true, "");
        h.cc_diag = kit::label(h.cc_hint_box, f.cj13, th.acc_t, 0, 8, tr("查原因 ›"));
        kit::tappable(h.cc_hint_box, Some(cc_hint_cb), 0);
        kit::show(h.cc_diag, false);
        kit::show(h.cc_hint_box, false);
        // the top line (operator · RAT · local/roaming) may be long: limited, ends in "…"
        h.cc_hero.st.set_size(CARD_W - 26 - PAD, 18);
        h.cc_hero.st.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
        // operator logo: transparent PNG, 16 high, ≤ 96 wide
        h.cc_logo = Obj::image(c);
        h.cc_logo.remove_flag(lv::LV_OBJ_FLAG_CLICKABLE);
        kit::show(h.cc_logo, false);
        // summary rows: 蜂窝 / Wi-Fi / 出口 each open their tab; 流量 is read-only
        h.hr_ca = home_row(c, tr("载波"), Some(super::tab_go_cb), TAB_CELL);
        h.hr_wifi = home_row(c, "Wi-Fi", Some(super::tab_go_cb), TAB_WIFI);
        h.hr_exit = home_row(c, tr("出口"), Some(exit_row_cb), 0); // with a 手动优先 notice: to the CHILL area
        h.hr_traf = home_row(c, tr("流量"), None, 0);
        h.hr_ca_sub = kit::label_w(h.hr_ca.bx, f.cj12, th.t3, PAD, 34, CARD_W - 2 * PAD - 16, false, "");
        h.hr_exit_sub = kit::label_w(h.hr_exit.bx, f.cj12, th.t3, PAD, 34, CARD_W - 2 * PAD - 16, false, "");
        // 手动优先: the mark before 「出口 ›」 (coloured by kind); the notice is the small line
        h.hr_exit_mark = kit::label(h.hr_exit.bx, f.cj14, th.warn_t, PAD, 11, "");
        kit::show(h.hr_exit_mark, false);
        for s in [h.hr_ca_sub, h.hr_exit_sub] {
            s.set_height(18);
            s.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
        }
        h.hr_ca.bx.set_y(HERO_H);
        h.hr_wifi.bx.set_y(HERO_H + ROW_H);
        h.hr_exit.bx.set_y(HERO_H + 2 * ROW_H);
        h.hr_traf.bx.set_y(HERO_H + 3 * ROW_H);
        h.hr_wifi.val.set_text("—");
        for v in [h.hr_exit.val, h.hr_ca.val] {
            v.set_text("—");
            v.set_width(210);
            v.set_style_text_align(lv::LV_TEXT_ALIGN_RIGHT, 0);
            v.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
            v.set_style_text_font(f.cj14, 0);
            v.set_height(20); // one line: ends in "…", never wraps
            v.align(lv::LV_ALIGN_TOP_RIGHT, -(PAD + 16), 11);
        }
        h.hr_traf.val.set_text("—");

        // 情景: what the device decided about where it is (opens the 情景 page)
        let s = kit::card(sc, MARGIN, 0, CARD_W, SC_CARD_H);
        kit::label(s, f.cj14, th.t2, PAD, 11, tr("情景"));
        h.sc_state = kit::label_r(s, f.cj14, th.t1, CARD_W - PAD - 16, 11, "");
        h.sc_note = kit::label_w(s, f.cj12, th.t2, PAD, 34, CARD_W - 2 * PAD, false, "");
        kit::chevron(s, 0); // opens a page: › (DESIGN.md §4)
        kit::tappable(s, Some(sc_card_cb), 0);
        kit::show(s, false);
        h.sc_card = s;

        // 载波明细: every carrier row (moved to the top of 蜂窝)
        let cc = kit::card(sc, MARGIN, 0, CARD_W, CA_CARD_TOP + 40);
        kit::show(cc, false);
        kit::label(cc, f.cj12, th.t3, PAD, 10, tr("载波"));
        h.ca_qos = kit::label_r(cc, f.n12, th.t2, CARD_W - PAD, 9, "");
        for i in 0..CA_SLOTS {
            let bx = kit::bx(cc, 0, CA_CARD_TOP + i as i32 * 40, CARD_W, 40, th.card, 0);
            bx.set_style_bg_opa(lv::OPA_TRANSP, 0);
            let k = CaSlot {
                bx,
                sep: kit::sep(bx, 0),
                band: kit::label(bx, f.n17, th.t1, PAD, 10, ""),
                bw: kit::label(bx, f.n12, th.t3, 60, 15, ""),
                rsrp: kit::label_r(bx, f.n15, th.t1, 144, 4, ""),
                rsrp_c: kit::label_r(bx, f.n11, th.t3, 144, 23, "RSRP"),
                sinr: kit::label_r(bx, f.n17, th.ok_t, 190, 3, ""),
                sinr_c: kit::label_r(bx, f.n11, th.t3, 190, 23, "SINR"),
                pci: kit::label_r(bx, f.n11, th.t3, CARD_W - PAD, 5, ""),
                arfcn: kit::label_r(bx, f.n11, th.t3, CARD_W - PAD, 21, ""),
                ina: kit::label(bx, f.n15, th.t3, PAD, 7, ""),
                ina_tag: kit::label(bx, f.cj12, th.t3, 100, 9, tr("未激活")),
                ina_info: kit::label_r(bx, f.n11, th.t3, CARD_W - PAD, 9, ""),
            };
            kit::show(bx, false);
            h.ca[i] = k;
        }
        h.ca_card = cc;

        // CHILL: shown on the exit tab (build_exit moves it)
        self.build_chill_card(sc);
        let h = &mut self.home;

        // Tailscale: hidden entirely without tailscaled; the card opens the peer list
        let ts = kit::card(sc, MARGIN, 500, CARD_W, ROW_H);
        // 「节点」「出口」here are Tailscale peers / exit node: own TRC keys
        let keys = ["Tailscale", trc!("tailscale", "本机"), trc!("tailscale", "节点"), trc!("tailscale", "子网"), trc!("tailscale", "出口")];
        for (i, key) in keys.iter().enumerate() {
            let y = i as i32 * ROW_H;
            if i > 0 {
                h.ts_sep[i] = kit::sep(ts, y);
            }
            h.ts_key[i] = kit::label(ts, f.cj14, if i > 0 { th.t2 } else { th.t1 }, PAD, y + 11, key);
            h.ts_val[i] = kit::label_r(
                ts,
                if i > 0 { f.n15 } else { f.cj14 },
                th.t1,
                CARD_W - PAD - if i > 0 { 0 } else { 16 },
                y + if i > 0 { 10 } else { 11 },
                "",
            );
        }
        h.ts_dot = kit::dot(ts, 0, 17, 7, th.green);
        kit::chevron(ts, 0); // the whole card opens the Tailscale page
        kit::tappable(ts, Some(super::tile_click_cb), Sub::Ts as usize);
        kit::show(ts, false);
        h.ts_card = ts;
        self.home_reflow();
    }

    /// Stack the home cards under whatever the status card is showing. Property changes on
    /// existing objects only.
    pub fn home_reflow(&mut self) {
        let h = &self.home;
        let mut y = 4;
        if visible_h(h.hn_box) >= 0 {
            y += h.hn_h; // 自动退回已打开 (DD18)
        }
        h.cell_card.set_y(y);
        y += visible_h(h.cell_card) + MARGIN;
        if visible_h(h.sc_card) >= 0 {
            h.sc_card.set_y(y);
            y += SC_CARD_H + MARGIN;
        }
        if !h.ts_card.hidden() {
            h.ts_card.set_y(y);
            y += h.ts_card.style_height(0) + MARGIN;
        }
        let net = self.charts.net_card;
        if !net.is_null() {
            net.set_y(y);
            y += net.style_height(0) + MARGIN;
        }
        kit::scroll_extent(h.scroll, y + TAB_PAD);
        // the carrier card and others are on other tabs: shown or hidden, those move too
        self.cell_reflow();
        self.net_relayout();
    }

    /// Load the logo for slug (None = hide). Files are optional (trademarks, not in the public
    /// repo): a missing file hides the logo rather than draw LVGL's broken-image box.
    fn home_logo_set(&mut self, slug: Option<&str>) {
        let slug = slug.filter(|s| !s.is_empty());
        let h = &mut self.home;
        if slug == h.logo_slug.as_deref() {
            return;
        }
        let path = slug.map(|s| {
            let dir = std::env::var("LUYAZ_UI_LOGO_DIR").ok().filter(|d| !d.is_empty());
            // Luya-Z D5: with the program (it was in /data/plugins/u60pro-devui)
            format!("{}/{s}{}.png", dir.as_deref().unwrap_or("/data/luyaz/ui/operator-logos"), if t().dark { "-w" } else { "" })
        });
        let readable = path.as_ref().is_some_and(|p| std::fs::File::open(p).is_ok());
        h.logo_slug = if readable { slug.map(String::from) } else { None };
        h.logo_w = 0;
        h.logo_h = 0;
        if let (Some(_), Some(p)) = (&h.logo_slug, path) {
            let src = format!("A:{p}");
            match lv::image_info(&src) {
                Some((w, ih)) if w > 0 && w <= 96 && ih <= 18 => {
                    h.cc_logo.set_image_src(&src);
                    h.cc_logo.set_style_image_opa(if t().dark { 217 } else { lv::OPA_COVER }, 0);
                    h.logo_w = w as i32;
                    h.logo_h = ih as i32;
                }
                _ => h.logo_slug = None,
            }
        }
    }

    /// The logo leftmost on the top line, instead of the dot and the operator name; without
    /// a logo the dot and the name as before.
    fn home_logo_place(&mut self) {
        let h = &self.home;
        let (w, lh) = (h.logo_w, h.logo_h);
        let x = if w != 0 { PAD + w + 6 } else { 26 };
        if w != 0 {
            h.cc_logo.set_pos(PAD, 16 - lh / 2);
        }
        kit::show(h.cc_logo, w > 0);
        kit::show(h.cc_hero.dot, w == 0);
        h.cc_hero.st.set_x(x);
        h.cc_hero.st.set_width(CARD_W - x - PAD);
    }

    /// No snapshot from the data service. Before the first one: 正在读取…; after, the
    /// numbers stay, dimmed, and the block says since when (an outage is not "no signal").
    pub(super) fn home_signal_down(&mut self) {
        let th = t();
        kit::hero_tone(&self.home.cc_hero, 3);
        self.home.cc_tone = 3;
        self.home_logo_set(None); // data stopped: whose network we are on is unknown
        self.home_logo_place();
        let hero = self.home.cc_hero;
        if !self.home.ever_valid {
            self.set(hero.st, tr("正在读取…"));
            hero.big.set_text("--");
            hero.rtop.set_text("");
            hero.r1.set_text("");
            hero.r2.set_text("");
            kit::hero_layout(&hero);
        } else {
            let alive = self.model.backend_alive_wall;
            let tt = if alive != 0 { alive } else { self.home.last_valid_wall };
            let hm = clock::strftime("%H:%M", &clock::local(tt));
            self.set(hero.st, cf!(tr("数据服务掉线 · 数字停在 %s"), &hm));
            hero.rtop.set_text("");
            hero.big.set_text(tr("读不到数据"));
        }
        for k in &self.home.ca {
            for l in [k.band, k.rsrp, k.sinr] {
                kit::text_color(l, th.t3);
            }
        }
        // the numbers stopped: everything that follows the network dims
        let h = &self.home;
        for l in [h.hr_wifi.val, h.hr_traf.val, h.hr_exit.val, h.hr_ca.val, h.cc_hero.r1] {
            kit::text_color(l, th.t3);
        }
        self.home_reflow();
    }

    /// The status card (refresh.c 「Home: signal card」), from datad's view of the network.
    pub(super) fn home_signal_paint(&mut self, v: &net_view_t, datad_silent: bool) {
        let th = t();
        let f = uf();
        let m = self.model.clone();
        let d = &m.data;
        if !datad_silent {
            self.home.ever_valid = true;
            self.home.last_valid_wall = self.now();
        }
        if datad_silent {
            return; // home_signal_down() already says since when
        }
        let now = lv::tick_get();
        // a new headline must hold 15 s (no flicker at a threshold); no service / no SIM at once
        {
            let h = &mut self.home;
            let key = hold_key(v);
            if v.story.tone >= NET_BAD || h.shown_tone >= NET_BAD {
                h.hold.have = false;
            }
            // a write transaction (net.home) shows at once and goes at once
            if [nv::ST_CHANGING, nv::ST_REVERT_FAIL].contains(&v.state) || [nv::ST_CHANGING, nv::ST_REVERT_FAIL].contains(&h.shown_state) {
                h.hold.have = false;
            }
            if logic::net_hold(&mut h.hold, key, now) {
                h.shown_tone = v.story.tone;
                h.shown_cause = v.story.cause;
                h.shown_state = v.state;
                h.shown_headline = v.story.headline.clone();
                h.shown_hint = v.story.hint.clone();
            }
        }
        self.top_label = v.story.rat.clone();
        let nosvc = v.nosvc != 0;
        if nosvc {
            if self.home.nosig_since == 0 {
                self.home.nosig_since = if now != 0 { now } else { 1 };
            }
        } else {
            self.home.nosig_since = 0;
        }
        let tone = match self.home.shown_tone {
            NET_OK => 0,
            NET_WARN => 1,
            NET_BAD => 2,
            _ => 3,
        };
        if tone != self.home.cc_tone {
            self.home.cc_tone = tone;
            kit::hero_tone(&self.home.cc_hero, tone);
        }
        let hint = self.home.shown_hint.clone();
        let hero = self.home.cc_hero;
        // top line: whose network · what network · local/roaming
        self.home_logo_set(Some(&v.logo));
        if v.sim_usable == 0 {
            self.set(hero.st, tr("没有 SIM 卡"));
        } else {
            // the status bar has the short form (5G-A / 4G+ …): here how it is built
            let mut fine = v.fine.clone();
            let mut line: Vec<u8> = Vec::new();
            // too long: first drop the RAT's extra (「5G NSA · 4G 锚点」→「5G NSA」); still too
            // long: the label ends in "…"
            for pass in 0..2 {
                if pass == 1 {
                    match fine.find(" · ") {
                        Some(i) => fine.truncate(i),
                        None => break,
                    }
                }
                let logo_w = self.home.logo_w;
                line = if logo_w != 0 {
                    cf!("%s%s%s", &fine, if !fine.is_empty() && !v.r#where.is_empty() { " · " } else { "" }, &v.r#where)
                } else {
                    cf!(
                        "%s%s%s%s%s",
                        &v.name,
                        if fine.is_empty() { "" } else { " · " },
                        &fine,
                        if v.r#where.is_empty() { "" } else { " · " },
                        if v.other != 0 {
                            if v.roam != 0 { tr("漫游") } else { "" }
                        } else {
                            v.r#where.as_str()
                        }
                    )
                };
                let avail = if logo_w != 0 { CARD_W - 2 * PAD - logo_w - 6 } else { CARD_W - 26 - PAD };
                if lv::text_w(&line, hero.st.style_text_font(0)) <= avail {
                    break;
                }
            }
            self.set(hero.st, line);
        }
        let headline = self.home.shown_headline.clone();
        self.set(hero.big, &headline);
        if nosvc {
            let mins = now.wrapping_sub(self.home.nosig_since) / 60000;
            if mins != 0 {
                self.set(hero.rtop, cf!(tr("已 %u 分钟"), mins));
            } else {
                self.set(hero.rtop, tr("刚刚"));
            }
        } else {
            self.set(hero.rtop, "");
        }
        // right: three things that don't decide each other, in fixed places.
        //   top: 信号强/中/弱 (the status bar's bars, same colour)
        //   below: 干扰小/中/大 · 负载正常/高 (no 负载 while nothing downloads)
        {
            let s = &v.story;
            let (mut a, mut b): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
            if nosvc {
                a = tr("正在搜网").into();
            } else if !s.sig.is_empty() {
                a = cf!(tr("信号%s"), &s.sig);
                // whole sentences, so the English can be worded on its own
                if !s.noise.is_empty() && !s.load.is_empty() {
                    b = cf!(tr("干扰%s · 负载%s"), &s.noise, &s.load);
                } else if !s.noise.is_empty() {
                    b = cf!(tr("干扰%s"), &s.noise);
                } else if !s.load.is_empty() {
                    b = cf!(tr("负载%s"), &s.load);
                } else if d.rssi != 0 {
                    b = cf!("RSSI %d dBm", d.rssi);
                }
            }
            self.set(hero.r1, &a);
            self.set(hero.r2, &b);
            let at = s.sig_tone;
            kit::text_color(
                hero.r1,
                if nosvc {
                    th.t1
                } else if at == NET_OK {
                    th.ok_t
                } else if at == NET_WARN {
                    th.warn_t
                } else {
                    th.bad_t
                },
            );
        }
        kit::hero_layout(&hero);
        self.home_logo_place();

        let mut y = HERO_H;
        {
            // abnormal (slow, not connected, connected but nothing back): the hint row opens
            // 网络诊断 with 「查原因 ›」 at its end; beside the hint if it costs no extra line
            let h = &mut self.home;
            let op = h.shown_state == nv::ST_CHANGING || h.shown_state == nv::ST_REVERT_FAIL;
            let diag = op || nv::abnormal(h.shown_state, h.shown_cause);
            // E4: the status block shows a change in progress; its › opens the 事务页
            if op != (h.cc_hint_sub == Sub::Op) {
                h.cc_hint_sub = if op { Sub::Op } else { Sub::Diag };
                h.cc_diag.set_text(if op { tr("详情 ›") } else { tr("查原因 ›") });
                h.c_diag = -1;
            }
            let (hint_box, hint_l, diag_l) = (h.cc_hint_box, h.cc_hint, h.cc_diag);
            kit::show(hint_box, !hint.is_empty() || diag);
            kit::show(diag_l, diag);
            if diag as i32 != h.c_diag {
                h.c_diag = diag as i32;
                if diag {
                    hint_box.add_flag(lv::LV_OBJ_FLAG_CLICKABLE);
                } else {
                    hint_box.remove_flag(lv::LV_OBJ_FLAG_CLICKABLE); // no › = not tappable
                }
            }
            if !hint.is_empty() || diag {
                let full = CARD_W - 2 * PAD;
                let lh = f.cj13.line_height();
                let (mut dw, mut w) = (0, full);
                if diag {
                    diag_l.update_layout();
                    dw = diag_l.width();
                }
                if diag && !hint.is_empty() {
                    // beside the hint only when that costs it no extra line, never a 3rd
                    let (_, sn) = lv::text_size(hint.as_bytes(), f.cj13, full - dw - 10);
                    let (_, sf) = lv::text_size(hint.as_bytes(), f.cj13, full);
                    w = if sn <= sf { full - dw - 10 } else { full };
                }
                hint_l.set_width(w);
                self.set(hint_l, &hint);
                kit::show(hint_l, !hint.is_empty());
                hint_l.update_layout();
                let th_ = if hint.is_empty() { 0 } else { hint_l.height() };
                let mut bh = 8 + if th_ != 0 { th_ } else { lh } + 8;
                if diag {
                    let hy;
                    if hint.is_empty() || w < full {
                        hy = 8 + if th_ > lh { th_ - lh } else { 0 }; // beside the last line
                    } else {
                        hy = 8 + th_ + 2; // its own line under it
                        bh = hy + lh + 8;
                    }
                    diag_l.set_pos(CARD_W - PAD - dw, hy);
                }
                hint_box.set_y(y);
                hint_box.set_height(bh);
                y += bh;
            }
        }
        // 载波: how many the cell gave, how many are in use; small line: down on which, up on
        // which (the primary only: uplink aggregation is not confirmed yet)
        {
            let (row, sub) = (self.home.hr_ca, self.home.hr_ca_sub);
            self.set(row.val, &v.ca_val);
            kit::text_color(
                row.val,
                if self.home.shown_cause == CAUSE_NARROW {
                    th.warn_t
                } else if v.act_n != 0 {
                    th.t1
                } else {
                    th.t3
                },
            );
            self.set(sub, &v.ca_sub);
            kit::show(sub, !v.ca_sub.is_empty());
            let ch = if v.ca_sub.is_empty() { ROW_H } else { 60 };
            row.bx.set_height(ch);
            row.bx.set_y(y);
            y += ch;
            let q = self.home.ca_qos;
            self.set(q, cf!("QCI %d · AMBR %d/%d", d.qci, d.ambr_dl as i32, d.ambr_ul as i32));
        }
        // Wi-Fi · 设备数 (opens the Wi-Fi tab)
        {
            let row = self.home.hr_wifi;
            if d.wifi_enabled == 0 {
                self.set(row.val, tr("已关闭"));
            } else {
                let ssid = if d.wifi_ssid.is_empty() { "-" } else { d.wifi_ssid.as_str() };
                self.set(row.val, cf!(trn("%.16s · %d 台", d.clients_total as i64), ssid, d.clients_total));
            }
            kit::text_color(row.val, if d.wifi_enabled != 0 { th.t1 } else { th.t3 });
            row.bx.set_y(y);
            y += ROW_H;
        }
        // 出口: one row; two (the small line says the other way) when there is another way
        {
            // the 手动优先 notice takes the small line, folded to two when one is not enough
            let mf = mf_view(&m.scenario);
            let proxy = m.netinfo.as_ref().is_some_and(|n| n.proxy.present != 0);
            let mut eh = if m.chill_online != 0 && proxy { 60 } else { ROW_H };
            if mf.on {
                eh = if lv::text_w(&mf.line, f.cj12) > CARD_W - 2 * PAD - 16 { 78 } else { 60 };
            }
            let row = self.home.hr_exit;
            row.bx.set_height(eh);
            row.bx.set_y(y);
            y += eh;
        }
        // 今日/本月: the firmware's calendar-day/month counters, not this boot's rx/tx
        {
            let row = self.home.hr_traf;
            let day = fmt_bytes_total(d.day_rx_bytes + d.day_tx_bytes);
            let month = fmt_bytes_total(d.month_rx_bytes + d.month_tx_bytes);
            self.set(row.val, cf!(tr("今日 %s · 本月 %s"), &day, &month));
            kit::text_color(row.val, th.t1);
            row.bx.set_y(y);
            y += ROW_H;
        }
        self.home.cell_card.set_height(y);
        self.ca_paint(v);
    }

    /// 载波明细 (on 蜂窝): one row per carrier.
    fn ca_paint(&mut self, v: &net_view_t) {
        let th = t();
        let ca_n = (v.ca_n.max(0) as usize).min(v.ca.len()).min(CA_SLOTS);
        let mut y = CA_CARD_TOP;
        for i in 0..CA_SLOTS {
            let k = self.home.ca[i];
            if i >= ca_n {
                kit::show(k.bx, false);
                continue;
            }
            let c = &v.ca[i];
            kit::show(k.bx, true);
            kit::show(k.sep, true);
            let act = c.active != 0;
            let fl = if c.kind == 'n' as i32 { "ARFCN" } else { "EARFCN" };
            for o in [k.band, k.bw, k.rsrp, k.rsrp_c, k.sinr, k.sinr_c, k.pci, k.arfcn] {
                kit::show(o, act);
            }
            for o in [k.ina, k.ina_tag, k.ina_info] {
                kit::show(o, !act);
            }
            if act {
                // datad sends the numbers already written
                self.set(k.band, &c.label);
                self.set(k.bw, cf!("%dM", c.bw));
                k.band.update_layout();
                k.bw.set_x(PAD + 3 + k.band.width());
                self.set(k.rsrp, &c.rsrp);
                self.set(k.sinr, &c.sinr);
                self.set(k.pci, cf!("PCI %d", c.pci));
                self.set(k.arfcn, cf!("%s %ld", fl, c.arfcn));
                kit::text_color(k.band, th.t1);
                kit::text_color(k.rsrp, th.t1);
                kit::text_color(
                    k.sinr,
                    if c.sinr_tone == NET_OK {
                        th.ok_t
                    } else if c.sinr_tone == NET_WARN {
                        th.warn_t
                    } else {
                        th.bad_t
                    },
                );
                k.bx.set_height(40);
            } else {
                self.set(k.ina, cf!("%s %dM", &c.label, c.bw));
                self.set(k.ina_info, cf!("%s %ld · PCI %d", fl, c.arfcn, c.pci));
                k.bx.set_height(32);
            }
            k.bx.set_y(y);
            y += if act { 40 } else { 32 };
        }
        self.home.ca_card.set_height(y + if ca_n > 0 { 2 } else { 0 });
        kit::show(self.home.ca_card, ca_n > 0);
    }

    /// 出口 (Home): IP and place from zte-agent's cache; operator and roaming are on the top
    /// line already.
    pub(super) fn home_exit_paint(&mut self) {
        if !self.tab_visible(TAB_HOME) {
            return;
        }
        let th = t();
        let m = self.model.clone();
        let default_ni = devui_model::netinfo_t::default();
        let n = m.netinfo.as_ref().unwrap_or(&default_ni);
        let dead = self.home.cc_tone >= 2; // no signal / no SIM: the exit is stale
        #[allow(unused_mut)]
        let mut sub: Vec<u8> = Vec::new();
        // 直连 and CHILL are two sides of one thing, said as CHILL routes now:
        //   off          → 直连 · 中国 深圳
        //   rules        → 国外走 CHILL · 日本 东京 / small: 国内直连 · 中国 深圳
        //   global       → 全走 CHILL · 日本 东京 / small: 设备自己的流量仍直连 …
        //   direct, AI   → 直连 · 中国 深圳 / small: AI 仍走 CHILL · 日本 东京
        //   all direct   → 全部直连 · 中国 深圳 / small: CHILL 开着，但不经过它
        let ex = m.chill_exit_raw.as_deref().unwrap_or("");
        let ch_on = m.chill_online != 0 && n.proxy.present != 0;
        let dg: String = if !n.direct.ip.is_empty() {
            if n.direct.geo.is_empty() { n.direct.ip.clone() } else { geo_short(&n.direct.geo) }
        } else if n.err.is_empty() {
            tr("查询中…").into()
        } else {
            "—".into()
        };
        let pg: String = if !ch_on {
            String::new()
        } else if !n.proxy.geo.is_empty() {
            geo_short(&n.proxy.geo)
        } else if !n.proxy.ip.is_empty() {
            n.proxy.ip.clone()
        } else {
            tr("查询中…").into()
        };
        let mut g: Vec<u8> = if !ch_on {
            cf!(tr("直连 · %s"), &dg)
        } else if ex == "direct_all" {
            sub = tr("CHILL 开着，但不经过它").into();
            cf!(tr("全部直连 · %s"), &dg)
        } else if ex == "direct_keep_ai" {
            // not 「全部直连」: AI still goes through CHILL
            sub = cf!(tr("AI 仍走 CHILL · %s"), &pg);
            cf!(tr("直连 · %s"), &dg)
        } else if ex == "global" {
            sub = cf!(tr("设备自己的流量仍直连 · %s"), &dg);
            cf!(tr("全走 CHILL · %s"), &pg)
        } else {
            sub = cf!(tr("国内直连 · %s"), &dg);
            cf!(tr("国外走 CHILL · %s"), &pg)
        };
        // 家里出口 on (DD2): the strongest item at the end of the row
        {
            let hsc = &m.scenario;
            if hsc.home_on & 4 != 0 {
                let left = (hsc.home_until - self.now()).max(0);
                g.extend(cf!(tr(" · 家里 剩 %ld:%02ld"), left / 3600, (left % 3600) / 60));
            } else if hsc.home_on & 1 != 0 {
                g.extend_from_slice(tr(" · 家里 版权").as_bytes());
            } else if hsc.home_on & 2 != 0 {
                g.extend_from_slice(tr(" · 家里 IP").as_bytes());
            }
        }
        let row = self.home.hr_exit;
        self.set(row.val, &g);
        kit::text_color(row.val, if dead { th.t3 } else { th.t1 });
        let exit_sub = self.home.hr_exit_sub;
        // 手动优先: the whole row washed, 「▲ 出口 ›」, the notice itself in the small line
        {
            let mf = mf_view(&m.scenario);
            let wash = if mf.on && mf.wash != 0 { mf.wash } else { 0 };
            let subcol = if mf.on { mf.col } else { th.t3 };
            let mut two = false;
            if mf.on {
                sub = mf_wrap(&mf.line, uf().cj12, CARD_W - 2 * PAD - 16);
                two = lv::text_w(&mf.line, uf().cj12) > CARD_W - 2 * PAD - 16;
            }
            exit_sub.set_height(if two { 36 } else { 18 });
            exit_sub.set_long_mode(if two { lv::LV_LABEL_LONG_MODE_WRAP } else { lv::LV_LABEL_LONG_MODE_DOTS });
            let mark = self.home.hr_exit_mark;
            self.set(mark, if mf.on { mf.mark } else { "" });
            kit::show(mark, mf.on && !mf.mark.is_empty());
            kit::text_color(mark, if mf.on { mf.mark_col } else { th.t2 });
            row.key.set_x(if mf.on && !mf.mark.is_empty() { PAD + 18 } else { PAD });
            if self.home.c_wash != Some(wash) {
                self.home.c_wash = Some(wash);
                if wash != 0 {
                    kit::bg(row.bx, wash);
                }
                row.bx.set_style_bg_opa(if wash != 0 { lv::OPA_COVER } else { lv::OPA_TRANSP }, 0);
            }
            if self.home.c_subcol != Some(subcol) {
                self.home.c_subcol = Some(subcol);
                kit::text_color(exit_sub, subcol);
            }
        }
        self.set(exit_sub, &sub);
        kit::show(exit_sub, !sub.is_empty());
    }

    /// 情景 (Home): repainted when zte-agent's answer changed.
    pub(super) fn home_scene_paint(&mut self, changed: bool) {
        if !changed {
            return;
        }
        let th = t();
        let m = self.model.clone();
        let sc = &m.scenario;
        let (card, state, note) = (self.home.sc_card, self.home.sc_state, self.home.sc_note);
        if sc.available == 0 {
            card.add_flag(lv::LV_OBJ_FLAG_HIDDEN);
            self.chill_master_sync();
            return;
        }
        card.remove_flag(lv::LV_OBJ_FLAG_HIDDEN);
        if sc.enabled == 0 {
            self.set(state, tr("已停用"));
        } else {
            self.set(state, if sc.name.is_empty() { tr("判定中") } else { sc.name.as_str() });
        }
        kit::text_color(state, if sc.enabled != 0 { th.t1 } else { th.t3 });
        let mut when = String::new();
        if sc.last_switch > 0 {
            // today: the time only; earlier: the date only
            let (tm, tn) = (clock::local(sc.last_switch), clock::local(self.now()));
            let today = tm.year == tn.year && tm.yday == tn.yday;
            when = clock::strftime(
                if today {
                    "%H:%M"
                } else if lang::is_en() {
                    "%d %b"
                } else {
                    "%m-%d"
                },
                &tm,
            );
        }
        // 在家: why Wi-Fi is gone; 判定中: why there is no answer yet; otherwise when it
        // switched. Pinned by hand: 已固定 first.
        let pinned = if sc.pin.is_empty() { "" } else { tr("已固定 · ") };
        #[allow(unused_mut)]
        let mut note_col = th.t2;
        if sc.enabled != 0 && sc.name.is_empty() {
            self.set(note, tr("开机后要连续两次扫描确认位置"));
        }
        else if sc.abroad != 0 && sc.chill_on == 1 {
            // abroad: how CHILL goes now; the card changes it
            note_col = th.acc_t;
            self.set(note, cf!("%sCHILL %s", pinned, m.chill_mode.as_deref().unwrap_or("")));
        } else if sc.abroad != 0 && sc.chill_on == 0 {
            note_col = th.acc_t;
            self.set(note, cf!("%s%s", pinned, if sc.chill_back != 0 { tr("CHILL 已关，回国自动开") } else { tr("CHILL 已关") }));
        }
        else {
            self.set(
                note,
                cf!(
                    "%s%s%s%s",
                    pinned,
                    if sc.wifi_off != 0 {
                        tr("Wi-Fi 已关 · ")
                    } else if when.is_empty() {
                        ""
                    } else {
                        tr("切换于 ")
                    },
                    &when,
                    if sc.wifi_off != 0 && when.is_empty() { tr("手机走家里网络") } else { "" }
                ),
            );
        }
        kit::text_color(note, note_col);
        self.chill_master_sync();
    }

    /// Whether 情景 should repaint: scenario_poll read something new, or the exit it shows
    /// changed.
    pub(super) fn home_scene_changed(&mut self) -> bool {
        let mut changed = self.polled.scenario;
        let m = self.model.clone();
        // the 情景 card shows CHILL's exit: repaint when it changes
        let ex = m.chill_exit_raw.clone().unwrap_or_default();
        if self.home.last_exit != ex {
            self.home.last_exit = ex;
            self.home.sc_force = true;
        }
        if self.home.sc_force {
            self.home.sc_force = false;
            changed = true;
        }
        changed
    }

    /// Tailscale (Home): repainted when tailscale_poll read something new.
    pub(super) fn home_ts_paint(&mut self) {
        if !self.polled.tailscale {
            return;
        }
        let m = self.model.clone();
        let th = t();
        let ts = &m.tailscale;
        let h = &self.home;
        if ts.available == 0 {
            h.ts_card.add_flag(lv::LV_OBJ_FLAG_HIDDEN);
            return;
        }
        let running = ts.ok != 0 && ts.state == "Running";
        h.ts_card.remove_flag(lv::LV_OBJ_FLAG_HIDDEN);
        let (state_txt, state_col, note): (&str, u32, &str) = if ts.ok == 0 {
            (tr("未运行"), th.t3, "")
        } else if running {
            if ts.self_online != 0 { (tr("已连接"), th.t1, "") } else { (tr("离线"), th.bad_t, "") }
        } else if ts.state == "Starting" {
            (tr("连接中"), th.warn_t, "")
        } else if ts.state == "NeedsLogin" {
            (tr("需要登录"), th.warn_t, tr("在管理网页打开登录链接"))
        } else if ts.state == "NeedsMachineAuth" {
            (tr("等待批准"), th.warn_t, tr("在 Tailscale 后台批准这台设备"))
        } else if ts.state == "Stopped" {
            (tr("已停止"), th.t3, "")
        } else {
            (if ts.state.is_empty() { "-" } else { ts.state.as_str() }, th.t3, "")
        };
        let dot_col = if running && ts.self_online != 0 {
            th.green
        } else if state_col == th.warn_t {
            th.orange
        } else if state_col == th.bad_t {
            th.red
        } else {
            th.t3
        };
        h.ts_val[0].set_text(state_txt);
        kit::text_color(h.ts_val[0], state_col);
        kit::bg(h.ts_dot, dot_col);
        h.ts_val[0].update_layout();
        h.ts_dot.set_x(h.ts_val[0].x() - 12);
        let mut rows = 1;
        if running {
            h.ts_val[1].set_text(cf!(
                "%s%s%s%s",
                &ts.ip,
                if !ts.ip.is_empty() && !ts.relay.is_empty() { " · " } else { "" },
                if ts.relay.is_empty() { "" } else { "DERP " },
                &ts.relay
            ));
            if ts.peers_online == 0 {
                h.ts_val[2].set_text(tr("只有本机在线"));
            } else {
                h.ts_val[2].set_text(cf!(tr("在线 %d/%d · 直连 %d · 中继 %d"), ts.peers_online, ts.peers, ts.direct, ts.active - ts.direct));
            }
            h.ts_val[3].set_text(if ts.routes.is_empty() { "-" } else { ts.routes.as_str() });
            rows = 4;
            // health beats the exit node: it is the actionable one
            if !ts.health.is_empty() {
                h.ts_key[4].set_text(tr("提示"));
                h.ts_val[4].set_text(&ts.health);
                kit::text_color(h.ts_val[4], th.warn_t);
                rows = 5;
            } else if !ts.exit_node.is_empty() {
                h.ts_key[4].set_text(tr("出口节点"));
                h.ts_val[4].set_text(cf!("%s%s", &ts.exit_node, if ts.exit_online != 0 { "" } else { tr(" · 离线") }));
                kit::text_color(h.ts_val[4], th.t1);
                rows = 5;
            }
        } else if !note.is_empty() {
            h.ts_key[1].set_text(tr("下一步"));
            h.ts_val[1].set_text(note);
            rows = 2;
        }
        if !running {
            h.ts_key[1].set_text(tr("下一步"));
        } else {
            h.ts_key[1].set_text(trc!("tailscale", "本机"));
        }
        for i in 1..TS_HOME_ROWS {
            for o in [h.ts_key[i], h.ts_val[i], h.ts_sep[i]] {
                kit::show(o, i < rows);
            }
        }
        h.ts_val[4].set_width(lv::SIZE_CONTENT);
        h.ts_card.set_height(rows as i32 * ROW_H);
        // the exit tab's 「Tailscale ›」 small line
        let tsl = self.tile_sub[Sub::Ts as usize];
        if running && ts.peers != 0 {
            self.set(tsl, cf!(tr("%s · 在线 %d/%d"), state_txt, ts.peers_online, ts.peers));
        } else {
            self.set(tsl, state_txt);
        }
        kit::text_color(tsl, if running && ts.self_online != 0 { th.ok_t } else { th.t2 });
        self.ts_page_paint();
    }

    /// The view the status card and status bar draw (refresh.c: /v2/screen, or a placeholder
    /// that says why there is none).
    pub(super) fn screen_view(&self) -> net_view_t {
        match &self.model.screen_net {
            Some(v) => v.clone(),
            None if self.model.screen_status == SF_OLD_DATAD => nv::placeholder("—", tr("数据服务版本太旧：要更新 luyaz-core")),
            None => nv::placeholder(tr("读取中…"), ""),
        }
    }
}

/// A Wi-Fi client's signal in words; zte-agent grades it (netinfo.rs signal_tier).
pub fn wifi_sig_word(tier: &str) -> Option<&'static str> {
    match tier {
        "great" => Some(tr("信号很好")),
        "good" => Some(tr("信号好")),
        "fair" => Some(tr("信号一般")),
        "weak" => Some(tr("信号弱")),
        _ => None,
    }
}

/// A switch the user just flipped keeps its new value this long, or until datad reports the
/// same value, so it does not snap back while datad's next read is on its way.
const AUX_HOLD_MS: u32 = 30000;

/// Device state the main snapshot fields don't cover (home.c 「auxiliary device state」): the
/// DHCP pool, direct power supply and the cellular data / roaming switches come from datad;
/// the radios (/sys) and Wi-Fi power save (iw) are read on the device while a page that shows
/// them is up, at most every 5 s.
pub struct Aux {
    pub w24: i32,
    pub w5: i32,
    pub psm: i32,
    pub dps: i32,
    pub data: i32,
    pub roam: i32,
    pub pool: String,
    pub hold_dps: u32,
    pub hold_data: u32,
    pub hold_roam: u32,
    pub hold_w24: u32,
    pub hold_w5: u32,
    /// the last device read (0 = never)
    read_at: u32,
}

impl Default for Aux {
    fn default() -> Self {
        Aux {
            w24: -1,
            w5: -1,
            psm: -1,
            dps: -1,
            data: -1,
            roam: -1,
            pool: String::new(),
            hold_dps: 0,
            hold_data: 0,
            hold_roam: 0,
            hold_w24: 0,
            hold_w5: 0,
            read_at: 0,
        }
    }
}

/// Take datad's value unless a local flip is still being held.
fn aux_take(cur: &mut i32, hold: &mut u32, from_datad: i32, now: u32) {
    if from_datad < 0 {
        return; // not in /state: keep what we have
    }
    if *hold != 0 && from_datad != *cur && now.wrapping_sub(*hold) < AUX_HOLD_MS {
        return;
    }
    *hold = 0;
    *cur = from_datad;
}

impl Ui {
    /// home.c aux_refresh: datad's values; the radios and power save from the device.
    pub(super) fn aux_refresh(&mut self, active: bool, d: &devui_model::devui_data_t) {
        let now = lv::tick_get();
        let a = &mut self.aux;
        if d.valid != 0 {
            aux_take(&mut a.dps, &mut a.hold_dps, d.dps_mode, now);
            aux_take(&mut a.data, &mut a.hold_data, d.cell_data, now);
            aux_take(&mut a.roam, &mut a.hold_roam, d.cell_roam, now);
            a.pool.clear();
            if !d.dhcp_ip.is_empty() && !d.dhcp_start.is_empty() {
                a.pool = logic::dhcp_pool_text(&d.dhcp_ip, &d.dhcp_start, &d.dhcp_limit);
            }
        }
        if !active || (a.read_at != 0 && now.wrapping_sub(a.read_at) < 5000) {
            return;
        }
        a.read_at = now;
        if let Some((w24, w5, psm)) = self.src.aux_read() {
            let a = &mut self.aux;
            aux_take(&mut a.w24, &mut a.hold_w24, w24, now);
            aux_take(&mut a.w5, &mut a.hold_w5, w5, now);
            if psm >= 0 {
                a.psm = psm;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geo_keeps_country_and_city() {
        assert_eq!(geo_short("中国 广东 深圳"), "中国 深圳");
        assert_eq!(geo_short("日本 东京都"), "日本 东京都");
        assert_eq!(geo_short("Taiwan, New Taipei, Banqiao"), "Taiwan, Banqiao");
        assert_eq!(geo_short("Japan, Tokyo"), "Japan, Tokyo");
        assert_eq!(geo_short("中国"), "中国");
    }

    #[test]
    fn byte_totals_as_c_prints_them() {
        assert_eq!(fmt_bytes_total(2352570100 + 190568990), b"2.4GB");
        assert_eq!(fmt_bytes_total(48589732137 + 15347738524), b"59.5GB");
        assert_eq!(fmt_bytes_total(5 * 1024 * 1024 + 1), b"5MB");
        assert_eq!(fmt_bytes_total(1536), b"2KB");
        assert_eq!(fmt_bytes_total(12), b"12B");
    }

    #[test]
    fn hold_key_prefers_the_code() {
        let mut v = net_view_t { state: 3, ..Default::default() };
        assert_eq!(hold_key(&v), 0x8000_0003);
        v.state = 0;
        v.story.headline = "a".into();
        assert_eq!(hold_key(&v), 5381u32.wrapping_mul(33) + b'a' as u32);
    }
}
