//! The CHILL panel's data (src/chill.c): mihomo's clash API on 127.0.0.1:9999 read directly
//! (the main group's pick, /configs at most every 30 s, /group, /connections, the node list and
//! the chain behind the pick), the node/group picks (sent without waiting), the group delay test
//! (its socket kept and read a little each round), the profile, and luyaz-core's 家里出口
//! (E3: GET/PUT /api/services/chill/home).
// SPDX-License-Identifier: MIT

use super::core_api::CoreApi;
use super::http::{self, read_into, Resp};
use super::util::{bytes_cap, find, find_byte, jget, jstr, mono_ms, text, trunc};
use crate::cf;
use crate::json;
use crate::lang::{self, tr};
use devui_model::{chill_group_info_t, chill_home_t, chill_node_info_t, chill_traffic_item_t};
use std::os::fd::{AsRawFd, OwnedFd};

/// Luya-Z D5: with the program (it was in /data/plugins/u60pro-devui)
const SC_CONF: &str = "/data/luyaz/ui/chill.conf";
const SC_IO_MS: i32 = 1500;
const SC_TTL_MS: i64 = 2000;
/// /configs is ~170 KB for one field (the TUN route exclusions): at most every 30 s
const SC_CFG_TTL_MS: i64 = 30000;
/// a manual group alone had 168 nodes (2026-09-22)
const SC_MAX_NODE: usize = 200;
const SC_NAME_MAX: usize = 96;
const SC_RESP_MAX: usize = 262144;
const SC_MAIN_GROUP: &str = "🚀 节点选择";
/// the card shows the top N (= the C UI's CHILL_TRAF_ROWS)
const SC_TOP_SHOW: usize = 6;
const SC_MAX_STAT_PAIR: usize = 200;
const SC_MAX_TRACK: usize = 256;
const SC_MAX_GRP: usize = 32;
const SC_MANUAL_GROUP: &str = "🎯 手动节点";
const SC_DELAY_MAX_MS: i64 = 8000;
const CHILL_STATE_FILE: &str = "/tmp/chill.state";

pub const CH_MEDIA: usize = 0;
pub const CH_IP: usize = 1;
pub const CH_REST: usize = 2;
const CH_KEYS: [&str; 3] = ["media", "ip", "rest"];

/// One (rule group, node) pair's bytes since this process started.
struct Pair {
    grp_raw: String,
    node_raw: String,
    up: i64,
    down: i64,
}

/// A connection's last counters, to take the difference each round.
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

/// UTF-8 code point at p: (cp, length); a broken sequence counts as one byte.
fn utf8_next(p: &[u8]) -> (u32, usize) {
    let b = |i: usize| p.get(i).copied().unwrap_or(0) as u32;
    let c = |i: usize| b(i) & 0xC0 == 0x80;
    if b(0) < 0x80 {
        return (b(0), usize::from(b(0) != 0));
    }
    if b(0) & 0xE0 == 0xC0 && c(1) {
        return ((b(0) & 0x1F) << 6 | (b(1) & 0x3F), 2);
    }
    if b(0) & 0xF0 == 0xE0 && c(1) && c(2) {
        return ((b(0) & 0x0F) << 12 | (b(1) & 0x3F) << 6 | (b(2) & 0x3F), 3);
    }
    if b(0) & 0xF8 == 0xF0 && c(1) && c(2) && c(3) {
        return ((b(0) & 0x07) << 18 | (b(1) & 0x3F) << 12 | (b(2) & 0x3F) << 6 | (b(3) & 0x3F), 4);
    }
    (b(0), 1)
}

fn cp_is_emoji(cp: u32) -> bool {
    (0x1F000..=0x1FAFF).contains(&cp)
        || (0x2600..=0x27BF).contains(&cp)
        || (0x2B00..=0x2BFF).contains(&cp)
        || (0xFE00..=0xFE0F).contains(&cp)
        || cp == 0x200D
}

/// A node or group name for the device fonts (chill_sanitize_name): a flag becomes its
/// country code (🇯🇵 → JP), other emoji go, runs of blanks shrink; a code the name repeats
/// right after is dropped; nothing left = "-". At most cap - 9 bytes, like C.
pub fn sanitize_name(input: &str, cap: usize) -> String {
    let p = input.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    let mut prev_space = true;
    while i < p.len() && p[i] != 0 && out.len() + 8 < cap {
        let (cp, len) = utf8_next(&p[i..]);
        if len == 0 {
            break;
        }
        if (0x1F1E6..=0x1F1FF).contains(&cp) {
            out.push(b'A' + (cp - 0x1F1E6) as u8);
            prev_space = false;
        } else if cp_is_emoji(cp) {
            // dropped
        } else if cp == ' ' as u32 {
            if !prev_space {
                out.push(b' ');
                prev_space = true;
            }
        } else {
            out.extend_from_slice(&p[i..(i + len).min(p.len())]);
            prev_space = false;
        }
        i += len;
    }
    while out.last() == Some(&b' ') {
        out.pop();
    }
    // the code the flag gave is often in the name already (🇯🇵 JP2-… → "JP JP2-…")
    let o = out.len();
    if o > 3 && out[2] == b' ' && out[0].is_ascii_uppercase() && out[1].is_ascii_uppercase() {
        let r0 = out.get(3).copied().unwrap_or(0);
        let r1 = out.get(4).copied().unwrap_or(0);
        if r0 & 0x80 == 0 && r1 & 0x80 == 0 && (r0 | 0x20) == (out[0] | 0x20) && (r1 | 0x20) == (out[1] | 0x20) {
            out.drain(..3);
        }
    }
    if out.is_empty() {
        return trunc("-", cap);
    }
    text(&out)
}

/// Percent-encode a group name for the request line (urlenc).
fn urlenc(input: &str, cap: usize) -> String {
    let mut o = String::new();
    for &c in input.as_bytes() {
        if o.len() + 4 >= cap {
            break;
        }
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
            o.push(c as char);
        } else {
            o.push_str(&format!("%{c:02X}"));
        }
    }
    o
}

fn human(v: i64) -> String {
    if v >= 1073741824 {
        text(&cf!("%.1fG", v as f64 / 1073741824.0))
    } else if v >= 1048576 {
        format!("{}M", v / 1048576)
    } else if v >= 1024 {
        format!("{}K", v / 1024)
    } else {
        format!("{v}B")
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

fn valid_profile(p: &str) -> bool {
    matches!(p, "eco" | "standard" | "perf")
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
    fn load_conf(&mut self) {
        if self.conf_loaded {
            return;
        }
        self.conf_loaded = true;
        let Ok(t) = std::fs::read(SC_CONF) else { return };
        for line in t.split(|&c| c == b'\n') {
            let line = text(line);
            let line = line.split(['\r', '\n']).next().unwrap_or("");
            if let Some(v) = line.strip_prefix("host=") {
                self.host = trunc(v, 64);
            } else if let Some(v) = line.strip_prefix("port=") {
                self.port = json::atol(v.as_bytes()) as u16;
            } else if let Some(v) = line.strip_prefix("secret=") {
                self.secret = trunc(v, 128);
            } else if let Some(v) = line.strip_prefix("group=") {
                self.group = trunc(v, SC_NAME_MAX);
            }
        }
    }

    fn auth(&self) -> String {
        if self.secret.is_empty() { String::new() } else { format!("Authorization: Bearer {}\r\n", self.secret) }
    }

    fn build_req(&self, method: &str, path: &str, body: Option<&str>) -> String {
        let r = match body {
            Some(b) => format!(
                "{method} {path} HTTP/1.1\r\nHost: {}:{}\r\n{}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                self.host,
                self.port,
                self.auth(),
                b.len()
            ),
            None => format!("{method} {path} HTTP/1.1\r\nHost: {}:{}\r\n{}Connection: close\r\n\r\n", self.host, self.port, self.auth()),
        };
        trunc(&r, 1024)
    }

    fn connect(&self) -> Option<OwnedFd> {
        http::connect_tcp(&self.host, self.port, SC_IO_MS)
    }

    /// The picks: sent, not waited for (a touch callback must not stall 1.5 s; the next poll
    /// draws what mihomo did).
    fn send_async(&self, method: &str, path: &str, body: &str) -> bool {
        let Some(fd) = self.connect() else { return false };
        http::send(fd.as_raw_fd(), self.build_req(method, path, Some(body)).as_bytes())
    }

    /// One request; the body (chunked decoded), None when there was no usable answer.
    fn get(&mut self, path: &str) -> Option<Vec<u8>> {
        self.last_truncated = false;
        let fd = self.connect()?;
        let r: Resp = http::exchange(fd, self.build_req("GET", path, None).as_bytes(), SC_RESP_MAX, SC_IO_MS);
        self.last_truncated = r.truncated;
        if r.status != 0 { r.body } else { None }
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
                traffic: trunc(&format!("↓{} ↑{}", human(p.down), human(p.up)), 40),
                bytes: p.up + p.down,
            })
            .collect();
    }

    /// The node names of an "all" array; a name keeps its measured delay across reads.
    fn parse_all_array(&mut self, arr: &[u8]) {
        let old: Vec<(String, i32)> = self.nodes_raw.iter().cloned().zip(self.node_delay.iter().copied()).collect();
        self.nodes.clear();
        self.nodes_raw.clear();
        self.node_delay.clear();
        let mut p = 0;
        while self.nodes.len() < SC_MAX_NODE {
            let Some(q) = find_byte(arr, b'"', p) else { break };
            let Some(e) = find_byte(arr, b'"', q + 1) else { break };
            let l = (e - q - 1).min(SC_NAME_MAX - 1);
            let raw = text(&arr[q + 1..q + 1 + l]);
            let raw = trunc(&raw, SC_NAME_MAX);
            self.nodes.push(sanitize_name(&raw, SC_NAME_MAX));
            self.node_delay.push(old.iter().find(|o| o.0 == raw).map_or(-1, |o| o.1));
            self.nodes_raw.push(raw);
            p = e + 1;
        }
    }

    /// /group: "name" then the next "type" per group; sorted by name (mihomo's order changes
    /// every call); the picked group found again by its name.
    fn parse_groups(&mut self, j: &[u8]) {
        let mut g: Vec<(String, String, String)> = Vec::new();
        let mut p = 0;
        while g.len() < SC_MAX_GRP {
            let Some(n) = find(j, b"\"name\":\"", p) else { break };
            let n = n + 8;
            let Some(e) = find_byte(j, b'"', n) else { break };
            let raw = text(&j[n..n + (e - n).min(SC_NAME_MAX - 1)]);
            let disp = sanitize_name(&raw, SC_NAME_MAX);
            let mut ty = String::new();
            if let Some(t) = find(j, b"\"type\":\"", e) {
                let t = t + 8;
                if let Some(te) = find_byte(j, b'"', t)
                    && te - t < 16
                {
                    ty = text(&j[t..te]);
                }
            }
            g.push((raw, disp, ty));
            p = e + 1;
        }
        g.sort_by(|a, b| (group_rank(&a.0), a.0.as_bytes()).cmp(&(group_rank(&b.0), b.0.as_bytes())));
        self.grp_raw = g.iter().map(|x| x.0.clone()).collect();
        self.grp_disp = g.iter().map(|x| x.1.clone()).collect();
        self.grp_type = g.into_iter().map(|x| x.2).collect();
        self.grp_idx = self.grp_raw.iter().position(|r| *r == self.group).map_or(-1, |i| i as i32);
        if self.grp_idx < 0 && !self.grp_raw.is_empty() {
            self.grp_idx = 0;
            self.group = self.grp_raw[0].clone();
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

    /// Read mihomo when `active` and the 2 s cache ran out (chill_poll). true = a read ran.
    pub fn poll(&mut self, active: bool) -> bool {
        let t = mono_ms();
        self.load_conf();
        self.delay_poll(); // every round, whatever the cache says
        if !active {
            self.last_ms = 0;
            self.cfg_ms = 0;
            return false;
        }
        if self.last_ms != 0 && t - self.last_ms < SC_TTL_MS {
            return false;
        }
        self.last_ms = t;
        self.read_profile_state(t);

        // the core is up: the main group's small answer (404 counts: a wrong name is not a
        // stopped core)
        let mpath = format!("/proxies/{}", urlenc(SC_MAIN_GROUP, 128));
        let Some(b) = self.get(&mpath) else {
            self.online = false;
            self.core_running = false;
            self.cfg_ms = 0; // the mode may have changed across a restart
            return true;
        };
        self.online = true;
        self.core_running = true;
        self.main_now = jget(&b, "now", SC_NAME_MAX);

        if self.cfg_again > 0 || self.cfg_ms == 0 || t - self.cfg_ms >= SC_CFG_TTL_MS {
            if self.cfg_again > 0 {
                self.cfg_again -= 1;
            }
            self.cfg_ms = t;
            // cut or failed: keep the last mode; only the three real values count
            if let Some(b) = self.get("/configs")
                && !self.last_truncated
                && let Some(m) = json::get(&b, "mode", 16)
                && matches!(&m[..], b"rule" | b"global" | b"direct")
            {
                self.mode_raw = text(&m);
            }
        }

        if let Some(b) = self.get("/group") {
            self.parse_groups(&b);
        }

        if let Some(b) = self.get("/connections") {
            let (cdl, cul) = (json::get_int(&b, "downloadTotal", 0), json::get_int(&b, "uploadTotal", 0));
            self.traffic = trunc(&format!("↓{} ↑{}", human(cdl), human(cul)), 48);
            let dt = t - self.prev_t;
            if self.prev_t != 0 && dt > 0 && cdl >= self.prev_dl && cul >= self.prev_ul {
                self.speed = trunc(&format!("↓{}/s ↑{}/s", human((cdl - self.prev_dl) * 1000 / dt), human((cul - self.prev_ul) * 1000 / dt)), 48);
            }
            self.prev_dl = cdl;
            self.prev_ul = cul;
            self.prev_t = t;
            let mut c = 0;
            self.conn_direct = 0;
            self.conn_proxy = 0;
            if let Some(p) = find(&b, b"\"connections\"", 0)
                && let Some(p) = find_byte(&b, b'[', p)
            {
                let mut depth = 0i32;
                let mut obj_start: Option<usize> = None;
                let mut q = p;
                while q < b.len() && b[q] != 0 {
                    match b[q] {
                        b'{' => {
                            if depth == 0 {
                                c += 1;
                                obj_start = Some(q);
                            }
                            depth += 1;
                        }
                        b'}' => {
                            depth -= 1;
                            if depth == 0
                                && let Some(s) = obj_start.take()
                            {
                                let obj = &b[s..=q];
                                if obj.len() < 2048 {
                                    self.count_connection(obj);
                                }
                            }
                        }
                        b']' if depth == 0 => break,
                        _ => {}
                    }
                    q += 1;
                }
            }
            self.conns = if self.last_truncated { format!("{c}+") } else { format!("{c}") };
            self.track_prune();
            self.pair_finish();
        }

        let path = format!("/proxies/{}", urlenc(&self.group, 384));
        if let Some(b) = self.get(&path) {
            let rawnow = json::get(&b, "now", SC_NAME_MAX).map(|v| text(&v));
            // this answer first: the chain reads below reuse the buffer in C
            if let Some(arr) = json::get(&b, "all", 8192) {
                self.parse_all_array(&arr);
            }
            self.fixed = find(&b, b"\"fixed\":\"", 0).map(|_| json::get(&b, "fixed", SC_NAME_MAX).map(|v| text(&v)).unwrap_or_default());
            if let Some(fixed) = &self.fixed {
                let now = rawnow.clone().unwrap_or_default();
                let label = auto_row_label(fixed, &now, lang::is_en());
                self.nodes.insert(0, label);
                self.nodes_raw.insert(0, String::new());
                self.node_delay.insert(0, -1);
            }
            if let Some(mut rawnow) = rawnow {
                self.node = sanitize_name(&rawnow, SC_NAME_MAX);
                // the main group often points at another group: follow "now" down 3 hops
                let disp = sanitize_name(&rawnow, SC_NAME_MAX);
                let mut chain = disp.clone();
                let mut o = chain.len();
                self.chain_raw = vec![rawnow.clone()];
                let mut exit_pin = None;
                for _ in 0..3 {
                    let p2 = format!("/proxies/{}", urlenc(&rawnow, 384));
                    let Some(b2) = self.get(&p2) else { break };
                    if find(&b2, b"\"all\"", 0).is_none() {
                        break; // not a group: the end
                    }
                    if self.chain_raw.len() == 1 {
                        exit_pin = find(&b2, b"\"fixed\":\"", 0)
                            .map(|_| sanitize_name(&json::get(&b2, "fixed", SC_NAME_MAX).map(|v| text(&v)).unwrap_or_default(), SC_NAME_MAX))
                            .map(|f| if f == "-" { String::new() } else { f });
                    }
                    let Some(nxt) = json::get(&b2, "now", SC_NAME_MAX).map(|v| text(&v)).filter(|v| !v.is_empty()) else { break };
                    if nxt == rawnow {
                        break; // points at itself
                    }
                    rawnow = nxt;
                    if self.chain_raw.len() < 4 {
                        self.chain_raw.push(rawnow.clone());
                    }
                    let hop = format!(" → {}", sanitize_name(&rawnow, SC_NAME_MAX));
                    o += hop.len();
                    chain.push_str(&hop);
                    if o >= 160 - 32 {
                        break;
                    }
                }
                self.chain = trunc(&chain, 160);
                self.exit_pin = if self.group == SC_MAIN_GROUP { exit_pin } else { self.exit_pin.take() };
            }
        }
        true
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

    // ---- getters ----
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
        if self.node.is_empty() { "-" } else { &self.node }
    }
    pub fn traffic(&self) -> &str {
        if self.traffic.is_empty() { "-" } else { &self.traffic }
    }
    pub fn speed(&self) -> &str {
        if self.speed.is_empty() { "-" } else { &self.speed }
    }
    pub fn exit_pin(&self) -> Option<String> {
        self.exit_pin.clone()
    }
    pub fn chain(&self) -> &str {
        if self.chain.is_empty() { "-" } else { &self.chain }
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
        bytes_cap(&cf!(tr("代理 %d · 直连 %d"), self.conn_proxy, self.conn_direct), 48)
    }

    pub fn select_group(&mut self, i: usize) -> bool {
        if i >= self.grp_raw.len() {
            return false;
        }
        self.grp_idx = i as i32;
        self.group = self.grp_raw[i].clone();
        // another group: the old nodes and delays are void
        self.nodes.clear();
        self.nodes_raw.clear();
        self.node_delay.clear();
        self.last_ms = 0;
        self.delay_cancel(); // a running test belongs to the old group
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

    pub fn select_node(&mut self, i: usize) -> bool {
        if i >= self.nodes_raw.len() {
            return false;
        }
        let path = format!("/proxies/{}", urlenc(&self.group, 384));
        // the 自动 row of an auto group: DELETE clears the pin, mihomo picks by itself again
        let ok = if self.fixed.is_some() && self.nodes_raw[i].is_empty() {
            self.send_async("DELETE", &path, "")
        } else {
            let body = trunc(&format!("{{\"name\":\"{}\"}}", self.nodes_raw[i]), 256);
            let ok = self.send_async("PUT", &path, &body);
            // A node picked by hand in the auto group 🚀 is on (手动优先's backup, or the owner's
            // region): it is a hand pick, so 🎯 gets it too. On the backup, 手动优先 sees the new
            // pick and goes back to 🎯 with it, instead of still reporting the old dead node.
            if ok && self.fixed.is_some() && self.group == self.main_now {
                let mpath = format!("/proxies/{}", urlenc(SC_MANUAL_GROUP, 128));
                let _ = self.send_async("PUT", &mpath, &body);
            }
            ok
        };
        self.last_ms = 0;
        ok
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

    // ---- the group delay test: asynchronous (the slowest node can take 3 s) ----
    fn delay_cancel(&mut self) {
        self.delay_fd = None;
        self.delay_buf.clear();
    }

    /// {"node":ms,…}; a node missing from it timed out (the core only answers the good ones).
    fn delay_apply(&mut self, b: &[u8]) {
        for i in 0..self.nodes_raw.len() {
            if self.nodes_raw[i].is_empty() {
                continue; // the 自动 row
            }
            let key = trunc(&format!("\"{}\"", self.nodes_raw[i]), SC_NAME_MAX + 4);
            self.node_delay[i] = match find(b, key.as_bytes(), 0) {
                None => 0,
                Some(p) => find_byte(b, b':', p + key.len()).map_or(0, |c| json::atol(&b[c + 1..]) as i32),
            };
        }
    }

    fn delay_poll(&mut self) {
        let Some(fd) = self.delay_fd.as_ref().map(|f| f.as_raw_fd()) else { return };
        loop {
            if self.delay_buf.len() + 1 >= 16384 {
                break; // full: take it as done
            }
            let room = 16384 - 1 - self.delay_buf.len();
            let n = read_into(fd, &mut self.delay_buf, room);
            if n > 0 {
                continue;
            }
            let e = std::io::Error::last_os_error().raw_os_error();
            if n < 0 && (e == Some(libc::EAGAIN) || e == Some(libc::EWOULDBLOCK)) {
                let waited = mono_ms() - self.delay_t0;
                if waited > SC_DELAY_MAX_MS {
                    eprintln!("chill: delay test gave up after {waited} ms");
                    self.delay_cancel();
                }
                return; // not all in yet
            }
            break; // 0 = closed; < 0 = an error
        }
        // HTTP/1.1: a big answer may come chunked
        let r = http::parse(&self.delay_buf);
        match r.body.filter(|_| r.status != 0) {
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

    /// 1 started, 2 the last one is still out, 0 could not send.
    pub fn test_delay(&mut self) -> i32 {
        if self.delay_fd.is_some() {
            return 2;
        }
        let Some(fd) = self.connect() else { return 0 };
        let req = trunc(
            &format!(
                "GET /group/{}/delay?url=http%3A%2F%2Fwww.gstatic.com%2Fgenerate_204&timeout=3000 HTTP/1.1\r\nHost: {}:{}\r\n{}Connection: close\r\n\r\n",
                urlenc(&self.group, 384),
                self.host,
                self.port,
                self.auth()
            ),
            1024,
        );
        if !http::send(fd.as_raw_fd(), req.as_bytes()) {
            return 0;
        }
        // SAFETY: fcntl on our fd
        unsafe {
            let f = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL, 0);
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, f | libc::O_NONBLOCK);
        }
        self.delay_fd = Some(fd);
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
