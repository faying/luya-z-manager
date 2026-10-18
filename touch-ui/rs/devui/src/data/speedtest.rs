//! 测速 (src/speedtest.c): luyaz-core's own speed test engine (/api/speedtest/*), its progress
//! every second while the page or 出口 is up, the server list once.
// SPDX-License-Identifier: MIT

use super::core_api::CoreApi;
use super::util::{bytes_cap, find, jget, jset, mono_ms, trunc};
use crate::cf;
use crate::json;
use crate::lang::{self, tr};
use devui_model::speedtest_server_t;

const ST_TTL_MS: i64 = 1000;
const ST_RESP_MAX: usize = 4096;
pub const ST_MAX_SERVERS: usize = 24;

pub const ST_IDLE: i32 = 0;
pub const ST_LATENCY: i32 = 1;
pub const ST_DOWNLOAD: i32 = 2;
pub const ST_UPLOAD: i32 = 3;
pub const ST_COMPLETE: i32 = 4;
pub const ST_CANCELLED: i32 = 5;
pub const ST_ERROR: i32 = 6;

pub struct Speedtest {
    online: bool,
    last_ms: i64,
    phase: i32,
    progress: i32,
    live: f64,
    ping: f64,
    jitter: f64,
    dl: f64,
    ul: f64,
    server: String,
    error: String,
    start_err: String,
    srv: Vec<speedtest_server_t>,
    selected: i32,
    srv_last_ms: i64,
}

impl Default for Speedtest {
    fn default() -> Self {
        Speedtest {
            online: false,
            last_ms: 0,
            phase: ST_IDLE,
            progress: 0,
            live: 0.0,
            ping: -1.0,
            jitter: -1.0,
            dl: -1.0,
            ul: -1.0,
            server: String::new(),
            error: String::new(),
            start_err: String::new(),
            srv: Vec::new(),
            selected: -1,
            srv_last_ms: 0,
        }
    }
}

/// A number that may be null (Option<f64> in the agent): def then.
fn get_double(j: &[u8], key: &str, def: f64) -> f64 {
    match json::get(j, key, 32) {
        None => def,
        Some(v) if v == b"null" => def,
        Some(v) => json::atof(&v),
    }
}

impl Speedtest {
    /// The progress when active and a second has passed. true = a read ran.
    pub fn poll(&mut self, agent: &mut CoreApi, active: bool) -> bool {
        let t = mono_ms();
        if !active || t - self.last_ms < ST_TTL_MS {
            return false;
        }
        self.last_ms = t;
        let (code, b) = agent.api("GET", "/api/speedtest/progress", None, ST_RESP_MAX);
        let Some(data) = b.filter(|_| code == 200).and_then(|b| json::get(&b, "data", ST_RESP_MAX)) else {
            self.online = false;
            return true;
        };
        self.online = true;
        let ph = jget(&data, "phase", 16);
        self.phase = match ph.as_str() {
            "idle" => ST_IDLE,
            "latency" => ST_LATENCY,
            "download" => ST_DOWNLOAD,
            "upload" => ST_UPLOAD,
            "complete" => ST_COMPLETE,
            "cancelled" => ST_CANCELLED,
            "error" => ST_ERROR,
            _ => self.phase,
        };
        self.progress = json::get_int(&data, "progress", 0) as i32;
        self.live = get_double(&data, "live_speed_mbps", 0.0);
        self.ping = get_double(&data, "ping_ms", -1.0);
        self.jitter = get_double(&data, "jitter_ms", -1.0);
        self.dl = get_double(&data, "download_mbps", -1.0);
        self.ul = get_double(&data, "upload_mbps", -1.0);
        jset(&data, "server", 96, &mut self.server);
        jset(&data, "error", 96, &mut self.error);
        if self.error == "null" {
            self.error.clear();
        }
        true
    }

    pub fn phase(&self) -> i32 {
        self.phase
    }

    pub fn phase_label(&self) -> &'static str {
        match self.phase {
            ST_LATENCY => tr("测延迟中…"),
            ST_DOWNLOAD => tr("下载中…"),
            ST_UPLOAD => tr("上传中…"),
            ST_COMPLETE => tr("完成"),
            ST_CANCELLED => tr("已取消"),
            ST_ERROR => tr("出错"),
            _ => tr("空闲"),
        }
    }

    pub fn progress(&self) -> i32 {
        self.progress
    }
    pub fn live(&self) -> f64 {
        self.live
    }
    pub fn ping(&self) -> f64 {
        self.ping
    }
    pub fn jitter(&self) -> f64 {
        self.jitter
    }
    pub fn dl(&self) -> f64 {
        self.dl
    }
    pub fn ul(&self) -> f64 {
        self.ul
    }
    pub fn server(&self) -> &str {
        &self.server
    }
    pub fn error(&self) -> &str {
        &self.error
    }
    pub fn start_error(&self) -> &str {
        &self.start_err
    }
    pub fn running(&self) -> bool {
        matches!(self.phase, ST_LATENCY | ST_DOWNLOAD | ST_UPLOAD)
    }
    pub fn reachable(&self) -> bool {
        self.online
    }

    /// true = started (the agent runs it in the background); refused: the agent's own words
    /// (e.g. 409 while a diagnosis runs) in start_error.
    pub fn start(&mut self, agent: &mut CoreApi) -> bool {
        let js = if self.selected >= 0 && (self.selected as usize) < self.srv.len() {
            format!("{{\"server_id\":{}}}", self.srv[self.selected as usize].id)
        } else {
            "{}".to_string()
        };
        let (code, b) = agent.api("POST", "/api/speedtest/start", Some(&js), ST_RESP_MAX);
        if code == 200 {
            self.last_ms = 0; // read again at once
        }
        self.start_err.clear();
        if code != 200 {
            let (mut zh, mut en) = (String::new(), String::new());
            if code != 0
                && let Some(b) = &b
            {
                jset(b, "error", 160, &mut zh);
                jset(b, "error_en", 160, &mut en);
                if zh == "null" {
                    zh.clear();
                }
                if en == "null" {
                    en.clear();
                }
            }
            self.start_err = if !zh.is_empty() {
                trunc(lang::pick(&zh, &en), 160)
            } else if code != 0 {
                bytes_cap(&cf!(tr("被拒绝（HTTP %d）"), code), 160)
            } else {
                trunc(tr("连不上管理后台"), 160)
            };
        }
        code == 200
    }

    pub fn stop(&mut self, agent: &mut CoreApi) -> bool {
        let (code, _) = agent.api("POST", "/api/speedtest/stop", Some("{}"), ST_RESP_MAX);
        if code == 200 {
            self.last_ms = 0;
        }
        code == 200
    }

    /// The objects of "data":[…] (parse_servers): each cut at 399 bytes.
    fn parse_servers(&mut self, body: &[u8]) {
        let Some(a) = find(body, b"\"data\":[", 0) else { return };
        let mut p = a + 8;
        self.srv.clear();
        while p < body.len() && body[p] != b']' && self.srv.len() < ST_MAX_SERVERS {
            while p < body.len() && matches!(body[p], b',' | b' ' | b'\n' | b'\r' | b'\t') {
                p += 1;
            }
            if body.get(p) != Some(&b'{') {
                break;
            }
            let Some(end) = skip_object(body, p) else { break };
            let obj = &body[p..=end.min(p + 398)];
            self.srv.push(speedtest_server_t {
                id: json::get_int(obj, "id", 0),
                name: jget(obj, "name", 48),
                sponsor: jget(obj, "sponsor", 64),
                country: jget(obj, "country", 32),
            });
            p = end + 1;
        }
    }

    /// The server list once (the agent caches it 5 min). true = read now.
    pub fn servers_poll(&mut self, agent: &mut CoreApi, active: bool) -> bool {
        let t = mono_ms();
        if !active || !self.srv.is_empty() || t - self.srv_last_ms < ST_TTL_MS {
            return false;
        }
        self.srv_last_ms = t;
        let (code, b) = agent.api("GET", "/api/speedtest/servers", None, ST_RESP_MAX);
        let Some(b) = b.filter(|_| code == 200) else { return false };
        self.parse_servers(&b);
        true
    }

    pub fn servers(&self) -> &[speedtest_server_t] {
        &self.srv
    }
    pub fn selected(&self) -> i32 {
        self.selected
    }
    /// -1 = automatic (the agent picks)
    pub fn select_server(&mut self, i: i32) {
        self.selected = if i >= 0 && (i as usize) < self.srv.len() { i } else { -1 };
    }
}

/// The '}' closing the object at p (skip_object: escapes skipped inside strings).
fn skip_object(s: &[u8], mut p: usize) -> Option<usize> {
    let (mut depth, mut in_str) = (0i32, false);
    while p < s.len() && s[p] != 0 {
        let c = s[p];
        if in_str {
            if c == b'\\' && p + 1 < s.len() {
                p += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(p);
            }
        }
        p += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_and_numbers() {
        let mut s = Speedtest::default();
        s.parse_servers(br#"{"ok":true,"data":[{"id":7,"name":"Tokyo","sponsor":"IPA","country":"Japan"}, {"id":9,"name":"x\"}","sponsor":"S","country":"C"}]}"#);
        assert_eq!(s.servers().len(), 2);
        assert!(s.servers()[0].id == 7 && s.servers()[0].sponsor == "IPA" && s.servers()[1].name == "x\"}");
        s.select_server(5);
        assert_eq!(s.selected(), -1);
        s.select_server(1);
        assert_eq!(s.selected(), 1);
        assert_eq!(get_double(br#"{"a":null,"b":"12.5","c":3}"#, "a", -1.0), -1.0);
        assert_eq!(get_double(br#"{"a":null,"b":"12.5","c":3}"#, "b", -1.0), 12.5);
        assert_eq!(get_double(br#"{"a":null,"b":"12.5","c":3}"#, "c", -1.0), 3.0);
    }
}
