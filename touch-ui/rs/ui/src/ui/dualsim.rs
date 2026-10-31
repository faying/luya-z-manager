//! 双卡 on the SIM 与 eSIM page (global edition, manager docs/designs/dual-sim-global.md): what the
//! two slots hold, five choices; two taps send one `sim.dual` through core, whose verdict and
//! countdown take the line on top until it ends. Hidden on a one-slot device.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::cellular::nz;
use super::network::NetFlash;
use super::Ui;
use crate::cf;
use crate::kit::{self, Hero, CARD_W, HERO_H, MARGIN, PAD};
use crate::lang::tr;
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};
use ui_model::sim_dual_t;

pub const DS_ROWS: usize = 5;
const DS_ROW_H: i32 = 50;
/// the footer under the choices: the hint, or core's words while a switch runs (two lines at most)
const DS_FOOT_H: i32 = 40;
/// where the choices' caption sits: section title, the current-mode card, gap
const DS_CHOICES_Y: i32 = 20 + HERO_H + 10;
/// the block's height on the page: section title, current mode, 切换到 + the choices, footer
pub const DS_H: i32 = DS_CHOICES_Y + 20 + DS_ROWS as i32 * DS_ROW_H + DS_FOOT_H + FWD_BLOCK;
const FWD_BLOCK: i32 = 0;
const ARM_MS: u32 = 5000;
/// sent and core never said anything about it (its own deadline is 120 s)
const WAIT_MS: u32 = 150_000;
const ITEM: &str = "sim.dual";

#[derive(Default)]
pub struct DualSim {
    pub sec: Obj,
    /// where things stand: its own tinted card (the page's 使用中 hero), apart from the choices
    now_card: Obj,
    now: Hero,
    cap: Obj,
    card: Obj,
    /// the hint, a refusal or core's words on a running switch
    line: Obj,
    row: [Obj; DS_ROWS],
    name: [Obj; DS_ROWS],
    sub: [Obj; DS_ROWS],
    tag: [Obj; DS_ROWS],
    pub flash: NetFlash,
    /// the choice the first tap armed, and when
    armed: Option<usize>,
    arm_at: u32,
    /// sent, waiting for core: the choice and when
    want: Option<usize>,
    sent: u32,
    /// the core's last words on it (kept after the transaction ended, until the next tap)
    said: String,
    /// `said` is the end of our own switch: kept until the next tap
    keep: bool,
    /// what was painted last (repaint only on a change)
    key: String,
}

/// Choice i as `sim.dual` params: 只用卡槽 1, 只用卡槽 2, 双卡主卡 1, 双卡主卡 2, 双卡智能.
pub fn params(i: usize) -> &'static str {
    [
        r#"{"mode":"single","active_slot":1}"#,
        r#"{"mode":"single","active_slot":2}"#,
        r#"{"mode":"dual","smart":false,"active_slot":1}"#,
        r#"{"mode":"dual","smart":false,"active_slot":2}"#,
        r#"{"mode":"dual","smart":true}"#,
    ][i]
}

/// The choice the device is on, None when it reads as neither.
pub fn current(d: &sim_dual_t) -> Option<usize> {
    match (d.mode.as_str(), d.smart != 0, d.active_slot) {
        ("dual", true, _) => Some(4),
        ("dual", false, s @ 1..=2) => Some(1 + s as usize),
        ("single", _, s @ 1..=2) => Some(s as usize - 1),
        _ => None,
    }
}

/// Why choice i can't be taken (no card in a slot it needs), None when it can.
fn missing(d: &sim_dual_t, i: usize) -> Option<i32> {
    let has = |n: i32| d.slots.iter().any(|s| s.slot == n && s.present != 0);
    let need: &[i32] = match i {
        0 => &[1],
        1 => &[2],
        _ => &[1, 2],
    };
    need.iter().copied().find(|&n| !has(n))
}

/// D4: the card choice i puts data on may roam here (its country is not the network's, or
/// can't be told: a slot that is off reports none). Smart: either card may carry data.
pub fn may_roam(d: &sim_dual_t, i: usize, serving_mcc: i32) -> bool {
    let slots: &[i32] = match i {
        0 | 2 => &[1],
        1 | 3 => &[2],
        _ => &[1, 2],
    };
    slots.iter().any(|&n| {
        let mcc = d.slots.iter().find(|s| s.slot == n).map(|s| s.mcc.as_str()).unwrap_or("");
        serving_mcc <= 0 || mcc.parse::<i32>().ok() != Some(serving_mcc)
    })
}

fn choice_name(i: usize) -> &'static str {
    [
        tr("只用卡槽 1（插拔卡）"),
        tr("只用卡槽 2（内置卡）"),
        tr("双卡 · 主卡 卡槽 1"),
        tr("双卡 · 主卡 卡槽 2"),
        tr("双卡 · 智能切换"),
    ][i]
}

/// The head line: the same words core uses for the item.
fn now_words(i: Option<usize>) -> &'static str {
    match i {
        Some(0) => tr("单卡 · 卡槽 1"),
        Some(1) => tr("单卡 · 卡槽 2"),
        Some(2) => tr("双卡 · 主卡 卡槽 1"),
        Some(3) => tr("双卡 · 主卡 卡槽 2"),
        Some(_) => tr("双卡 · 智能切换"),
        None => "—",
    }
}

/// 「CUCC · 460-01 · 尾号 1234」, or why there is nothing to say.
fn slot_words(d: &sim_dual_t, n: i32) -> Vec<u8> {
    let Some(s) = d.slots.iter().find(|s| s.slot == n) else { return tr("没有卡").into() };
    if s.present == 0 {
        return tr("没有卡").into();
    }
    let mut parts: Vec<String> = Vec::new();
    if !s.operator.is_empty() {
        parts.push(s.operator.clone());
    }
    if !s.mcc.is_empty() {
        parts.push(format!("{}-{}", s.mcc, s.mnc));
    }
    if !s.iccid_tail.is_empty() {
        parts.push(String::from_utf8_lossy(&cf!(tr("尾号 %.4s"), s.iccid_tail.as_str())).into_owned());
    }
    if s.provisioned == 0 {
        parts.push(tr("没启用").into());
    }
    parts.join(" · ").into_bytes()
}

unsafe extern "C" fn ds_row_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let i = unsafe { lv::event_user(e) };
    super::with(|u| u.ds_tap(i));
}

impl DualSim {
    fn show(&self, on: bool) {
        for o in [self.sec, self.now_card, self.cap, self.card, self.line] {
            kit::show(o, on);
        }
    }

    fn big_set(&self, txt: &str) {
        self.now.big.set_text(txt);
    }
}

impl Ui {
    /// The block at y on the eSIM page's scroll; hidden until core reports two slots.
    pub(super) fn build_dualsim(&mut self, sc: Obj, y: i32) {
        let th = t();
        let f = uf();
        let ds = &mut self.ds;
        ds.sec = kit::section(sc, y, tr("双卡"));
        ds.now_card = kit::card(sc, MARGIN, y + 20, CARD_W, HERO_H);
        ds.now = kit::hero(ds.now_card, f.cj22b);
        kit::hero_tone(&ds.now, 4);
        ds.now.st.set_text(tr("当前模式"));
        ds.big_set("—");
        ds.cap = kit::section(sc, y + DS_CHOICES_Y, tr("切换到"));
        ds.card = kit::card(sc, MARGIN, y + DS_CHOICES_Y + 20, CARD_W, DS_ROWS as i32 * DS_ROW_H);
        ds.card.set_style_clip_corner(true, 0);
        let foot_y = y + DS_CHOICES_Y + 20 + DS_ROWS as i32 * DS_ROW_H + 6;
        ds.line = kit::label_w(sc, f.cj12, th.t2, MARGIN + 6, foot_y, CARD_W - 12, true, "");
        for i in 0..DS_ROWS {
            let row = kit::bx(ds.card, 0, i as i32 * DS_ROW_H, CARD_W, DS_ROW_H, th.card, 0);
            kit::tappable(row, Some(ds_row_cb), i);
            if i > 0 {
                kit::sep(row, 0);
            }
            ds.row[i] = row;
            ds.name[i] = kit::label_w(row, f.cj14, th.t1, PAD, 8, 200, false, choice_name(i));
            ds.sub[i] = kit::label_w(row, f.n11, th.t3, PAD, 29, CARD_W - 2 * PAD, false, "");
            ds.tag[i] = kit::label_r(row, f.cj13, th.acc_t, CARD_W - PAD, 16, "");
        }
        ds.show(false);
    }

    /// A tap on a choice: arm, then send; when nothing can move, say why (never silently).
    fn ds_tap(&mut self, i: usize) {
        let th = t();
        let now = lv::tick_get();
        let Some(d) = self.model.data.sim_dual.clone() else { return };
        let a = &self.op.view.active;
        let msg: Option<&str> = if self.ds.want.is_some() || (a.have && a.item == ITEM) {
            Some(tr("正在切换，等它做完"))
        } else if a.have {
            Some(tr("正在改别的设置，等它做完"))
        } else if d.switching != 0 {
            Some(tr("设备正在自己换卡，等一下"))
        } else if current(&d) == Some(i) {
            Some(tr("已经是这样了"))
        } else {
            None
        };
        let ds = &mut self.ds;
        if let Some(m) = msg {
            ds.armed = None;
            ds.flash.set(th.t2, 3000, m.into());
        } else if let Some(n) = missing(&d, i) {
            ds.armed = None;
            ds.flash.set(th.bad_t, 4000, cf!(tr("卡槽 %d 没有卡"), n));
        } else if ds.armed == Some(i) && now.wrapping_sub(ds.arm_at) < ARM_MS {
            ds.armed = None;
            ds.flash.until = 0;
            ds.want = Some(i);
            ds.sent = nz(now);
            ds.keep = false;
            ds.said = tr("已下发，等设备确认…").into();
            if !self.data_control(ITEM, params(i), None) {
                let ds = &mut self.ds;
                ds.want = None;
                ds.keep = true;
                ds.said = tr("没发出去：连不上核心，等一下再试").into();
            }
        } else {
            ds.armed = Some(i);
            ds.arm_at = nz(now);
            ds.keep = false;
            ds.flash.until = 0;
        }
        self.ds.key.clear();
        self.ds_paint();
    }

    /// Every refresh: close the confirm window, follow core's verdict, repaint on a change.
    pub(super) fn ds_paint(&mut self) {
        let th = t();
        let now = lv::tick_get();
        let d = self.model.data.sim_dual.clone();
        let serving = self.model.data.mcc;
        if self.ds.armed.is_some() && now.wrapping_sub(self.ds.arm_at) >= ARM_MS {
            self.ds.armed = None;
        }
        // core's words while it has a sim.dual (ours or the web page's), and the result of ours
        let since = if self.ds.want.is_some() { self.ds.sent } else { 0 };
        let running = self.op.view.active.have && self.op.view.active.item == ITEM;
        let core = self.op_item_line(ITEM, since);
        let ds = &mut self.ds;
        match core {
            Some(ol) => {
                ds.said = ol;
                if ds.want.is_some() && !running {
                    ds.want = None; // ended: its verdict stays until the next tap
                    ds.keep = true;
                }
            }
            None if ds.want.is_some() => {
                // core turned it down before starting (usbmode.rs's way): its words, at once
                let m = &self.model;
                if m.notice_ok != 0 && m.notice.action == ITEM && m.notice.at.wrapping_sub(ds.sent) as i32 >= 0 {
                    ds.said = if m.notice.say_zh.is_empty() {
                        tr("没切成 · 可以再试一次").into()
                    } else {
                        crate::lang::pick(&m.notice.say_zh, &m.notice.say_en).into()
                    };
                    ds.want = None;
                    ds.keep = true;
                } else if !running && now.wrapping_sub(ds.sent) >= WAIT_MS {
                    ds.said = tr("设备没回话，看「改动记录」").into();
                    ds.want = None;
                    ds.keep = true;
                }
            }
            None if !ds.keep => ds.said.clear(),
            None => {}
        }
        let Some(d) = d else {
            if !ds.key.is_empty() {
                ds.key.clear();
                ds.show(false);
            }
            return;
        };
        let cur = current(&d);
        let line: Vec<u8> = if ds.flash.on() {
            ds.flash.txt.clone()
        } else if !ds.said.is_empty() {
            ds.said.clone().into_bytes()
        } else if d.switching != 0 {
            tr("设备正在自己换卡…").into()
        } else {
            tr("点两次确认切换，会断网约 10 秒").into()
        };
        let busy = ds.want.is_some() || running;
        let key = format!("{d:?}|{cur:?}|{:?}|{:?}|{}|{}|{}", ds.armed, ds.want, String::from_utf8_lossy(&line), busy, serving);
        if key == ds.key {
            return;
        }
        ds.key = key;
        ds.show(true);
        // where things stand lives in its own tinted card; a switch underway turns it amber
        let moving = busy || d.switching != 0;
        kit::hero_tone(&ds.now, if moving { 1 } else { 4 });
        ds.now.st.set_text(if moving { tr("正在切换") } else { tr("当前模式") });
        ds.big_set(now_words(cur));
        ds.line.set_text(&line);
        kit::text_color(ds.line, if ds.flash.on() { ds.flash.col } else { th.t2 });
        for i in 0..DS_ROWS {
            let armed = ds.armed == Some(i);
            let going = ds.want == Some(i);
            let on = cur == Some(i);
            let gone = missing(&d, i).is_some();
            let sub: Vec<u8> = if armed {
                if may_roam(&d, i, serving) {
                    tr("再点一次切换 · 断网约 10 秒 · 可能算漫游").into()
                } else {
                    tr("再点一次切换 · 断网约 10 秒").into()
                }
            } else {
                match i {
                    0 => slot_words(&d, 1),
                    1 => slot_words(&d, 2),
                    2 => tr("两张卡都开，上网用卡槽 1").into(),
                    3 => tr("两张卡都开，上网用卡槽 2").into(),
                    _ => tr("两张卡都开，设备自己选上网的卡").into(),
                }
            };
            ds.sub[i].set_text(&sub);
            ds.tag[i].set_text(if going {
                tr("切换中…")
            } else if armed {
                tr("再点一次确认")
            } else if on {
                tr("使用中")
            } else if gone {
                tr("没有卡")
            } else {
                ""
            });
            kit::bg(ds.row[i], if armed { th.fill_orange } else { th.card });
            kit::text_color(ds.name[i], if armed { 0xffffff } else if gone || (busy && !going) { th.t3 } else { th.t1 });
            kit::text_color(ds.sub[i], if armed { th.on_fill } else { th.t3 });
            kit::text_color(ds.tag[i], if armed { 0xffffff } else if on || going { th.acc_t } else { th.t3 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui_model::sim_slot_t;

    fn dual(mode: &str, smart: i32, slot: i32, mcc2: &str) -> sim_dual_t {
        let s = |n: i32, mcc: &str| sim_slot_t { slot: n, present: 1, provisioned: 1, mcc: mcc.into(), ..Default::default() };
        sim_dual_t { mode: mode.into(), smart, active_slot: slot, switching: 0, slots: vec![s(1, "460"), s(2, mcc2)] }
    }

    #[test]
    fn current_matches_params() {
        // each choice's params read back as that choice (core's readback has the same shape)
        for i in 0..DS_ROWS {
            let p = params(i).as_bytes();
            let d = sim_dual_t {
                mode: crate::json::get_str(p, "mode", 16).unwrap(),
                smart: (crate::json::get(p, "smart", 8).as_deref() == Some(b"true")) as i32,
                active_slot: crate::json::get_int(p, "active_slot", 1) as i32,
                ..Default::default()
            };
            assert_eq!(current(&d), Some(i), "{}", params(i));
        }
        // single mode keeps a stale smart flag: still single
        assert_eq!(current(&dual("single", 1, 2, "")), Some(1));
        assert_eq!(current(&dual("single", 0, 0, "")), None);
    }

    #[test]
    fn missing_and_roaming() {
        let mut d = dual("single", 0, 1, "");
        assert_eq!(missing(&d, 1), None);
        d.slots[1].present = 0;
        assert_eq!(missing(&d, 1), Some(2));
        assert_eq!(missing(&d, 4), Some(2));
        assert_eq!(missing(&d, 0), None);
        // slot 2 off: no MCC, can't tell → warn; slot 1 home → no warning; no network → warn
        assert!(may_roam(&d, 1, 460) && may_roam(&d, 3, 460) && may_roam(&d, 4, 460));
        assert!(!may_roam(&d, 0, 460) && !may_roam(&d, 2, 460));
        assert!(may_roam(&d, 0, 0));
        assert!(!may_roam(&dual("dual", 1, 1, "460"), 4, 460));
    }
}
