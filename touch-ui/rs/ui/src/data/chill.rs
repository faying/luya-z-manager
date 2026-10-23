//! What the CHILL panel shows, read straight from mihomo's clash API (127.0.0.1:9999 unless
//! chill.conf says otherwise): the main group's choice, the mode from /configs (no more than
//! every 30 s), the group list, /connections, the open group's nodes and the chain behind its
//! choice. Picks go out without waiting for the answer; the group delay test keeps its socket
//! and reads a bit each round. The profile goes through luyaz-core, and so does 家里出口
//! (E3: GET/PUT /api/services/chill/home).

use super::core_api::CoreApi;
use super::http::{self, read_into};
use super::util::{bytes_cap, find, find_byte, jget, jstr, mono_ms, text, trunc};
use crate::cf;
use crate::json;
use crate::lang::{self, tr};
use ui_model::{chill_group_info_t, chill_home_t, chill_node_info_t, chill_traffic_item_t};
use std::fmt::Write as _;
use std::os::fd::{AsRawFd, OwnedFd};

/// beside the program (Luya-Z D5)
const SC_CONF: &str = "/data/luyaz/ui/chill.conf";
const SC_IO_MS: i32 = 1500;
const SC_TTL_MS: i64 = 5000; // was 2000; reduced poll rate (P2 觸屏降頻 2026-10-19)
/// /configs is about 170 KB (mostly the TUN route exclusions) for the one field we want
const SC_CFG_TTL_MS: i64 = 30000;
/// one hand-picked group had 168 nodes on 2026-09-22
const SC_MAX_NODE: usize = 200;
const SC_NAME_MAX: usize = 96;
const SC_RESP_MAX: usize = 262144;
const SC_MAIN_GROUP: &str = "🚀 节点选择";
/// rows on the traffic card
const SC_TOP_SHOW: usize = 6;
const SC_MAX_STAT_PAIR: usize = 200;
const SC_MAX_TRACK: usize = 256;
const SC_MAX_GRP: usize = 32;
const SC_MANUAL_GROUP: &str = "🎯 手动节点";
/// a delay test still unanswered after this is dropped
const SC_DELAY_MAX_MS: i64 = 8000;
/// the delay answer is read into a buffer this big; full counts as done
const SC_DELAY_BUF: usize = 16384;
/// chains longer than this stop following
const SC_CHAIN_STOP: usize = 160 - 32;
const CHILL_STATE_FILE: &str = "/tmp/chill.state";

pub const CH_MEDIA: usize = 0;
pub const CH_IP: usize = 1;
pub const CH_REST: usize = 2;
const CH_KEYS: [&str; 3] = ["media", "ip", "rest"];

/// One (rule group, node) pair's bytes since this process started.
#[derive(Clone)]
struct Pair {
    grp_raw: String,
    node_raw: String,
    up: i64,
    down: i64,
}

/// A connection's last counters, to take the difference each round.
#[derive(Clone)]
struct Track {
    id: String,
    up: i64,
    down: i64,
    seen: bool,
}

pub struct Chill {
    host: String,
    port: u16,
    secret: String,
    group: String,
    conf_loaded: bool,

    online: bool,
    mode_raw: String,
    profile: String,
    profile_eff: String,
    thermal_eco: bool,
    profile_pending_ms: i64,
    node: String,
    /// 「🚀 节点选择」's pick as is: the exit state looks at whether it is DIRECT
    main_now: String,
    traffic: String,
    speed: String,
    chain: String,
    chain_raw: Vec<String>,
    prev_dl: i64,
    prev_ul: i64,
    prev_t: i64,
    conns: String,
    core_running: bool,

    /// shown (emoji gone) / as is (for the API) / delay (-1 = not measured)
    nodes: Vec<String>,
    nodes_raw: Vec<String>,
    node_delay: Vec<i32>,
    /// url-test / fallback groups only: the hand-picked node ("" = automatic). mihomo uses it
    /// while it is alive and the fastest one otherwise, and goes back once it is alive again.
    fixed: Option<String>,
    /// The same for the group 🚀 points at (sanitized), read while following the chain.
    exit_pin: Option<String>,

    stat_pair: Vec<Pair>,
    track: Vec<Track>,
    top_pair: Vec<chill_traffic_item_t>,

    grp_raw: Vec<String>,
    grp_disp: Vec<String>,
    grp_type: Vec<String>,
    grp_idx: i32,
    conn_direct: i32,
    conn_proxy: i32,
    last_ms: i64,
    cfg_ms: i64,
    cfg_again: i32,
    last_truncated: bool,

    delay_fd: Option<OwnedFd>,
    delay_t0: i64,
    delay_buf: Vec<u8>,

    home: chill_home_t,
    home_ms: i64,
}
impl super::background::ReadCopy for Chill {
    fn read_copy(&self) -> Self {
        Self {
            host: self.host.clone(),
            port: self.port,
            secret: self.secret.clone(),
            group: self.group.clone(),
            conf_loaded: self.conf_loaded,
            online: self.online,
            mode_raw: self.mode_raw.clone(),
            profile: self.profile.clone(),
            profile_eff: self.profile_eff.clone(),
            thermal_eco: self.thermal_eco,
            profile_pending_ms: self.profile_pending_ms,
            node: self.node.clone(),
            main_now: self.main_now.clone(),
            traffic: self.traffic.clone(),
            speed: self.speed.clone(),
            chain: self.chain.clone(),
            chain_raw: self.chain_raw.clone(),
            prev_dl: self.prev_dl,
            prev_ul: self.prev_ul,
            prev_t: self.prev_t,
            conns: self.conns.clone(),
            core_running: self.core_running,
            nodes: self.nodes.clone(),
            nodes_raw: self.nodes_raw.clone(),
            node_delay: self.node_delay.clone(),
            fixed: self.fixed.clone(),
            exit_pin: self.exit_pin.clone(),
            stat_pair: self.stat_pair.clone(),
            track: self.track.clone(),
            top_pair: self.top_pair.clone(),
            grp_raw: self.grp_raw.clone(),
            grp_disp: self.grp_disp.clone(),
            grp_type: self.grp_type.clone(),
            grp_idx: self.grp_idx,
            conn_direct: self.conn_direct,
            conn_proxy: self.conn_proxy,
            last_ms: self.last_ms,
            cfg_ms: self.cfg_ms,
            cfg_again: self.cfg_again,
            last_truncated: self.last_truncated,
            delay_fd: None,
            delay_t0: 0,
            delay_buf: Vec::new(),
            home: self.home.clone(),
            home_ms: self.home_ms,
        }
    }
    fn apply(&mut self, mut next: Self) {
        next.delay_fd = self.delay_fd.take();
        next.delay_buf = std::mem::take(&mut self.delay_buf);
        next.delay_t0 = self.delay_t0;
        *self = next;
    }
}


/// The first row of an auto (url-test / fallback) group's node list.
fn auto_row_label(fixed: &str, now: &str, en: bool) -> String {
    let now = sanitize_name(now, SC_NAME_MAX);
    let s = match (fixed.is_empty(), en) {
        (true, false) => format!("自动 · 在用 {now}"),
        (true, true) => format!("Auto · on {now}"),
        (false, _) if now == sanitize_name(fixed, SC_NAME_MAX) => (if en { "Auto" } else { "自动" }).to_string(),
        (false, false) => format!("自动 · 首选不通，暂用 {now}"),
        (false, true) => format!("Auto · pick down, on {now}"),
    };
    trunc(&s, SC_NAME_MAX)
}

/// Chip order: the main group and AI first (byte order would put 🚀/🤖 last), the per-region
/// auto groups and GLOBAL last, the rest by name.
fn group_rank(raw: &str) -> u8 {
    if raw.ends_with("节点选择") {
        0
    } else if raw.ends_with(" AI") || raw == "AI" {
        1
    } else if raw.ends_with("·自动") {
        3
    } else if raw == "GLOBAL" {
        4
    } else {
        2
    }
}

impl Default for Chill {
    fn default() -> Self {
        Chill {
            host: "127.0.0.1".into(),
            port: 9999,
            secret: String::new(),
            group: SC_MAIN_GROUP.into(),
            conf_loaded: false,
            online: false,
            mode_raw: String::new(),
            profile: "standard".into(),
            profile_eff: "standard".into(),
            thermal_eco: false,
            profile_pending_ms: 0,
            node: String::new(),
            main_now: String::new(),
            traffic: String::new(),
            speed: String::new(),
            chain: String::new(),
            chain_raw: Vec::new(),
            prev_dl: 0,
            prev_ul: 0,
            prev_t: 0,
            conns: String::new(),
            core_running: false,
            nodes: Vec::new(),
            nodes_raw: Vec::new(),
            node_delay: Vec::new(),
            fixed: None,
            exit_pin: None,
            stat_pair: Vec::new(),
            track: Vec::new(),
            top_pair: Vec::new(),
            grp_raw: Vec::new(),
            grp_disp: Vec::new(),
            grp_type: Vec::new(),
            grp_idx: -1,
            conn_direct: 0,
            conn_proxy: 0,
            last_ms: 0,
            cfg_ms: 0,
            cfg_again: 0,
            last_truncated: false,
            delay_fd: None,
            delay_t0: 0,
            delay_buf: Vec::new(),
            home: chill_home_t { in_china: -1, roaming: -1, home_ok: -1, ms: -1, nitem: -1, on: vec![0; 3], ..Default::default() },
            home_ms: 0,
        }
    }
}

/// The code point that starts `p` and its length in bytes. A NUL gives length 0; a byte that does
/// not start a well-formed sequence stands for itself, one byte long.
fn decode(p: &[u8]) -> (u32, usize) {
    let lead = p.first().copied().unwrap_or(0);
    let len = match lead {
        0 => return (0, 0),
        0x01..=0x7F => return (lead as u32, 1),
        _ if lead >> 5 == 0b110 => 2,
        _ if lead >> 4 == 0b1110 => 3,
        _ if lead >> 3 == 0b11110 => 4,
        _ => return (lead as u32, 1),
    };
    match p.get(1..len).filter(|rest| rest.iter().all(|&c| c >> 6 == 0b10)) {
        Some(rest) => (rest.iter().fold(lead as u32 & (0x7F >> len), |cp, &c| cp << 6 | (c & 0x3F) as u32), len),
        None => (lead as u32, 1),
    }
}

/// Pictographs, symbols, variation selectors and the joiner: nothing the device fonts can draw.
fn is_emoji(cp: u32) -> bool {
    const RANGES: [(u32, u32); 4] = [(0x1F000, 0x1FAFF), (0x2600, 0x27BF), (0x2B00, 0x2BFF), (0xFE00, 0xFE0F)];
    cp == 0x200D || RANGES.iter().any(|&(lo, hi)| (lo..=hi).contains(&cp))
}

/// A node or group name the device fonts can show. Flags turn into their two letters
/// (🇯🇵 → JP), other emoji are dropped and blanks collapse to one; when the name already starts
/// with the flag's letters ("🇯🇵 JP2-…") the extra pair goes. An empty result is "-".
/// The output stays below cap - 8 bytes.
pub fn sanitize_name(input: &str, cap: usize) -> String {
    let src = input.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut at = 0;
    // leading blanks go too
    let mut after_blank = true;
    while out.len() + 8 < cap {
        let (cp, len) = decode(&src[at..]);
        if len == 0 {
            break;
        }
        match cp {
            0x1F1E6..=0x1F1FF => {
                out.push(b'A' + (cp - 0x1F1E6) as u8);
                after_blank = false;
            }
            _ if is_emoji(cp) => {}
            0x20 => {
                if !after_blank {
                    out.push(b' ');
                }
                after_blank = true;
            }
            _ => {
                out.extend_from_slice(&src[at..at + len]);
                after_blank = false;
            }
        }
        at += len;
    }
    while out.ends_with(b" ") {
        out.pop();
    }
    let doubled = out.len() > 4 && out[2] == b' ' && out[..2].iter().all(u8::is_ascii_uppercase) && out[3..5].eq_ignore_ascii_case(&out[..2]);
    if doubled {
        out.drain(..3);
    }
    if out.is_empty() { trunc("-", cap) } else { text(&out) }
}

/// A name as a path segment: unreserved bytes as they are, the rest %XX; it stops before the
/// encoded text would reach cap - 4 bytes.
fn path_escape(name: &str, cap: usize) -> String {
    let mut out = String::new();
    for &c in name.as_bytes() {
        if out.len() + 4 >= cap {
            break;
        }
        if c.is_ascii_alphanumeric() || b"-_.~".contains(&c) {
            out.push(c as char);
        } else {
            let _ = write!(out, "%{c:02X}");
        }
    }
    out
}

/// Bytes for the traffic lines: 1.5G, 300M, 12K, 800B.
fn size(n: i64) -> String {
    const K: i64 = 1 << 10;
    const M: i64 = 1 << 20;
    const G: i64 = 1 << 30;
    match n {
        _ if n >= G => text(&cf!("%.1fG", n as f64 / G as f64)),
        _ if n >= M => format!("{}M", n / M),
        _ if n >= K => format!("{}K", n / K),
        _ => format!("{n}B"),
    }
}

/// The first (last = false) or last quoted string of an array text (chains_pick).
fn chains_pick(arr: &[u8], last: bool, cap: usize) -> String {
    let mut out = String::new();
    let mut q = 0;
    while let Some(a) = find_byte(arr, b'"', q) {
        let Some(e) = find_byte(arr, b'"', a + 1) else { break };
        let l = (e - a - 1).min(cap - 1);
        out = text(&arr[a + 1..a + 1 + l]);
        if !last {
            return out;
        }
        q = e + 1;
    }
    out
}

/// Nothing read yet shows as "-".
fn or_dash(s: &str) -> &str {
    if s.is_empty() { "-" } else { s }
}

fn valid_profile(p: &str) -> bool {
    matches!(p, "eco" | "standard" | "perf")
}

/// Every "…" in `s`, from one quote to the next (names from mihomo carry no escaped quotes).
fn plain_strings(s: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut from = 0;
    std::iter::from_fn(move || {
        let open = find_byte(s, b'"', from)?;
        let close = find_byte(s, b'"', open + 1)?;
        from = close + 1;
        Some(&s[open + 1..close])
    })
}

/// An auto group's pin: Some("") when there is a "fixed" key but no usable value, None when the
/// group has none (only url-test and fallback groups do).
fn pin_of(group: &[u8]) -> Option<String> {
    find(group, b"\"fixed\":\"", 0)?;
    Some(json::get(group, "fixed", SC_NAME_MAX).map(|v| text(&v)).unwrap_or_default())
}

/// A name as mihomo spelled it, at most SC_NAME_MAX - 1 bytes of it.
fn raw_name(b: &[u8]) -> String {
    text(&b[..b.len().min(SC_NAME_MAX - 1)])
}

fn jbool(o: &[u8], key: &str) -> i32 {
    match json::get(o, key, 8).as_deref() {
        Some(b"true") => 1,
        Some(b"false") => 0,
        _ => -1,
    }
}

fn home_item(s: &str) -> i32 {
    CH_KEYS.iter().position(|&k| k == s).map_or(-1, |i| i as i32)
}

/// GET /api/services/chill/home's "data" (chill_home_parse).
pub fn home_parse(data: &[u8]) -> chill_home_t {
    let mut h = chill_home_t { known: 1, in_china: -1, roaming: -1, home_ok: -1, ms: -1, nitem: -1, on: vec![0; 3], ..Default::default() };
    h.configured = (jbool(data, "configured") == 1) as i32;
    h.updating = (jbool(data, "updating") == 1) as i32;
    if h.configured == 0 || h.updating != 0 {
        return h;
    }
    if let Some(o) = json::get(data, "where", 512) {
        h.in_china = jbool(&o, "in_china");
        h.roaming = jbool(&o, "true_roaming");
    }
    h.blocked = jstr(data, "blocked", 16);
    if let Some(items) = json::get(data, "items", 1024) {
        for (i, k) in CH_KEYS.iter().enumerate() {
            if let Some(it) = json::get(&items, k, 256) {
                h.on[i] = (jbool(&it, "on") == 1) as i32;
                if i == CH_IP {
                    h.sites = json::get_int(&it, "sites", 0) as i32;
                }
                if i == CH_REST {
                    h.until = json::get_int(&it, "until", 0);
                    h.can_extend = (jbool(&it, "can_extend") == 1) as i32;
                }
            }
        }
    }
    if let Some(o) = json::get(data, "home", 512) {
        h.home_ok = jbool(&o, "ok");
        h.ms = match json::get(&o, "ms", 64) {
            Some(v) if v != b"null" => json::atol(&v) as i32,
            _ => -1,
        };
    }
    let v = jstr(data, "fallback_to", 64);
    if !v.is_empty() {
        h.fallback_to = sanitize_name(&v, 64);
    }
    h.ts = jstr(data, "tailscale", 12);
    if let Some(o) = json::get(data, "busy", 512)
        && o.first() == Some(&b'{')
    {
        h.busy = 1;
        h.busy_item = home_item(&jstr(&o, "item", 64));
        h.busy_on = (jbool(&o, "on") == 1) as i32;
        h.busy_phase = jstr(&o, "phase", 12);
        h.busy_until = json::get_int(&o, "until", 0);
    }
    if let Some(o) = json::get(data, "notice", 512)
        && o.first() == Some(&b'{')
    {
        h.nkind = jstr(&o, "kind", 16);
        h.nitem = home_item(&jstr(&o, "item", 64));
        let zh = jstr(&o, "text", 200);
        let en = jstr(&o, "text_en", 200);
        h.ntext = trunc(if lang::is_en() && !en.is_empty() { &en } else { &zh }, 200);
        h.ncode = jstr(&o, "code", 24);
        h.nat = json::get_int(&o, "at", 0);
    }
    h
}

impl Chill {
    /// chill.conf (key=value lines), once: where mihomo listens, its secret, the group to open on.
    fn load_conf(&mut self) {
        if std::mem::replace(&mut self.conf_loaded, true) {
            return;
        }
        let Ok(raw) = std::fs::read(SC_CONF) else { return };
        for line in raw.split(|&c| c == b'\n') {
            let line = text(line);
            let Some((key, value)) = line.split('\r').next().unwrap_or_default().split_once('=') else { continue };
            match key {
                "host" => self.host = trunc(value, 64),
                "port" => self.port = json::atol(value.as_bytes()) as u16,
                "secret" => self.secret = trunc(value, 128),
                "group" => self.group = trunc(value, SC_NAME_MAX),
                _ => {}
            }
        }
    }


    /// A whole HTTP/1.1 request for mihomo, one connection each; at most 1023 bytes.
    fn request(&self, method: &str, path: &str, body: Option<&str>) -> String {
        let mut r = format!("{method} {path} HTTP/1.1\r\nHost: {}:{}\r\n", self.host, self.port);
        if !self.secret.is_empty() {
            let _ = write!(r, "Authorization: Bearer {}\r\n", self.secret);
        }
        if let Some(b) = body {
            let _ = write!(r, "Content-Type: application/json\r\nContent-Length: {}\r\n", b.len());
        }
        r += "Connection: close\r\n\r\n";
        r += body.unwrap_or_default();
        trunc(&r, 1024)
    }

    fn dial(&self) -> Option<OwnedFd> {
        http::connect_tcp(&self.host, self.port, SC_IO_MS)
    }

    /// Send and forget, for the picks: a touch handler cannot sit 1.5 s on an answer, and the
    /// next read shows what mihomo made of it.
    fn fire(&self, method: &str, path: &str, body: &str) -> bool {
        self.dial().is_some_and(|conn| http::send(conn.as_raw_fd(), self.request(method, path, Some(body)).as_bytes()))
    }

    /// GET from mihomo: the body, chunking undone, or None when nothing usable came back.
    /// last_truncated says whether it was cut at SC_RESP_MAX.
    fn get(&mut self, path: &str) -> Option<Vec<u8>> {
        self.last_truncated = false;
        let conn = self.dial()?;
        let resp = http::exchange(conn, self.request("GET", path, None).as_bytes(), SC_RESP_MAX, SC_IO_MS);
        self.last_truncated = resp.truncated;
        resp.body.filter(|_| resp.status != 0)
    }

    fn pair_add(&mut self, grp: &str, node: &str, up: i64, down: i64) {
        if grp.is_empty() || node.is_empty() {
            return;
        }
        if let Some(p) = self.stat_pair.iter_mut().find(|p| p.grp_raw == grp && p.node_raw == node) {
            p.up += up;
            p.down += down;
            return;
        }
        if self.stat_pair.len() >= SC_MAX_STAT_PAIR {
            return; // the long tail goes; the top N does not change
        }
        self.stat_pair.push(Pair { grp_raw: trunc(grp, SC_NAME_MAX), node_raw: trunc(node, SC_NAME_MAX), up, down });
    }

    /// This round's bytes of a connection (all of them for a new one).
    fn track_update(&mut self, id: &str, up: i64, down: i64) -> (i64, i64) {
        if let Some(t) = self.track.iter_mut().find(|t| t.id == id) {
            let (u, d) = (up - t.up, down - t.down);
            t.up = up;
            t.down = down;
            t.seen = true;
            return (u.max(0), d.max(0));
        }
        if self.track.len() < SC_MAX_TRACK {
            self.track.push(Track { id: trunc(id, 40), up, down, seen: true });
        }
        (up, down)
    }

    fn track_prune(&mut self) {
        self.track.retain(|t| t.seen);
        for t in &mut self.track {
            t.seen = false;
        }
    }

    fn pair_finish(&mut self) {
        self.stat_pair.sort_by_key(|p| std::cmp::Reverse(p.up + p.down));
        self.top_pair = self
            .stat_pair
            .iter()
            .take(SC_TOP_SHOW)
            .map(|p| chill_traffic_item_t {
                name: trunc(&format!("{} → {}", sanitize_name(&p.grp_raw, SC_NAME_MAX), sanitize_name(&p.node_raw, SC_NAME_MAX)), 64),
                traffic: trunc(&format!("↓{} ↑{}", size(p.down), size(p.up)), 40),
                bytes: p.up + p.down,
            })
            .collect();
    }

    /// The open group's "all" list. A node keeps the delay it was measured at, found by name.
    fn parse_all_array(&mut self, arr: &[u8]) {
        let before: Vec<(String, i32)> = self.nodes_raw.drain(..).zip(self.node_delay.drain(..)).collect();
        self.nodes.clear();
        for raw in plain_strings(arr).take(SC_MAX_NODE).map(|b| trunc(&raw_name(b), SC_NAME_MAX)) {
            let delay = before.iter().find(|(name, _)| *name == raw).map_or(-1, |&(_, ms)| ms);
            self.nodes.push(sanitize_name(&raw, SC_NAME_MAX));
            self.node_delay.push(delay);
            self.nodes_raw.push(raw);
        }
    }

    /// /group: each group's "name" with the "type" that follows it. mihomo lists them in a new
    /// order every time, so they are sorted here, and the open group is looked up again by name
    /// (the first one when it is gone).
    fn parse_groups(&mut self, j: &[u8]) {
        const NAME: &[u8] = b"\"name\":\"";
        const TYPE: &[u8] = b"\"type\":\"";
        let mut found: Vec<(String, String)> = Vec::new();
        let mut from = 0;
        while found.len() < SC_MAX_GRP {
            let Some(start) = find(j, NAME, from).map(|at| at + NAME.len()) else { break };
            let Some(end) = find_byte(j, b'"', start) else { break };
            let kind = find(j, TYPE, end)
                .map(|at| at + TYPE.len())
                .and_then(|t| find_byte(j, b'"', t).filter(|&e| e - t < 16).map(|e| text(&j[t..e])))
                .unwrap_or_default();
            found.push((raw_name(&j[start..end]), kind));
            from = end + 1;
        }
        found.sort_by(|a, b| (group_rank(&a.0), a.0.as_bytes()).cmp(&(group_rank(&b.0), b.0.as_bytes())));
        self.grp_disp = found.iter().map(|(raw, _)| sanitize_name(raw, SC_NAME_MAX)).collect();
        (self.grp_raw, self.grp_type) = found.into_iter().unzip();
        match self.grp_raw.iter().position(|r| *r == self.group) {
            Some(i) => self.grp_idx = i as i32,
            None if !self.grp_raw.is_empty() => {
                self.grp_idx = 0;
                self.group = self.grp_raw[0].clone();
            }
            None => self.grp_idx = -1,
        }
    }

    /// The profile chill.sh writes in /tmp/chill.state (a few hundred bytes, read directly).
    fn read_profile_state(&mut self, t: i64) {
        if self.profile_pending_ms != 0 && t - self.profile_pending_ms < 15000 {
            return;
        }
        self.profile_pending_ms = 0;
        let Ok(mut b) = std::fs::read(CHILL_STATE_FILE) else { return };
        b.truncate(1023);
        let v = jget(&b, "profile", 16);
        self.profile = if valid_profile(&v) { v } else { "standard".into() };
        let v = jget(&b, "profile_effective", 16);
        self.profile_eff = if valid_profile(&v) { v } else { self.profile.clone() };
        self.thermal_eco = find(&b, b"\"thermal_eco\":true", 0).is_some();
    }

    /// Read mihomo if `active` and the last read is older than SC_TTL_MS. true when a read ran.
    pub fn poll(&mut self, active: bool) -> bool {
        let now = mono_ms();
        self.load_conf();
        // the delay test is read every round, cache or not
        self.delay_poll();
        if !active {
            self.last_ms = 0;
            self.cfg_ms = 0;
            return false;
        }
        if self.last_ms != 0 && now - self.last_ms < SC_TTL_MS {
            return false;
        }
        self.last_ms = now;
        self.read_profile_state(now);
        // is mihomo up at all: the main group is a small answer (a 404 still means it runs)
        let Some(main) = self.get(&format!("/proxies/{}", path_escape(SC_MAIN_GROUP, 128))) else {
            self.online = false;
            self.core_running = false;
            // a restart may have changed the mode: read it again next time
            self.cfg_ms = 0;
            return true;
        };
        self.online = true;
        self.core_running = true;
        self.main_now = jget(&main, "now", SC_NAME_MAX);
        self.read_mode(now);
        if let Some(b) = self.get("/group") {
            self.parse_groups(&b);
        }
        if let Some(b) = self.get("/connections") {
            self.read_connections(&b, now);
        }
        self.read_open_group();
        true
    }

    /// The mode, from /configs: when asked to (cfg_again) or every SC_CFG_TTL_MS. A cut or
    /// failed answer, or anything but the three modes, leaves the last one.
    fn read_mode(&mut self, now: i64) {
        let due = self.cfg_ms == 0 || now - self.cfg_ms >= SC_CFG_TTL_MS;
        if self.cfg_again == 0 && !due {
            return;
        }
        self.cfg_again = (self.cfg_again - 1).max(0);
        self.cfg_ms = now;
        let Some(b) = self.get("/configs") else { return };
        if self.last_truncated {
            return;
        }
        if let Some(mode) = json::get(&b, "mode", 16).filter(|m| [&b"rule"[..], b"global", b"direct"].contains(&&m[..])) {
            self.mode_raw = text(&mode);
        }
    }

    /// /connections: the totals and the rate since the last read, then each connection.
    fn read_connections(&mut self, b: &[u8], now: i64) {
        let down = json::get_int(b, "downloadTotal", 0);
        let up = json::get_int(b, "uploadTotal", 0);
        self.traffic = trunc(&format!("↓{} ↑{}", size(down), size(up)), 48);
        let dt = now - self.prev_t;
        // a counter that went back (mihomo restarted) gives no rate this time
        if self.prev_t != 0 && dt > 0 && down >= self.prev_dl && up >= self.prev_ul {
            let per_s = |n: i64, was: i64| size((n - was) * 1000 / dt);
            self.speed = trunc(&format!("↓{}/s ↑{}/s", per_s(down, self.prev_dl), per_s(up, self.prev_ul)), 48);
        }
        (self.prev_dl, self.prev_ul, self.prev_t) = (down, up, now);
        self.conn_direct = 0;
        self.conn_proxy = 0;
        let list = find(b, b"\"connections\"", 0).and_then(|at| find_byte(b, b'[', at));
        let mut count = 0;
        if let Some(open) = list {
            // braces are counted as they come, strings included; only top-level objects count,
            // and ones too long to be a connection are not looked into
            let mut depth = 0i32;
            let mut start = None;
            for (i, &c) in b.iter().enumerate().skip(open).take_while(|&(_, &c)| c != 0) {
                match c {
                    b']' if depth == 0 => break,
                    b'{' => {
                        if depth == 0 {
                            count += 1;
                            start = Some(i);
                        }
                        depth += 1;
                    }
                    b'}' => {
                        depth -= 1;
                        let whole = if depth == 0 { start.take() } else { None };
                        if let Some(from) = whole.filter(|&from| i - from < 2047) {
                            self.count_connection(&b[from..=i]);
                        }
                    }
                    _ => {}
                }
            }
        }
        self.conns = format!("{count}{}", if self.last_truncated { "+" } else { "" });
        self.track_prune();
        self.pair_finish();
    }

    /// The open group: its nodes, whether it is an auto group with a pin, its choice and the
    /// chain behind that choice (a group often points at another group: up to 3 more hops).
    fn read_open_group(&mut self) {
        let Some(b) = self.get(&format!("/proxies/{}", path_escape(&self.group, 384))) else { return };
        let now = json::get(&b, "now", SC_NAME_MAX).map(|v| text(&v));
        if let Some(all) = json::get(&b, "all", 8192) {
            self.parse_all_array(&all);
        }
        self.fixed = pin_of(&b);
        if let Some(pin) = &self.fixed {
            let label = auto_row_label(pin, now.as_deref().unwrap_or_default(), lang::is_en());
            self.nodes.insert(0, label);
            self.nodes_raw.insert(0, String::new());
            self.node_delay.insert(0, -1);
        }
        let Some(mut at) = now else { return };
        let first = sanitize_name(&at, SC_NAME_MAX);
        self.node = first.clone();
        let mut chain = first;
        self.chain_raw = vec![at.clone()];
        let mut exit_pin = None;
        for hop in 0..3 {
            let Some(b) = self.get(&format!("/proxies/{}", path_escape(&at, 384))) else { break };
            if find(&b, b"\"all\"", 0).is_none() {
                break; // a node, not a group: the chain ends here
            }
            if hop == 0 {
                exit_pin = pin_of(&b).map(|p| sanitize_name(&p, SC_NAME_MAX)).map(|p| if p == "-" { String::new() } else { p });
            }
            let Some(next) = json::get(&b, "now", SC_NAME_MAX).map(|v| text(&v)).filter(|n| !n.is_empty() && *n != at) else { break };
            at = next;
            if self.chain_raw.len() < 4 {
                self.chain_raw.push(at.clone());
            }
            let _ = write!(chain, " → {}", sanitize_name(&at, SC_NAME_MAX));
            if chain.len() >= SC_CHAIN_STOP {
                break;
            }
        }
        self.chain = trunc(&chain, 160);
        // only the main group's chain says what 🚀 is pinned to
        if self.group == SC_MAIN_GROUP {
            self.exit_pin = exit_pin;
        }
    }

    /// One connection of /connections: its bytes into the (rule group, node) table, and
    /// whether it went direct.
    fn count_connection(&mut self, obj: &[u8]) {
        let (Some(chains), Some(id)) = (json::get(obj, "chains", 768), json::get(obj, "id", 40)) else { return };
        let node = chains_pick(&chains, false, SC_NAME_MAX);
        let grp = chains_pick(&chains, true, SC_NAME_MAX);
        let id = text(&id);
        if node.is_empty() || id.is_empty() {
            return;
        }
        let (up, down) = (json::get_int(obj, "upload", 0), json::get_int(obj, "download", 0));
        let (du, dd) = self.track_update(&id, up, down);
        if du != 0 || dd != 0 {
            self.pair_add(&grp, &node, du, dd);
        }
        // the first of chains is where it went out: DIRECT, or a proxy
        if node == "DIRECT" {
            self.conn_direct += 1;
        } else {
            self.conn_proxy += 1;
        }
    }

    // ---- what the panel reads ----
    pub fn core(&self) -> &'static str {
        if self.core_running { tr("运行中") } else { tr("已停止") }
    }
    pub fn mode_raw(&self) -> &str {
        &self.mode_raw
    }
    pub fn group(&self) -> String {
        sanitize_name(&self.group, SC_NAME_MAX)
    }
    pub fn node(&self) -> &str {
        or_dash(&self.node)
    }
    pub fn traffic(&self) -> &str {
        or_dash(&self.traffic)
    }
    pub fn speed(&self) -> &str {
        or_dash(&self.speed)
    }
    pub fn exit_pin(&self) -> Option<String> {
        self.exit_pin.clone()
    }
    pub fn chain(&self) -> &str {
        or_dash(&self.chain)
    }
    pub fn conns(&self) -> &str {
        if self.conns.is_empty() { "0" } else { &self.conns }
    }
    pub fn online(&self) -> bool {
        self.online
    }

    /// The exit state, luyaz-api chill.rs exit_state()'s rule: the mode, then in rule mode
    /// whether 「🚀 节点选择」 is DIRECT.
    pub fn exit_raw(&self) -> &'static str {
        match self.mode_raw.as_str() {
            "direct" => "direct_all",
            "global" => "global",
            "rule" => {
                if self.main_now == "DIRECT" {
                    "direct_keep_ai"
                } else {
                    "proxy"
                }
            }
            _ => "",
        }
    }

    pub fn mode(&self) -> &'static str {
        match self.exit_raw() {
            "proxy" => tr("代理"),
            "direct_keep_ai" => tr("直连 · AI 不动"),
            "direct_all" => tr("全部直连"),
            "global" => tr("全局"),
            _ => "-",
        }
    }

    /// The exit through luyaz-core (PUT /api/services/chill/exit); shown as asked until the next
    /// read says otherwise.
    pub fn set_exit(&mut self, agent: &mut CoreApi, state: &str) -> bool {
        if state.is_empty() {
            return false;
        }
        let code = agent.request_status("PUT", "/api/services/chill/exit", Some(&trunc(&format!("{{\"state\":\"{state}\"}}"), 48)));
        if !(200..300).contains(&code) {
            return false;
        }
        if state == "direct_all" {
            self.mode_raw = "direct".into();
        } else {
            self.mode_raw = "rule".into();
            if state == "direct_keep_ai" {
                self.main_now = "DIRECT".into();
            } else if self.main_now == "DIRECT" {
                self.main_now.clear();
            }
        }
        self.last_ms = 0;
        self.cfg_again = 2; // read the real mode back
        true
    }

    pub fn conn_split(&self) -> String {
        let line = cf!(tr("代理 %d · 直连 %d"), self.conn_proxy, self.conn_direct);
        bytes_cap(&line, 48)
    }

    /// Open group i. Its nodes and delays are read afresh, and a delay test still running for
    /// the group before is dropped.
    pub fn select_group(&mut self, i: usize) -> bool {
        let Some(name) = self.grp_raw.get(i).cloned() else { return false };
        self.group = name;
        self.grp_idx = i as i32;
        for list in [&mut self.nodes, &mut self.nodes_raw] {
            list.clear();
        }
        self.node_delay.clear();
        self.last_ms = 0;
        self.delay_cancel();
        true
    }

    pub fn groups(&self) -> Vec<chill_group_info_t> {
        (0..self.grp_raw.len())
            .map(|i| chill_group_info_t {
                name: trunc(&self.grp_disp[i], 64),
                selected: (i as i32 == self.grp_idx) as i32,
                auto_pick: (self.grp_type[i] != "Selector") as i32,
            })
            .collect()
    }

    pub fn nodes(&self) -> Vec<chill_node_info_t> {
        (0..self.nodes.len())
            .map(|i| {
                // auto groups: the tick marks the owner's choice (the 自动 row or the pinned node),
                // not whichever node mihomo is on right now
                let selected = match &self.fixed {
                    Some(f) => self.nodes_raw[i] == *f,
                    None => self.nodes[i] == self.node,
                };
                chill_node_info_t { name: trunc(&self.nodes[i], 64), delay: self.node_delay[i], selected: selected as i32 }
            })
            .collect()
    }

    pub fn top_pairs(&self) -> Vec<chill_traffic_item_t> {
        self.top_pair.clone()
    }

    /// Pick node i in the open group, without waiting for mihomo's answer.
    pub fn select_node(&mut self, i: usize) -> bool {
        let Some(raw) = self.nodes_raw.get(i).cloned() else { return false };
        let path = format!("/proxies/{}", path_escape(&self.group, 384));
        let auto_group = self.fixed.is_some();
        let sent = if auto_group && raw.is_empty() {
            // the 自动 row: dropping the pin lets mihomo choose again
            self.fire("DELETE", &path, "")
        } else {
            let body = trunc(&format!(r#"{{"name":"{raw}"}}"#), 256);
            let sent = self.fire("PUT", &path, &body);
            // A node picked by hand in the auto group 🚀 is on (手动优先's backup, or the owner's
            // region): it is a hand pick, so 🎯 gets it too. On the backup, 手动优先 sees the new
            // pick and goes back to 🎯 with it, instead of still reporting the old dead node.
            if sent && auto_group && self.group == self.main_now {
                let _ = self.fire("PUT", &format!("/proxies/{}", path_escape(SC_MANUAL_GROUP, 128)), &body);
            }
            sent
        };
        self.last_ms = 0;
        sent
    }

    pub fn profile_raw(&self) -> &str {
        &self.profile
    }
    pub fn profile_effective_raw(&self) -> &str {
        &self.profile_eff
    }
    pub fn thermal_eco(&self) -> bool {
        self.thermal_eco
    }

    /// eco / standard / perf through luyaz-core (in or out of eco restarts the core).
    pub fn set_profile(&mut self, agent: &mut CoreApi, p: &str) -> bool {
        if !valid_profile(p) {
            return false;
        }
        let code = agent.request_status("PUT", "/api/services/chill/profile", Some(&format!("{{\"profile\":\"{p}\"}}")));
        if !(200..300).contains(&code) {
            return false;
        }
        self.profile = p.into();
        if !self.thermal_eco {
            self.profile_eff = p.into();
        }
        self.profile_pending_ms = mono_ms();
        self.last_ms = 0;
        true
    }

    // ---- the group delay test, read in the background (the slowest node may take 3 s) ----
    fn delay_cancel(&mut self) {
        self.delay_fd = None;
        self.delay_buf.clear();
    }

    /// The answer maps node names to ms; a node it leaves out did not answer in time (mihomo
    /// only lists the ones that did) and gets 0.
    fn delay_apply(&mut self, b: &[u8]) {
        for (raw, delay) in self.nodes_raw.iter().zip(self.node_delay.iter_mut()) {
            if raw.is_empty() {
                continue; // the 自动 row has no delay of its own
            }
            let key = trunc(&format!("\"{raw}\""), SC_NAME_MAX + 4);
            let colon = find(b, key.as_bytes(), 0).and_then(|at| find_byte(b, b':', at + key.len()));
            *delay = colon.map_or(0, |c| json::atol(&b[c + 1..]) as i32);
        }
    }

    /// Take whatever of the delay answer has arrived; once the peer closes (or the buffer is
    /// full) apply it. A test that runs past SC_DELAY_MAX_MS is dropped.
    pub(super) fn delay_poll(&mut self) {
        let Some(fd) = self.delay_fd.as_ref().map(AsRawFd::as_raw_fd) else { return };
        let limit = SC_DELAY_BUF - 1;
        while self.delay_buf.len() < limit {
            let room = limit - self.delay_buf.len();
            let got = read_into(fd, &mut self.delay_buf, room);
            if got > 0 {
                continue;
            }
            let err = std::io::Error::last_os_error().raw_os_error();
            let would_block = got < 0 && err.is_some_and(|e| e == libc::EAGAIN || e == libc::EWOULDBLOCK);
            if !would_block {
                break; // closed, or failed: use what there is
            }
            let waited = mono_ms() - self.delay_t0;
            if waited > SC_DELAY_MAX_MS {
                eprintln!("chill: delay test gave up after {waited} ms");
                self.delay_cancel();
            }
            return;
        }
        // HTTP/1.1, so a long answer can be chunked: parse undoes that
        let resp = http::parse(&self.delay_buf);
        match resp.body.filter(|_| resp.status != 0) {
            Some(b) => {
                self.delay_apply(&b);
                eprintln!("chill: delay test done in {} ms ({} bytes)", mono_ms() - self.delay_t0, self.delay_buf.len());
            }
            None => eprintln!("chill: delay test got no HTTP response"),
        }
        self.delay_cancel();
    }

    pub fn delay_pending(&self) -> bool {
        self.delay_fd.is_some()
    }

    /// Start a delay test of the open group: 1 started, 2 one is still out, 0 it could not be
    /// sent.
    pub fn test_delay(&mut self) -> i32 {
        if self.delay_fd.is_some() {
            return 2;
        }
        let path = format!("/group/{}/delay?url=http%3A%2F%2Fwww.gstatic.com%2Fgenerate_204&timeout=3000", path_escape(&self.group, 384));
        let Some(conn) = self.dial() else { return 0 };
        if !http::send(conn.as_raw_fd(), self.request("GET", &path, None).as_bytes()) {
            return 0;
        }
        // SAFETY: fcntl on a descriptor we own
        unsafe {
            let flags = libc::fcntl(conn.as_raw_fd(), libc::F_GETFL, 0);
            libc::fcntl(conn.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        self.delay_fd = Some(conn);
        self.delay_buf.clear();
        self.delay_t0 = mono_ms();
        eprintln!("chill: delay test started for group {}", self.group);
        1
    }

    // ---- 家里出口 (E3) ----

    /// Read when the CHILL page is up: every second while a switch runs, else 5 s. true =
    /// something new to draw.
    pub fn home_poll(&mut self, agent: &mut CoreApi, active: bool, fast: bool) -> bool {
        let t = mono_ms();
        if !active {
            self.home_ms = 0;
            return false;
        }
        if self.home_ms != 0 && t - self.home_ms < if fast { 1000 } else { 5000 } {
            return false;
        }
        self.home_ms = t;
        let (code, b) = agent.api("GET", "/api/services/chill/home", None, 4096);
        let data = b.filter(|_| code == 200).and_then(|b| json::get(&b, "data", 3072));
        let Some(data) = data else {
            if self.home.known == 0 {
                return false;
            }
            let was = self.home.stale;
            self.home.stale = 1;
            return was == 0;
        };
        let h = home_parse(&data);
        if h == self.home {
            return false;
        }
        self.home = h;
        true
    }

    pub fn home(&self) -> &chill_home_t {
        &self.home
    }

    /// One item on or off (the second tap): the HTTP status, and the agent's code on 409.
    pub fn home_set(&mut self, agent: &mut CoreApi, item: usize, on: bool) -> (i32, String) {
        if item >= 3 {
            return (0, String::new());
        }
        // the user has seen 「会断一下、走漫游流量」: confirm
        let body = format!("{{\"item\":\"{}\",\"on\":{},\"confirm\":true}}", CH_KEYS[item], on);
        let (rc, b) = agent.api("PUT", "/api/services/chill/home", Some(&body), 1024);
        let code = match (&b, rc) {
            (Some(b), 409) => jstr(b, "code", 24),
            _ => String::new(),
        };
        self.home_ms = 0; // read the progress at once
        (rc, code)
    }

    /// 其余 two more hours: Some(the new end), None = not extended.
    pub fn home_extend(&mut self, agent: &mut CoreApi) -> Option<i64> {
        let (rc, b) = agent.api("PUT", "/api/services/chill/home", Some("{\"action\":\"extend\"}"), 512);
        let b = b.filter(|_| rc == 200)?;
        let until = json::get(&b, "data", 128).map_or(0, |d| json::get_int(&d, "until", 0));
        self.home_ms = 0;
        Some(until)
    }
}

#[cfg(test)]
mod tests;
