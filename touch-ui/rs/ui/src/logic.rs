//! The touch UI's pure decisions: no LVGL, no I/O.
//! The power-key cases' tests go with key_input.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

const DARK_FROM_DEFAULT: i32 = 19 * 60;
const DARK_TO_DEFAULT: i32 = 7 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Appear {
    #[default]
    Light,
    Dark,
    Auto,
}

/// "light" / "dark" / "auto"; anything else → Light and Err (reported by the caller).
pub fn appear_parse(s: Option<&str>) -> (Appear, bool) {
    match s {
        Some("dark") => (Appear::Dark, true),
        Some("auto") => (Appear::Auto, true),
        Some("light") => (Appear::Light, true),
        _ => (Appear::Light, false),
    }
}

pub fn appear_name(a: Appear) -> &'static str {
    match a {
        Appear::Dark => "dark",
        Appear::Auto => "auto",
        Appear::Light => "light",
    }
}

/// Exactly H:MM or HH:MM → minute of the day; -1 otherwise.
pub fn hhmm_parse(s: Option<&str>) -> i32 {
    let Some(s) = s else { return -1 };
    let b = s.as_bytes();
    let (mut h, mut i) = (0i32, 0usize);
    while i < b.len() && b[i].is_ascii_digit() && i < 2 {
        h = h * 10 + (b[i] - b'0') as i32;
        i += 1;
    }
    if i == 0 || b.get(i) != Some(&b':') {
        return -1;
    }
    let r = &b[i + 1..];
    if r.len() != 2 || !r[0].is_ascii_digit() || !r[1].is_ascii_digit() {
        return -1;
    }
    let m = ((r[0] - b'0') * 10 + (r[1] - b'0')) as i32;
    if h > 23 || m > 59 {
        return -1;
    }
    h * 60 + m
}

fn minute_ok(m: i32) -> bool {
    (0..24 * 60).contains(&m)
}

pub fn is_dark(a: Appear, now_min: i32, mut from_min: i32, mut to_min: i32) -> bool {
    match a {
        Appear::Dark => return true,
        Appear::Light => return false,
        Appear::Auto => {}
    }
    if !minute_ok(from_min) || !minute_ok(to_min) {
        from_min = DARK_FROM_DEFAULT;
        to_min = DARK_TO_DEFAULT;
    }
    if !minute_ok(now_min) || from_min == to_min {
        return false;
    }
    if from_min < to_min {
        now_min >= from_min && now_min < to_min
    } else {
        now_min >= from_min || now_min < to_min
    }
}

// ---- exec guard ----
pub const EXEC_COOLDOWN_S: i64 = 600;
pub const EXEC_WINDOW_S: i64 = 3600;
pub const EXEC_STRIKES: i32 = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecHist {
    pub first_s: i64,
    pub last_s: i64,
    pub count: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecVerdict {
    Allow,
    Cooldown,
    Suspend,
}

pub fn exec_check(h: Option<&ExecHist>, now_s: i64) -> ExecVerdict {
    let Some(h) = h else { return ExecVerdict::Allow };
    if h.count <= 0 || h.first_s <= 0 {
        return ExecVerdict::Allow;
    }
    let in_window = now_s - h.first_s < EXEC_WINDOW_S && now_s >= h.first_s;
    if in_window && h.count >= EXEC_STRIKES {
        return ExecVerdict::Suspend;
    }
    if h.last_s > 0 && now_s >= h.last_s && now_s - h.last_s < EXEC_COOLDOWN_S {
        return ExecVerdict::Cooldown;
    }
    ExecVerdict::Allow
}

pub fn exec_next(h: Option<&ExecHist>, now_s: i64) -> ExecHist {
    let mut n = ExecHist { first_s: now_s, last_s: now_s, count: 1 };
    if let Some(h) = h
        && h.count > 0
        && h.first_s > 0
        && now_s >= h.first_s
        && now_s - h.first_s < EXEC_WINDOW_S
    {
        n.first_s = h.first_s;
        n.count = h.count + 1;
    }
    n
}

/// Start arguments an in-process exec (theme, language) carries over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Launch {
    pub tab: i32,
    pub scroll_y: i32,
    /// 1 = went off by itself (auto-off), 2 = the power key turned it off
    pub screen_off: i32,
    pub autooff_ms: i32,
    pub bright: i32,
    pub hist: ExecHist,
}

impl Default for Launch {
    fn default() -> Self {
        Launch { tab: -1, scroll_y: 0, screen_off: 0, autooff_ms: -1, bright: -1, hist: ExecHist::default() }
    }
}

fn arg_long(a: &str, key: &str) -> Option<i64> {
    let v = a.strip_prefix(key)?;
    if v.is_empty() {
        return None;
    }
    // strtol: optional sign, digits, nothing after
    let n: i64 = v.parse().ok()?;
    (n >= 0).then_some(n)
}

pub fn launch_parse<S: AsRef<str>>(argv: &[S]) -> Launch {
    let mut out = Launch::default();
    for a in argv.iter().skip(1) {
        let a = a.as_ref();
        if a == "--screen-off" {
            out.screen_off = 1;
        } else if a == "--screen-off=key" {
            out.screen_off = 2;
        } else if let Some(v) = arg_long(a, "--tab=") {
            if v <= 4 {
                out.tab = v as i32;
            }
        } else if let Some(v) = arg_long(a, "--scroll=") {
            if v <= 100000 {
                out.scroll_y = v as i32;
            }
        } else if let Some(v) = arg_long(a, "--autooff=") {
            if v <= 86400000 {
                out.autooff_ms = v as i32;
            }
        } else if let Some(v) = arg_long(a, "--bright=") {
            if (1..=255).contains(&v) {
                out.bright = v as i32;
            }
        } else if let Some(v) = arg_long(a, "--exec-first=") {
            out.hist.first_s = v;
        } else if let Some(v) = arg_long(a, "--exec-last=") {
            out.hist.last_s = v;
        } else if let Some(v) = arg_long(a, "--exec-n=")
            && v <= 100
        {
            out.hist.count = v as i32;
        }
    }
    let h = &mut out.hist;
    if (h.count > 0 && (h.first_s <= 0 || h.last_s < h.first_s)) || h.count == 0 {
        *h = ExecHist::default();
    }
    out
}

pub fn launch_argv(l: &Launch) -> Vec<String> {
    let mut v = Vec::new();
    if (0..=4).contains(&l.tab) {
        v.push(format!("--tab={}", l.tab));
    }
    if l.scroll_y > 0 {
        v.push(format!("--scroll={}", l.scroll_y));
    }
    if l.screen_off == 1 {
        v.push("--screen-off".into());
    } else if l.screen_off == 2 {
        v.push("--screen-off=key".into());
    }
    if l.autooff_ms >= 0 {
        v.push(format!("--autooff={}", l.autooff_ms));
    }
    if l.bright >= 1 {
        v.push(format!("--bright={}", l.bright));
    }
    if l.hist.count > 0 {
        v.push(format!("--exec-first={}", l.hist.first_s));
        v.push(format!("--exec-last={}", l.hist.last_s));
        v.push(format!("--exec-n={}", l.hist.count));
    }
    v
}

pub const CLOCK_SANE_AFTER: i64 = 1704067200;

pub fn clock_sane(wall_s: i64) -> bool {
    wall_s >= CLOCK_SANE_AFTER
}

/// ui.conf autooff → one of the 系统 page's choices: 0 (常亮), 30 s, 2 min.
pub fn autooff_snap(ms: i64) -> i32 {
    if ms == 0 {
        0
    } else if ms > 0 && ms <= 30000 {
        30000
    } else {
        120000
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AutoIn {
    pub appear: Appear,
    pub cur_dark: bool,
    pub wall_s: i64,
    pub now_min: i32,
    pub from_min: i32,
    pub to_min: i32,
    pub screen_off: bool,
    pub always_on: bool,
    pub idle_ms: i64,
    pub busy: bool,
    pub suspended: bool,
    pub hist: ExecHist,
    pub now_boot_s: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auto {
    Stay,
    Wait,
    Exec,
    Suspend,
}

pub const AUTO_ALWAYS_ON_IDLE_MS: i64 = 60000;

pub fn auto_decide(i: &AutoIn) -> Auto {
    if i.appear != Appear::Auto || i.suspended || !clock_sane(i.wall_s) {
        return Auto::Stay;
    }
    let want = is_dark(Appear::Auto, i.now_min, i.from_min, i.to_min);
    if want == i.cur_dark {
        return Auto::Stay;
    }
    let unseen = i.screen_off || (i.always_on && i.idle_ms >= AUTO_ALWAYS_ON_IDLE_MS);
    if !unseen || i.busy {
        return Auto::Wait;
    }
    match exec_check(Some(&i.hist), i.now_boot_s) {
        ExecVerdict::Suspend => Auto::Suspend,
        ExecVerdict::Cooldown => Auto::Wait,
        ExecVerdict::Allow => Auto::Exec,
    }
}

// ---- touch while the screen is dark ----
pub const TAP_MAX_MS: i64 = 350;
pub const TAP_SLOP: i32 = 24;
pub const DTAP_GAP_MS: i64 = 450;
pub const DTAP_SLOP: i32 = 60;
pub const WAKE_QUIET_MS: i64 = 300;
pub const TG_FORWARD: i32 = 1;
pub const TG_WAKE: i32 = 2;

#[derive(Clone, Copy, Debug, Default)]
pub struct TGate {
    pub was: bool,
    pub taps: i32,
    pub px: i32,
    pub py: i32,
    pub x0: i32,
    pub y0: i32,
    pub suppress: bool,
    pub t_down: i64,
    pub t_up: i64,
    pub wake_ms: i64,
}

fn near(x0: i32, y0: i32, x1: i32, y1: i32, r: i32) -> bool {
    (x1 - x0) * (x1 - x0) + (y1 - y0) * (y1 - y0) <= r * r
}

impl TGate {
    pub fn woke(&mut self, now_ms: i64) {
        self.suppress = true;
        self.wake_ms = now_ms;
        self.taps = 0;
    }

    pub fn step(&mut self, lit: bool, touch_wakes: bool, pressed: bool, x: i32, y: i32, now_ms: i64) -> i32 {
        let down = pressed && !self.was;
        let up = !pressed && self.was;
        self.was = pressed;
        if lit {
            self.taps = 0;
            if self.suppress {
                if pressed || now_ms - self.wake_ms < WAKE_QUIET_MS {
                    return 0;
                }
                self.suppress = false;
            }
            return if pressed { TG_FORWARD } else { 0 };
        }
        if !touch_wakes {
            self.taps = 0;
            return 0;
        }
        if down {
            if self.taps == 1 && now_ms - self.t_up <= DTAP_GAP_MS && near(self.x0, self.y0, x, y, DTAP_SLOP) {
                self.taps = 2;
            } else {
                self.taps = 1;
                self.x0 = x;
                self.y0 = y;
            }
            self.px = x;
            self.py = y;
            self.t_down = now_ms;
        } else if up && self.taps != 0 {
            if now_ms - self.t_down > TAP_MAX_MS || !near(self.px, self.py, x, y, TAP_SLOP) {
                self.taps = 0;
            } else if self.taps == 2 {
                self.woke(now_ms);
                return TG_WAKE;
            } else {
                self.t_up = now_ms;
            }
        }
        0
    }
}

// ---- status bar ----
/// Widest of the three rate forms that fits in maxw (the last one regardless).
pub fn rate_pick(widths: [i32; 3], maxw: i32) -> usize {
    (0..2).find(|&i| widths[i] <= maxw).unwrap_or(2)
}

// ---- battery ----
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatState {
    Normal,
    Charging,
    Low,
}

pub fn bat_state(pct: i32, charging: bool) -> BatState {
    if charging {
        BatState::Charging
    } else if pct <= 20 {
        BatState::Low
    } else {
        BatState::Normal
    }
}

/// 27, 30 at 100 %; charging only recolours the body.
pub fn bat_body_w(pct: i32, _charging: bool) -> i32 {
    if pct >= 100 { 30 } else { 27 }
}

/// The low state's red fill: ≥ 3 px, ≤ 20 % of the body.
pub fn bat_red_w(pct: i32, body_w: i32) -> i32 {
    let pct = pct.clamp(0, 20);
    (body_w * pct / 100).max(3)
}

// ---- radio technology names ----
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rat {
    None,
    G2,
    G3,
    G4,
    G5Nsa,
    G5Sa,
}

pub fn rat(raw: &str) -> Rat {
    if raw.is_empty() {
        return Rat::None;
    }
    let u: String = raw.chars().take(31).collect::<String>().to_ascii_uppercase();
    let has = |w: &str| u.contains(w);
    if has("LIMIT") || has("EMERGENCY") || has("NO_SERVICE") || has("NOSERVICE") {
        return Rat::None;
    }
    if has("NSA") || has("ENDC") || has("EN-DC") {
        return Rat::G5Nsa;
    }
    if u == "SA" || u == "NR" || has("5G") || has("NR5G") || u.starts_with("SA_") || u.starts_with("NR_") || has("_SA") || has(" SA") {
        return Rat::G5Sa;
    }
    if has("LTE") || has("4G") {
        return Rat::G4;
    }
    if [
        "WCDMA", "UMTS", "HSPA", "HSDPA", "HSUPA", "TD-SCDMA", "TDSCDMA", "CDMA2000", "EVDO", "EV-DO", "EHRPD", "HRPD", "3G",
    ]
    .iter()
    .any(|w| has(w))
    {
        return Rat::G3;
    }
    if ["GSM", "GPRS", "EDGE", "2G", "CDMA", "1XRTT", "1X"].iter().any(|w| has(w)) {
        return Rat::G2;
    }
    Rat::None
}

fn atoi(s: &[u8]) -> i64 {
    let mut v = 0i64;
    for &c in s.iter().take_while(|c| c.is_ascii_digit()) {
        v = v * 10 + (c - b'0') as i64;
    }
    v
}

/// "NR5G BAND 78" → n78, "LTE BAND 3" → B3; frequencies ("GSM 900") and names stay.
pub fn band_short(raw: &str, nr: bool) -> String {
    if raw.is_empty() {
        return "-".into();
    }
    let b = raw.as_bytes();
    let last = (0..b.len()).rev().find(|&i| b[i].is_ascii_digit() && (i == 0 || !b[i - 1].is_ascii_digit()));
    match last {
        Some(i) if atoi(&b[i..]) < 450 => format!("{}{}", if nr { 'n' } else { 'B' }, atoi(&b[i..])),
        _ => raw.into(),
    }
}

// ---- the network verdict ----
pub const NET_OK: i32 = 0;
pub const NET_WARN: i32 = 1;
pub const NET_BAD: i32 = 2;
pub const NET_NEUTRAL: i32 = 3;

pub const CAUSE_NONE: i32 = 0;
pub const CAUSE_LIMIT: i32 = 1;
pub const CAUSE_WEAK: i32 = 2;
pub const CAUSE_NOISE: i32 = 3;
pub const CAUSE_CROWD: i32 = 4;
pub const CAUSE_NARROW: i32 = 5;

pub const NET_HOLD_MS: u32 = 15000;

#[derive(Clone, Copy, Debug, Default)]
pub struct NetHold {
    pub shown: u32,
    pub pending: u32,
    pub since: u32,
    pub have: bool,
}

/// A new headline shows only after it held for 15 s; the first one at once.
pub fn net_hold(h: &mut NetHold, key: u32, now_ms: u32) -> bool {
    if !h.have || key == h.shown {
        h.have = true;
        h.shown = key;
        h.pending = key;
        h.since = now_ms;
        return true;
    }
    if key != h.pending {
        h.pending = key;
        h.since = now_ms;
        return false;
    }
    if now_ms.wrapping_sub(h.since) < NET_HOLD_MS {
        return false;
    }
    h.shown = key;
    true
}

// ---- which card is in use ----
fn iccid_len(s: &str) -> usize {
    s.trim_end_matches(['F', 'f', ' ']).len()
}

pub fn iccid_same(a: &str, b: &str) -> bool {
    let (la, lb) = (iccid_len(a), iccid_len(b));
    la != 0 && la == lb && a.as_bytes()[..la].eq_ignore_ascii_case(&b.as_bytes()[..lb])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimKind {
    None,
    Plain,
    Esim,
}

pub fn sim_kind(sim_state: &str, modem_iccid: &str, esim_enabled_iccid: &str) -> SimKind {
    let ready = sim_state.contains("ready");
    if !ready && iccid_len(modem_iccid) == 0 {
        return SimKind::None;
    }
    // the vendor's sim_iccid can stay empty after a hot swap or an eSIM switch (2026-10-11):
    // one slot, so a card that is ready with an enabled profile on the eUICC is that eSIM
    if iccid_len(modem_iccid) == 0 && iccid_len(esim_enabled_iccid) != 0 {
        return SimKind::Esim;
    }
    if iccid_same(modem_iccid, esim_enabled_iccid) { SimKind::Esim } else { SimKind::Plain }
}

fn strtol_full(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    s.parse().ok()
}

/// "192.168.0.2 - 192.168.0.253" from the core's dhcp ip/start/limit; "" when unusable.
pub fn dhcp_pool_text(ip: &str, start: &str, limit: &str) -> String {
    let Some(dot) = ip.rfind('.').filter(|&d| d > 0) else { return String::new() };
    let start = start.rfind('.').map_or(start, |d| &start[d + 1..]);
    let Some(st) = strtol_full(start).filter(|v| (1..=254).contains(v)) else { return String::new() };
    let pre = &ip[..dot];
    match strtol_full(limit).filter(|&l| l >= 1) {
        None => format!("{pre}.{st}"),
        Some(lim) => format!("{pre}.{st} - {pre}.{}", (st + lim - 1).min(254)),
    }
}

// ---- core /control reply ----
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ctl {
    Ok,
    Busy,
    Full,
    Failed,
    NoReply,
}

pub const CTL_BUSY: i32 = 1;
pub const CTL_FULL: i32 = 2;

pub fn control_reply(reply: &[u8]) -> Ctl {
    if !reply.starts_with(b"HTTP/1.") {
        return Ctl::NoReply;
    }
    let Some(sp) = reply.iter().position(|&c| c == b' ') else { return Ctl::NoReply };
    let d = |i: usize| reply.get(sp + i).copied().unwrap_or(0);
    if !(b'1'..=b'5').contains(&d(1)) || !d(2).is_ascii_digit() || !d(3).is_ascii_digit() {
        return Ctl::NoReply;
    }
    let code = (d(1) - b'0') as i32 * 100 + (d(2) - b'0') as i32 * 10 + (d(3) - b'0') as i32;
    match code {
        200..=299 => Ctl::Ok,
        409 => Ctl::Busy,
        503 => Ctl::Full,
        _ => Ctl::Failed,
    }
}

/// The emergency script's command line, or None if action/args carry anything a shell would
/// read. `cap`: the C buffer size (the command must fit).
pub fn control_fallback_cmd(script: &str, action: &str, args: Option<&str>, cap: usize) -> Option<String> {
    let args = args?;
    if script.is_empty() || action.is_empty() {
        return None;
    }
    if !action.bytes().all(|c| c.is_ascii_lowercase() || c == b'.' || c == b'_') {
        return None;
    }
    let a = args.as_bytes();
    for (i, &c) in a.iter().enumerate() {
        let ok = c.is_ascii_alphanumeric()
            || b"_.,:=+-".contains(&c)
            || (c == b' ' && a.get(i + 1) != Some(&b' ') && i != 0);
        if !ok {
            return None;
        }
    }
    let s = format!("{script} --by screen {action}{}{args} >/dev/null 2>&1 &", if args.is_empty() { "" } else { " " });
    (s.len() < cap).then_some(s)
}

// ---- two-tap confirm ----
#[derive(Clone, Copy, Debug, Default)]
pub struct Arm {
    pub at: u32,
    pub id: i32,
}

impl Arm {
    pub fn live(&self, id: i32, now: u32, ms: u32) -> bool {
        self.at != 0 && (id < 0 || self.id == id) && now.wrapping_sub(self.at) < ms
    }
    /// First tap arms (false), the second within `ms` confirms (true).
    pub fn tap(&mut self, id: i32, now: u32, ms: u32) -> bool {
        if self.live(id, now, ms) {
            self.at = 0;
            return true;
        }
        self.at = if now != 0 { now } else { 1 };
        self.id = id;
        false
    }
    pub fn expire(&mut self, now: u32, ms: u32) -> bool {
        if self.at != 0 && now.wrapping_sub(self.at) >= ms {
            self.at = 0;
            return true;
        }
        false
    }
    pub fn clear(&mut self) {
        self.at = 0;
    }
}

// ---- 插线时的 USB 用法 ----
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsbPref {
    #[default]
    Ask,
    Share,
    Fast,
    Accessory,
}

const USB_PREF: [&str; 4] = ["ask", "share", "fast_charge", "accessory"];

pub fn usb_pref_name(p: UsbPref) -> &'static str {
    USB_PREF[p as usize]
}

/// The value up to the first blank or newline; None if it is not one of the names.
pub fn usb_pref_parse(s: &str) -> Option<UsbPref> {
    let w = s.split([' ', '\t', '\r', '\n']).next().unwrap_or("");
    let i = USB_PREF.iter().position(|&n| n == w)?;
    Some([UsbPref::Ask, UsbPref::Share, UsbPref::Fast, UsbPref::Accessory][i])
}

pub const USB_UNPLUG_MS: u32 = 2000;

#[derive(Clone, Copy, Debug, Default)]
pub struct UsbTrack {
    pub seen0: bool,
    pub attached: bool,
    pub off_since: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbEv {
    None,
    Plugged,
    Unplugged,
}

pub fn usb_track(t: &mut UsbTrack, cc: i32, phone: bool, now_ms: u32) -> UsbEv {
    if cc == 0 {
        if !t.attached {
            t.seen0 = true;
            return UsbEv::None;
        }
        if t.off_since == 0 {
            t.off_since = if now_ms != 0 { now_ms } else { 1 };
            return UsbEv::None;
        }
        if now_ms.wrapping_sub(t.off_since) < USB_UNPLUG_MS {
            return UsbEv::None;
        }
        t.attached = false;
        t.off_since = 0;
        t.seen0 = true;
        return UsbEv::Unplugged;
    }
    if cc != 1 {
        return UsbEv::None;
    }
    t.off_since = 0;
    if t.attached || !t.seen0 || !phone {
        return UsbEv::None;
    }
    t.attached = true;
    t.seen0 = false;
    UsbEv::Plugged
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn m(h: i32, mi: i32) -> i32 {
        h * 60 + mi
    }

    #[test]
    fn appearance_parse() {
        assert_eq!(appear_parse(Some("light")), (Appear::Light, true));
        assert_eq!(appear_parse(Some("dark")), (Appear::Dark, true));
        assert_eq!(appear_parse(Some("auto")), (Appear::Auto, true));
        assert_eq!(appear_parse(None), (Appear::Light, false));
        assert_eq!(appear_parse(Some("Dark")), (Appear::Light, false));
        assert_eq!(appear_parse(Some("")), (Appear::Light, false));
        assert!(appear_name(Appear::Auto) == "auto" && appear_name(Appear::Dark) == "dark");
    }

    #[test]
    fn hhmm() {
        assert_eq!(hhmm_parse(Some("19:00")), m(19, 0));
        assert_eq!(hhmm_parse(Some("7:05")), m(7, 5));
        assert_eq!(hhmm_parse(Some("00:00")), 0);
        assert_eq!(hhmm_parse(Some("23:59")), m(23, 59));
        for bad in ["24:00", "12:60", "123:00", "19:0", "19:00x"] {
            assert_eq!(hhmm_parse(Some(bad)), -1, "{bad}");
        }
        assert_eq!(hhmm_parse(None), -1);
    }

    #[test]
    fn dark_window() {
        let (f, t) = (m(19, 0), m(7, 0));
        use Appear::*;
        assert!(!is_dark(Light, m(23, 0), f, t));
        assert!(is_dark(Dark, m(12, 0), f, t));
        assert!(!is_dark(Auto, m(18, 59), f, t));
        assert!(is_dark(Auto, m(19, 0), f, t));
        assert!(is_dark(Auto, m(23, 59), f, t));
        assert!(is_dark(Auto, 0, f, t));
        assert!(is_dark(Auto, m(6, 59), f, t));
        assert!(!is_dark(Auto, m(7, 0), f, t));
        assert!(is_dark(Auto, m(14, 0), m(13, 0), m(15, 0)));
        assert!(!is_dark(Auto, m(16, 0), m(13, 0), m(15, 0)));
        assert!(!is_dark(Auto, m(20, 0), m(19, 0), m(19, 0)));
        assert!(is_dark(Auto, m(22, 0), -1, 5000));
        assert!(!is_dark(Auto, -5, f, t));
    }

    #[test]
    fn exec_guard() {
        let none = ExecHist::default();
        assert_eq!(exec_check(Some(&none), 1000), ExecVerdict::Allow);
        assert_eq!(exec_check(None, 1000), ExecVerdict::Allow);
        let h1 = exec_next(Some(&none), 1000);
        assert_eq!(h1, ExecHist { first_s: 1000, last_s: 1000, count: 1 });
        assert_eq!(exec_check(Some(&h1), 1001), ExecVerdict::Cooldown);
        assert_eq!(exec_check(Some(&h1), 1599), ExecVerdict::Cooldown);
        assert_eq!(exec_check(Some(&h1), 1600), ExecVerdict::Allow);
        let h2 = exec_next(Some(&h1), 1000 + 3600);
        assert!(h2.first_s == 4600 && h2.count == 1);
        let s3 = ExecHist { first_s: 1000, last_s: 2300, count: 3 };
        assert_eq!(exec_check(Some(&s3), 2400), ExecVerdict::Suspend);
        assert_eq!(exec_check(Some(&s3), 1000 + 3600), ExecVerdict::Allow);
        let w1 = exec_next(Some(&h1), 1600);
        let w2 = exec_next(Some(&w1), 2200);
        assert!(w1.count == 2 && w2.count == 3 && w2.first_s == 1000);
        assert_eq!(exec_check(Some(&w2), 2300), ExecVerdict::Suspend);
        let day = exec_next(Some(&ExecHist { first_s: 1000, last_s: 1000, count: 1 }), 1000 + 12 * 3600);
        assert_eq!(day.count, 1);
        let inw = exec_next(Some(&ExecHist { first_s: 1000, last_s: 1100, count: 2 }), 1200);
        assert!(inw.first_s == 1000 && inw.count == 3 && inw.last_s == 1200);
        assert_eq!(exec_check(Some(&h1), 500), ExecVerdict::Allow);
    }

    #[test]
    fn launch_args() {
        let l = launch_parse(&["luyaz-ui", "--tab=3", "--exec-first=1000", "--exec-last=1200", "--exec-n=2"]);
        assert!(l.tab == 3 && l.hist == ExecHist { first_s: 1000, last_s: 1200, count: 2 });
        assert_eq!(launch_parse(&["luyaz-ui"]), Launch::default());
        assert_eq!(launch_parse(&["x", "--tab=4"]).tab, 4);
        assert_eq!(launch_parse(&["x", "--tab=5"]).tab, -1);
        let g = launch_parse(&["x", "--tab=9", "--exec-n=abc", "--exec-first=-4", "--junk", "--bright=0", "--bright=999", "--scroll=-3"]);
        assert!(g.tab == -1 && g.hist == ExecHist::default() && g.bright == -1 && g.scroll_y == 0);
        assert_eq!(launch_parse(&["x", "--exec-n=2", "--exec-first=900", "--exec-last=100"]).hist, ExecHist::default());
        assert_eq!(launch_parse(&["x", "--exec-first=900", "--exec-last=950"]).hist, ExecHist::default());

        let src = Launch { tab: 2, scroll_y: 340, screen_off: 1, autooff_ms: 30000, bright: 232, hist: ExecHist { first_s: 1000, last_s: 1300, count: 2 } };
        let mut a = vec!["luyaz-ui".to_string()];
        a.extend(launch_argv(&src));
        assert_eq!(a.len(), 9);
        assert_eq!(launch_parse(&a), src);
        assert!(launch_argv(&Launch::default()).is_empty());
        let keyoff = Launch { tab: 1, screen_off: 2, autooff_ms: 30000, ..Launch::default() };
        let mut a = vec!["x".to_string()];
        a.extend(launch_argv(&keyoff));
        assert_eq!(launch_parse(&a).screen_off, 2);
        let off0 = Launch { tab: 0, autooff_ms: 0, ..Launch::default() };
        let mut a = vec!["x".to_string()];
        a.extend(launch_argv(&off0));
        let b = launch_parse(&a);
        assert!(b.autooff_ms == 0 && b.tab == 0);
    }

    #[test]
    fn clock() {
        assert!(!clock_sane(31536000));
        assert!(!clock_sane(1704067199));
        assert!(clock_sane(1704067200));
    }

    #[test]
    fn auto_appearance() {
        let ai = AutoIn {
            appear: Appear::Auto,
            cur_dark: false,
            wall_s: 1790000000,
            now_min: m(21, 0),
            from_min: m(19, 0),
            to_min: m(7, 0),
            screen_off: true,
            always_on: false,
            idle_ms: 0,
            busy: false,
            suspended: false,
            hist: ExecHist::default(),
            now_boot_s: 5000,
        };
        assert_eq!(auto_decide(&ai), Auto::Exec);
        assert_eq!(auto_decide(&AutoIn { cur_dark: true, ..ai }), Auto::Stay);
        assert_eq!(auto_decide(&AutoIn { appear: Appear::Light, ..ai }), Auto::Stay);
        assert_eq!(auto_decide(&AutoIn { wall_s: 40000000, ..ai }), Auto::Stay);
        assert_eq!(auto_decide(&AutoIn { screen_off: false, ..ai }), Auto::Wait);
        let a2 = AutoIn { screen_off: false, always_on: true, idle_ms: 59999, ..ai };
        assert_eq!(auto_decide(&a2), Auto::Wait);
        assert_eq!(auto_decide(&AutoIn { idle_ms: 60000, ..a2 }), Auto::Exec);
        assert_eq!(auto_decide(&AutoIn { busy: true, ..ai }), Auto::Wait);
        assert_eq!(auto_decide(&AutoIn { suspended: true, ..ai }), Auto::Stay);
        let a3 = AutoIn { hist: ExecHist { first_s: 4800, last_s: 4800, count: 1 }, ..ai };
        assert_eq!(auto_decide(&a3), Auto::Wait);
        assert_eq!(auto_decide(&AutoIn { now_boot_s: 5400, ..a3 }), Auto::Exec);
        assert_eq!(auto_decide(&AutoIn { hist: ExecHist { first_s: 4800, last_s: 4900, count: 3 }, ..ai }), Auto::Suspend);
        assert_eq!(auto_decide(&AutoIn { cur_dark: true, now_min: m(7, 0), ..ai }), Auto::Exec);
    }

    #[test]
    fn status_bar_rate() {
        let w = [96, 72, 40];
        assert_eq!(rate_pick(w, 120), 0);
        assert_eq!(rate_pick(w, 96), 0);
        assert_eq!(rate_pick(w, 80), 1);
        assert_eq!(rate_pick(w, 50), 2);
        assert_eq!(rate_pick(w, 10), 2);
    }

    #[test]
    fn battery() {
        assert_eq!(bat_state(78, false), BatState::Normal);
        assert_eq!(bat_state(21, false), BatState::Normal);
        assert_eq!(bat_state(20, false), BatState::Low);
        assert_eq!(bat_state(18, true), BatState::Charging);
        assert_eq!(bat_body_w(78, false), 27);
        assert_eq!(bat_body_w(100, false), 30);
        assert!(bat_body_w(62, true) == 27 && bat_body_w(100, true) == 30);
        assert_eq!(bat_red_w(18, 27), 4);
        assert!(bat_red_w(1, 27) == 3 && bat_red_w(0, 27) == 3);
        assert!(bat_red_w(20, 27) == 5 && bat_red_w(90, 27) == 5);
    }

    #[test]
    fn touch_while_dark() {
        let mut g = TGate::default();
        let mut t = 10000i64;
        assert_eq!(g.step(true, false, true, 100, 100, t), TG_FORWARD);
        t += 80;
        assert_eq!(g.step(true, false, false, 100, 100, t), 0);

        g = TGate::default();
        t += 1000;
        assert_eq!(g.step(false, false, true, 100, 100, t), 0);
        t += 80;
        g.step(false, false, false, 100, 100, t);
        t += 100;
        g.step(false, false, true, 100, 100, t);
        t += 80;
        assert_eq!(g.step(false, false, false, 100, 100, t), 0);

        g = TGate::default();
        t += 1000;
        assert_eq!(g.step(false, true, true, 100, 100, t), 0);
        t += 80;
        assert_eq!(g.step(false, true, false, 101, 100, t), 0);
        t += 2000;
        g.step(false, true, true, 100, 100, t);
        t += 80;
        assert_eq!(g.step(false, true, false, 100, 100, t), 0);
        t += 150;
        g.step(false, true, true, 110, 105, t);
        t += 90;
        assert_eq!(g.step(false, true, false, 110, 105, t), TG_WAKE);
        t += 50;
        assert_eq!(g.step(true, false, true, 200, 200, t), 0);
        t += 400;
        assert_eq!(g.step(true, false, true, 200, 200, t), 0);
        t += 10;
        g.step(true, false, false, 200, 200, t);
        t += 10;
        assert_eq!(g.step(true, false, true, 200, 200, t), TG_FORWARD);

        // second touch held 600 ms / dragged 80 px: not a tap, no wake
        for (hold, x3) in [(600, 100), (90, 180)] {
            g = TGate::default();
            t += 1000;
            g.step(false, true, true, 100, 100, t);
            t += 80;
            g.step(false, true, false, 100, 100, t);
            t += 150;
            g.step(false, true, true, 100, 100, t);
            t += hold;
            assert_eq!(g.step(false, true, false, x3, 100, t), 0, "{hold} {x3}");
        }

        g = TGate::default();
        t += 1000;
        g.step(false, true, true, 20, 20, t);
        t += 80;
        g.step(false, true, false, 20, 20, t);
        t += 150;
        g.step(false, true, true, 250, 400, t);
        t += 80;
        assert_eq!(g.step(false, true, false, 250, 400, t), 0, "pocket");

        g = TGate::default();
        t += 1000;
        g.woke(t);
        assert_eq!(g.step(true, false, true, 50, 50, t), 0);
        t += 500;
        g.step(true, false, false, 50, 50, t);
        t += 10;
        assert_eq!(g.step(true, false, true, 50, 50, t), TG_FORWARD);
    }

    #[test]
    fn radio_names() {
        assert_eq!(rat("ENDC"), Rat::G5Nsa);
        assert_eq!(rat("LTE-A"), Rat::G4);
        assert_eq!(rat("WCDMA"), Rat::G3);
        assert_eq!(rat("GSM"), Rat::G2);
        assert_eq!(rat("UNREGISTERED"), Rat::None);
        assert_eq!(rat("LIMITED_SERVICE_SA"), Rat::None);
        assert_eq!(rat("NR5G_SA"), Rat::G5Sa);
        assert_eq!(band_short("LTE BAND 3", false), "B3");
        assert_eq!(band_short("NR5G BAND 78", true), "n78");
        assert_eq!(band_short("n78", true), "n78");
        assert_eq!(band_short("B41", false), "B41");
        assert_eq!(band_short("", false), "-");
        assert_eq!(band_short("DCS", false), "DCS");
        assert_eq!(band_short("GSM 900", false), "GSM 900");
        assert_eq!(band_short("n261", true), "n261");
    }

    #[test]
    fn screen_off_snap() {
        assert_eq!(autooff_snap(0), 0);
        assert_eq!(autooff_snap(30000), 30000);
        assert_eq!(autooff_snap(120000), 120000);
        assert_eq!(autooff_snap(60000), 120000);
        assert_eq!(autooff_snap(10000), 30000);
        assert_eq!(autooff_snap(-5), 120000);
        assert_eq!(autooff_snap(3600000), 120000);
    }

    #[test]
    fn headline_hold() {
        let mut h = NetHold::default();
        assert!(net_hold(&mut h, 1, 1000));
        assert!(net_hold(&mut h, 1, 2000));
        assert!(!net_hold(&mut h, 2, 3000) && !net_hold(&mut h, 2, 17000));
        assert!(net_hold(&mut h, 2, 18000));
        assert!(!net_hold(&mut h, 3, 19000) && net_hold(&mut h, 2, 20000));
        assert!(!net_hold(&mut h, 3, 21000) && !net_hold(&mut h, 3, 35000) && net_hold(&mut h, 3, 36000));
    }

    #[test]
    fn which_card() {
        assert!(iccid_same("8986000000000000012F", "8986000000000000012"));
        assert!(iccid_same("8986000000000000a12f", "8986000000000000A12"));
        assert!(!iccid_same("8986000000000000012", "8986000000000000013"));
        assert!(!iccid_same("", "") && !iccid_same("FFFF", ""));
        assert_eq!(sim_kind("", "", ""), SimKind::None);
        assert_eq!(sim_kind("sim absent", "", "8986"), SimKind::None);
        assert_eq!(sim_kind("sim ready", "8986000000000000012F", ""), SimKind::Plain);
        assert_eq!(sim_kind("sim ready", "8986000000000000012F", "8944000000000000003"), SimKind::Plain);
        assert_eq!(sim_kind("sim ready", "8986000000000000012F", "8986000000000000012"), SimKind::Esim);
        assert_eq!(sim_kind("", "8986000000000000012", ""), SimKind::Plain);
        // no ICCID from the modem, a profile enabled: the eSIM
        assert_eq!(sim_kind("sim ready", "", "8985300000000000022"), SimKind::Esim);
        assert_eq!(sim_kind("sim ready", "", ""), SimKind::Plain);
    }

    #[test]
    fn dhcp_pool() {
        assert_eq!(dhcp_pool_text("192.168.0.1", "100", "50"), "192.168.0.100 - 192.168.0.149");
        assert_eq!(dhcp_pool_text("192.168.0.1", "2", "252"), "192.168.0.2 - 192.168.0.253");
        assert_eq!(dhcp_pool_text("192.168.0.1", "2", "400"), "192.168.0.2 - 192.168.0.254");
        assert_eq!(dhcp_pool_text("192.168.0.1", "192.168.0.10", "5"), "192.168.0.10 - 192.168.0.14");
        assert_eq!(dhcp_pool_text("192.168.0.1", "100", ""), "192.168.0.100");
        assert_eq!(dhcp_pool_text("192.168.0.1", "", "50"), "");
        assert_eq!(dhcp_pool_text("nodot", "100", "50"), "");
        assert_eq!(dhcp_pool_text("192.168.0.1", "x", "50"), "");
    }

    #[test]
    fn control() {
        assert_eq!(control_reply(b"HTTP/1.1 200 OK\r\n"), Ctl::Ok);
        assert_eq!(control_reply(b"HTTP/1.1 409 Conflict"), Ctl::Busy);
        assert_eq!(control_reply(b"HTTP/1.1 503 Service Unavailable"), Ctl::Full);
        assert_eq!(control_reply(b"HTTP/1.0 503 x"), Ctl::Full);
        assert_eq!(control_reply(b"HTTP/1.1 400 Bad Request"), Ctl::Failed);
        assert_eq!(control_reply(b"HTTP/1.1 502 Bad Gateway"), Ctl::Failed);
        assert_eq!(control_reply(b""), Ctl::NoReply);
        assert_eq!(control_reply(b"xx 503"), Ctl::NoReply);
        assert_eq!(control_reply(b"HTTP/1.1 50"), Ctl::NoReply);
        let sc = "/data/luyaz/scripts/luyaz-fallback.sh";
        assert_eq!(
            control_fallback_cmd(sc, "network.set_mode", Some("mode=WL_AND_5G"), 256).as_deref(),
            Some("/data/luyaz/scripts/luyaz-fallback.sh --by screen network.set_mode mode=WL_AND_5G >/dev/null 2>&1 &")
        );
        assert_eq!(
            control_fallback_cmd(sc, "band.reset", Some(""), 256).as_deref(),
            Some("/data/luyaz/scripts/luyaz-fallback.sh --by screen band.reset >/dev/null 2>&1 &")
        );
        assert!(control_fallback_cmd(sc, "wifi.radio", Some("ap_2g=1 ap_5g=0"), 256).is_some());
        assert!(control_fallback_cmd(sc, "band.set_lte", Some("bands=1,3,28"), 256).is_some());
        assert!(control_fallback_cmd(sc, "network.set_mode", Some("mode='x'"), 256).is_none());
        assert!(control_fallback_cmd(sc, "band.reset", Some("a=1;reboot"), 256).is_none());
        assert!(control_fallback_cmd(sc, "band.reset", Some("a=$(x)"), 256).is_none());
        assert!(control_fallback_cmd(sc, "band.reset;x", Some(""), 256).is_none());
        assert!(control_fallback_cmd(sc, "wifi.radio", Some("a=1  b=2"), 256).is_none());
        assert!(control_fallback_cmd(sc, "band.reset", None, 256).is_none());
        assert!(control_fallback_cmd(sc, "band.reset", Some(""), 20).is_none());
    }

    #[test]
    fn usb() {
        assert_eq!(usb_pref_parse("share\n"), Some(UsbPref::Share));
        assert_eq!(usb_pref_parse("fast_charge"), Some(UsbPref::Fast));
        assert_eq!(usb_pref_parse("shares"), None);
        assert_eq!(usb_pref_parse(usb_pref_name(UsbPref::Accessory)), Some(UsbPref::Accessory));
        assert_eq!(usb_pref_name(UsbPref::Ask), "ask");

        let mut t = UsbTrack::default();
        assert_eq!(usb_track(&mut t, 1, true, 100), UsbEv::None);
        assert!(usb_track(&mut t, -1, false, 200) == UsbEv::None && !t.seen0);
        let mut u = UsbTrack::default();
        assert!(usb_track(&mut u, 0, false, 100) == UsbEv::None && u.seen0);
        assert!(usb_track(&mut u, 1, false, 200) == UsbEv::None && u.seen0);
        assert_eq!(usb_track(&mut u, 1, true, 300), UsbEv::Plugged);
        assert_eq!(usb_track(&mut u, 1, false, 400), UsbEv::None);
        assert!(usb_track(&mut u, 0, false, 1000) == UsbEv::None && usb_track(&mut u, 0, false, 2999) == UsbEv::None);
        assert_eq!(usb_track(&mut u, 1, true, 3000), UsbEv::None);
        usb_track(&mut u, 0, false, 5000);
        assert!(usb_track(&mut u, 0, false, 7000) == UsbEv::Unplugged && !u.attached);
        assert_eq!(usb_track(&mut u, 1, true, 8000), UsbEv::Plugged);
    }

    #[test]
    fn two_tap_confirm() {
        let mut a = Arm::default();
        assert!(!a.tap(1, 1000, 5000) && a.live(1, 1000, 5000));
        assert!(a.tap(1, 5999, 5000) && !a.live(-1, 5999, 5000));
        a.tap(1, 1000, 5000);
        assert!(!a.tap(1, 6000, 5000) && a.at == 6000);
        assert!(!a.tap(2, 6001, 5000) && a.id == 2);
        assert!(a.live(-1, 6002, 5000) && !a.live(1, 6002, 5000));
        assert!(!a.tap(3, 0, 4000) && a.at == 1);
        assert!(!a.expire(4000, 4000) && a.at != 0);
        assert!(a.expire(4001, 4000) && !a.expire(4002, 4000));
        a.tap(3, 10, 4000);
        a.clear();
        assert!(!a.live(-1, 11, 4000));
        a = Arm { at: 0xffff_fff0, id: 4 };
        assert!(a.live(4, 0x10, 5000));
    }
}
