//! 首页 (DESIGN.md §4 Home hierarchy): four fixed slots. The status card (SIM line, verdict,
//! speed or the hint, SINR ring, carriers or the action), the scenario card and the bottom row
//! (ui/home_scene.rs); below the fold 落地 and Tailscale details.
//! Content changes inside a slot and never moves the slots below it.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::home_scene::Scene;
use super::{Sub, Ui, SF_OLD_DATAD, TAB_CELL, VIEW_H};
use crate::cf;
use crate::clock;
use crate::kit::{self, CARD_W, MARGIN, PAD, R_CARD, ROW_H, TAB_PAD};
use crate::lang::tr;
use crate::logic::{self, NetHold, NET_BAD, NET_OK, NET_WARN};
use crate::lv::{self, sys, Obj, Series};
use crate::net_view as nv;
use crate::theme::{t, uf};
use crate::trc;
use ui_model::net_view_t;
use ui_model::scenario_status_t;

/// 5 carrier slots: 3 NR + 2 LTE covers EN-DC on this modem with headroom.
pub const CA_SLOTS: usize = 5;
/// the speed wave's floor (rate_scale 50 = 10 KB/s): at or under it is idle, drawn as a gap
const WAVE_FLOOR: i32 = 50;
/// the foot's carrier bars are drawn against this many MHz (or the total, when wider)
const FOOT_FULL_MHZ: i32 = 100;
pub const CA_CARD_TOP: i32 = 34;
pub const SC_CARD_H: i32 = 72;
/// Tailscale · 本机 · 节点 · 子网 · 出口/提示
pub const TS_HOME_ROWS: usize = 5;

// the slots, in the home scroll's coordinates (the screen's minus the 26 px status bar)
pub const HERO_Y: i32 = 4;
pub const HERO_BOX_H: i32 = 147;
pub const SCENE_Y: i32 = 156;
pub const SCENE_H: i32 = 174;
pub const ROW_Y: i32 = 336;
pub const CELL_H: i32 = 54;
/// below the fold: 落地, Tailscale (the speed chart is on 系统)
pub const FOLD_Y: i32 = ROW_Y + CELL_H + 2 * MARGIN;
// inside the status card: SIM line 29 · body 92 · footer 26; dual standby: two SIM lines 39 ·
// body 82 (a smaller ring; the wave keeps its place at the body's foot)
const SIM_H: i32 = 29;
const SIM_H2: i32 = 39;
const BODY_Y: i32 = SIM_H;
const FOOT_Y: i32 = SIM_H + 92;
const FOOT_H: i32 = HERO_BOX_H - FOOT_Y;
/// the ring and its word take the body's right 92 px
const RING_D: i32 = 74;
const RING_D2: i32 = 66;
const BODY_W: i32 = CARD_W - 92;
/// speed wave: points kept (one per chart step)
const WAVE_PTS: u32 = 30;

/// One SIM line: logo · words …… a role word on the right (dual standby only).
#[derive(Default, Clone)]
pub struct SimRow {
    pub logo: Obj,
    pub l: Obj,
    pub r: Obj,
    slug: Option<String>,
    w: i32,
    h: i32,
    opa: i32,
}

/// What the status card's footer offers on its right end, when there is something to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HeroAct {
    None,
    /// 查原因 › (network diagnosis)
    Diag,
    /// 详情 › (a write in progress: the transaction page)
    Op,
    /// 换这张上网 › (dual standby: the data card has no service, the other one has signal)
    Swap,
    /// 去启用 › (no SIM: SIM and eSIM)
    Enable,
}

/// One carrier in the footer: band label + a bar as wide as its MHz share.
#[derive(Default, Clone, Copy)]
pub struct FootCa {
    pub band: Obj,
    pub bar: Obj,
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
    // status card
    pub cell_card: Obj,
    /// SIM lines: the data card; dual standby adds the other card's line under it
    sim: [SimRow; 2],
    /// the card's inner layout as last set (dual standby: two SIM lines, a tighter body)
    hero_dual: Option<bool>,
    /// body: verdict (+ since), speed + noise · load + wave, or the hint; the SINR ring
    big: Obj,
    since: Obj,
    spd: Obj,
    spd_u: Obj,
    spd_up: Obj,
    spd_upu: Obj,
    nl: Obj,
    wave: Obj,
    wave_s: Series,
    /// the snapshot the wave last moved on: a step with no new data leaves it still
    wave_ver: u64,
    hint: Obj,
    /// the hint as broken into lines last (wrap_cjk measures: only when it changes)
    hint_src: String,
    ring: Obj,
    ring_v: Obj,
    ring_c: Obj,
    /// footer: hairline, carriers or a status line, total or the action
    foot_ca: [FootCa; CA_SLOTS],
    foot_txt: Obj,
    foot_tot: Obj,
    act_box: Obj,
    act_l: Obj,
    pub act: HeroAct,
    /// tone colours as last set: (card wash, ring arc)
    c_wash: Option<u32>,
    c_arc: Option<u32>,
    /// the scenario card and the bottom row (home_scene.rs)
    pub sn: Scene,
    // 落地: a new country, for 24 h (travel-assist.md §5)
    ld_card: Obj,
    ld_title: Obj,
    ld_when: Obj,
    ld_note: Obj,
    pub sc_force: bool,
    // 载波明细 (lives on 蜂窝)
    pub ca_card: Obj,
    ca_qos: Obj,
    ca: [CaSlot; CA_SLOTS],
    // Tailscale (below the fold: the details the column has no room for)
    ts_card: Obj,
    ts_dot: Obj,
    ts_val: [Obj; TS_HOME_ROWS],
    ts_key: [Obj; TS_HOME_ROWS],
    ts_sep: [Obj; TS_HOME_ROWS],
    // refresh state
    ever_valid: bool,
    pub last_valid_wall: i64,
    pub cc_tone: i32,
    shown_tone: i32,
    shown_cause: i32,
    shown_headline: String,
    shown_hint: String,
    shown_state: i32,
    hold: NetHold,
    nosig_since: u32,
    last_exit: String,
}

impl Default for Home {
    fn default() -> Self {
        Home {
            scroll: Obj::NULL,
            cell_card: Obj::NULL,
            sim: [SimRow::default(), SimRow::default()],
            hero_dual: None,
            big: Obj::NULL,
            since: Obj::NULL,
            spd: Obj::NULL,
            spd_u: Obj::NULL,
            spd_up: Obj::NULL,
            spd_upu: Obj::NULL,
            nl: Obj::NULL,
            wave: Obj::NULL,
            wave_s: Series::default(),
            wave_ver: 0,
            hint: Obj::NULL,
            hint_src: String::new(),
            ring: Obj::NULL,
            ring_v: Obj::NULL,
            ring_c: Obj::NULL,
            foot_ca: [FootCa::default(); CA_SLOTS],
            foot_txt: Obj::NULL,
            foot_tot: Obj::NULL,
            act_box: Obj::NULL,
            act_l: Obj::NULL,
            act: HeroAct::None,
            c_wash: None,
            c_arc: None,
            sn: Scene::default(),
            ld_card: Obj::NULL,
            ld_title: Obj::NULL,
            ld_when: Obj::NULL,
            ld_note: Obj::NULL,
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
            last_exit: String::new(),
        }
    }
}

/// Long SSIDs keep both ends: the start names the network, the end often tells bands or
/// floors apart (first n−6 characters + 「…」 + last 5).
pub fn midcut(x: &str, n: usize) -> String {
    let c: Vec<char> = x.chars().collect();
    if c.len() <= n || n < 7 {
        return x.into();
    }
    let head: String = c[..n - 6].iter().collect();
    let tail: String = c[c.len() - 5..].iter().collect();
    format!("{head}…{tail}")
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

/// A key for the headline hold: the core's verdict code, or the headline's djb hash (a core from
/// before 10-01 sends no code).
fn hold_key(v: &net_view_t) -> u32 {
    if v.state != nv::ST_UNKNOWN {
        return 0x8000_0000 | v.state as u32;
    }
    v.story.headline.bytes().fold(5381u32, |k, c| k.wrapping_mul(33).wrapping_add(c as u32))
}

/// CHILL 手动优先的提示: what luyaz-core says, with a mark and a
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

/// `chain`: CHILL's exit chain ("🇹🇼 台湾 → 🇨🇳 TW03"); on a backup the line ends with the node it
/// is on, so 「换到 台湾（自动）」 says which node 自动 picked.
/// `pin`: the node hand-picked in the auto group 🚀 is on (see `chill_pin`).
pub fn mf_view(sc: &scenario_status_t, chain: &str, pin: Option<&str>) -> MfView {
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
    let node = chain.rsplit(" → ").next().unwrap_or("").trim();
    // On 手动优先's backup group, a node picked there by hand is what counts: say what
    // mihomo really does with it, not 手动优先's story about the 🎯 node.
    if sc.mf_kind == "backup"
        && let Some(pin) = pin.filter(|p| !p.is_empty())
    {
        if node.is_empty() || node == pin {
            return m; // the hand pick is in use: nothing to warn about
        }
        m.on = true;
        (m.mark, m.col, m.wash) = ("▲", th.warn_t, th.wash_w);
        m.mark_col = m.col;
        m.line = cf!(tr("首选 %s 不通，暂用 %s"), pin, node);
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
    // 「手选的 X 不通，已临时换到 台湾（自动）」 is three lines on the card and does not say
    // which node 自动 picked: keep the first half, then the node it is on.
    if sc.mf_kind == "backup" && sc.mf_stale == 0 && !node.is_empty() {
        let head = sc.mf_notice.split(['，', ';']).next().unwrap_or("");
        if !head.is_empty() && head.len() < sc.mf_notice.len() {
            m.line = head.as_bytes().to_vec();
            m.line.extend(cf!(tr("，暂用 %s"), node));
        }
    }
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

unsafe extern "C" fn act_cb(_e: *mut sys::lv_event_t) {
    super::with(|u| u.home_act());
}

fn visible_h(o: Obj) -> i32 {
    if o.hidden() { -1 } else { o.style_height(0) }
}

/// Load slug's logo into img once (cached in cur/w/h; None or unreadable = hidden).
fn logo_cache(img: Obj, cur: &mut Option<String>, w: &mut i32, h: &mut i32, slug: Option<&str>) {
    let slug = slug.filter(|s| !s.is_empty());
    if slug == cur.as_deref() {
        return;
    }
    *cur = slug.map(String::from);
    (*w, *h) = slug.and_then(|s| Ui::logo_load(img, s)).unwrap_or((0, 0));
    if *w == 0 {
        *cur = None;
    }
}


/// The footer's right end without an action: how many carriers, how wide together.
pub fn ca_total(v: &net_view_t) -> Vec<u8> {
    let n = (v.ca_n.max(0) as usize).min(v.ca.len());
    let act: Vec<&ui_model::nv_carrier_t> = v.ca[..n].iter().filter(|c| c.active != 0).collect();
    if act.is_empty() {
        return Vec::new();
    }
    let mhz: i32 = act.iter().map(|c| c.bw.max(0)).sum();
    let nr = act.iter().any(|c| c.kind == 'n' as i32);
    let lte = act.iter().any(|c| c.kind != 'n' as i32);
    if act.len() == 1 {
        cf!(tr("单载波 · %d MHz"), mhz)
    } else if nr && lte && v.fine.contains("NSA") {
        cf!(tr("锚点 + 5G · %d MHz"), mhz)
    } else {
        cf!(tr("%d 条聚合 · %d MHz"), act.len() as i32, mhz)
    }
}

/// The SINR the ring shows: the first active carrier's.
fn ring_sinr(v: &net_view_t) -> Option<(f64, i32)> {
    let n = (v.ca_n.max(0) as usize).min(v.ca.len());
    let c = v.ca[..n].iter().find(|c| c.active != 0)?;
    c.sinr.trim().parse::<f64>().ok().map(|s| (s, c.sinr_tone))
}

impl Ui {
    pub(super) fn build_home(&mut self, tile: Obj) {
        let th = t();
        let f = uf();
        // the first screen is fixed; below the fold 落地 + Tailscale (home_reflow moves
        // the spacer to the real height)
        let sc = kit::scroll(tile, 0, VIEW_H, 1100);
        self.home.scroll = sc;
        self.build_home_hero(sc);
        self.build_home_scene(sc);
        let h = &mut self.home;

        // 落地: where the device has just arrived (read-only; gone after 24 h)
        let l = kit::card(sc, MARGIN, 0, CARD_W, SC_CARD_H);
        h.ld_title = kit::label(l, f.cj14, th.t1, PAD, 11, "");
        h.ld_when = kit::label_r(l, f.cj14, th.t2, CARD_W - PAD, 11, "");
        h.ld_note = kit::label_w(l, f.cj12, th.t2, PAD, 34, CARD_W - 2 * PAD, false, "");
        h.ld_note.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
        kit::show(l, false);
        h.ld_card = l;

        // 载波明细: every carrier row (moved to the top of 蜂窝)
        let cc = kit::card(sc, MARGIN, 0, CARD_W, CA_CARD_TOP + 40);
        kit::show(cc, false);
        kit::label(cc, f.cj12, th.t3, PAD, 10, tr("载波"));
        h.ca_qos = kit::label_r(cc, f.cj12, th.t2, CARD_W - PAD, 9, "");
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

    /// The status card: one card tinted by the tone, three bands (SIM line · body · footer).
    fn build_home_hero(&mut self, sc: Obj) {
        let th = t();
        let f = uf();
        let h = &mut self.home;
        let c = kit::bx(sc, MARGIN, HERO_Y, CARD_W, HERO_BOX_H, th.wash, R_CARD);
        c.set_style_clip_corner(true, 0);
        h.c_wash = Some(th.wash);
        kit::tappable(c, Some(super::tab_go_cb), TAB_CELL);
        h.cell_card = c;
        // SIM line (20 px at y 9): logo · words …… the other card's logo · its state
        let ly = 9 + (20 - f.cj12.line_height()) / 2;
        for i in 0..2 {
            let r = &mut h.sim[i];
            r.logo = Obj::image(c);
            r.logo.remove_flag(lv::LV_OBJ_FLAG_CLICKABLE);
            kit::show(r.logo, false);
            r.l = kit::label_w(c, f.cj12, if i == 0 { th.t1 } else { th.t2 }, PAD, ly, CARD_W - 2 * PAD, false, "");
            r.r = kit::label_w(c, f.cj12, th.t2, CARD_W - PAD - 120, ly, 120, false, "");
            r.r.set_style_text_align(lv::LV_TEXT_ALIGN_RIGHT, 0);
            r.opa = -1;
        }
        // body: verdict (+ 已 12 分钟), then speed · noise/load · wave, or the hint
        h.big = kit::label(c, f.cj24b, th.ok_t, PAD, BODY_Y + 2, "");
        h.big.set_style_max_width(BODY_W - PAD, 0);
        h.big.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
        h.big.set_height(f.cj24b.line_height());
        h.since = kit::label_w(c, f.cj12, th.t2, PAD, BODY_Y + 3 + f.cj12.line_height(), BODY_W - PAD, false, "");
        let sy = BODY_Y + 36;
        h.spd = kit::label(c, f.n20, th.t1, PAD, sy, "");
        h.spd_u = kit::label(c, f.n11, th.t2, PAD, sy + f.n20.line_height() - f.n11.line_height() - 3, "Mbps");
        h.spd_up = kit::label(c, f.n15, th.t2, PAD, sy + f.n20.line_height() - f.n15.line_height() - 2, "");
        h.spd_upu = kit::label(c, f.n11, th.t2, PAD, sy + f.n20.line_height() - f.n11.line_height() - 3, "Mbps");
        h.nl = kit::label_w(c, f.cj12, th.t2, PAD, BODY_Y + 3, BODY_W - PAD, false, "");
        let (w, ws, _) = kit::chart(c, 0, BODY_Y + 66, BODY_W, 24, WAVE_PTS, th.green, 0);
        w.chart_set_div_line_count(0, 0);
        // the net chart's scale (system.rs): ~10 KB/s to 100 MB/s; idle is left out
        w.chart_set_axis_range(sys::lv_chart_axis_t::LV_CHART_AXIS_PRIMARY_Y, WAVE_FLOOR, 100);
        (h.wave, h.wave_s) = (w, ws);
        h.hint = kit::label_w(c, f.cj13, th.t1, PAD, sy, BODY_W - PAD, true, "");
        h.hint.set_height(3 * f.cj13.line_height());
        h.hint.set_long_mode(lv::LV_LABEL_LONG_MODE_DOTS);
        kit::show(h.hint, false);
        // SINR ring: a 270° track, the arc as long as the quality, coloured by it
        let rx = CARD_W - 10 - RING_D;
        let r = Obj::arc(c);
        r.remove_flag(lv::LV_OBJ_FLAG_CLICKABLE);
        r.set_size(RING_D, RING_D);
        r.set_pos(rx, BODY_Y - 2);
        r.arc_set_rotation(135);
        r.arc_set_bg_angles(0, 270);
        r.arc_set_range(0, 100);
        r.arc_set_value(0);
        r.set_style_bg_opa(lv::OPA_TRANSP, lv::LV_PART_MAIN);
        r.set_style_pad_all(0, lv::LV_PART_MAIN);
        for part in [lv::LV_PART_MAIN, lv::LV_PART_INDICATOR] {
            r.set_style_arc_width(7, part);
            r.set_style_arc_rounded(true, part);
        }
        r.set_style_arc_color(th.track, lv::LV_PART_MAIN);
        r.set_style_arc_color(th.track, lv::LV_PART_INDICATOR);
        r.set_style_bg_opa(lv::OPA_TRANSP, lv::LV_PART_KNOB);
        r.set_style_pad_all(0, lv::LV_PART_KNOB);
        h.ring = r;
        h.c_arc = Some(th.track);
        let vy = BODY_Y - 2 + RING_D / 2 - f.n20.line_height() / 2 - 5;
        h.ring_v = kit::label_w(c, f.n20, th.t1, rx, vy, RING_D, false, "—");
        h.ring_v.set_style_text_align(lv::LV_TEXT_ALIGN_CENTER, 0);
        h.ring_c = kit::label_w(c, f.n11, th.t3, rx, vy + f.n20.line_height() - 3, RING_D, false, "SINR");
        h.ring_c.set_style_text_align(lv::LV_TEXT_ALIGN_CENTER, 0);
        // footer: a hairline on the same tint; carriers or a status line; total or the action
        let hair = kit::bx(c, 0, FOOT_Y, CARD_W, 1, if th.dark { 0xffffff } else { 0x000000 }, 0);
        hair.set_style_bg_opa(if th.dark { 26 } else { 20 }, 0);
        let fy = FOOT_Y + (FOOT_H - f.cj12.line_height()) / 2;
        for i in 0..CA_SLOTS {
            let k = FootCa {
                band: kit::label(c, f.n12, th.t1, PAD, FOOT_Y + (FOOT_H - f.n12.line_height()) / 2, ""),
                bar: kit::bx(c, PAD, FOOT_Y + (FOOT_H - 8) / 2, 10, 8, th.blue, 4),
            };
            kit::show(k.band, false);
            kit::show(k.bar, false);
            h.foot_ca[i] = k;
        }
        h.foot_txt = kit::label_w(c, f.cj12, th.t2, PAD, fy, CARD_W - 2 * PAD, false, "");
        h.foot_tot = kit::label_r(c, f.cj12, th.t2, CARD_W - PAD, fy, "");
        let ab = kit::bx(c, CARD_W - 110, FOOT_Y, 110, FOOT_H, th.card, 0);
        ab.set_style_bg_opa(lv::OPA_TRANSP, 0);
        h.act_l = kit::label(ab, f.cj13, th.acc_t, 0, 0, "");
        h.act_l.align(lv::LV_ALIGN_RIGHT_MID, -PAD, 0);
        kit::tappable(ab, Some(act_cb), 0);
        kit::show(ab, false);
        h.act_box = ab;
    }

    /// The first screen is fixed; below it 落地 and Tailscale stack.
    pub fn home_reflow(&mut self) {
        let h = &self.home;
        let mut y = FOLD_Y;
        if visible_h(h.ld_card) >= 0 {
            h.ld_card.set_y(y);
            y += SC_CARD_H + MARGIN;
        }
        if !h.ts_card.hidden() {
            h.ts_card.set_y(y);
            y += h.ts_card.style_height(0) + MARGIN;
        }
        kit::scroll_extent(h.scroll, y + TAB_PAD);
        // the carrier card and others are on other tabs: shown or hidden, those move too
        self.cell_reflow();
        self.net_relayout();
    }

    /// Load the logo for slug (None = hide). Files are optional (trademarks, not in the public
    /// repo): a missing file hides the logo rather than draw LVGL's broken-image box.
    /// Load operator-logos/<slug>(-w).png into img: Some((w, h)) when readable and ≤ 96×18.
    pub(super) fn logo_load(img: Obj, slug: &str) -> Option<(i32, i32)> {
        let dir = std::env::var("LUYAZ_UI_LOGO_DIR").ok().filter(|d| !d.is_empty());
        let p = format!("{}/{slug}{}.png", dir.as_deref().unwrap_or("/data/luyaz/ui/operator-logos"), if t().dark { "-w" } else { "" });
        std::fs::File::open(&p).ok()?;
        let src = format!("A:{p}");
        match lv::image_info(&src) {
            Some((w, ih)) if w > 0 && w <= 96 && ih <= 18 => {
                img.set_image_src(&src);
                img.set_style_image_opa(if t().dark { 217 } else { lv::OPA_COVER }, 0);
                Some((w as i32, ih as i32))
            }
            _ => None,
        }
    }

    /// SIM line i's logo (cached: reloads only when the slug changes); opa dims the idle card's.
    fn home_logo_set(&mut self, i: usize, slug: Option<&str>, opa: u8) -> (i32, i32) {
        let r = &mut self.home.sim[i];
        let before = r.slug.clone();
        logo_cache(r.logo, &mut r.slug, &mut r.w, &mut r.h, slug);
        if r.slug != before {
            r.opa = -1; // logo_load set its own
        }
        if r.w > 0 && r.opa != opa as i32 {
            r.opa = opa as i32;
            r.logo.set_style_image_opa(opa, 0);
        }
        (r.w, r.h)
    }

    /// The card's inside for one card or two: SIM lines, then the body under them. Only when
    /// it changes (sizes are not compared by LVGL).
    fn home_hero_layout(&mut self, dual: bool) {
        if self.home.hero_dual == Some(dual) {
            return;
        }
        self.home.hero_dual = Some(dual);
        let f = uf();
        let h = self.home.clone();
        let lh = f.cj12.line_height();
        let rows: [i32; 2] = if dual { [5 + (16 - lh) / 2, 21 + (16 - lh) / 2] } else { [9 + (20 - lh) / 2, 0] };
        for (r, y) in h.sim.iter().zip(rows) {
            r.l.set_y(y);
            r.r.set_y(y);
        }
        let (by, d) = if dual { (SIM_H2, RING_D2) } else { (BODY_Y, RING_D) };
        // verdict, then speed / the hint, then noise · load; the wave stays put (y = BODY_Y + 66) in both
        let top = if dual { by } else { by + 2 };
        let sy = if dual { by + 32 } else { by + 36 };
        h.big.set_y(top);
        // the two small lines beside the verdict: centred on its line
        let ly = top + (f.cj24b.line_height() - 2 * f.cj12.line_height()) / 2;
        h.since.set_y(ly + f.cj12.line_height());
        h.spd.set_y(sy);
        h.spd_u.set_y(sy + f.n20.line_height() - f.n11.line_height() - 3);
        h.spd_up.set_y(sy + f.n20.line_height() - f.n15.line_height() - 2);
        h.spd_upu.set_y(sy + f.n20.line_height() - f.n11.line_height() - 3);
        h.nl.set_y(ly);
        h.hint.set_y(sy);
        h.ring.set_size(d, d);
        h.ring_v.set_width(d);
        h.ring_c.set_width(d);
    }

    /// The ring, centred in the body (its signal word is on the verdict's line).
    fn home_ring_place(&mut self) {
        let f = uf();
        let h = &self.home;
        let dual = h.hero_dual == Some(true);
        let (by, d) = if dual { (SIM_H2, RING_D2) } else { (BODY_Y, RING_D) };
        let rx = CARD_W - 10 - RING_D + (RING_D - d) / 2;
        let ry = by + (FOOT_Y - by - d) / 2;
        h.ring.set_pos(rx, ry);
        let vy = ry + d / 2 - f.n20.line_height() / 2 - 5;
        h.ring_v.set_pos(rx, vy);
        h.ring_c.set_pos(rx, vy + f.n20.line_height() - 3);
    }

    /// 信号强/中/弱 in the status bar's tiers and colour; 正在搜网 without service.
    fn home_sig_word(&self, v: &net_view_t) -> (Vec<u8>, u32) {
        let th = t();
        let s = &v.story;
        if v.sim_usable == 0 {
            (Vec::new(), th.t2)
        } else if v.nosvc != 0 {
            (tr("正在搜网").into(), th.t2)
        } else if !s.sig.is_empty() {
            let col = if s.sig_tone == NET_OK { th.ok_t } else if s.sig_tone == NET_WARN { th.warn_t } else { th.bad_t };
            (cf!(tr("信号%s"), &s.sig), col)
        } else {
            (Vec::new(), th.t2)
        }
    }

    /// The footer's action.
    fn home_act(&mut self) {
        match self.home.act {
            HeroAct::Diag => self.sub_open(Sub::Diag),
            HeroAct::Op => self.sub_open(Sub::Op),
            HeroAct::Enable => self.sub_open(Sub::Esim),
            // 蜂窝's dual-SIM rows switch the data card (with their own confirm)
            HeroAct::Swap | HeroAct::None => self.tab_go(TAB_CELL),
        }
    }

    /// The SIM lines: the data card (logo · RAT · local/roaming); in dual standby a second line
    /// for the other card, dimmed, and on the right of each what it is doing (上网中 / 待机 /
    /// 待机 · 信号弱). Too wide: whole trailing parts go first, roaming never does.
    fn home_sim_paint(&mut self, v: &net_view_t) {
        let th = t();
        let f = uf();
        let m = self.model.clone();
        let d = &m.data;
        let dual = d.sim_dual.as_ref().filter(|x| x.mode == "dual" && !x.slots.is_empty());
        self.cell_dual(dual);
        let two = dual.is_some_and(|x| x.slots.len() >= 2);
        self.home_hero_layout(two);
        let dk = t().dark;
        let (lw, lh) = self.home_logo_set(0, if v.sim_usable != 0 { Some(&v.logo) } else { None }, if dk { 217 } else { lv::OPA_COVER });
        let rows = self.home.sim.clone();
        let row_y = |i: usize| if two { 5 + 16 * i as i32 } else { 9 };
        let row_h = if two { 16 } else { 20 };

        // line 0: the data card
        let r0 = &rows[0];
        kit::show(r0.logo, lw > 0);
        let mut x = PAD;
        if lw > 0 {
            r0.logo.set_pos(PAD, row_y(0) + (row_h - lh) / 2);
            x += lw + 6;
        }
        let (right, rcol): (&str, u32) = match dual {
            Some(x) if two && x.switching != 0 => (tr("切换中"), th.t2),
            Some(_) if two && v.nosvc != 0 => (tr("没连上"), th.bad_t),
            Some(_) if two => (tr("上网中"), th.t2),
            _ => ("", th.t2),
        };
        let rw = if right.is_empty() { 0 } else { lv::text_w(right.as_bytes(), f.cj12) + 8 };
        let avail = CARD_W - PAD - rw - x;
        let line: Vec<u8> = if v.sim_usable == 0 {
            if d.sim_say.is_empty() { tr("没有 SIM 卡").into() } else { cf!(tr("没有 SIM 卡 · %s"), &d.sim_say) }
        } else {
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
                line = if lw != 0 {
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
                if lv::text_w(&line, f.cj12) <= avail {
                    break;
                }
            }
            line
        };
        r0.l.set_x(x);
        r0.l.set_width(avail.max(1));
        kit::text_color(r0.l, th.t1);
        self.set(r0.l, &line);
        kit::show(r0.r, !right.is_empty());
        self.set(r0.r, right);
        kit::text_color(r0.r, rcol);

        // line 1: the other card (dual standby)
        let r1 = &rows[1];
        let idle = dual.filter(|_| two).and_then(|x| x.slots.iter().find(|s| s.role != "data"));
        for o in [r1.l, r1.r] {
            kit::show(o, idle.is_some());
        }
        let Some(s) = idle else {
            kit::show(r1.logo, false);
            return;
        };
        let (mcc, mnc) = (s.mcc.parse().unwrap_or(0), s.mnc.parse().unwrap_or(-1));
        let slug: Option<String> =
            Some(s.logo.clone()).filter(|g| !g.is_empty()).or_else(|| crate::data::core_link::mainland_logo(mcc, mnc, &s.operator).map(String::from));
        let (sw, sh) = self.home_logo_set(1, slug.as_deref(), 140);
        kit::show(r1.logo, sw > 0);
        let mut x = PAD;
        if sw > 0 {
            r1.logo.set_pos(PAD, row_y(1) + (row_h - sh) / 2);
            x += sw + 6;
        }
        // right: what this card is doing; a state word when it cannot
        let (right, rcol): (&str, u32) = match s.state.as_str() {
            "" | "ready" => {
                if s.has_sig != 0 && (1..=2).contains(&s.sig_bars) {
                    // core's bars_tier (screen.rs): 1–2 = 弱; only that is worth a word on the idle card
                    (tr("待机 · 信号弱"), th.warn_t)
                } else if s.has_sig != 0 && v.nosvc != 0 {
                    (tr("待机 · 信号好"), th.t2)
                } else {
                    (tr("待机"), th.t2)
                }
            }
            "locked" => (tr("PIN 锁"), th.t2),
            "absent" => (tr("无卡"), th.t2),
            "off" => (tr("未启用"), th.t2),
            _ => (tr("未就绪"), th.t2),
        };
        self.set(r1.r, right);
        kit::text_color(r1.r, rcol);
        let avail = CARD_W - PAD - (lv::text_w(right.as_bytes(), f.cj12) + 8) - x;
        // left: the idle card has no data call, so its RAT is plain (idle on NR is always SA)
        let rat = match s.sig_rat.as_str() {
            "nr" => "5G SA",
            "lte" => "4G",
            _ => "",
        };
        let name: String = if sw > 0 {
            String::new()
        } else {
            crate::data::core_link::mainland_operator_cn(mcc, mnc, &s.operator).unwrap_or(s.operator.as_str()).into()
        };
        let place = crate::data::core_link::slot_place(s);
        let parts: Vec<&str> = [name.as_str(), rat, place].into_iter().filter(|p| !p.is_empty()).collect();
        let mut left: Vec<u8> = parts.join(" · ").into_bytes();
        // too wide: the RAT goes before the place does
        if lv::text_w(&left, f.cj12) > avail && !place.is_empty() {
            let short: Vec<&str> = [name.as_str(), place].into_iter().filter(|p| !p.is_empty()).collect();
            left = short.join(" · ").into_bytes();
        }
        r1.l.set_x(x);
        r1.l.set_width(avail.max(1));
        self.set(r1.l, &left);
    }

    /// No snapshot from the data service. Before the first one: 正在读取…; after, the
    /// numbers stay, dimmed, and the card says since when (an outage is not "no signal").
    pub(super) fn home_signal_down(&mut self) {
        let th = t();
        self.home_tone(3);
        self.home_hero_layout(false);
        self.home_logo_set(0, None, 0); // data stopped: whose network we are on is unknown
        let h = self.home.clone();
        let r0 = &h.sim[0];
        for o in [r0.logo, r0.r, h.sim[1].logo, h.sim[1].l, h.sim[1].r] {
            kit::show(o, false);
        }
        r0.l.set_x(PAD);
        r0.l.set_width(CARD_W - 2 * PAD);
        if !self.home.ever_valid {
            self.set(r0.l, tr("正在读取…"));
            self.set(h.big, "--");
        } else {
            let alive = self.model.backend_alive_wall;
            let tt = if alive != 0 { alive } else { self.home.last_valid_wall };
            let hm = clock::strftime("%H:%M", &clock::local(tt));
            self.set(r0.l, cf!(tr("数据服务掉线 · 数字停在 %s"), &hm));
            self.set(h.big, tr("读不到数据"));
        }
        self.set(h.since, "");
        self.set(h.nl, "");
        for o in [h.spd, h.spd_u, h.spd_up, h.spd_upu, h.wave, h.hint] {
            kit::show(o, false);
        }
        self.set(h.ring_v, "—");
        h.ring.arc_set_value(0);
        self.home_act_set(HeroAct::None);
        for k in &self.home.ca {
            for l in [k.band, k.rsrp, k.sinr] {
                kit::text_color(l, th.t3);
            }
        }
        for k in &self.home.foot_ca {
            kit::text_color(k.band, th.t3);
        }
    }

    /// tone: 0 good, 1 warn, 2 bad, 3 neutral: the card's tint and the verdict's colour.
    fn home_tone(&mut self, tone: i32) {
        let th = t();
        let tone = tone.clamp(0, 3);
        self.home.cc_tone = tone;
        let wash = [th.wash, th.wash_w, th.wash_b, th.track][tone as usize];
        if self.home.c_wash != Some(wash) {
            self.home.c_wash = Some(wash);
            kit::bg(self.home.cell_card, wash);
        }
        kit::text_color(self.home.big, [th.ok_t, th.warn_t, th.bad_t, th.t3][tone as usize]);
    }

    fn home_act_set(&mut self, a: HeroAct) {
        self.home.act = a;
        let txt = match a {
            HeroAct::None => "",
            HeroAct::Diag => tr("查原因 ›"),
            HeroAct::Op => tr("详情 ›"),
            HeroAct::Swap => tr("换这张上网 ›"),
            HeroAct::Enable => tr("去启用 ›"),
        };
        let (bx, l) = (self.home.act_box, self.home.act_l);
        kit::show(bx, a != HeroAct::None);
        self.set(l, txt);
        if a != HeroAct::None {
            let w = lv::text_w(txt.as_bytes(), uf().cj13) + PAD + 12;
            bx.set_width(w);
            bx.set_x(CARD_W - w);
            l.align(lv::LV_ALIGN_RIGHT_MID, -PAD, 0);
        }
    }

    /// The status card, from the core's view of the network.
    pub(super) fn home_signal_paint(&mut self, v: &net_view_t, core_silent: bool) {
        let th = t();
        let f = uf();
        let m = self.model.clone();
        let d = &m.data;
        if !core_silent {
            self.home.ever_valid = true;
            self.home.last_valid_wall = self.now();
        }
        if core_silent {
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
        self.home_tone(tone);
        self.home_sim_paint(v);
        let h = self.home.clone();

        // verdict, and how long for no service
        let headline = self.home.shown_headline.clone();
        self.set(h.big, &headline);
        let since: Vec<u8> = if nosvc {
            let mins = now.wrapping_sub(self.home.nosig_since) / 60000;
            if mins != 0 { cf!(tr("已 %u 分钟"), mins) } else { tr("刚刚").into() }
        } else {
            Vec::new()
        };
        h.big.update_layout();
        // beside the verdict, two small lines: the signal word (its colour), then 干扰 · 负载 (or
        // how long without service); short of room, the load goes, then the noise
        {
            let s = &v.story;
            let lx = PAD + h.big.width() + 8;
            let room = (BODY_W - lx).max(1);
            let (word, wcol) = self.home_sig_word(v);
            let mut parts: Vec<Vec<u8>> = Vec::new();
            if !since.is_empty() {
                parts.push(since.clone());
            } else if v.sim_usable != 0 {
                if !s.noise.is_empty() {
                    parts.push(cf!(tr("干扰%s"), &s.noise));
                }
                if !s.load.is_empty() {
                    parts.push(cf!(tr("负载%s"), &s.load));
                }
                if parts.is_empty() && d.rssi != 0 {
                    parts.push(cf!("RSSI %d dBm", d.rssi));
                }
            }
            let sep = " · ".as_bytes();
            while parts.len() > 1 && lv::text_w(&parts.join(sep), f.cj12) > room {
                parts.pop();
            }
            for (o, w) in [(h.nl, room), (h.since, room)] {
                o.set_x(lx);
                o.set_width(w);
            }
            self.set(h.nl, &word);
            kit::text_color(h.nl, wcol);
            self.set(h.since, parts.join(sep));
        }

        // the hint replaces speed and the wave
        let hint = self.home.shown_hint.clone();
        let with_hint = !hint.is_empty();
        kit::show(h.hint, with_hint);
        for o in [h.spd, h.spd_u, h.spd_up, h.spd_upu] {
            kit::show(o, !with_hint);
        }
        kit::show(h.wave, !with_hint);
        if with_hint {
            if self.home.hint_src != hint {
                let w = kit::wrap_cjk(hint.as_bytes(), f.cj13, BODY_W - PAD);
                self.set(h.hint, &w);
                self.home.hint_src = hint.clone();
            }
        } else {
            // each direction carries its own unit: 12.3 Mbps down and 115 Kbps up can share a line
            use super::refresh::rate_unit;
            let bits = self.conf.speed_bits;
            let ((dn, du), (un, uu)) = (rate_unit(d.down_bps, bits), rate_unit(d.up_bps, bits));
            self.set(h.spd, cf!("↓ %s", &dn));
            self.set(h.spd_u, &du);
            self.set(h.spd_up, cf!("↑ %s", &un));
            self.set(h.spd_upu, &uu);
            h.spd.update_layout();
            let ux = PAD + h.spd.width() + 2;
            h.spd_u.set_x(ux);
            h.spd_u.update_layout();
            let vx = ux + h.spd_u.width() + 10;
            h.spd_up.set_x(vx);
            h.spd_up.update_layout();
            h.spd_upu.set_x(vx + h.spd_up.width() + 2);
        }
        self.home_ring_place();

        // SINR ring (its word is on the verdict's line)
        let ring = if nosvc || v.sim_usable == 0 { None } else { ring_sinr(v) };
        let (val, arc, q): (Vec<u8>, u32, i32) = match ring {
            Some((sn, tn)) => {
                let q = (((sn + 7.0) / 32.0).clamp(0.03, 1.0) * 100.0) as i32;
                let col = if tn == NET_OK { th.green } else if tn == NET_WARN { th.orange } else { th.red };
                (cf!("%.0f", sn), col, q)
            }
            None => ("—".into(), th.track, 0),
        };
        self.set(h.ring_v, &val);
        h.ring.arc_set_value(q);
        if self.home.c_arc != Some(arc) {
            self.home.c_arc = Some(arc);
            h.ring.set_style_arc_color(arc, lv::LV_PART_INDICATOR);
        }

        // footer: what to do, else how many carriers and how wide
        let op = self.home.shown_state == nv::ST_CHANGING || self.home.shown_state == nv::ST_REVERT_FAIL;
        let dual = d.sim_dual.as_ref().filter(|x| x.mode == "dual" && !x.slots.is_empty());
        let idle = dual.and_then(|x| x.slots.iter().find(|s| s.role != "data"));
        let swap = nosvc && idle.is_some_and(|s| s.has_sig != 0 && s.sig_bars >= 3 && (s.state.is_empty() || s.state == "ready"));
        let act = if op {
            HeroAct::Op
        } else if v.sim_usable == 0 {
            HeroAct::Enable
        } else if swap {
            HeroAct::Swap
        } else if nv::abnormal(self.home.shown_state, self.home.shown_cause) {
            HeroAct::Diag
        } else {
            HeroAct::None
        };
        self.home_act_set(act);
        let right: Vec<u8> = if act == HeroAct::None { ca_total(v) } else { Vec::new() };
        self.set(h.foot_tot, &right);
        kit::show(h.foot_tot, !right.is_empty());
        let rw = if act != HeroAct::None {
            h.act_box.style_width(0) - PAD + 8
        } else if right.is_empty() {
            0
        } else {
            lv::text_w(&right, f.cj12) + 8
        };
        let n = (v.ca_n.max(0) as usize).min(v.ca.len());
        let act_ca: Vec<&ui_model::nv_carrier_t> = v.ca[..n].iter().filter(|c| c.active != 0).take(CA_SLOTS).collect();
        // each band with its own width: the total on the right goes when 查原因 › takes its place
        let labels: Vec<String> = act_ca
            .iter()
            .map(|c| {
                let l = if c.label_short.is_empty() { c.label.as_str() } else { c.label_short.as_str() };
                if c.bw > 0 { format!("{l} {}M", c.bw) } else { l.to_string() }
            })
            .collect();
        let lws: Vec<i32> = labels.iter().map(|l| lv::text_w(l.as_bytes(), f.n12)).collect();
        let fixed: i32 = lws.iter().map(|w| w + 4).sum::<i32>() + 8 * (act_ca.len() as i32 - 1).max(0);
        let bars = CARD_W - 2 * PAD - rw - fixed;
        let show_ca = !act_ca.is_empty() && bars >= 6 * act_ca.len() as i32;
        // bars against 100 MHz (one n78), or the total when wider: one n5 15M is a short bar
        let mhz: i32 = act_ca.iter().map(|c| c.bw.max(1)).sum::<i32>().max(FOOT_FULL_MHZ);
        let mut x = PAD;
        for i in 0..CA_SLOTS {
            let k = self.home.foot_ca[i];
            let on = show_ca && i < act_ca.len();
            kit::show(k.band, on);
            kit::show(k.bar, on);
            if !on {
                continue;
            }
            self.set(k.band, labels[i].as_str());
            kit::text_color(k.band, th.t1);
            k.band.set_x(x);
            x += lws[i] + 4;
            let bw = (bars * act_ca[i].bw.max(1) / mhz).max(6);
            k.bar.set_x(x);
            k.bar.set_width(bw);
            x += bw + 8;
        }
        // no carriers: one status line instead
        let txt: Vec<u8> = if show_ca {
            Vec::new()
        } else if v.sim_usable == 0 {
            let names: Vec<&str> = m.esim_profiles.iter().map(|p| p.name.as_str()).filter(|n| !n.is_empty()).collect();
            if names.is_empty() { Vec::new() } else { cf!("eSIM：%s", &names.join("、")) }
        } else if let Some(s) = idle.filter(|s| s.has_sig != 0) {
            let (mcc, mnc) = (s.mcc.parse().unwrap_or(0), s.mnc.parse().unwrap_or(-1));
            let name = crate::data::core_link::mainland_operator_cn(mcc, mnc, &s.operator).unwrap_or(s.operator.as_str());
            cf!(tr("待机卡 %s %s"), name, if (1..=2).contains(&s.sig_bars) { tr("信号弱") } else { tr("信号好") })
        } else if nosvc {
            tr("没有在用的载波").into()
        } else {
            v.ca_val.as_bytes().to_vec()
        };
        kit::show(h.foot_txt, !txt.is_empty());
        h.foot_txt.set_width((CARD_W - 2 * PAD - rw).max(1));
        self.set(h.foot_txt, &txt);

        let q = self.home.ca_qos;
        self.set(q, qos_line(d));
        self.ca_paint(v);
    }

    /// One point on the status card's speed wave (the net chart's step and average).
    pub(super) fn home_wave_push(&mut self, avg_rx: i64) {
        let ver = self.model.core_version;
        let h = &mut self.home;
        if h.wave.is_null() || ver == h.wave_ver {
            return;
        }
        h.wave_ver = ver;
        // idle (the axis floor, ~10 KB/s: background chatter) is a gap, not a flat line along the
        // floor that reads like a broken screen
        let v = super::system::rate_scale(avg_rx);
        let v = if v <= WAVE_FLOOR { lv::CHART_POINT_NONE } else { v };
        h.wave.chart_set_next_value(h.wave_s, v);
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
                // core sends the numbers already written
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

    /// 落地 (Home): 「到了日本 · 2 小时前」, 「从中国来 · 落地以来 120MB」; shown for 24 h after a
    /// landing the core recorded. Country names come from /api/netinfo (serving / home operator).
    pub(super) fn home_landing_paint(&mut self) {
        let m = self.model.clone();
        let now = self.now();
        let card = self.home.ld_card;
        let show = m.landing.as_ref().filter(|l| now >= l.at - 300 && now - l.at < 86400);
        let was = !card.hidden();
        let Some(l) = show else {
            if was {
                kit::show(card, false);
                self.home_reflow();
            }
            return;
        };
        let ni = m.netinfo.clone().unwrap_or_default();
        let name = |mcc: i32| -> Option<String> {
            [&ni.serving, &ni.home].into_iter().find(|o| o.mcc.parse::<i32>().ok() == Some(mcc) && !o.country.is_empty()).map(|o| o.country.clone())
        };
        let here = name(l.mcc).unwrap_or_else(|| format!("MCC {}", l.mcc));
        let (title, when, note) = (self.home.ld_title, self.home.ld_when, self.home.ld_note);
        self.set(title, cf!(tr("到了%s"), &here));
        self.set(when, super::diagnose::ago(now - l.at));
        let used = String::from_utf8_lossy(&fmt_bytes_total(l.rx + l.tx)).into_owned();
        match name(l.from) {
            Some(from) => self.set(note, cf!(tr("从%s来 · 落地以来 %s"), &from, &used)),
            None => self.set(note, cf!(tr("落地以来 %s"), &used)),
        }
        if !was {
            kit::show(card, true);
            self.home_reflow();
        }
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
        let mut y = 0;
        for i in 0..rows {
            if i > 0 {
                h.ts_sep[i].set_y(y);
            }
            y += kit::pair_reflow(h.ts_key[i], h.ts_val[i], y, CARD_W - PAD - if i == 0 { 16 } else { 0 });
        }
        // The status dot follows the state value even if a long state wraps.
        h.ts_val[0].update_layout();
        h.ts_dot.set_pos(h.ts_val[0].x() - 12, h.ts_val[0].y() + 6);
        h.ts_card.set_height(y);
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

    /// The view the status card and status bar draw (/v2/screen, or a placeholder
    /// that says why there is none).
    pub(super) fn screen_view(&self) -> net_view_t {
        match &self.model.screen_net {
            Some(v) => v.clone(),
            None if self.model.screen_status == SF_OLD_DATAD => nv::placeholder("—", tr("数据服务版本太旧：要更新 luyaz-core")),
            None => nv::placeholder(tr("读取中…"), ""),
        }
    }
}

/// A Wi-Fi client's signal in words; luyaz-core grades it (netinfo.rs signal_tier).
pub fn wifi_sig_word(tier: &str) -> Option<&'static str> {
    match tier {
        "great" => Some(tr("信号很好")),
        "good" => Some(tr("信号好")),
        "fair" => Some(tr("信号一般")),
        "weak" => Some(tr("信号弱")),
        _ => None,
    }
}

/// A switch the user just flipped keeps its new value this long, or until core reports the
/// same value, so it does not snap back while the core's next read is on its way.
const AUX_HOLD_MS: u32 = 30000;

/// Device state the main snapshot fields don't cover: the
/// DHCP pool, direct power supply and the cellular data / roaming switches come from core;
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

/// Take the core's value unless a local flip is still being held.
fn aux_take(cur: &mut i32, hold: &mut u32, from_core: i32, now: u32) {
    if from_core < 0 {
        return; // not in /state: keep what we have
    }
    if *hold != 0 && from_core != *cur && now.wrapping_sub(*hold) < AUX_HOLD_MS {
        return;
    }
    *hold = 0;
    *cur = from_core;
}

impl Ui {
    /// the core's values; the radios and power save from the device.
    pub(super) fn aux_refresh(&mut self, active: bool, d: &ui_model::core_snapshot_t) {
        let now = lv::tick_get();
        let a = &mut self.aux;
        if d.populated != 0 {
            aux_take(&mut a.dps, &mut a.hold_dps, d.dps_mode, now);
            aux_take(&mut a.data, &mut a.hold_data, d.cell_data, now);
            aux_take(&mut a.roam, &mut a.hold_roam, d.cell_roam, now);
            a.pool.clear();
            if !d.lan_ip.is_empty() && !d.dhcp_first.is_empty() {
                a.pool = logic::dhcp_pool_text(&d.lan_ip, &d.dhcp_first, &d.dhcp_count);
            }
        }
        if !active || (a.read_at != 0 && now.wrapping_sub(a.read_at) < 5000) {
            return;
        }
        if let Some((w24, w5, psm)) = self.src.aux_read() {
            let a = &mut self.aux;
            a.read_at = now;
            aux_take(&mut a.w24, &mut a.hold_w24, w24, now);
            aux_take(&mut a.w5, &mut a.hold_w5, w5, now);
            if psm >= 0 {
                a.psm = psm;
            }
        }
    }
}

/// 载波 card's QoS line: QCI or 5QI by the core's `bearer` (not the radio mode), AMBR as the
/// network's ceiling in Mbps (未知 when the core has the QCI but no AMBR), （旧） when the core marks the values stale; empty without a value.
pub(crate) fn qos_line(d: &ui_model::core_snapshot_t) -> Vec<u8> {
    if d.qos_class <= 0 && d.ambr_down <= 0.0 && d.ambr_up <= 0.0 {
        return Vec::new();
    }
    let kind = if d.qos_bearer == "nr_pdu" { "5QI" } else { "QCI" };
    let mut s = if d.qos_class > 0 { format!("{kind} {}", d.qos_class) } else { format!("{kind} —") }.into_bytes();
    if d.ambr_down > 0.0 || d.ambr_up > 0.0 {
        s.extend(cf!(tr(" · 上限 %d/%d Mbps"), d.ambr_down as i32, d.ambr_up as i32));
    } else if d.qos_stale == 0 {
        // the QCI is live but the bearer line with the AMBR is gone (until the next connection)
        s.extend_from_slice(tr(" · 上限 未知").as_bytes());
    }
    if d.qos_stale != 0 {
        s.extend_from_slice(tr("（旧）").as_bytes());
    }
    s
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

    #[test]
    fn qos_line_labels_by_bearer_and_marks_stale() {
        let mut d = ui_model::core_snapshot_t { qos_class: 9, ambr_down: 1668.64, ambr_up: 1008.4, ..Default::default() };
        d.qos_bearer = "eps".into();
        assert_eq!(qos_line(&d), "QCI 9 · 上限 1668/1008 Mbps".as_bytes());
        d.qos_bearer = "nr_pdu".into();
        assert_eq!(qos_line(&d), "5QI 9 · 上限 1668/1008 Mbps".as_bytes());
        d.qos_stale = 1;
        assert_eq!(qos_line(&d), "5QI 9 · 上限 1668/1008 Mbps（旧）".as_bytes());
        let mut live = ui_model::core_snapshot_t { qos_class: 9, ..Default::default() };
        live.qos_bearer = "nr_pdu".into();
        assert_eq!(qos_line(&live), "5QI 9 · 上限 未知".as_bytes());
        let empty = ui_model::core_snapshot_t::default();
        assert!(qos_line(&empty).is_empty());
    }
}
