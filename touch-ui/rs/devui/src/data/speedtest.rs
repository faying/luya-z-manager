//! 测速 (src/speedtest.c): luyaz-core's own speed test engine (/api/speedtest/*), its progress
//! every second while the page or 出口 is up, the server list once.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::core_api::CoreApi;
use super::util::{bytes_cap, find, jget, jset, mono_ms, trunc};
use crate::cf;
use crate::json;
use crate::lang::{self, tr};
use devui_model::speedtest_server_t;

const ST_TTL_MS: i64 = 1000;
const ST_RESP_MAX: usize = 65536;
pub const ST_MAX_SERVERS: usize = 24;

pub const ST_IDLE: i32 = 0;
pub const ST_LATENCY: i32 = 1;
pub const ST_DOWNLOAD: i32 = 2;
pub const ST_UPLOAD: i32 = 3;
pub const ST_COMPLETE: i32 = 4;
pub const ST_CANCELLED: i32 = 5;
pub const ST_ERROR: i32 = 6;
pub const ST_DISCOVERING: i32 = 7;

pub struct Speedtest {
    online: bool,
    /// The progress was asked at least once (it is asked only while the speed page is open).
    checked: bool,
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
    servers_error: String,
}

impl Default for Speedtest {
    fn default() -> Self {
        Speedtest {
            online: false,
            checked: false,
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
            servers_error: String::new(),
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
        self.checked = true;
        let Some(data) = b.filter(|_| code == 200).and_then(|b| json::get(&b, "data", ST_RESP_MAX)) else {
            self.online = false;
            return true;
        };
        self.online = true;
        let ph = jget(&data, "phase", 16);
        self.phase = match ph.as_str() {
            "idle" => ST_IDLE,
            "discovering" => ST_DISCOVERING,
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
        let zh = jget(&data, "error", 256);
        let en = jget(&data, "error_en", 256);
        let en = if en == "null" { "" } else { &en };
        self.error = lang::pick(&zh, en).to_string();
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
            ST_DISCOVERING => tr("寻找节点…"),
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
        matches!(self.phase, ST_DISCOVERING | ST_LATENCY | ST_DOWNLOAD | ST_UPLOAD)
    }
    /// Not asked yet counts as reachable: the cellular page's row said 「服务不可用」 until the
    /// speed page had been opened once (10-18).
    pub fn reachable(&self) -> bool {
        self.online || !self.checked
    }

    /// true = started (the agent runs it in the background); refused: the agent's own words
    /// (e.g. 409 while a diagnosis runs) in start_error.
    pub fn start(&mut self, agent: &mut CoreApi) -> bool {
        let js = if self.selected >= 0 && (self.selected as usize) < self.srv.len() {
            format!("{{\"provider\":\"{}\",\"server_id\":{}}}", if self.srv[self.selected as usize].provider == "librespeed" { "librespeed" } else { "ookla" }, self.srv[self.selected as usize].id)
        } else {
            match self.selected { -2 => "{\"provider\":\"ookla\"}", -3 => "{\"provider\":\"librespeed\"}", _ => "{\"provider\":\"auto\"}" }.to_string()
        };
        let (code, b) = agent.api("POST", "/api/speedtest/start", Some(&js), ST_RESP_MAX);
        if code == 200 {
            self.phase = ST_DISCOVERING;
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

    /// The objects of "data":[…] (parse_servers): each bounded at 2048 bytes.
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
            let obj = &body[p..=end.min(p + 2047)];
            self.srv.push(speedtest_server_t {
                provider: jget(obj, "provider", 24),
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
        if !active || self.running() || t - self.srv_last_ms < 5000 {
            return false;
        }
        self.srv_last_ms = t;
        let (code, b) = agent.api("GET", "/api/speedtest/servers", None, ST_RESP_MAX);
        self.servers_error.clear();
        if code == 200 {
            let selected = self.srv.get(self.selected as usize).map(|s| (s.provider.clone(), s.id));
            if let Some(b) = b {
                self.parse_servers(&b);
                if self.srv.is_empty() { self.servers_error = tr("正在更新节点列表").into(); }
                if let Some((provider, id)) = selected {
                    self.selected = self.srv.iter().position(|s| s.id == id && s.provider == provider).map_or(-1, |i| i as i32);
                }
            }
        } else if let Some(b) = b {
            let zh = jget(&b, "error", 256);
            let en = jget(&b, "error_en", 256);
            self.servers_error = lang::pick(&zh, &en).to_string();
        } else { self.servers_error = tr("节点列表暂不可用，请重试").into(); }
        true
    }

    pub fn servers_error(&self) -> &str { &self.servers_error }

    pub fn servers(&self) -> &[speedtest_server_t] {
        &self.srv
    }
    pub fn selected(&self) -> i32 {
        self.selected
    }
    /// -1 = automatic (the agent picks)
    pub fn select_server(&mut self, i: i32) {
        self.selected = if (-3..=-1).contains(&i) || (i >= 0 && (i as usize) < self.srv.len()) { i } else { -1 };
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

#[cfg(test)]
mod provider_tests {
    use super::*;
    use std::io::{Read, Write};
    fn mock(body: &'static str) -> (CoreApi, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut api = CoreApi::default();
        api.config(listener.local_addr().unwrap().port());
        let thread = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0;4096];
            loop {
                let n = socket.read(&mut buf).unwrap();
                if n == 0 { break; }
                bytes.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&bytes);
                if let Some((headers, data)) = text.split_once("\r\n\r\n") {
                    let len:usize = headers.lines().find_map(|s| s.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
                    if data.len() >= len { break; }
                }
            }
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            String::from_utf8(bytes).unwrap()
        });
        (api,thread)
    }
    #[test]
    fn not_asked_yet_is_not_unavailable() {
        let mut speed = Speedtest::default();
        assert!(speed.reachable());
        speed.checked = true;
        assert!(!speed.reachable());
        speed.online = true;
        assert!(speed.reachable());
    }

    #[test]
    fn starting_provider_and_explicit_node_are_scoped_and_immediately_running() {
        let mut speed = Speedtest::default();
        speed.parse_servers(br#"{"data":[{"id":7,"provider":"librespeed","name":"Fallback","sponsor":"LibreSpeed","country":""}]}"#);
        for (selection, expected) in [(-1,"\"provider\":\"auto\""),(-2,"\"provider\":\"ookla\""),(-3,"\"provider\":\"librespeed\""),(0,"\"server_id\":7")] {
            speed.select_server(selection);
            let (mut api, thread) = mock(r#"{"ok":true,"data":{"status":"started"}}"#);
            assert!(speed.start(&mut api));
            assert!(speed.running());
            let request = thread.join().unwrap();
            assert!(request.starts_with("POST /v2/api/speedtest/start "));
            assert!(request.contains(expected), "{request}");
            if selection == 0 { assert!(request.contains("\"provider\":\"librespeed\"")); }
        }
    }
    #[test]
    fn provider_error_is_localized_while_core_is_reachable() {
        lang::set_en(true);
        let (mut api, thread) = mock(r#"{"ok":true,"data":{"phase":"error","error":"Ookla 服务无法连接","error_en":"Ookla speed-test service is unreachable","error_code":"provider_unreachable"}}"#);
        let mut speed = Speedtest::default();
        assert!(speed.poll(&mut api, true));
        thread.join().unwrap();
        assert!(speed.reachable());
        assert_eq!(speed.phase(), ST_ERROR);
        assert_eq!(speed.error(), "Ookla speed-test service is unreachable");
        assert!(!speed.running());
        lang::set_en(false);
    }
}
